use crate::controller::activity::{self, ActivityKind, InterruptionReason};
use crate::controller::store;
use crate::model::{Phase, TaskId, WorkerEvent};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One captured hook event. `payload` is Claude's raw event JSON. Used only as
/// an in-memory parse target while reading a firing; it is never persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntakePayload {
    pub event: String,
    pub payload: serde_json::Value,
}

/// Map a captured hook firing to a human-involvement activity fact. None = the
/// firing is not a human-involvement event and no activity should be logged.
/// Steers and interruptions are the observability signals we fold into
/// context-switch metrics; everything else (session lifecycle, non-blocking
/// notifications like auth_success) yields None.
///
/// `agent_busy` is whether Claude Code reports the session as busy, which for a
/// Stop means it yielded the turn to a subagent rather than to the human.
fn to_activity(item: &IntakePayload, agent_busy: bool) -> Option<ActivityKind> {
    match item.event.as_str() {
        "user-prompt-submit" => Some(ActivityKind::Steer),
        // A stop that only yields to a running subagent costs the human nothing, so
        // counting it would inflate interruptions-per-task with work the agent did
        // unattended.
        "stop" if agent_busy => None,
        "stop" => Some(ActivityKind::Interruption { reason: InterruptionReason::Idle }),
        "notification" => match item.payload.get("notification_type").and_then(|v| v.as_str()) {
            Some("permission_prompt") => {
                Some(ActivityKind::Interruption { reason: InterruptionReason::PermissionPrompt })
            }
            Some("idle_prompt") => Some(ActivityKind::Interruption { reason: InterruptionReason::Idle }),
            _ => None,
        },
        _ => None,
    }
}

/// Record what a hook firing tells us. The agent reports its own state, so this
/// no longer touches `state.yaml`: it appends the human-involvement activity
/// fact and captures the session metadata only the hook payload carries.
///
/// `raw_payload` is Claude's stdin JSON (treated as a bare string if not valid
/// JSON).
pub fn record_hook(root: &Path, id: TaskId, event: &str, raw_payload: &str) -> anyhow::Result<()> {
    let payload: serde_json::Value = serde_json::from_str(raw_payload)
        .unwrap_or_else(|_| serde_json::Value::String(raw_payload.to_string()));
    let item = IntakePayload { event: event.to_string(), payload };
    let agent_busy = agent_is_busy(&item.payload);
    // Best-effort activity fact: a human-involvement firing (steer/interruption)
    // is appended to the root activity log. Any failure is warned and swallowed.
    if let Some(kind) = to_activity(&item, agent_busy) {
        let profile = store::load_task(root, id)
            .ok()
            .and_then(|t| t.spec.profile)
            .unwrap_or_else(|| "default".to_string());
        let ev = activity::ActivityEvent {
            observed_at: time::OffsetDateTime::now_utc(),
            task: id,
            profile,
            kind,
        };
        if let Err(e) = activity::append(root, &ev) {
            tracing::warn!(error = %e, "failed to append activity event");
        }
    }
    // Best-effort session metadata capture (session_id on start; transcript copy
    // on end). A failure here must not lose the activity fact above, so it is
    // warned and swallowed.
    if let Err(e) = capture_session_metadata(root, id, &item.event, &item.payload) {
        tracing::warn!(error = %e, "failed to capture session metadata");
    }
    Ok(())
}

/// Whether Claude Code currently reports this session as busy, looked up by the
/// `session_id` its hook payload carries.
///
/// Unknown counts as not busy: over-counting an interruption is a smaller lie
/// than dropping a real one, and this only ever suppresses a fact.
fn agent_is_busy(payload: &serde_json::Value) -> bool {
    use crate::agent::registry::{find_by_session_id, registry_dir, Status};
    let Some(sid) = payload.get("session_id").and_then(|v| v.as_str()) else { return false };
    let Some(dir) = registry_dir() else { return false };
    find_by_session_id(&dir, sid).and_then(|r| r.status) == Some(Status::Busy)
}

