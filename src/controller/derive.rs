use crate::model::{Notification, Phase, WorkerEvent, WorkerEventKind};

/// Derive worker phase from the ordered event stream. Level-triggered: recomputed
/// from scratch every call, so it is idempotent and restart-safe.
pub fn derive(events: &[WorkerEvent]) -> Phase {
    match events.last().map(|e| &e.kind) {
        None => Phase::Pending,
        Some(WorkerEventKind::Started | WorkerEventKind::Working) => Phase::Working,
        Some(WorkerEventKind::HumanInputRequired(Notification::PermissionPrompt)) => Phase::WaitingHuman,
        Some(WorkerEventKind::HumanInputRequired(Notification::IdlePrompt)) => Phase::Idle,
        Some(WorkerEventKind::Completed) => Phase::Completed,
        Some(WorkerEventKind::Failed) => Phase::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WorkerEventList;

    fn events(yaml: &str) -> Vec<WorkerEvent> {
        let full = format!("metadata:\n  name: s\nitems:\n{yaml}");
        serde_yml::from_str::<WorkerEventList>(&full).unwrap().items
    }

    #[test]
    fn no_events_is_pending() {
        assert_eq!(derive(&[]), Phase::Pending);
        assert!(!Phase::Pending.needs_human_input());
    }

    #[test]
    fn permission_prompt_waits_for_human() {
        let e = events("  - {type: human_input_required, source: h, notificationType: permission_prompt, observedAt: \"2026-06-17T10:00:00Z\"}\n");
        assert_eq!(derive(&e), Phase::WaitingHuman);
        assert!(Phase::WaitingHuman.needs_human_input());
    }

    #[test]
    fn idle_prompt_is_idle_and_needs_input() {
        let e = events("  - {type: human_input_required, source: h, notificationType: idle_prompt, observedAt: \"2026-06-17T10:00:00Z\"}\n");
        assert_eq!(derive(&e), Phase::Idle);
        assert!(Phase::Idle.needs_human_input());
    }

    #[test]
    fn working_after_human_input_clears_the_flag() {
        let e = events(
            "  - {type: human_input_required, source: h, notificationType: permission_prompt, observedAt: \"2026-06-17T10:00:00Z\"}\n  - {type: working, source: c, observedAt: \"2026-06-17T10:01:00Z\"}\n");
        assert_eq!(derive(&e), Phase::Working);
        assert!(!Phase::Working.needs_human_input());
    }

    #[test]
    fn completed_and_failed() {
        let c = events("  - {type: completed, source: c, observedAt: \"2026-06-17T10:00:00Z\"}\n");
        assert_eq!(derive(&c), Phase::Completed);
        let f = events("  - {type: failed, source: c, observedAt: \"2026-06-17T10:00:00Z\"}\n");
        assert_eq!(derive(&f), Phase::Failed);
    }
}
