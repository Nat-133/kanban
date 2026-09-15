//! Reading Claude Code's own per-session status.
//!
//! Claude Code writes one file per running session at
//! `$CLAUDE_CONFIG_DIR/sessions/<pid>.json`, rewritten on every status change.
//! An agent supervisor knows its child's pid, so it reads exactly one of them.
//!
//! The format is private to Claude Code, so every field is optional and every
//! failure degrades to "no opinion" — a status we can't read must leave the
//! last known state standing rather than replace it with a guess.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Claude Code's own view of what a session is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Busy,
    Shell,
    Idle,
    Waiting,
}

impl Status {
    /// The worker event this status represents.
    ///
    /// `waiting` covers any open dialog, not only a permission prompt, so the
    /// notification kind is narrower than the states it stands for. Both idle
    /// and waiting derive to phases that report `needs_human_input`, which is
    /// the distinction the board actually draws.
    pub fn event_kind(self) -> crate::model::WorkerEventKind {
        use crate::model::{Notification, WorkerEventKind};
        match self {
            Status::Busy | Status::Shell => WorkerEventKind::Working,
            Status::Idle => WorkerEventKind::HumanInputRequired(Notification::IdlePrompt),
            Status::Waiting => WorkerEventKind::HumanInputRequired(Notification::PermissionPrompt),
        }
    }
}

/// One session's registry record.
#[derive(Debug, Clone, Default)]
pub struct Record {
    pub status: Option<Status>,
    pub session_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Raw {
    status: Option<String>,
    session_id: Option<String>,
}

/// Where Claude Code keeps its per-session status files. Mirrors Claude Code's
/// own rule, so a non-default config dir doesn't silently strand us.
pub fn registry_dir() -> Option<PathBuf> {
    match std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(d) if !d.is_empty() => Some(PathBuf::from(d).join("sessions")),
        _ => std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".claude/sessions")),
    }
}

/// Read one session's record. None = nothing usable there.
pub fn read(dir: &Path, pid: i32) -> Option<Record> {
    let body = std::fs::read_to_string(dir.join(format!("{pid}.json"))).ok()?;
    let raw: Raw = serde_json::from_str(&body).ok()?;
    Some(Record {
        status: raw.status.as_deref().and_then(parse_status),
        session_id: raw.session_id,
    })
}

/// Find a session's record by its Claude session id, for a caller that knows the
/// id but not the pid — a hook process, which is handed the id in its payload.
/// Scans the directory, so prefer `read` when the pid is known.
pub fn find_by_session_id(dir: &Path, session_id: &str) -> Option<Record> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let pid = entry
            .file_name()
            .to_string_lossy()
            .strip_suffix(".json")
            .and_then(|s| s.parse::<i32>().ok());
        let Some(pid) = pid else { continue };
        match read(dir, pid) {
            Some(r) if r.session_id.as_deref() == Some(session_id) => return Some(r),
            _ => {}
        }
    }
    None
}

fn parse_status(s: &str) -> Option<Status> {
    match s {
        "busy" => Some(Status::Busy),
        "shell" => Some(Status::Shell),
        "idle" => Some(Status::Idle),
        "waiting" => Some(Status::Waiting),
        other => {
            tracing::debug!(status = %other, "unrecognised claude session status");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, pid: i32, body: &str) {
        std::fs::write(dir.join(format!("{pid}.json")), body).unwrap();
    }

    #[test]
    fn reads_the_status_for_our_pid() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), 42, r#"{"pid":42,"status":"busy","sessionId":"abc"}"#);
        let r = read(d.path(), 42).unwrap();
        assert_eq!(r.status, Some(Status::Busy));
        assert_eq!(r.session_id.as_deref(), Some("abc"));
    }

    #[test]
    fn every_observed_status_parses() {
        for (s, want) in [
            ("busy", Status::Busy),
            ("shell", Status::Shell),
            ("idle", Status::Idle),
            ("waiting", Status::Waiting),
        ] {
            let d = tempfile::tempdir().unwrap();
            write(d.path(), 1, &format!(r#"{{"pid":1,"status":"{s}"}}"#));
            assert_eq!(read(d.path(), 1).unwrap().status, Some(want));
        }
    }

    #[test]
    fn an_unknown_status_is_no_opinion_not_a_guess() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), 1, r#"{"pid":1,"status":"hibernating"}"#);
        assert_eq!(read(d.path(), 1).unwrap().status, None);
    }

    #[test]
    fn a_missing_or_unparseable_file_is_none() {
        let d = tempfile::tempdir().unwrap();
        assert!(read(d.path(), 7).is_none());
        write(d.path(), 8, "not json at all");
        assert!(read(d.path(), 8).is_none());
    }

    #[test]
    fn finds_a_session_by_its_claude_session_id() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), 10, r#"{"pid":10,"status":"idle","sessionId":"other"}"#);
        write(d.path(), 11, r#"{"pid":11,"status":"busy","sessionId":"ours"}"#);
        assert_eq!(find_by_session_id(d.path(), "ours").unwrap().status, Some(Status::Busy));
        assert!(find_by_session_id(d.path(), "nobody").is_none());
    }

    #[test]
    fn a_non_pid_filename_is_skipped_not_fatal() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("notes.txt"), "junk").unwrap();
        std::fs::write(d.path().join("42.key"), "junk").unwrap();
        write(d.path(), 11, r#"{"pid":11,"status":"busy","sessionId":"ours"}"#);
        assert_eq!(find_by_session_id(d.path(), "ours").unwrap().status, Some(Status::Busy));
    }

    #[test]
    fn status_maps_to_the_phase_the_board_shows() {
        use crate::model::{Notification as N, WorkerEventKind as K};
        assert_eq!(Status::Busy.event_kind(), K::Working);
        // Shell mode is the human running a command in the pane. The agent is
        // not waiting on them, so it must not raise the warning icon.
        assert_eq!(Status::Shell.event_kind(), K::Working);
        assert_eq!(Status::Idle.event_kind(), K::HumanInputRequired(N::IdlePrompt));
        assert_eq!(Status::Waiting.event_kind(), K::HumanInputRequired(N::PermissionPrompt));
    }

    #[test]
    fn a_record_missing_every_optional_field_still_parses() {
        // Upstream dropping a field must degrade, never error.
        let d = tempfile::tempdir().unwrap();
        write(d.path(), 9, r#"{"pid":9}"#);
        let r = read(d.path(), 9).unwrap();
        assert_eq!(r.status, None);
        assert_eq!(r.session_id, None);
    }
}
