use crate::controller::store;
use crate::model::TaskId;
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
}
