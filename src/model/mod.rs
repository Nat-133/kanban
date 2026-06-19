// types — Tasks 1–6

pub mod proto;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use time::OffsetDateTime;

/// Single-variant enums: the only representable value is the correct constant,
/// and any other string fails to deserialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ApiVersion {
    #[default]
    #[serde(rename = "kanban.local/v1alpha1")]
    V1Alpha1,
}

macro_rules! kind_tag {
    ($name:ident => $variant:ident => $lit:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
        pub enum $name {
            #[default]
            #[serde(rename = $lit)]
            $variant,
        }
    };
}
kind_tag!(BoardKind => Board => "Board");
kind_tag!(TaskKind => Task => "Task");
kind_tag!(WorkerSessionKind => WorkerSession => "WorkerSession");
kind_tag!(WorkerEventListKind => WorkerEventList => "WorkerEventList");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub name: String,
    #[serde(default, with = "time::serde::rfc3339::option", skip_serializing_if = "Option::is_none")]
    pub creation_timestamp: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IdError {
    #[error("invalid task id: {0:?} (expected `task-NNNN`)")]
    Task(String),
    #[error("invalid column id: must be non-empty")]
    EmptyColumn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(u32);

impl TaskId {
    pub fn new(n: u32) -> Self { Self(n) }
    pub fn as_u32(self) -> u32 { self.0 }
}
impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "task-{:04}", self.0) }
}
impl FromStr for TaskId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, IdError> {
        let n = s.strip_prefix("task-").ok_or_else(|| IdError::Task(s.to_string()))?;
        let v = n.parse::<u32>().map_err(|_| IdError::Task(s.to_string()))?;
        Ok(TaskId(v))
    }
}
impl Serialize for TaskId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for TaskId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ColumnId(String);

impl ColumnId {
    pub fn as_str(&self) -> &str { &self.0 }
}
impl fmt::Display for ColumnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}
impl FromStr for ColumnId {
    type Err = IdError;
    fn from_str(s: &str) -> Result<Self, IdError> {
        if s.trim().is_empty() { return Err(IdError::EmptyColumn); }
        Ok(ColumnId(s.to_string()))
    }
}
impl Serialize for ColumnId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for ColumnId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub id: ColumnId,
    pub title: String,
}

/// Wire shape (mirrors board.yaml exactly). Public, structural — no invariants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawBoard {
    pub api_version: ApiVersion,
    pub kind: BoardKind,
    pub metadata: Metadata,
    pub spec: RawBoardSpec,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, serde_yml::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawBoardSpec {
    pub columns: Vec<Column>,
    pub cards: BTreeMap<ColumnId, Vec<TaskId>>,
}

/// Domain Board: private fields, only constructable via TryFrom<RawBoard>.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawBoard", into = "RawBoard")]
pub struct Board {
    metadata: Metadata,
    columns: Vec<Column>,
    cards: BTreeMap<ColumnId, Vec<TaskId>>,
}

