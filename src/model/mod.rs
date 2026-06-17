// types — Tasks 1–6

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
}
