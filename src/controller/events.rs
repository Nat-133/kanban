use crate::controller::store;
use crate::model::{Notification, Phase, TaskId, WorkerEvent, WorkerEventKind};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One captured hook event. `payload` is Claude's raw event JSON. Used only as an
/// in-memory parse target while mapping a hook firing to a worker event; it is
/// never persisted (the session's `state.yaml` holds the mapped event instead).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntakePayload {
    pub event: String,
    pub payload: serde_json::Value,
}

/// Map a captured hook firing to a worker event. None = not tracked (e.g. a
/// non-permission/idle Notification like auth_success), in which case the
/// session's state is left untouched.
fn to_event(item: &IntakePayload) -> Option<WorkerEvent> {
    let kind = match item.event.as_str() {
        "session-start" => WorkerEventKind::Started,
        "user-prompt-submit" => WorkerEventKind::Working,
        // Stop fires when claude finishes its turn and hands control back to the
        // human — it is waiting for a response, not working. Treat it as idle so
        // the card flips to the warning, not the spinner.
        "stop" => WorkerEventKind::HumanInputRequired(Notification::IdlePrompt),
        "session-end" => WorkerEventKind::Completed,
        "stop-failure" => WorkerEventKind::Failed,
        "notification" => match item.payload.get("notification_type").and_then(|v| v.as_str()) {
            Some("permission_prompt") => WorkerEventKind::HumanInputRequired(Notification::PermissionPrompt),
            Some("idle_prompt") => WorkerEventKind::HumanInputRequired(Notification::IdlePrompt),
            _ => return None,
        },
        _ => return None,
    };
    Some(WorkerEvent {
        kind,
        source: "claude-code-hook".to_string(),
        observed_at: time::OffsetDateTime::now_utc(),
        payload_ref: None,
    })
}

