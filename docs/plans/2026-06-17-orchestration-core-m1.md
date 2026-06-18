# Personal Orchestration System — Milestone 1 (Core Model) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use @superpowers:executing-plans to implement this plan task-by-task. Each task uses @superpowers:test-driven-development (test first, watch it fail, minimal implementation, watch it pass, commit).

**Goal:** Build the pure, testable heart of the orchestration system — resource data types **designed so invalid states are unrepresentable**, deterministic state-derivation from a worker event stream, and the filesystem store (atomic load/save + task-ID allocation) — with no daemon, socket, TUI, or child processes yet.

**Architecture:** A single `kanban` binary crate (lib + bin). M1 implements two internal subsystems: `model/` (the data types — the shared vocabulary the TUI and controller both speak) and `controller/{store,derive}` (filesystem I/O and pure state derivation, both controller-owned). Module boundaries are one-directional (`model` depends on nothing internal; `controller` depends on `model`) so they can become separate crates later with no logic changes.

**Type-system principles (the spine of this milestone):**
- **Sum types over flag soup.** Optional payloads live on the one variant that has them (`WorkerEventKind::HumanInputRequired(Notification)`), so `match` is total and the compiler enforces coverage.
- **Derived state is never stored independently.** `needs_human_input` is a *method on `Phase`*, not a field; worker phase is computed by `derive`, not persisted as authoritative.
- **Newtypes for identifiers.** `TaskId(u32)` (rendered `task-0001`), `ColumnId`. Malformed ids can't exist.
- **Constants enforced at the type level.** `apiVersion`/`kind` are single-variant enums, so a `Board` with `kind: "Banana"` won't deserialize or construct.
- **Parse, don't validate.** Where structural typing can't capture an invariant (board referential integrity), domain types have private fields and are built only via `TryFrom<Raw…>`, wired into serde with `#[serde(try_from, into)]`. Any value you hold is already valid.

**Tech Stack:** Rust, `serde` + `serde_yml` (YAML), `time` (RFC3339 timestamps), `thiserror`/`anyhow` (errors), `tempfile` (test temp dirs). `tokio`, `clap`, `ratatui`, `notify`, `serde_json` are deferred to later milestones.

**Reference:** `spec.md`. Key sections for M1: *Board/Task/Worker Session/Event Stream Resources*, *State Derivation*.

---

## Milestone Map (context — only M1 is detailed below)