/// Capture Claude Code hook metadata into the session record: the `session_id`
/// (seeds a future resume) and, on `session-end`, a copy of the transcript into
/// the session dir (Claude GCs the original after ~30 days, so we preserve it
/// where the archive keeps it). Best-effort: a missing session or an unreadable
/// transcript is not an error.
fn capture_session_metadata(
    root: &Path,
    id: TaskId,
    event: &str,
    payload: &serde_json::Value,
) -> anyhow::Result<()> {
    let Some(mut session) = store::load_session(root, id)? else { return Ok(()); };
    let mut changed = false;
    if let Some(sid) = payload.get("session_id").and_then(|v| v.as_str()) {
        if session.status.session_id.as_deref() != Some(sid) {
            session.status.session_id = Some(sid.to_string());
            changed = true;
        }
    }
    if event == "session-end" {
        if let Some(tp) = payload.get("transcript_path").and_then(|v| v.as_str()) {
            let dst = store::session_dir(root, id).join("transcript.jsonl");
            match std::fs::copy(tp, &dst) {
                Ok(_) => {
                    session.status.transcript_ref = Some("transcript.jsonl".into());
                    changed = true;
                }
                Err(e) => tracing::warn!(transcript_path = %tp, error = %e, "could not copy worker transcript"),
            }
        }
    }
    if changed {
        store::save_session(root, &session)?;
    }
    Ok(())
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
        Phase::Working | Phase::WaitingHuman | Phase::Idle | Phase::Failed | Phase::Interrupted => Some("doing"),
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
    use crate::model::{Notification, TaskId, WorkerEventKind};

    /// A launcher that pretends every session starts and stays alive.
    struct NoLaunch;
    impl crate::controller::handoff::Launcher for NoLaunch {
        fn launch(&self, _s: &crate::model::WorkerSession, _n: &str) -> anyhow::Result<()> { Ok(()) }
        fn kill(&self, _n: &str) {}
        fn is_alive(&self, _n: &str) -> bool { true }
    }

    /// Create task 1 in `todo` in a fresh workspace.
    fn workspace_with_task() -> (tempfile::TempDir, std::path::PathBuf, TaskId) {
        use crate::controller::apply::apply;
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { text: "A".into(), column: "todo".parse().unwrap() }).unwrap();
        (dir, root, TaskId::new(1))
    }

    /// As above, but handed off so `session.yaml` exists.
    fn workspace_with_session() -> (tempfile::TempDir, std::path::PathBuf, TaskId) {
        let (dir, root, id) = workspace_with_task();
        crate::controller::handoff::handoff(&root, id, "claude", &NoLaunch).unwrap();
        (dir, root, id)
    }

    fn column_of(root: &Path, id: TaskId) -> Option<String> {
        let board = store::load_board(root).unwrap();
        board.cards().iter().find(|(_, v)| v.contains(&id)).map(|(c, _)| c.to_string())
    }

    #[test]
    fn a_hook_firing_does_not_write_worker_state() {
        // The agent owns its own state. A hook records activity and metadata only,
        // so two writers can't fight over the same file.
        let (_d, root, id) = workspace_with_task();
        record_hook(&root, id, "stop", "{}").unwrap();
        record_hook(&root, id, "notification", "{\"notification_type\":\"permission_prompt\"}").unwrap();
        record_hook(&root, id, "session-end", "{\"reason\":\"prompt_input_exit\"}").unwrap();
        assert!(store::load_state(&root, id).unwrap().is_none());
    }

    #[test]
    fn hooks_still_record_the_activity_log() {
        let (_d, root, id) = workspace_with_task();
        record_hook(&root, id, "user-prompt-submit", "{}").unwrap();
        record_hook(&root, id, "notification", "{\"notification_type\":\"permission_prompt\"}").unwrap();
        record_hook(&root, id, "stop", "{}").unwrap();
        let acts = activity::load(&root).unwrap();
        assert_eq!(acts.len(), 3);
        assert!(matches!(acts[0].kind, ActivityKind::Steer));
        assert!(matches!(acts[1].kind,
            ActivityKind::Interruption { reason: InterruptionReason::PermissionPrompt }));
        assert!(matches!(acts[2].kind,
            ActivityKind::Interruption { reason: InterruptionReason::Idle }));
    }

    #[test]
    fn permission_prompt_appends_interruption_activity() {
        let (_d, root, id) = workspace_with_task();
        record_hook(&root, id, "notification", "{\"notification_type\":\"permission_prompt\"}").unwrap();
        let acts = activity::load(&root).unwrap();
        assert_eq!(acts.len(), 1);
        assert_eq!(acts[0].task, id);
        assert!(matches!(acts[0].kind,
            ActivityKind::Interruption { reason: InterruptionReason::PermissionPrompt }));
    }

    #[test]
    fn a_non_involvement_notification_appends_no_activity() {
        let (_d, root, id) = workspace_with_task();
        record_hook(&root, id, "notification", "{\"notification_type\":\"auth_success\"}").unwrap();
        assert!(activity::load(&root).unwrap().is_empty());
    }

    #[test]
    fn ingest_needs_human_keeps_card_in_doing() {
        let (_d, root, id) = workspace_with_task();
        store::set_state_for_test(&root, id,
            WorkerEventKind::HumanInputRequired(Notification::PermissionPrompt));

        assert!(ingest_session(&root, id).unwrap());
        // needs-human is an in-progress state -> stays in doing; the warning icon
        // (not the column) signals it needs attention.
        assert_eq!(column_of(&root, id).as_deref(), Some("doing"));
    }

    #[test]
    fn ingest_completed_advances_to_done() {
        let (_d, root, id) = workspace_with_task();
        store::set_state_for_test(&root, id, WorkerEventKind::Completed);
        ingest_session(&root, id).unwrap();
        assert_eq!(column_of(&root, id).as_deref(), Some("done"));
    }

    #[test]
    fn ingest_interrupted_stays_in_doing() {
        // Unfinished work must never read as finished.
        let (_d, root, id) = workspace_with_task();
        store::set_state_for_test(&root, id, WorkerEventKind::Interrupted);
        ingest_session(&root, id).unwrap();
        assert_eq!(column_of(&root, id).as_deref(), Some("doing"));
    }

    #[test]
    fn ingest_is_idempotent_once_card_is_placed() {
        let (_d, root, id) = workspace_with_task();
        store::set_state_for_test(&root, id,
            WorkerEventKind::HumanInputRequired(Notification::IdlePrompt));
        assert!(ingest_session(&root, id).unwrap()); // first move reports change
        assert!(!ingest_session(&root, id).unwrap()); // already placed -> no broadcast
    }

    #[test]
    fn ingest_does_not_resurrect_archived_task() {
        let (_d, root, id) = workspace_with_task();
        let mut task = store::load_task(&root, id).unwrap();
        task.status.archived = true;
        store::save_task(&root, &task).unwrap();
        store::set_state_for_test(&root, id, WorkerEventKind::Working);

        ingest_session(&root, id).unwrap();

        let board = store::load_board(&root).unwrap();
        assert!(!board.cards().get(&"doing".parse().unwrap()).unwrap().contains(&id));
    }

    #[test]
    fn ingest_no_state_is_noop() {
        let (_d, root, id) = workspace_with_task();
        // a created-but-not-yet-handed-off task has no state file -> Pending -> no move
        assert!(!ingest_session(&root, id).unwrap());
    }

    #[test]
    fn session_phase_is_pending_without_state() {
        let (_d, root, id) = workspace_with_task();
        assert_eq!(session_phase(&root, id).unwrap(), Phase::Pending);
    }

    #[test]
    fn fingerprint_changes_on_phase_change_without_column_move() {
        let (_d, root, id) = workspace_with_session();

        store::set_state_for_test(&root, id, WorkerEventKind::Working);
        ingest_session(&root, id).unwrap(); // -> doing
        let working_fp = observable_fingerprint(&root).unwrap();
        assert_eq!(working_fp, observable_fingerprint(&root).unwrap(), "stable when nothing changes");

        // Idle stays in doing (no column move), but the icon must still flip.
        store::set_state_for_test(&root, id,
            WorkerEventKind::HumanInputRequired(Notification::IdlePrompt));
        ingest_session(&root, id).unwrap();
        assert_ne!(working_fp, observable_fingerprint(&root).unwrap(),
            "phase change must change the fingerprint even without a card move");
    }

    #[test]
    fn session_end_copies_transcript_into_session_dir() {
        let (d, root, id) = workspace_with_session();

        let tpath = d.path().join("orig-transcript.jsonl");
        std::fs::write(&tpath, "{\"m\":1}\n{\"m\":2}\n").unwrap();
        let payload = format!("{{\"session_id\":\"abc-123\",\"transcript_path\":\"{}\"}}",
            tpath.display());

        record_hook(&root, id, "session-end", &payload).unwrap();

        let copied = store::session_dir(&root, id).join("transcript.jsonl");
        assert_eq!(std::fs::read_to_string(&copied).unwrap(), "{\"m\":1}\n{\"m\":2}\n");
        let s = store::load_session(&root, id).unwrap().unwrap();
        assert_eq!(s.status.transcript_ref.as_deref(), Some("transcript.jsonl"));
    }

    #[test]
    fn a_hook_payload_records_the_session_id() {
        let (_d, root, id) = workspace_with_session();
        record_hook(&root, id, "session-end", "{\"session_id\":\"abc-123\"}").unwrap();
        let s = store::load_session(&root, id).unwrap().unwrap();
        assert_eq!(s.status.session_id.as_deref(), Some("abc-123"));
    }

    #[test]
    fn reconcile_all_ingests_each_session() {
        let (_d, root, id) = workspace_with_task();
        store::set_state_for_test(&root, id, WorkerEventKind::Working);
        assert!(reconcile_all(&root).unwrap());
        assert_eq!(column_of(&root, id).as_deref(), Some("doing"));
    }
}
