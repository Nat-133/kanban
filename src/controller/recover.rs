//! Liveness reconciliation for worker sessions.
//!
//! Agents report their own state, which leaves one thing they cannot report: an
//! agent killed outright — at machine shutdown, say — never gets to say it died,
//! and its `state.yaml` goes on claiming it is working. So the supervisor beats
//! while it lives, and this module is the level-triggered correction: any session
//! whose state claims it is running without a recent beat is marked interrupted.
//!
//! A beat rather than a terminal probe because it says whether the *agent* is
//! alive, not whether something is. A live tmux pane can hold a dead agent, and
//! an agent on another machine has no pane here at all.

use crate::controller::{events, store};
use crate::model::{Phase, WorkerEvent, WorkerEventKind};
use std::path::Path;

/// How long a session may go without a beat before we call it dead. Generous
/// against a loaded machine: the supervisor beats every five seconds.
const STALE_AFTER: time::Duration = time::Duration::seconds(30);

/// True for the phases that assert an agent is currently running. Only these are
/// worth checking: an agent is *expected* to be gone once the work finished, was
/// already found interrupted, or never started.
fn claims_to_be_running(phase: Phase) -> bool {
    match phase {
        Phase::Working | Phase::WaitingHuman | Phase::Idle => true,
        Phase::Pending | Phase::Completed | Phase::Interrupted | Phase::Failed => false,
    }
}

/// Mark every session that claims to be running without a recent beat as
/// interrupted. Level-triggered and idempotent: a session already recorded as
/// interrupted is skipped, so repeated sweeps neither rewrite state nor report
/// change. Returns true if any session was newly marked.
pub fn reconcile_liveness(root: &Path) -> anyhow::Result<bool> {
    let mut any = false;
    for session in store::load_all_sessions(root)? {
        let id = session.spec.task_ref;
        if !claims_to_be_running(events::session_phase(root, id)?) {
            continue;
        }
        let beat = store::read_heartbeat(root, id);
        let now = time::OffsetDateTime::now_utc();
        if beat.is_some_and(|b| now - b < STALE_AFTER) {
            continue;
        }
        tracing::warn!(task = %id, last_beat = ?beat,
            "agent has not beaten but its state claims it is running; marking interrupted");
        store::save_state(root, id, &interrupted_event())?;
        events::ingest_session(root, id)?;
        any = true;
    }
    Ok(any)
}

fn interrupted_event() -> WorkerEvent {
    WorkerEvent {
        kind: WorkerEventKind::Interrupted,
        source: "liveness-sweep".to_string(),
        observed_at: time::OffsetDateTime::now_utc(),
        payload_ref: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{apply::apply, handoff};
    use crate::model::proto::Intent;
    use crate::model::{TaskId, WorkerSession};

    /// A launcher that just records the launch; liveness no longer comes from it.
    struct Alive;
    impl handoff::Launcher for Alive {
        fn launch(&self, _s: &WorkerSession, _n: &str) -> anyhow::Result<()> { Ok(()) }
        fn kill(&self, _n: &str) {}
        fn is_alive(&self, _n: &str) -> bool { true }
    }

    /// Workspace with task 1 handed off and mid-work.
    fn working_session() -> (tempfile::TempDir, std::path::PathBuf, TaskId) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { text: "A".into(), column: "todo".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);
        handoff::handoff(&root, id, "claude", &Alive).unwrap();
        store::set_state_for_test(&root, id, WorkerEventKind::Working);
        events::ingest_session(&root, id).unwrap();
        (dir, root, id)
    }

    fn column_of(root: &Path, id: TaskId) -> Option<String> {
        let board = store::load_board(root).unwrap();
        board.cards().iter().find(|(_, v)| v.contains(&id)).map(|(c, _)| c.to_string())
    }


    /// Push a session's beat far enough into the past to read as dead.
    fn beat_long_ago(root: &Path, id: TaskId) {
        store::write_heartbeat(root, id,
            time::OffsetDateTime::now_utc() - time::Duration::seconds(120)).unwrap();
    }

    #[test]
    fn a_stale_heartbeat_marks_the_session_interrupted() {
        let (_d, root, id) = working_session();
        assert_eq!(events::session_phase(&root, id).unwrap(), Phase::Working);
        beat_long_ago(&root, id);

        let changed = reconcile_liveness(&root).unwrap();

        assert!(changed);
        assert_eq!(events::session_phase(&root, id).unwrap(), Phase::Interrupted);
        // Unfinished work: the card stays where it was, it does not advance.
        assert_eq!(column_of(&root, id).as_deref(), Some("doing"));
        assert_eq!(store::load_state(&root, id).unwrap().unwrap().source, "liveness-sweep");
    }

    #[test]
    fn a_fresh_heartbeat_is_left_alone() {
        let (_d, root, id) = working_session();
        // handoff beat on the agent's behalf, so this session is already fresh
        let changed = reconcile_liveness(&root).unwrap();
        assert!(!changed);
        assert_eq!(events::session_phase(&root, id).unwrap(), Phase::Working);
    }

    #[test]
    fn a_session_with_no_heartbeat_at_all_is_interrupted() {
        // A session whose machine died, or one predating the supervisor.
        let (_d, root, id) = working_session();
        std::fs::remove_file(store::heartbeat_path(&root, id)).unwrap();
        assert!(reconcile_liveness(&root).unwrap());
        assert_eq!(events::session_phase(&root, id).unwrap(), Phase::Interrupted);
    }

    #[test]
    fn an_unreadable_heartbeat_is_treated_as_dead() {
        let (_d, root, id) = working_session();
        std::fs::write(store::heartbeat_path(&root, id), "half-written").unwrap();
        assert!(reconcile_liveness(&root).unwrap());
    }

    #[test]
    fn liveness_is_idempotent_once_interrupted() {
        let (_d, root, id) = working_session();
        beat_long_ago(&root, id);
        assert!(reconcile_liveness(&root).unwrap());
        // Already interrupted -> nothing more to say, and no spurious wake.
        assert!(!reconcile_liveness(&root).unwrap());
    }

    #[test]
    fn a_completed_session_is_never_reopened_by_the_sweep() {
        // The human exited deliberately; the agent is gone by design and its beat
        // with it. The sweep must not drag a finished ticket back out of `done`.
        let (_d, root, id) = working_session();
        store::set_state_for_test(&root, id, WorkerEventKind::Completed);
        events::ingest_session(&root, id).unwrap();
        std::fs::remove_file(store::heartbeat_path(&root, id)).unwrap();

        assert!(!reconcile_liveness(&root).unwrap());
        assert_eq!(events::session_phase(&root, id).unwrap(), Phase::Completed);
        assert_eq!(column_of(&root, id).as_deref(), Some("done"));
    }

    #[test]
    fn session_never_handed_off_is_not_swept() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { text: "A".into(), column: "todo".parse().unwrap() }).unwrap();
        // No session.yaml at all -> Pending -> nothing to sweep.
        assert!(!reconcile_liveness(&root).unwrap());
    }

    #[test]
    fn archived_session_is_not_swept() {
        let (_d, root, id) = working_session();
        beat_long_ago(&root, id);
        apply(&root, Intent::ArchiveTask { task: id }).unwrap();
        assert!(!reconcile_liveness(&root).unwrap());
    }
}
