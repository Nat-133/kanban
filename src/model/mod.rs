// types — Tasks 1–6

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
}
