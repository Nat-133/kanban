//! Supervising an agent and reporting its own state.
//!
//! The controller cannot read agent-local detail once an agent runs off this
//! machine, so the agent reports upward instead: this module owns the write to
//! `state.yaml`, and the controller only ever consumes it.

use crate::agent::registry::Status;
use crate::controller::store;
use crate::model::{TaskId, WorkerEvent};
use std::path::Path;

/// Write this session's state if the status actually changed. Returns true when
/// it did, so the caller knows whether to ring the controller's doorbell.
///
/// `None` means the registry had no usable opinion, and the last state stands
/// rather than being replaced by a guess.
pub fn report(root: &Path, id: TaskId, status: Option<Status>) -> anyhow::Result<bool> {
    let Some(status) = status else { return Ok(false) };
    let kind = status.event_kind();
    if store::load_state(root, id)?.is_some_and(|prev| prev.kind == kind) {
        return Ok(false);
    }
    store::save_state(
        root,
        id,
        &WorkerEvent {
            kind,
            source: "claude-agents".to_string(),
            observed_at: time::OffsetDateTime::now_utc(),
            payload_ref: None,
        },
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::store;
    use crate::model::{TaskId, WorkerEventKind};

    fn workspace() -> (tempfile::TempDir, std::path::PathBuf, TaskId) {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        crate::controller::apply::apply(
            &root,
            crate::model::proto::Intent::CreateTask {
                text: "A".into(),
                column: "todo".parse().unwrap(),
            },
        )
        .unwrap();
        (d, root, TaskId::new(1))
    }

    #[test]
    fn a_new_status_is_written() {
        let (_d, root, id) = workspace();
        assert!(report(&root, id, Some(Status::Busy)).unwrap());
        assert_eq!(
            store::load_state(&root, id).unwrap().unwrap().kind,
            WorkerEventKind::Working
        );
    }

    #[test]
    fn an_unchanged_status_is_not_rewritten() {
        // Polling twice a second must not churn the file or wake every TUI.
        let (_d, root, id) = workspace();
        assert!(report(&root, id, Some(Status::Busy)).unwrap());
        assert!(!report(&root, id, Some(Status::Busy)).unwrap());
    }

    #[test]
    fn no_opinion_leaves_the_last_state_alone() {
        let (_d, root, id) = workspace();
        report(&root, id, Some(Status::Busy)).unwrap();
        assert!(!report(&root, id, None).unwrap());
        assert_eq!(
            store::load_state(&root, id).unwrap().unwrap().kind,
            WorkerEventKind::Working
        );
    }

    #[test]
    fn the_event_names_the_reporter() {
        let (_d, root, id) = workspace();
        report(&root, id, Some(Status::Idle)).unwrap();
        assert_eq!(store::load_state(&root, id).unwrap().unwrap().source, "claude-agents");
    }
}