/// Record a hook firing into the session's state file, overwriting the previous
/// state. `raw_payload` is Claude's stdin JSON (treated as a bare string if not
/// valid JSON). Returns true when the event was tracked and the state changed —
/// the caller uses this to decide whether to wake the controller. Untracked
/// firings leave the existing state in place and return false.
pub fn record_state(root: &Path, id: TaskId, event: &str, raw_payload: &str) -> anyhow::Result<bool> {
    let payload: serde_json::Value = serde_json::from_str(raw_payload)
        .unwrap_or_else(|_| serde_json::Value::String(raw_payload.to_string()));
    let item = IntakePayload { event: event.to_string(), payload };
    match to_event(&item) {
        Some(ev) => {
            store::save_state(root, id, &ev)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// The phase a session is currently in, derived from its state file. A session
/// with no state file yet is `Pending`.
pub fn session_phase(root: &Path, id: TaskId) -> anyhow::Result<Phase> {
    let events: Vec<WorkerEvent> = store::load_state(root, id)?.into_iter().collect();
    Ok(crate::controller::derive::derive(&events))
}

/// Board column for a derived phase. The board has only the workflow stages
/// (todo / doing / done); the card's *icon* conveys the live agent state
/// (spinner while working, warning when it needs a human). So every active
/// phase maps to `doing` and only a clean completion advances to `done`.
/// None = leave the card where it is.
fn phase_column(phase: Phase) -> Option<&'static str> {
    match phase {
        Phase::Working | Phase::WaitingHuman | Phase::Idle | Phase::Failed => Some("doing"),
        Phase::Completed => Some("done"),
        Phase::Pending => None,
    }
}

/// Re-read a session's state and move its card to match. Level-triggered and
/// idempotent: returns true only when the card actually changed column, so
/// repeated reconciles/wakes don't spuriously report change.
pub fn ingest_session(root: &Path, id: TaskId) -> anyhow::Result<bool> {
    // Never move an archived (or vanished) task's card onto the board: a lingering
    // session firing events would otherwise resurrect it.
    let archived = store::load_task(root, id).map(|t| t.status.archived).unwrap_or(true);
    if archived {
        return Ok(false);
    }
    let phase = session_phase(root, id)?;
    match phase_column(phase) {
        Some(col) => move_card_to(root, id, col),
        None => Ok(false),
    }
}

/// Move `id` to `column` if it isn't already there. Returns true iff the board changed.
fn move_card_to(root: &Path, id: TaskId, column: &str) -> anyhow::Result<bool> {
    use crate::model::{Board, RawBoard};
    let col = column.parse().map_err(|_| anyhow::anyhow!("bad column id: {column}"))?;
    let mut raw: RawBoard = store::load_board(root)?.into();
    // Already in the target column with nothing else to do -> no-op.
    if raw.spec.cards.get(&col).is_some_and(|v| v.contains(&id)) {
        return Ok(false);
    }
    for v in raw.spec.cards.values_mut() {
        v.retain(|t| *t != id);
    }
    raw.spec.cards.entry(col).or_default().push(id);
    let board = Board::try_from(raw).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    store::save_board(root, &board)?;
    Ok(true)
}

/// A hash of everything the UI can observe: the board's card layout plus every
/// session's derived phase. The reconcile loop broadcasts whenever this changes,
/// so phase-only transitions (e.g. working -> idle, both in `doing`) still push an
/// update even though no card moved column.
pub fn observable_fingerprint(root: &Path) -> anyhow::Result<u64> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // board card layout (deterministic: columns are ordered, cards is a BTreeMap)
    serde_yml::to_string(&store::load_board(root)?)?.hash(&mut h);
    // each session's phase, in a stable order
    let mut sessions = store::load_all_sessions(root)?;
    sessions.sort_by_key(|s| s.spec.task_ref.as_u32());
    for s in &sessions {
        s.spec.task_ref.as_u32().hash(&mut h);
        format!("{:?}", session_phase(root, s.spec.task_ref)?).hash(&mut h);
    }
    Ok(h.finish())
}

/// Re-read every session's state and reconcile its card. Returns true if any
/// card moved.
pub fn reconcile_all(root: &Path) -> anyhow::Result<bool> {
    let sessions = root.join("sessions");
    let mut any = false;
    if sessions.exists() {
        for e in std::fs::read_dir(&sessions)? {
            let name = e?.file_name().to_string_lossy().into_owned();
            if let Ok(id) = name.parse::<TaskId>() {
                any |= ingest_session(root, id)?;
            }
        }
    }
    Ok(any)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskId;

    #[test]
    fn record_state_writes_mapped_event_for_tracked_firing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();
        let id = TaskId::new(1);

        let tracked = record_state(&root, id, "notification", "{\"notification_type\":\"permission_prompt\"}").unwrap();
        assert!(tracked);
        let state = store::load_state(&root, id).unwrap().unwrap();
        assert_eq!(state.kind, WorkerEventKind::HumanInputRequired(Notification::PermissionPrompt));
    }

    #[test]
    fn record_state_overwrites_previous_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        let id = TaskId::new(1);
        record_state(&root, id, "session-start", "{}").unwrap();
        record_state(&root, id, "notification", "{\"notification_type\":\"idle_prompt\"}").unwrap();
        // only the latest tracked event is retained
        let state = store::load_state(&root, id).unwrap().unwrap();
        assert_eq!(state.kind, WorkerEventKind::HumanInputRequired(Notification::IdlePrompt));
    }

    #[test]
    fn record_state_ignores_untracked_firing_and_keeps_prior_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        let id = TaskId::new(1);
        record_state(&root, id, "user-prompt-submit", "{}").unwrap(); // Working
        let tracked = record_state(&root, id, "notification", "{\"notification_type\":\"auth_success\"}").unwrap();
        assert!(!tracked); // untracked -> caller should not wake
        // prior Working state must survive an untracked firing
        assert_eq!(store::load_state(&root, id).unwrap().unwrap().kind, WorkerEventKind::Working);
    }

    #[test]
    fn ingest_permission_prompt_keeps_card_in_doing() {
        use crate::controller::apply::apply;
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);
        record_state(&root, id, "notification", "{\"notification_type\":\"permission_prompt\"}").unwrap();

        let changed = ingest_session(&root, id).unwrap();
        assert!(changed);
        // needs-human is an in-progress state -> stays in doing; the warning icon
        // (not the column) signals it needs attention.
        let board = store::load_board(&root).unwrap();
        assert!(board.cards().get(&"doing".parse().unwrap()).unwrap().contains(&id));
    }

    #[test]
    fn stop_marks_session_as_needing_human_not_working() {
        use crate::controller::apply::apply;
        use crate::model::proto::Intent;
        use crate::model::Phase;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);
        // claude finished its turn and handed control back -> it needs the human,
        // it is NOT working, so no spinner.
        record_state(&root, id, "stop", "{}").unwrap();
        assert_eq!(session_phase(&root, id).unwrap(), Phase::Idle);
        assert!(session_phase(&root, id).unwrap().needs_human_input());
        ingest_session(&root, id).unwrap();
        let board = store::load_board(&root).unwrap();
        // in-progress (needs human) -> stays in doing; the warning icon shows it's waiting.
        assert!(board.cards().get(&"doing".parse().unwrap()).unwrap().contains(&id));
    }

    #[test]
    fn ingest_session_end_moves_to_done() {
        use crate::controller::apply::apply;
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);
        record_state(&root, id, "session-end", "{}").unwrap();
        ingest_session(&root, id).unwrap();
        let board = store::load_board(&root).unwrap();
        assert!(board.cards().get(&"done".parse().unwrap()).unwrap().contains(&id));
    }

    #[test]
    fn ingest_is_idempotent_once_card_is_placed() {
        use crate::controller::apply::apply;
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);
        record_state(&root, id, "notification", "{\"notification_type\":\"idle_prompt\"}").unwrap();
        assert!(ingest_session(&root, id).unwrap()); // first move reports change
        assert!(!ingest_session(&root, id).unwrap()); // already placed -> no change, no broadcast
    }

    #[test]
    fn ingest_does_not_resurrect_archived_task() {
        use crate::controller::apply::apply;
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);
        let mut task = store::load_task(&root, id).unwrap();
        task.status.archived = true;
        store::save_task(&root, &task).unwrap();
        // a 'session-start' event would normally move the card to 'doing'…
        record_state(&root, id, "session-start", "{}").unwrap();
        ingest_session(&root, id).unwrap();
        // …but an archived task must not be placed on the board.
        let board = store::load_board(&root).unwrap();
        assert!(!board.cards().get(&"doing".parse().unwrap()).unwrap().contains(&id));
    }

    #[test]
    fn ingest_no_state_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        crate::controller::apply::apply(&root, crate::model::proto::Intent::CreateTask {
            title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() }).unwrap();
        // a created-but-not-yet-handed-off task has no state file -> Pending -> no move
        assert!(!ingest_session(&root, TaskId::new(1)).unwrap());
    }

    #[test]
    fn session_phase_is_pending_without_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        assert_eq!(session_phase(&root, TaskId::new(1)).unwrap(), Phase::Pending);
    }

    #[test]
    fn fingerprint_changes_on_phase_change_without_column_move() {
        use crate::controller::{apply::apply, handoff};
        use crate::model::proto::Intent;
        struct NoLaunch;
        impl handoff::Launcher for NoLaunch {
            fn launch(&self, _s: &crate::model::WorkerSession, _n: &str) -> anyhow::Result<()> { Ok(()) }
            fn kill(&self, _n: &str) {}
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);
        handoff::handoff(&root, id, "claude", &NoLaunch).unwrap(); // writes session.yaml

        record_state(&root, id, "user-prompt-submit", "{}").unwrap(); // Working
        ingest_session(&root, id).unwrap(); // -> doing
        let working_fp = observable_fingerprint(&root).unwrap();
        // stable when nothing changes
        assert_eq!(working_fp, observable_fingerprint(&root).unwrap());

        record_state(&root, id, "stop", "{}").unwrap(); // Idle, still in doing (no column move)
        ingest_session(&root, id).unwrap();
        let idle_fp = observable_fingerprint(&root).unwrap();
        assert_ne!(working_fp, idle_fp, "phase change must change the fingerprint even without a card move");
    }

    #[test]
    fn reconcile_all_ingests_each_session() {
        use crate::controller::apply::apply;
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);
        record_state(&root, id, "session-start", "{}").unwrap();
        assert!(reconcile_all(&root).unwrap());
        let board = store::load_board(&root).unwrap();
        assert!(board.cards().get(&"doing".parse().unwrap()).unwrap().contains(&id)); // Started -> Working -> doing
    }
}
