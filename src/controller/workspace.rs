use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Serialized access to the workspace tree.
///
/// Every mutation is a read-modify-write of a whole YAML document — above all
/// `board.yaml`, which the intent handler, the wake handler, the reconcile tick
/// and the liveness sweep all rewrite in full. `store::atomic_write` makes each
/// individual write all-or-nothing, but that is not the same as making the
/// read and the write one step: two writers that both load, then both save,
/// leave only the later one's copy, silently discarding the other's edit.
///
/// So the guard has to span the entire operation, which is why the root is only
/// reachable inside a closure — a caller cannot hold a `&Path` across the point
/// where the lock is released, and cannot reach the store without taking the
/// lock at all.
///
/// One lock covers the whole tree. Every mutation touches `board.yaml`, so
/// finer granularity would serialize on that node anyway while adding
/// lock-ordering and upgrade-deadlock hazards. Reads do run concurrently: the
/// TUI re-fetches a full snapshot on every change event, and those must not
/// queue behind each other.
#[derive(Clone)]
pub struct Workspace {
    root: Arc<PathBuf>,
    lock: Arc<RwLock<()>>,
}

impl Workspace {
    pub fn new(root: PathBuf) -> Self {
        Workspace { root: Arc::new(root), lock: Arc::new(RwLock::new(())) }
    }

    /// Run `f` with shared access. Concurrent with other readers, excluded by a
    /// writer. `f` must not mutate the workspace.
    pub fn read<T>(&self, f: impl FnOnce(&Path) -> T) -> T {
        // A panic inside one operation says nothing about the next one's ability
        // to read the tree, and wedging the daemon on it would turn a single
        // failed request into a dead board.
        let _guard = self.lock.read().unwrap_or_else(|e| e.into_inner());
        f(&self.root)
    }

    /// Run `f` with exclusive access. Hold this across the whole read-modify-write.
    pub fn write<T>(&self, f: impl FnOnce(&Path) -> T) -> T {
        let _guard = self.lock.write().unwrap_or_else(|e| e.into_inner());
        f(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::store;
    use crate::model::proto::Intent;
    use crate::model::{Board, RawBoard, TaskId};
    use std::sync::mpsc;
    use std::time::Duration;

    fn setup() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        (dir, Workspace::new(root))
    }

    /// The read-modify-write every real writer performs, with a hook to widen the
    /// window between the load and the save. Inlined rather than driven through
    /// `apply`, because the interleaving that loses an update happens *inside*
    /// that span and can't be provoked from outside it.
    fn drop_card(root: &Path, id: TaskId, before_save: impl FnOnce()) {
        let mut raw: RawBoard = store::load_board(root).unwrap().into();
        before_save();
        for v in raw.spec.cards.values_mut() {
            v.retain(|t| *t != id);
        }
        store::save_board(root, &Board::try_from(raw).unwrap()).unwrap();
    }

    #[test]
    fn concurrent_writers_do_not_lose_each_others_board_edits() {
        let (_d, ws) = setup();
        ws.write(|r| {
            for t in ["A", "B"] {
                crate::controller::apply::apply(r, Intent::CreateTask {
                    text: t.into(), column: "todo".parse().unwrap() }).unwrap();
            }
        });

        let (entered_tx, entered_rx) = mpsc::channel();
        let slow = {
            let ws = ws.clone();
            std::thread::spawn(move || {
                ws.write(|r| drop_card(r, TaskId::new(1), || {
                    entered_tx.send(()).unwrap();
                    // Long enough that the second writer would certainly have
                    // loaded the board by now, were it able to.
                    std::thread::sleep(Duration::from_millis(200));
                }))
            })
        };

        entered_rx.recv().unwrap(); // the slow writer is inside its critical section
        ws.write(|r| drop_card(r, TaskId::new(2), || {}));
        slow.join().unwrap();

        // Unlocked, the second writer loads a board that still holds card 1 and
        // saves it back over the first writer's removal.
        let board = ws.read(|r| store::load_board(r).unwrap());
        assert!(board.cards().values().all(|v| v.is_empty()), "both removals must survive: {board:?}");
    }

    #[test]
    fn readers_run_concurrently() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (_d, ws) = setup();
        let other_read = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let held = {
            let (ws, other_read) = (ws.clone(), other_read.clone());
            std::thread::spawn(move || ws.read(|_| {
                tx.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(200));
                // An exclusive `read` would have kept the second reader out for
                // the whole sleep, leaving this false.
                assert!(other_read.load(Ordering::SeqCst), "a second reader must not queue behind this one");
            }))
        };
        rx.recv().unwrap();
        ws.read(|r| {
            store::load_board(r).unwrap();
            other_read.store(true, Ordering::SeqCst);
        });
        held.join().unwrap();
    }
}
