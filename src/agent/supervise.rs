//! Supervising an agent and reporting its own state.
//!
//! The controller cannot read agent-local detail once an agent runs off this
//! machine, so the agent reports upward instead: this module owns the write to
//! `state.yaml`, and the controller only ever consumes it.

use crate::agent::registry::Status;
use crate::controller::store;
use crate::model::{TaskId, WorkerEvent};
use std::path::Path;

/// How often to re-read the registry. Fast enough that a card's icon tracks the
/// pane, cheap enough to be irrelevant: one small file read.
const POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// How often to beat. Well inside the controller's staleness threshold, so a
/// loaded machine missing a beat or two doesn't read as a dead agent.
const BEAT: std::time::Duration = std::time::Duration::from_secs(5);

/// Map the child's exit to a terminal event. `None` = killed by a signal.
pub fn terminal_kind(code: Option<i32>) -> crate::model::WorkerEventKind {
    use crate::model::WorkerEventKind as K;
    match code {
        Some(0) => K::Completed,
        Some(_) => K::Failed,
        None => K::Interrupted,
    }
}

/// Run `command` as a child, reporting its state until it exits. Returns the
/// code this process should exit with, so the supervisor stays transparent.
///
/// stdio is inherited and the child stays in our process group, so the pane
/// behaves exactly as it did when tmux ran the agent directly: resize, Ctrl-C
/// and job control all reach the child without us forwarding anything.
pub fn run(root: &Path, id: TaskId, command: &[String]) -> anyhow::Result<i32> {
    let (exe, args) =
        command.split_first().ok_or_else(|| anyhow::anyhow!("empty agent command"))?;
    // Beat before spawning: from here on, no heartbeat means no agent.
    store::write_heartbeat(root, id, time::OffsetDateTime::now_utc())?;
    let mut child = std::process::Command::new(exe).args(args).spawn()?;
    let pid = child.id() as i32;

    let dir = crate::agent::registry::registry_dir();
    if dir.is_none() {
        tracing::warn!("no claude session registry path; live state will not update");
    }
    let mut last_beat = std::time::Instant::now();
    let mut have_session_id = false;

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if let Some(dir) = &dir {
            let record = crate::agent::registry::read(dir, pid);
            if report(root, id, record.as_ref().and_then(|r| r.status))? {
                crate::controller::server::poke_blocking(root, id);
            }
            if !have_session_id {
                if let Some(sid) = record.and_then(|r| r.session_id) {
                    have_session_id = record_session_id(root, id, &sid).is_ok();
                }
            }
        }
        if last_beat.elapsed() >= BEAT {
            let _ = store::write_heartbeat(root, id, time::OffsetDateTime::now_utc());
            last_beat = std::time::Instant::now();
        }
        std::thread::sleep(POLL);
    };

    let code = status.code();
    store::save_state(
        root,
        id,
        &WorkerEvent {
            kind: terminal_kind(code),
            source: "claude-agents".to_string(),
            observed_at: time::OffsetDateTime::now_utc(),
            payload_ref: None,
        },
    )?;
    // Drop the beat so a supervisor that exits cleanly isn't later reported dead
    // by a stale one.
    let _ = std::fs::remove_file(store::heartbeat_path(root, id));
    crate::controller::server::poke_blocking(root, id);

    use std::os::unix::process::ExitStatusExt as _;
    Ok(code.unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

/// Persist the agent's own session id, which every later resume needs. Comes
/// from the registry, so no hook has to report it.
fn record_session_id(root: &Path, id: TaskId, session_id: &str) -> anyhow::Result<()> {
    let Some(mut session) = store::load_session(root, id)? else {
        anyhow::bail!("no session record for {id}");
    };
    if session.status.session_id.as_deref() != Some(session_id) {
        session.status.session_id = Some(session_id.to_string());
        store::save_session(root, &session)?;
    }
    Ok(())
}

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
    fn exit_status_decides_the_terminal_state() {
        use crate::model::WorkerEventKind as K;
        // A clean exit is the human quitting deliberately: the work is finished.
        assert_eq!(terminal_kind(Some(0)), K::Completed);
        // Anything else — a crash, a kill, a torn-down pane — is unfinished work
        // and must never advance a card to done.
        assert_eq!(terminal_kind(Some(1)), K::Failed);
        assert_eq!(terminal_kind(None), K::Interrupted);
    }

    #[test]
    fn the_event_names_the_reporter() {
        let (_d, root, id) = workspace();
        report(&root, id, Some(Status::Idle)).unwrap();
        assert_eq!(store::load_state(&root, id).unwrap().unwrap().source, "claude-agents");
    }
}
