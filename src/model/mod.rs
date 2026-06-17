// types — Tasks 1–6

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
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
        if s.is_empty() { return Err(IdError::EmptyColumn); }
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
    #[error("card {task} placed in unknown column {column}")]
    UnknownColumn { task: TaskId, column: ColumnId },
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
                let task = tasks.first().copied().unwrap_or(TaskId::new(0));
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn board_round_trips_through_yaml() {
        let b = Board::try_from(raw_board(&[("inbox", &["task-0001"])], &["inbox"])).unwrap();
        let y = serde_yml::to_string(&b).unwrap();
        let b2: Board = serde_yml::from_str(&y).unwrap();
        assert_eq!(b, b2);
    }
}
