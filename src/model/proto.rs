// proto wire types — Task 1

use crate::model::{Board, ColumnId, Phase, Task, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Requests a client sends to the controller. Internally tagged on `type`
/// (camelCase) so the JSON reads `{"type":"createTask", ...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Intent {
    InitWorkspace,
    GetBoard,
    CreateTask { title: String, summary: String, column: ColumnId },
    EditTask { task: TaskId, title: Option<String>, summary: Option<String> },
    MoveCard { task: TaskId, to_column: ColumnId, position: Option<usize> },
    ReorderCard { task: TaskId, position: usize },
    ArchiveTask { task: TaskId },
    Handoff { task: TaskId, worker: String },
    SetProfile { task: TaskId, profile: String },
}

/// Replies the controller sends back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Response {
    /// Full board snapshot: the board, every live (non-archived) task, its
    /// worker sessions, and each task's long-form description keyed by id.
    /// Descriptions are prose from the per-task `description.md`; a task with
    /// no description file simply has no entry.
    Snapshot {
        board: Board,
        tasks: Vec<Task>,
        sessions: Vec<SessionView>,
        #[serde(default)]
        descriptions: BTreeMap<TaskId, String>,
    },
    /// A mutation succeeded; carries the affected task id when relevant.
    Ok { task: Option<TaskId> },
    Error { message: String },
}

/// Body of a `/v1/wake` request: the hook telling the controller that a session's
/// state file changed and its card should be reconciled. Carries only the id —
/// the new state lives in the session's `state.yaml`, which the controller re-reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeRequest {
    pub task: TaskId,
}

/// A worker session as surfaced in a board snapshot: identity plus derived phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub task: TaskId,
    pub session_name: String,
    pub phase: Phase,
    pub needs_human_input: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_task_intent_round_trips_json() {
        let intent = Intent::CreateTask { title: "Build X".into(), summary: "do X".into(), column: "todo".parse().unwrap() };
        let j = serde_json::to_string(&intent).unwrap();
        let back: Intent = serde_json::from_str(&j).unwrap();
        assert_eq!(intent, back);
        assert!(j.contains("\"type\":\"createTask\""));
    }

    #[test]
    fn move_card_intent_round_trips() {
        let intent = Intent::MoveCard { task: TaskId::new(1), to_column: "doing".parse().unwrap(), position: Some(0) };
        let back: Intent = serde_json::from_str(&serde_json::to_string(&intent).unwrap()).unwrap();
        assert_eq!(intent, back);
    }

    #[test]
    fn handoff_intent_round_trips() {
        let i = Intent::Handoff { task: TaskId::new(1), worker: "claude".into() };
        let back: Intent = serde_json::from_str(&serde_json::to_string(&i).unwrap()).unwrap();
        assert_eq!(i, back);
    }

    #[test]
    fn set_profile_intent_round_trips() {
        let i = Intent::SetProfile { task: TaskId::new(1), profile: "cluster-ops".into() };
        let back: Intent = serde_json::from_str(&serde_json::to_string(&i).unwrap()).unwrap();
        assert_eq!(i, back);
    }

    #[test]
    fn session_view_round_trips() {
        let sv = SessionView { task: TaskId::new(1), session_name: "kanban-task-0001".into(), phase: crate::model::Phase::WaitingHuman, needs_human_input: true };
        let back: SessionView = serde_json::from_str(&serde_json::to_string(&sv).unwrap()).unwrap();
        assert_eq!(sv, back);
    }

    #[test]
    fn wake_request_round_trips() {
        let w = WakeRequest { task: TaskId::new(7) };
        let j = serde_json::to_string(&w).unwrap();
        let back: WakeRequest = serde_json::from_str(&j).unwrap();
        assert_eq!(w, back);
        assert!(j.contains("task-0007"));
    }

    #[test]
    fn response_error_round_trips() {
        let r = Response::Error { message: "nope".into() };
        let back: Response = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);
    }
}