impl Board {
    pub fn metadata(&self) -> &Metadata { &self.metadata }
    pub fn columns(&self) -> &[Column] { &self.columns }
    pub fn cards(&self) -> &BTreeMap<ColumnId, Vec<TaskId>> { &self.cards }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BoardError {
    #[error("duplicate column id: {0}")]
    DuplicateColumn(ColumnId),
    #[error("a card is placed in unknown column {column}")]
    UnknownColumn { task: Option<TaskId>, column: ColumnId },
    #[error("task {0} appears in more than one column")]
    DuplicateTask(TaskId),
}

impl TryFrom<RawBoard> for Board {
    type Error = BoardError;
    fn try_from(raw: RawBoard) -> Result<Self, BoardError> {
        let mut known = BTreeSet::new();
        for col in &raw.spec.columns {
            if !known.insert(col.id.clone()) {
                return Err(BoardError::DuplicateColumn(col.id.clone()));
            }
        }
        let mut seen = BTreeSet::new();
        for (column, tasks) in &raw.spec.cards {
            if !known.contains(column) {
                let task = tasks.first().copied();
                return Err(BoardError::UnknownColumn { task, column: column.clone() });
            }
            for &t in tasks {
                if !seen.insert(t) {
                    return Err(BoardError::DuplicateTask(t));
                }
            }
        }
        Ok(Board { metadata: raw.metadata, columns: raw.spec.columns, cards: raw.spec.cards })
    }
}

impl From<Board> for RawBoard {
    fn from(b: Board) -> RawBoard {
        RawBoard {
            api_version: ApiVersion::V1Alpha1,
            kind: BoardKind::Board,
            metadata: b.metadata,
            spec: RawBoardSpec { columns: b.columns, cards: b.cards },
            unknown: Default::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub api_version: ApiVersion,
    pub kind: TaskKind,
    pub metadata: Metadata,
    pub spec: TaskSpec,
    #[serde(default)]
    pub status: TaskStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSpec {
    pub title: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    pub description_ref: String,
    pub notes_ref: String,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default)]
    pub jira: Jira,
    #[serde(default)]
    pub context: Context,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Jira {
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Context {
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    #[serde(default)]
    pub worker_session_ref: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerEventKind {
    Started,
    Working,
    HumanInputRequired(Notification),
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notification {
    PermissionPrompt,
    IdlePrompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Pending,
    Working,
    WaitingHuman,
    Idle,
    Completed,
    Failed,
}

impl Phase {
    /// Derived, not stored: true exactly when the worker is blocked on a human.
    pub fn needs_human_input(&self) -> bool {
        matches!(self, Phase::WaitingHuman | Phase::Idle)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawWorkerEvent", into = "RawWorkerEvent")]
pub struct WorkerEvent {
    pub kind: WorkerEventKind,
    pub source: String,
    #[allow(dead_code)]
    pub observed_at: OffsetDateTime,
    pub payload_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawEventType { Started, Working, HumanInputRequired, Completed, Failed }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawWorkerEvent {
    #[serde(rename = "type")]
    event_type: RawEventType,
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notification_type: Option<Notification>,
    #[serde(with = "time::serde::rfc3339")]
    observed_at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    payload_ref: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EventError {
    #[error("human_input_required event is missing notificationType")]
    MissingNotification,
    #[error("notificationType is only valid on human_input_required events")]
    UnexpectedNotification,
}

impl TryFrom<RawWorkerEvent> for WorkerEvent {
    type Error = EventError;
    fn try_from(r: RawWorkerEvent) -> Result<Self, EventError> {
        use RawEventType as T;
        let kind = match (r.event_type, r.notification_type) {
            (T::HumanInputRequired, Some(n)) => WorkerEventKind::HumanInputRequired(n),
            (T::HumanInputRequired, None) => return Err(EventError::MissingNotification),
            (_, Some(_)) => return Err(EventError::UnexpectedNotification),
            (T::Started, None) => WorkerEventKind::Started,
            (T::Working, None) => WorkerEventKind::Working,
            (T::Completed, None) => WorkerEventKind::Completed,
            (T::Failed, None) => WorkerEventKind::Failed,
        };
        Ok(WorkerEvent { kind, source: r.source, observed_at: r.observed_at, payload_ref: r.payload_ref })
    }
}

impl From<WorkerEvent> for RawWorkerEvent {
    fn from(e: WorkerEvent) -> RawWorkerEvent {
        let (event_type, notification_type) = match e.kind {
            WorkerEventKind::Started => (RawEventType::Started, None),
            WorkerEventKind::Working => (RawEventType::Working, None),
            WorkerEventKind::HumanInputRequired(n) => (RawEventType::HumanInputRequired, Some(n)),
            WorkerEventKind::Completed => (RawEventType::Completed, None),
            WorkerEventKind::Failed => (RawEventType::Failed, None),
        };
        RawWorkerEvent { event_type, source: e.source, notification_type, observed_at: e.observed_at, payload_ref: e.payload_ref }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerSession {
    pub api_version: ApiVersion,
    pub kind: WorkerSessionKind,
    pub metadata: Metadata,
    pub spec: WorkerSessionSpec,
    #[serde(default)]
    pub status: WorkerSessionStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerSessionSpec {
    pub task_ref: TaskId,
    pub worker: String,
    pub workspace: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<PathBuf>,
    #[serde(default)]
    pub command: Vec<String>,
}

/// Only non-derived facts. Phase/needs_human_input are computed from events.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerSessionStatus {
    #[serde(default, with = "time::serde::rfc3339::option", skip_serializing_if = "Option::is_none")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option", skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_ref: Option<String>,
}

/// Unlike other resources, the envelope fields (`apiVersion`/`kind`/`metadata`)
/// are intentionally permissive here: the events file is loaded without requiring
/// a `kind` tag, and the loader and tests construct it minimally (often with only
/// `items`). This looseness is by design, not an oversight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerEventList {
    #[serde(default)]
    pub api_version: Option<ApiVersion>,
    #[serde(default)]
    pub kind: Option<WorkerEventListKind>,
    #[serde(default)]
    pub metadata: Option<Metadata>,
    #[serde(default)]
    pub items: Vec<WorkerEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub agents: AgentConfig,
    #[serde(default)]
    pub workers: BTreeMap<String, WorkerConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    #[serde(default)]
    pub base_dirs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    pub terminal: TerminalConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalConfig {
    #[serde(rename = "type")]
    pub kind: String,
    pub session_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_parses_and_defaults_status() {
        let yaml = "\
apiVersion: kanban.local/v1alpha1
kind: Task
metadata:
  name: task-0001
spec:
  title: Build structured task format
  summary: Define YAML-backed task resources.
  color: blue
  descriptionRef: description.md
  notesRef: notes.md
  acceptanceCriteria:
    - Task metadata is machine-readable.
  repo: ~/vcs/my-project
  context:
    include: [description.md, notes.md]
    exclude: [../../secrets/]
";
        let t: Task = serde_yml::from_str(yaml).unwrap();
        assert_eq!(t.spec.title, "Build structured task format");
        assert_eq!(t.spec.repo.as_deref(), Some("~/vcs/my-project"));
        assert_eq!(t.spec.context.include.len(), 2);
        assert_eq!(t.status.worker_session_ref, None);
    }

    #[test]
    fn wrong_kind_fails_to_deserialize() {
        // BoardKind only accepts "Board"
        let r: Result<BoardKind, _> = serde_yml::from_str("Banana");
        assert!(r.is_err());
        assert_eq!(serde_yml::from_str::<BoardKind>("Board").unwrap(), BoardKind::Board);
    }

    #[test]
    fn metadata_round_trips_with_rfc3339() {
        let yaml = "\
name: task-0001
creationTimestamp: \"2026-06-17T09:00:00Z\"
labels:
  area: tooling
";
        let md: Metadata = serde_yml::from_str(yaml).unwrap();
        assert_eq!(md.name, "task-0001");
        assert!(md.creation_timestamp.is_some());
        assert_eq!(md.labels.get("area").map(String::as_str), Some("tooling"));
    }

    #[test]
    fn task_id_renders_and_parses() {
        let id: TaskId = "task-0042".parse().unwrap();
        assert_eq!(id.as_u32(), 42);
        assert_eq!(id.to_string(), "task-0042");
        assert!("nope".parse::<TaskId>().is_err());
        assert!("task-xx".parse::<TaskId>().is_err());
    }

    #[test]
    fn task_id_serde_is_string_form() {
        let id = TaskId::new(7);
        let y = serde_yml::to_string(&id).unwrap();
        assert_eq!(y.trim(), "task-0007");
        let back: TaskId = serde_yml::from_str("task-0007").unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn column_id_rejects_empty() {
        assert!("inbox".parse::<ColumnId>().is_ok());
        assert!("".parse::<ColumnId>().is_err());
        assert!(" ".parse::<ColumnId>().is_err());
    }

    fn raw_board(cards: &[(&str, &[&str])], cols: &[&str]) -> RawBoard {
        RawBoard {
            api_version: ApiVersion::V1Alpha1,
            kind: BoardKind::Board,
            metadata: Metadata { name: "default".into(), creation_timestamp: None, labels: Default::default() },
            spec: RawBoardSpec {
                columns: cols.iter().map(|c| Column { id: c.parse().unwrap(), title: c.to_string() }).collect(),
                cards: cards.iter().map(|(c, ts)| (
                    c.parse().unwrap(),
                    ts.iter().map(|t| t.parse().unwrap()).collect(),
                )).collect(),
            },
            unknown: Default::default(),
        }
    }

    #[test]
    fn valid_board_parses() {
        let b = Board::try_from(raw_board(&[("inbox", &["task-0001"]), ("doing", &[])], &["inbox", "doing"])).unwrap();
        assert_eq!(b.columns().len(), 2);
        assert_eq!(b.cards().get(&"inbox".parse().unwrap()).unwrap(), &vec![TaskId::new(1)]);
    }

    #[test]
    fn card_in_unknown_column_is_rejected() {
        let err = Board::try_from(raw_board(&[("ghost", &["task-0001"])], &["inbox"])).unwrap_err();
        assert!(matches!(err, BoardError::UnknownColumn { .. }));
    }

    #[test]
    fn task_in_two_columns_is_rejected() {
        let raw = raw_board(&[("inbox", &["task-0001"]), ("doing", &["task-0001"])], &["inbox", "doing"]);
        assert!(matches!(Board::try_from(raw).unwrap_err(), BoardError::DuplicateTask(_)));
    }

    #[test]
    fn duplicate_column_is_rejected() {
        let raw = raw_board(&[], &["inbox", "inbox"]);
        assert!(matches!(Board::try_from(raw).unwrap_err(), BoardError::DuplicateColumn(_)));
    }

    #[test]
    fn task_listed_twice_in_one_column_is_rejected() {
        let raw = raw_board(&[("inbox", &["task-0001", "task-0001"])], &["inbox"]);
        assert!(matches!(Board::try_from(raw).unwrap_err(), BoardError::DuplicateTask(_)));
    }

    #[test]
    fn board_round_trips_through_yaml() {
        let b = Board::try_from(raw_board(&[("inbox", &["task-0001"])], &["inbox"])).unwrap();
        let y = serde_yml::to_string(&b).unwrap();
        let b2: Board = serde_yml::from_str(&y).unwrap();
        assert_eq!(b, b2);
    }

    #[test]
    fn event_list_parses_sum_type() {
        let yaml = "\
apiVersion: kanban.local/v1alpha1
kind: WorkerEventList
metadata:
  name: s
items:
  - type: started
    source: controller
    observedAt: \"2026-06-17T10:00:00Z\"
  - type: human_input_required
    source: claude-code-hook
    notificationType: permission_prompt
    observedAt: \"2026-06-17T10:30:00Z\"
    payloadRef: hooks/processed/event-0002.json
";
        let list: WorkerEventList = serde_yml::from_str(yaml).unwrap();
        assert_eq!(list.items.len(), 2);
        assert_eq!(list.items[0].kind, WorkerEventKind::Started);
        assert_eq!(list.items[1].kind, WorkerEventKind::HumanInputRequired(Notification::PermissionPrompt));
        assert_eq!(list.items[1].payload_ref.as_deref(), Some("hooks/processed/event-0002.json"));
    }

    #[test]
    fn human_input_without_notification_is_rejected() {
        let yaml = "\
type: human_input_required
source: x
observedAt: \"2026-06-17T10:30:00Z\"
";
        assert!(serde_yml::from_str::<WorkerEvent>(yaml).is_err());
    }

    #[test]
    fn non_notification_event_with_notification_is_rejected() {
        let yaml = "\
type: started
source: x
notificationType: idle_prompt
observedAt: \"2026-06-17T10:30:00Z\"
";
        assert!(serde_yml::from_str::<WorkerEvent>(yaml).is_err());
    }

    #[test]
    fn worker_session_round_trips() {
        let yaml = "\
apiVersion: kanban.local/v1alpha1
kind: WorkerSession
metadata:
  name: task-0001-claude
spec:
  taskRef: task-0001
  worker: claude
  workspace: .kanban/sessions/task-0001
status:
  transcriptRef: transcript.jsonl
";
        let s: WorkerSession = serde_yml::from_str(yaml).unwrap();
        assert_eq!(s.spec.task_ref, TaskId::new(1));
        assert_eq!(s.status.transcript_ref.as_deref(), Some("transcript.jsonl"));
    }

    #[test]
    fn config_parses() {
        let yaml = "\
agents:
  baseDirs:
    - ~/vcs/*
workers:
  claude:
    command: claude
    args: [\"--add-dir\", \".kanban/sessions/{task_id}\"]
    workdir: \"{repo}\"
    terminal:
      type: tmux
      sessionName: kanban-{task_id}
";
        let cfg: Config = serde_yml::from_str(yaml).unwrap();
        assert_eq!(cfg.agents.base_dirs, vec!["~/vcs/*".to_string()]);
        let w = cfg.workers.get("claude").unwrap();
        assert_eq!(w.command, "claude");
        assert_eq!(w.terminal.session_name, "kanban-{task_id}");
    }

    #[test]
    fn worker_event_round_trips_through_yaml() {
        let yaml = "type: human_input_required\nsource: claude-code-hook\nnotificationType: permission_prompt\nobservedAt: \"2026-06-17T10:30:00Z\"\npayloadRef: hooks/processed/e.json\n";
        let e: WorkerEvent = serde_yml::from_str(yaml).unwrap();
        let s = serde_yml::to_string(&e).unwrap();
        let e2: WorkerEvent = serde_yml::from_str(&s).unwrap();
        assert_eq!(e, e2);
        assert_eq!(e.kind, WorkerEventKind::HumanInputRequired(Notification::PermissionPrompt));
    }
}