- **M1 — Core model:** types, store, derivation. *(this plan)*
- **M2 — Daemon skeleton:** UDS server, `proto` intent/response types, board/task CRUD, `clap` dispatch.
  - *Deferred from M1 review:* permissive parsing is kept (no `deny_unknown_fields`), but the store load boundary should **warn on unknown YAML fields** — capture them via a flattened catch-all `BTreeMap<String, serde_yml::Value>` on the `Raw*` types, load through the `Raw*` type so unknowns are inspectable, and emit a warning via the M2 logger (don't log from the pure `model` layer). This surfaces typos (a misspelled field silently parses as missing today) without rejecting forward-compatible extra fields.
- **M3 — TUI:** ratatui board, navigation, intents over the socket, live push.
- **M4 — Worker handoff:** tmux adapter, session workspace + allowlist symlinks + base-dir grants.
- **M5 — Hooks & events:** `kanban hook`, intake-spool draining, event ingestion, derived state → card transitions.
- **M6 — Polish:** Jira indicators, archive, search/filter, external-change warnings.

---

## Conventions for every task

- **The Rust toolchain lives in a devcontainer, not on the host.** Run every cargo/rust command through the `./x` wrapper, which executes inside the container, e.g. `./x cargo build`, `./x cargo test`, `./x cargo test <name>`, `./x cargo clippy`. (`.devcontainer/` and `./x` were created during Task 0 setup; `devcontainer up --workspace-folder .` is already done.)
- YAML field names are camelCase; structs use `#[serde(rename_all = "camelCase")]`.
- Run tests with `./x cargo test` (all) or `./x cargo test <name>` (one).
- Commit after each green test (Conventional Commits).
- Keep `model` free of any dependency on `controller`.

---

## Task 0: Scaffold the project

**Files:** Create `Cargo.toml`, `src/lib.rs`, `src/main.rs`, `.gitignore`, module placeholders.

**Step 1: Init repo**
```bash
cd /Users/nathaniel.manley/Projects/personal-orchestration-system
git init
printf '/target\n' > .gitignore
```

**Step 2: `Cargo.toml`**
```toml
[package]
name = "kanban"
version = "0.1.0"
edition = "2021"

[lib]
name = "kanban"
path = "src/lib.rs"

[[bin]]
name = "kanban"
path = "src/main.rs"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_yml = "0.0.12"
time = { version = "0.3", features = ["serde-well-known", "macros", "parsing", "formatting"] }
thiserror = "1"
anyhow = "1"

[dev-dependencies]
tempfile = "3"
```

**Step 3: Skeleton**

`src/lib.rs`:
```rust
pub mod model;
pub mod controller;
```
`src/main.rs`:
```rust
fn main() {
    // Subcommand dispatch (tui | daemon | hook) is added in M2.
    eprintln!("kanban: no subcommands yet (M1 is library-only)");
}
```
`src/model/mod.rs`: `// types — Tasks 1–6`
`src/controller/mod.rs`:
```rust
pub mod store;
pub mod derive;
```
`src/controller/store.rs`: `// store — Tasks 8–11`
`src/controller/derive.rs`: `// derivation — Task 7`

**Step 4:** Run `cargo build` → compiles (empty-module warnings ok).

**Step 5:** `git add -A && git commit -m "chore: scaffold kanban crate"`

---

## Task 1: Type tags + Metadata

Make `apiVersion`/`kind` correct-by-construction (single-variant enums) and give `Metadata` a typed timestamp.

**Files:** Modify `src/model/mod.rs`.

**Step 1: Failing tests**
```rust
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
```

**Step 2:** `cargo test --lib model` → FAIL (`BoardKind`/`Metadata` missing).

**Step 3: Implementation** (prepend to `src/model/mod.rs`)
```rust
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
```

**Step 4:** `cargo test --lib model` → PASS.

**Step 5:** `git add -A && git commit -m "feat(model): type tags + Metadata with typed timestamp"`

---

## Task 2: Identifier newtypes (`TaskId`, `ColumnId`)

**Files:** Modify `src/model/mod.rs`.

**Step 1: Failing tests** (add to `tests`)
```rust
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
```

**Step 2:** `cargo test --lib task_id` → FAIL.

**Step 3: Implementation** (add to `src/model/mod.rs`)
```rust
use std::fmt;
use std::str::FromStr;

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
```

**Step 4:** `cargo test --lib task_id && cargo test --lib column_id` → PASS.

**Step 5:** `git add -A && git commit -m "feat(model): TaskId/ColumnId newtypes with validated parsing"`

---

## Task 3: Board with validated construction (parse, don't validate)

Domain `Board` has private fields and is built only via `TryFrom<RawBoard>`, which rejects cards in unknown columns, the same task in two columns, and duplicate column ids. Wired into serde with `#[serde(try_from, into)]`.

**Files:** Modify `src/model/mod.rs`.

**Step 1: Failing tests** (add to `tests`)
```rust
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
```

**Step 2:** `cargo test --lib board` → FAIL.

**Step 3: Implementation** (add to `src/model/mod.rs`)
```rust
use std::collections::BTreeSet;

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
```

**Step 4:** `cargo test --lib board` → PASS.

**Step 5:** `git add -A && git commit -m "feat(model): Board with validated TryFrom<RawBoard> construction"`

---

## Task 4: Task type

**Files:** Modify `src/model/mod.rs`.

**Step 1: Failing test**
```rust
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
```

**Step 2:** `cargo test --lib task_parses` → FAIL.

**Step 3: Implementation**
```rust
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
    /// taskRef-style name of the active session; controller-owned.
    #[serde(default)]
    pub worker_session_ref: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<OffsetDateTime>,
}
```

**Step 4:** `cargo test --lib task_parses` → PASS.

**Step 5:** `git add -A && git commit -m "feat(model): Task type"`

---

## Task 5: WorkerEvent as a sum type (validated wire boundary)

The domain event attaches the notification payload to `HumanInputRequired` only. The `RawWorkerEvent` DTO mirrors the loose file shape; `TryFrom` rejects `human_input_required` without a `notificationType` and any other variant *with* one.

**Files:** Modify `src/model/mod.rs`.

**Step 1: Failing tests**
```rust
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
```

**Step 2:** `cargo test --lib event` → FAIL.

**Step 3: Implementation**
```rust
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawWorkerEvent", into = "RawWorkerEvent")]
pub struct WorkerEvent {
    pub kind: WorkerEventKind,
    pub source: String,
    #[allow(dead_code)]
    pub observed_at: OffsetDateTime,
    pub payload_ref: Option<String>,
}

// --- wire DTO ---
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
```

**Step 4:** `cargo test --lib event` → PASS (all three).

**Step 5:** `git add -A && git commit -m "feat(model): WorkerEvent sum type with validated wire boundary"`

---

## Task 6: WorkerSession (no stored derived state)

`status` holds only non-derived facts. Phase and `needs_human_input` are computed (Task 7), never stored — so they can't disagree with the events.

**Files:** Modify `src/model/mod.rs`.

**Step 1: Failing test**
```rust
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
```

**Step 2:** `cargo test --lib worker_session` → FAIL.

**Step 3: Implementation**
```rust
use std::path::PathBuf;

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
```

**Step 4:** `cargo test --lib worker_session` → PASS.

**Step 5:** `git add -A && git commit -m "feat(model): WorkerSession with no stored derived state"`

---

## Task 7: State derivation (total function over the event sum type)

`derive` returns `Phase`; `needs_human_input` is a method on `Phase`. The `match` is exhaustive with **no `_` arm** — adding an event variant later forces a compile error here.

**Files:** Modify `src/model/mod.rs` (add `Phase`) and `src/controller/derive.rs`.

**Step 1: Failing tests** — replace `src/controller/derive.rs`:
```rust
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
```

**Step 2:** `cargo test --lib controller::derive` → FAIL (`Phase` missing).

**Step 3: Implementation** — add `Phase` to `src/model/mod.rs`:
```rust
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
```

**Step 4:** `cargo test --lib controller::derive` → PASS (all).

**Step 5:** `git add -A && git commit -m "feat(controller): total state derivation over the event sum type"`

---

## Task 8: Store — atomic write

**Files:** Replace `src/controller/store.rs`.

**Step 1: Failing test**
```rust
use crate::model::*;
use std::fs;
use std::io::Write;
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_parent_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/board.yaml");
        atomic_write(&path, "hello: world\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello: world\n");
        assert!(!dir.path().join("nested/board.tmp").exists());
    }
}
```

**Step 2:** `cargo test --lib store::tests::atomic_write` → FAIL.

**Step 3: Implementation**
```rust
/// Atomic write: sibling temp file + fsync + rename. Safe because the controller
/// is the single writer of any given file.
pub fn atomic_write(path: &Path, contents: &str) -> anyhow::Result<()> {
    let parent = path.parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}
```

**Step 4:** PASS. **Step 5:** `git commit -m "feat(controller): atomic_write"`

---

## Task 9: Store — load/save Board

**Files:** Modify `src/controller/store.rs`.

**Step 1: Failing test**
```rust
#[test]
fn board_saves_and_loads_back_equal() {
    let dir = tempfile::tempdir().unwrap();
    let raw = RawBoard {
        api_version: ApiVersion::V1Alpha1,
        kind: BoardKind::Board,
        metadata: Metadata { name: "default".into(), creation_timestamp: None, labels: Default::default() },
        spec: RawBoardSpec {
            columns: vec![Column { id: "inbox".parse().unwrap(), title: "Inbox".into() }],
            cards: [("inbox".parse().unwrap(), vec![TaskId::new(1)])].into_iter().collect(),
        },
    };
    let board = Board::try_from(raw).unwrap();
    save_board(dir.path(), &board).unwrap();
    assert_eq!(load_board(dir.path()).unwrap(), board);
}
```

**Step 2:** FAIL.

**Step 3: Implementation**
```rust
use std::path::PathBuf;

pub fn board_path(root: &Path) -> PathBuf { root.join("board.yaml") }

pub fn load_board(root: &Path) -> anyhow::Result<Board> {
    let text = fs::read_to_string(board_path(root))?;
    Ok(serde_yml::from_str(&text)?)
}

pub fn save_board(root: &Path, board: &Board) -> anyhow::Result<()> {
    let text = serde_yml::to_string(board)?;
    atomic_write(&board_path(root), &text)
}
```

**Step 4:** PASS. **Step 5:** `git commit -m "feat(controller): load/save Board"`

---

## Task 10: Store — sequential `TaskId` allocation

**Files:** Modify `src/controller/store.rs`.

**Step 1: Failing tests**
```rust
#[test]
fn next_task_id_starts_at_one() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(next_task_id(dir.path()).unwrap(), TaskId::new(1));
}

#[test]
fn next_task_id_is_max_plus_one() {
    let dir = tempfile::tempdir().unwrap();
    let tasks = dir.path().join("tasks");
    fs::create_dir_all(tasks.join("task-0001")).unwrap();
    fs::create_dir_all(tasks.join("task-0007")).unwrap();
    fs::create_dir_all(tasks.join("not-a-task")).unwrap();
    assert_eq!(next_task_id(dir.path()).unwrap(), TaskId::new(8));
}
```

**Step 2:** FAIL.

**Step 3: Implementation**
```rust
/// Next sequential id. Collision-free because the controller is the single writer.
pub fn next_task_id(root: &Path) -> anyhow::Result<TaskId> {
    let tasks = root.join("tasks");
    let mut max = 0u32;
    if tasks.exists() {
        for entry in fs::read_dir(&tasks)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if let Ok(id) = name.parse::<TaskId>() {
                max = max.max(id.as_u32());
            }
        }
    }
    Ok(TaskId::new(max + 1))
}
```

**Step 4:** PASS. **Step 5:** `git commit -m "feat(controller): sequential TaskId allocation"`

---

## Task 11: Store — load a session's event stream

**Files:** Modify `src/controller/store.rs`.

**Step 1: Failing tests**
```rust
#[test]
fn load_events_empty_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    assert!(load_events(dir.path()).unwrap().is_empty());
}

#[test]
fn load_events_parses_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let yaml = "\
metadata:
  name: s
items:
  - {type: started, source: controller, observedAt: \"2026-06-17T10:00:00Z\"}
  - {type: human_input_required, source: h, notificationType: idle_prompt, observedAt: \"2026-06-17T10:30:00Z\"}
";
    fs::write(dir.path().join("events.yaml"), yaml).unwrap();
    let e = load_events(dir.path()).unwrap();
    assert_eq!(e.len(), 2);
    assert_eq!(e[0].kind, WorkerEventKind::Started);
    assert_eq!(e[1].kind, WorkerEventKind::HumanInputRequired(Notification::IdlePrompt));
}
```

**Step 2:** FAIL.

**Step 3: Implementation**
```rust
pub fn load_events(session_dir: &Path) -> anyhow::Result<Vec<WorkerEvent>> {
    let path = session_dir.join("events.yaml");
    if !path.exists() { return Ok(Vec::new()); }
    let text = fs::read_to_string(&path)?;
    let list: WorkerEventList = serde_yml::from_str(&text)?;
    Ok(list.items)
}
```

**Step 4:** PASS. **Step 5:** `git commit -m "feat(controller): load a session event stream"`

---

## M1 Done — verification

Run: `cargo test` → all pass (≈ 20 tests).
Run: `cargo build` → clean.
Run: `cargo clippy` → address warnings (expect notes on the `#[allow(dead_code)]` for `observed_at`, used from M5 onward).

**Achieved:** a resource vocabulary where the invalid states we identified — wrong `kind`, malformed ids, a `human_input_required` event with no notification, a phase that disagrees with `needs_human_input`, a card in a non-existent column, a task in two columns — are either uncompilable or rejected at the parse boundary. Plus a total, restart-safe derivation function and an atomic store. M2 (daemon + intents) builds on `store` and adds `model::proto`.
