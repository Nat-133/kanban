use crate::controller::store;
use crate::model::TaskId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

/// One immutable human-involvement fact. Append-only; never mutated. Derived
/// metrics (interruptions-per-task, approve/deny ratios) are computed by folding
/// these, never stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    #[serde(with = "time::serde::rfc3339")]
    pub observed_at: OffsetDateTime,
    pub task: TaskId,
    /// Active profile at emit time — denormalized on purpose so the promote-query
    /// is one pass and survives later edits to the profile timeline.
    pub profile: String,
    #[serde(flatten)]
    pub kind: ActivityKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ActivityKind {
    /// The worker blocked and needs the human (a context-switch cost event).
    Interruption { reason: InterruptionReason },
    /// The human sent the worker a message / prompt.
    Steer,
    /// The human (or a future classifier) changed the task's profile.
    ProfileChanged { from: Option<String>, to: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InterruptionReason {
    PermissionPrompt,
    Idle,
}

pub fn activity_dir(root: &Path) -> PathBuf {
    root.join("activity")
}

/// Append one immutable fact. Filename = nanos-pid so it is time-ordered and
/// unique across the hook and daemon processes; never overwrites an existing file.
///
/// The `{nanos}-{pid}` name assumes distinct nanoseconds per process: two appends
/// in the same process at the same nanosecond would collide and the second would
/// silently overwrite the first. This is realistically unreachable at nanosecond
/// resolution, and the hook path emits one event per process anyway.
pub fn append(root: &Path, event: &ActivityEvent) -> anyhow::Result<()> {
    let nanos = event.observed_at.unix_timestamp_nanos();
    let pid = std::process::id();
    let name = format!("{nanos:020}-{pid}.json");
    let path = activity_dir(root).join(name);
    store::atomic_write(&path, &serde_json::to_string(event)?)
}

/// Load every fact, ascending by filename (i.e. by time). Missing dir => empty.
///
/// A single unreadable or unparseable file is skipped (warned) rather than fatal —
/// the activity log is a best-effort observability record, so one corrupt byte must
/// not take down the whole query. An unreadable directory is still fatal.
///
/// The lexical-sort == time-order invariant assumes post-epoch (non-negative)
/// timestamps, so the zero-padded nanos filenames sort in chronological order.
pub fn load(root: &Path) -> anyhow::Result<Vec<ActivityEvent>> {
    let dir = activity_dir(root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut names: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .filter(|n| n.to_string_lossy().ends_with(".json"))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for n in names {
        let path = dir.join(&n);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(file = %n.to_string_lossy(), error = %e, "skipping unreadable activity file");
                continue;
            }
        };
        match serde_json::from_str(&text) {
            Ok(event) => out.push(event),
            Err(e) => {
                tracing::warn!(file = %n.to_string_lossy(), error = %e, "skipping unparseable activity file");
                continue;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_then_load_returns_events_in_time_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        std::fs::create_dir_all(&root).unwrap();
        let mk = |secs: i64, task: u32| ActivityEvent {
            observed_at: time::OffsetDateTime::from_unix_timestamp(secs).unwrap(),
            task: TaskId::new(task),
            profile: "default".into(),
            kind: ActivityKind::Steer,
        };
        append(&root, &mk(200, 2)).unwrap();
        append(&root, &mk(100, 1)).unwrap();
        let all = load(&root).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].observed_at.unix_timestamp(), 100, "sorted ascending by time");
        assert_eq!(all[1].task, TaskId::new(2));
    }

    #[test]
    fn load_skips_unreadable_files_and_returns_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        std::fs::create_dir_all(&root).unwrap();
        // one valid event...
        append(&root, &ActivityEvent {
            observed_at: time::OffsetDateTime::from_unix_timestamp(100).unwrap(),
            task: TaskId::new(1),
            profile: "default".into(),
            kind: ActivityKind::Steer,
        }).unwrap();
        // ...and one corrupt file in the activity dir
        std::fs::write(activity_dir(&root).join("00000000000000000001-99.json"), "{ not json").unwrap();
        let all = load(&root).unwrap();
        assert_eq!(all.len(), 1, "corrupt file skipped, valid event still returned");
        assert_eq!(all[0].task, TaskId::new(1));
    }

    #[test]
    fn load_is_empty_when_no_activity_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn profile_changed_event_survives_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        std::fs::create_dir_all(&root).unwrap();
        let e = ActivityEvent {
            observed_at: time::OffsetDateTime::from_unix_timestamp(300).unwrap(),
            task: TaskId::new(7),
            profile: "careful".into(),
            kind: ActivityKind::ProfileChanged {
                from: Some("default".into()),
                to: "careful".into(),
            },
        };
        append(&root, &e).unwrap();
        let all = load(&root).unwrap();
        assert_eq!(all, vec![e]);
    }

    #[test]
    fn activity_event_round_trips_json() {
        let e = ActivityEvent {
            observed_at: time::OffsetDateTime::UNIX_EPOCH,
            task: TaskId::new(1),
            profile: "default".into(),
            kind: ActivityKind::Interruption { reason: InterruptionReason::PermissionPrompt },
        };
        let j = serde_json::to_string(&e).unwrap();
        let back: ActivityEvent = serde_json::from_str(&j).unwrap();
        assert_eq!(e, back);
        assert!(j.contains("\"permissionPrompt\""));
    }
}
