use crate::controller::store;
use crate::model::{Notification, Phase, TaskId, WorkerEvent, WorkerEventKind};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One captured hook event awaiting ingestion. `payload` is Claude's raw event JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntakePayload {
    pub event: String,
    pub payload: serde_json::Value,
}

fn intake_dir(root: &Path, id: TaskId) -> std::path::PathBuf {
    store::session_dir(root, id).join("hooks/intake")
}

/// Next zero-padded intake filename (count-based; hooks from one session fire serially).
fn next_intake_name(dir: &Path) -> String {
    let mut max = 0u32;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Some(stem) = e.path().file_stem().and_then(|s| s.to_str()) {
                if let Ok(n) = stem.parse::<u32>() {
                    max = max.max(n);
                }
            }
        }
    }
    format!("{:04}.json", max + 1)
}

/// Write one intake payload. `raw_payload` is Claude's stdin JSON (stored as a string if not valid JSON).
pub fn record_intake(root: &Path, id: TaskId, event: &str, raw_payload: &str) -> anyhow::Result<()> {
    let dir = intake_dir(root, id);
    std::fs::create_dir_all(&dir)?;
    let payload: serde_json::Value = serde_json::from_str(raw_payload)
        .unwrap_or_else(|_| serde_json::Value::String(raw_payload.to_string()));
    let item = IntakePayload {
        event: event.to_string(),
        payload,
    };
    let name = next_intake_name(&dir);
    store::atomic_write(&dir.join(name), &serde_json::to_string_pretty(&item)?)?;
    Ok(())
}

/// Map a captured intake item to a worker event. None = not tracked (e.g. a
/// non-permission/idle Notification like auth_success).
fn to_event(item: &IntakePayload, payload_ref: String) -> Option<WorkerEvent> {
    let kind = match item.event.as_str() {
        "session-start" => WorkerEventKind::Started,
        "user-prompt-submit" | "stop" => WorkerEventKind::Working,
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
        payload_ref: Some(payload_ref),
    })
}

/// Board column for a derived phase. None = leave the card where it is.
fn phase_column(phase: Phase) -> Option<&'static str> {
    match phase {
        Phase::Working => Some("doing"),
        Phase::WaitingHuman => Some("blocked"),
        Phase::Idle => Some("waiting-human"),
        Phase::Completed | Phase::Failed => Some("review"),
        Phase::Pending => None,
    }
}

/// Drain a session's intake, append tracked events, and move the card to match the
/// derived phase. Returns true if any tracked event was recorded.
pub fn ingest_session(root: &Path, id: TaskId) -> anyhow::Result<bool> {
    let intake = store::list_intake(root, id)?;
    if intake.is_empty() {
        return Ok(false);
    }
    let mut any = false;
    for path in intake {
        let text = std::fs::read_to_string(&path)?;
        let item: IntakePayload = serde_json::from_str(&text)?;
        let processed_ref = format!("hooks/processed/{}", path.file_name().unwrap().to_string_lossy());
        if let Some(event) = to_event(&item, processed_ref) {
            store::append_worker_event(root, id, &event)?;
            any = true;
        }
        store::mark_processed(root, id, &path)?;
    }
    if any {
        let phase = crate::controller::derive::derive(&store::load_events(&store::session_dir(root, id))?);
        if let Some(col) = phase_column(phase) {
            move_card_to(root, id, col)?;
        }
    }
    Ok(any)
}

fn move_card_to(root: &Path, id: TaskId, column: &str) -> anyhow::Result<()> {
    use crate::model::{Board, RawBoard};
    let mut raw: RawBoard = store::load_board(root)?.into();
    let col = column.parse().map_err(|_| anyhow::anyhow!("bad column id: {column}"))?;
    for v in raw.spec.cards.values_mut() {
        v.retain(|t| *t != id);
    }
    raw.spec.cards.entry(col).or_default().push(id);
    let board = Board::try_from(raw).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    store::save_board(root, &board)
}

/// Drain every session's intake. Returns true if anything changed.
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
    fn record_writes_intake_payload() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();
        let id = TaskId::new(1);
        std::fs::create_dir_all(root.join("sessions/task-0001/hooks/intake")).unwrap();

        record_intake(&root, id, "notification", "{\"notification_type\":\"permission_prompt\"}").unwrap();
        record_intake(&root, id, "stop", "{}").unwrap();

        let intake = root.join("sessions/task-0001/hooks/intake");
        let mut files: Vec<_> = std::fs::read_dir(&intake).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        files.sort();
        assert_eq!(files.len(), 2);
        let first = std::fs::read_to_string(intake.join(&files[0])).unwrap();
        assert!(first.contains("notification"));
        assert!(first.contains("permission_prompt"));
    }

    #[test]
    fn ingest_permission_prompt_moves_card_to_blocked() {
        use crate::controller::{store, apply::apply};
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);
        record_intake(&root, id, "notification", "{\"notification_type\":\"permission_prompt\"}").unwrap();

        let changed = ingest_session(&root, id).unwrap();
        assert!(changed);
        assert_eq!(store::load_events(&store::session_dir(&root, id)).unwrap().len(), 1);
        let board = store::load_board(&root).unwrap();
        assert!(board.cards().get(&"blocked".parse().unwrap()).unwrap().contains(&id));
        assert!(store::list_intake(&root, id).unwrap().is_empty());
        assert!(store::session_dir(&root, id).join("hooks/processed/0001.json").exists());
    }

    #[test]
    fn ingest_session_end_moves_to_review() {
        use crate::controller::{store, apply::apply};
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);
        record_intake(&root, id, "session-end", "{}").unwrap();
        ingest_session(&root, id).unwrap();
        let board = store::load_board(&root).unwrap();
        assert!(board.cards().get(&"review".parse().unwrap()).unwrap().contains(&id));
    }

    #[test]
    fn ingest_untracked_notification_records_no_event() {
        use crate::controller::{store, apply::apply};
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);
        record_intake(&root, id, "notification", "{\"notification_type\":\"auth_success\"}").unwrap();
        let changed = ingest_session(&root, id).unwrap();
        assert!(!changed); // untracked notification -> no event, no move
        assert!(store::load_events(&store::session_dir(&root, id)).unwrap().is_empty());
        // but the intake file is still drained (moved to processed)
        assert!(store::list_intake(&root, id).unwrap().is_empty());
    }

    #[test]
    fn ingest_no_intake_is_noop() {
        use crate::controller::store;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        let id = TaskId::new(1);
        std::fs::create_dir_all(store::session_dir(&root, id)).unwrap();
        assert!(!ingest_session(&root, id).unwrap());
    }

    #[test]
    fn reconcile_all_ingests_each_session() {
        use crate::controller::{store, apply::apply};
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);
        record_intake(&root, id, "session-start", "{}").unwrap();
        assert!(reconcile_all(&root).unwrap());
        let board = store::load_board(&root).unwrap();
        assert!(board.cards().get(&"doing".parse().unwrap()).unwrap().contains(&id)); // Started -> Working -> doing
    }
}
