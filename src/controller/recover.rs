//! Liveness reconciliation for worker sessions.
//!
//! Agents run as detached terminal sessions, so the controller never holds a
//! child handle and never sees an exit code — everything it knows arrives via
//! hooks. When the machine shuts down the agent is killed outright and no hook
//! fires, leaving a session whose `state.yaml` still claims it is working. This
//! module is the level-triggered correction: probe whether the terminal session
//! is actually alive, and mark the dead ones interrupted.

use crate::controller::{events, handoff::Launcher, store};
use crate::model::{Phase, WorkerEvent, WorkerEventKind};
use std::path::Path;

/// True for the phases that assert an agent is currently running. Only these are
/// worth probing: a terminal session is *expected* to be gone once the work
/// finished, was already found interrupted, or never started.
fn claims_to_be_running(phase: Phase) -> bool {
    match phase {
        Phase::Working | Phase::WaitingHuman | Phase::Idle => true,
        Phase::Pending | Phase::Completed | Phase::Interrupted | Phase::Failed => false,
    }
}

/// Probe every session that claims to be running and mark the dead ones
/// interrupted. Level-triggered and idempotent: a session already recorded as
/// interrupted is skipped, so repeated sweeps neither rewrite state nor report
/// change. Returns true if any session was newly marked.
pub fn reconcile_liveness(root: &Path, launcher: &dyn Launcher) -> anyhow::Result<bool> {
    let mut any = false;
    for session in store::load_all_sessions(root)? {
        let id = session.spec.task_ref;
        // No terminal session name means nothing we can probe; leave it be rather
        // than guessing it is dead.
        let Some(name) = session.spec.session_name.as_deref() else { continue };
        if !claims_to_be_running(events::session_phase(root, id)?) {
            continue;
        }
        if launcher.is_alive(name) {
            continue;
        }
        tracing::warn!(task = %id, session = %name,
            "worker session is gone but its state claims it is running; marking interrupted");
        store::save_state(root, id, &interrupted_event())?;
        events::ingest_session(root, id)?;
        any = true;
    }
    Ok(any)
}

fn interrupted_event() -> WorkerEvent {
    WorkerEvent {
        kind: WorkerEventKind::Interrupted,
        source: "liveness-probe".to_string(),
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

    /// A launcher whose sessions are all dead (as after a laptop shutdown).
    struct Dead;
    impl handoff::Launcher for Dead {
        fn launch(&self, _s: &WorkerSession, _n: &str) -> anyhow::Result<()> { Ok(()) }
        fn kill(&self, _n: &str) {}
        fn is_alive(&self, _n: &str) -> bool { false }
    }

    /// A launcher whose sessions are all still running.
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
        events::record_state(&root, id, "user-prompt-submit", "{}").unwrap();
        events::ingest_session(&root, id).unwrap();
        (dir, root, id)
    }

    fn column_of(root: &Path, id: TaskId) -> Option<String> {
        let board = store::load_board(root).unwrap();
        board.cards().iter().find(|(_, v)| v.contains(&id)).map(|(c, _)| c.to_string())
    }

    #[test]
    fn dead_terminal_session_marks_the_session_interrupted() {
        let (_d, root, id) = working_session();
        assert_eq!(events::session_phase(&root, id).unwrap(), Phase::Working);

        let changed = reconcile_liveness(&root, &Dead).unwrap();

        assert!(changed);
        assert_eq!(events::session_phase(&root, id).unwrap(), Phase::Interrupted);
        // Unfinished work: the card stays where it was, it does not advance.
        assert_eq!(column_of(&root, id).as_deref(), Some("doing"));
        assert_eq!(store::load_state(&root, id).unwrap().unwrap().source, "liveness-probe");
    }

    #[test]
    fn live_terminal_session_is_left_alone() {
        let (_d, root, id) = working_session();
        let changed = reconcile_liveness(&root, &Alive).unwrap();
        assert!(!changed);
        assert_eq!(events::session_phase(&root, id).unwrap(), Phase::Working);
    }

    #[test]
    fn liveness_is_idempotent_once_interrupted() {
        let (_d, root, _id) = working_session();
        assert!(reconcile_liveness(&root, &Dead).unwrap());
        // Already interrupted -> nothing more to say, and no spurious wake.
        assert!(!reconcile_liveness(&root, &Dead).unwrap());
    }

    #[test]
    fn a_completed_session_is_never_reopened_by_the_probe() {
        // The human exited deliberately; its terminal session is gone by design.
        // The probe must not drag a finished ticket back out of `done`.
        let (_d, root, id) = working_session();
        events::record_state(&root, id, "session-end", "{\"reason\":\"prompt_input_exit\"}").unwrap();
        events::ingest_session(&root, id).unwrap();

        assert!(!reconcile_liveness(&root, &Dead).unwrap());
        assert_eq!(events::session_phase(&root, id).unwrap(), Phase::Completed);
        assert_eq!(column_of(&root, id).as_deref(), Some("done"));
    }

    #[test]
    fn session_never_handed_off_is_not_probed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { text: "A".into(), column: "todo".parse().unwrap() }).unwrap();
        // No session.yaml at all -> Pending -> nothing to probe.
        assert!(!reconcile_liveness(&root, &Dead).unwrap());
    }

    #[test]
    fn archived_session_is_not_probed() {
        let (_d, root, id) = working_session();
        apply(&root, Intent::ArchiveTask { task: id }).unwrap();
        assert!(!reconcile_liveness(&root, &Dead).unwrap());
    }
}
