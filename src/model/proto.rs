// proto wire types — Task 1

use crate::model::{Board, ColumnId, Task, TaskId};
use serde::{Deserialize, Serialize};

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
}

/// Replies the controller sends back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Response {
    /// Full board snapshot: the board plus every live (non-archived) task.
    Snapshot { board: Board, tasks: Vec<Task> },
    /// A mutation succeeded; carries the affected task id when relevant.
    Ok { task: Option<TaskId> },
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_task_intent_round_trips_json() {
        let intent = Intent::CreateTask { title: "Build X".into(), summary: "do X".into(), column: "inbox".parse().unwrap() };
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
    fn response_error_round_trips() {
        let r = Response::Error { message: "nope".into() };
        let back: Response = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);
    }
}
