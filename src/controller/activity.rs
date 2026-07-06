use crate::model::TaskId;
use serde::{Deserialize, Serialize};
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

#[cfg(test)]
mod tests {
    use super::*;

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
