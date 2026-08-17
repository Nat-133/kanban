use crate::controller::store;
use crate::model::TaskId;
use std::path::{Path, PathBuf};

/// One marker file per subagent the session has spawned and not yet seen finish.
/// A marker per subagent rather than a counter because every hook firing is its
/// own short-lived process: create/unlink needs no lock, a read-modify-write
/// counter would lose concurrent updates.
fn dir(root: &Path, id: TaskId) -> PathBuf {
    store::session_dir(root, id).join("background")
}

/// Record that a subagent has started. Names follow the activity log's
/// `{nanos}-{pid}` scheme so markers are time-ordered and unique across
/// processes, with a per-process sequence number because the clock is not
/// guaranteed to tick between two spawns in the same process.
pub fn started(root: &Path, id: TaskId) -> anyhow::Result<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = dir(root, id);
    std::fs::create_dir_all(&d)?;
    let nanos = time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::fs::write(d.join(format!("{nanos:020}-{pid}-{seq}")), "")?;
    Ok(())
}

/// Record that a subagent has finished, retiring the oldest outstanding marker.
/// Nothing outstanding is a no-op: the count can never go negative, so a
/// `SubagentStop` we never saw start cannot make a live session look idle.
pub fn finished(root: &Path, id: TaskId) -> anyhow::Result<()> {
    if let Some(oldest) = markers(root, id)?.into_iter().min() {
        match std::fs::remove_file(dir(root, id).join(&oldest)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Drop every marker. Called when the human takes the turn back (a new prompt, a
/// fresh or resumed session): whatever was outstanding belongs to a turn that is
/// over, so keeping it would pin the card to "working" forever.
pub fn clear(root: &Path, id: TaskId) -> anyhow::Result<()> {
    match std::fs::remove_dir_all(dir(root, id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// True while at least one spawned subagent has not reported back.
pub fn pending(root: &Path, id: TaskId) -> bool {
    !markers(root, id).unwrap_or_default().is_empty()
}

fn markers(root: &Path, id: TaskId) -> anyhow::Result<Vec<String>> {
    let d = dir(root, id);
    if !d.exists() {
        return Ok(Vec::new());
    }
    Ok(std::fs::read_dir(&d)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        std::fs::create_dir_all(&root).unwrap();
        (dir, root)
    }

    #[test]
    fn nothing_is_pending_before_a_subagent_starts() {
        let (_d, root) = root();
        assert!(!pending(&root, TaskId::new(1)));
    }

    #[test]
    fn a_started_subagent_is_pending_until_it_finishes() {
        let (_d, root) = root();
        let id = TaskId::new(1);
        started(&root, id).unwrap();
        assert!(pending(&root, id));
        finished(&root, id).unwrap();
        assert!(!pending(&root, id));
    }

    #[test]
    fn two_subagents_stay_pending_until_both_finish() {
        let (_d, root) = root();
        let id = TaskId::new(1);
        started(&root, id).unwrap();
        started(&root, id).unwrap();
        finished(&root, id).unwrap();
        assert!(pending(&root, id), "one subagent is still running");
        finished(&root, id).unwrap();
        assert!(!pending(&root, id));
    }

    #[test]
    fn finishing_with_nothing_outstanding_is_a_no_op() {
        let (_d, root) = root();
        let id = TaskId::new(1);
        finished(&root, id).unwrap();
        started(&root, id).unwrap();
        assert!(pending(&root, id), "the stray finish must not have gone negative");
    }

    #[test]
    fn clear_drops_everything_outstanding() {
        let (_d, root) = root();
        let id = TaskId::new(1);
        started(&root, id).unwrap();
        started(&root, id).unwrap();
        clear(&root, id).unwrap();
        assert!(!pending(&root, id));
        clear(&root, id).unwrap();
    }

    #[test]
    fn sessions_do_not_share_pending_state() {
        let (_d, root) = root();
        started(&root, TaskId::new(1)).unwrap();
        assert!(!pending(&root, TaskId::new(2)));
    }
}
