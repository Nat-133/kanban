# Personal Orchestration System — Milestone 2 (Daemon Skeleton) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use @superpowers:executing-plans (or @superpowers:subagent-driven-development) to implement this plan task-by-task. Each task uses @superpowers:test-driven-development.

**Goal:** A working controller daemon you can drive over HTTP: board/task CRUD intents go in as JSON, mutations flow through the single-writer store, snapshots come back. No TUI yet (M3), no worker handoff (M4), no live push/SSE (M3).

**Architecture:** Build on M1's `model` + `controller::store`. Add `model::proto` (the `Intent`/`Response` wire vocabulary — JSON, reusing our validated domain types), `controller::apply` (the pure intent-application core, tested against a temp-dir store with no HTTP involved), and `controller::server` (an `axum` HTTP server bound to loopback that just deserializes an `Intent`, calls `apply`, serializes the `Response`). `clap` adds `kanban init` and `kanban daemon`.

**Key principle (same as M1):** the logic lives in `apply()` and is unit-tested directly against a temp store. The HTTP layer is thin and gets only a couple of integration tests via `reqwest`. Wire format is JSON; files stay YAML.

**Tech Stack additions:** `tokio` (current-thread), `axum`, `serde_json`, `tracing`, `tracing-subscriber`, `clap`; dev: `reqwest` (blocking or async), `tokio` test macros. (`time`/`serde`/`serde_yml`/`thiserror`/`anyhow`/`tempfile` already present.)

**Reference:** `spec.md` (Intents, Board/Task resources, Storage Layout, Safety), and M1 plan `docs/plans/2026-06-17-orchestration-core-m1.md`.

---

## Conventions (same as M1)

- **Rust runs in the devcontainer — use `./x`**: `./x cargo test`, `./x cargo test <name>`, `./x cargo clippy --all-targets`, `./x cargo build`. Plain `cargo` fails.
- **No `Co-Authored-By: Claude...` trailer** in commits (a hook rejects it). Commit via `git -c user.name='Nathaniel Manley' -c user.email='nat.manley@portswigger.net' commit ...`.
- YAML files are camelCase; JSON wire uses the serde types as-is. TDD: test → fail → implement → pass → commit. Commit only the files a task changes.

---

## Task 0: Dependencies + module scaffold

**Files:** `Cargo.toml`, `src/model/mod.rs` (add `pub mod proto;`), `src/model/proto.rs` (new placeholder), `src/controller/mod.rs` (add `apply`, `server`), new placeholder files, `src/main.rs`.

**Step 1:** Add to `Cargo.toml` `[dependencies]`:
```toml
tokio = { version = "1", features = ["rt", "macros", "net", "io-util", "signal"] }
axum = "0.7"
serde_json = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
clap = { version = "4", features = ["derive"] }
```
And `[dev-dependencies]`:
```toml
reqwest = { version = "0.12", features = ["json"] }
tokio = { version = "1", features = ["rt", "macros", "net", "time"] }
```

**Step 2:** `src/model/mod.rs`: add `pub mod proto;` near the top. Create `src/model/proto.rs` with `// proto wire types — Task 1`.

**Step 3:** `src/controller/mod.rs`: becomes
```rust
pub mod store;
pub mod derive;
pub mod apply;
pub mod server;
```
Create `src/controller/apply.rs` (`// intent application — Task 4`) and `src/controller/server.rs` (`// http server — Task 5`).

**Step 4:** Leave `src/main.rs` as the M1 stub for now (clap wired in Task 6). Run `./x cargo build` → compiles (resolving the new deps will take a while; that's expected). If a dep version fails to resolve, STOP and report rather than changing versions.

**Step 5:** Commit: `chore(m2): add daemon dependencies and module scaffold`.

---

## Task 1: `proto` — Intent / Response wire types

The wire vocabulary. Reuses domain types (`Task`, `Board`, `TaskId`, `ColumnId`). JSON, internally tagged so it reads naturally.

**Files:** `src/model/proto.rs`.

**Step 1: Write failing tests** (in `src/model/proto.rs`):
```rust
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
    fn response_error_round_trips() {
        let r = Response::Error { message: "nope".into() };
        let back: Response = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(r, back);
    }
}
```

**Step 2:** `./x cargo test --lib proto` → FAIL.

**Step 3: Implement** in `src/model/proto.rs`:
```rust
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
```
(`#[serde(rename_all = "camelCase")]` on the enum also renames the variant tags: `CreateTask` → `"createTask"`, matching the test.)

**Step 4:** `./x cargo test --lib proto` → PASS. Then `./x cargo test --lib`.

**Step 5:** Commit: `feat(proto): Intent/Response wire vocabulary`.

---

## Task 2: store — workspace init + Task CRUD + archive

**Files:** `src/controller/store.rs`.

**Step 1: Failing tests** (add to the existing `tests` module):
```rust
#[test]
fn init_workspace_creates_layout_and_default_board() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    init_workspace(&root).unwrap();
    assert!(root.join("board.yaml").exists());
    assert!(root.join("tasks").is_dir());
    let board = load_board(&root).unwrap();
    assert!(!board.columns().is_empty());
}

#[test]
fn task_saves_and_loads_back() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    init_workspace(&root).unwrap();
    let task = sample_task(TaskId::new(1), "First");
    save_task(&root, &task).unwrap();
    assert_eq!(load_task(&root, TaskId::new(1)).unwrap(), task);
}

#[test]
fn load_all_tasks_returns_every_saved_task() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    init_workspace(&root).unwrap();
    save_task(&root, &sample_task(TaskId::new(1), "A")).unwrap();
    save_task(&root, &sample_task(TaskId::new(2), "B")).unwrap();
    let mut all = load_all_tasks(&root).unwrap();
    all.sort_by_key(|t| t.metadata.name.clone());
    assert_eq!(all.len(), 2);
}

#[test]
fn archive_task_moves_dir_out_of_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    init_workspace(&root).unwrap();
    save_task(&root, &sample_task(TaskId::new(1), "A")).unwrap();
    archive_task(&root, TaskId::new(1)).unwrap();
    assert!(!root.join("tasks/task-0001").exists());
    assert!(root.join("archive/task-0001").exists());
}

// test helper
fn sample_task(id: TaskId, title: &str) -> Task {
    Task {
        api_version: ApiVersion::V1Alpha1,
        kind: TaskKind::Task,
        metadata: Metadata { name: id.to_string(), creation_timestamp: None, labels: Default::default() },
        spec: TaskSpec {
            title: title.into(), summary: String::new(), color: None,
            description_ref: "description.md".into(), notes_ref: "notes.md".into(),
            acceptance_criteria: vec![], repo: None, jira: Default::default(), context: Default::default(),
        },
        status: Default::default(),
    }
}
```

**Step 2:** `./x cargo test --lib store` → FAIL.

**Step 3: Implement** in `src/controller/store.rs` (a default-board constant + the four functions). The task directory is `tasks/<id>/task.yaml`:
```rust
pub fn tasks_dir(root: &Path) -> PathBuf { root.join("tasks") }
pub fn task_dir(root: &Path, id: TaskId) -> PathBuf { tasks_dir(root).join(id.to_string()) }
fn task_file(root: &Path, id: TaskId) -> PathBuf { task_dir(root, id).join("task.yaml") }

pub fn init_workspace(root: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(tasks_dir(root))?;
    fs::create_dir_all(root.join("sessions"))?;
    fs::create_dir_all(root.join("archive"))?;
    if !board_path(root).exists() {
        let board = Board::try_from(default_board())
            .map_err(|e| anyhow::anyhow!("default board invalid: {e}"))?;
        save_board(root, &board)?;
    }
    Ok(())
}

fn default_board() -> RawBoard {
    let columns = ["inbox", "ready", "doing", "blocked", "waiting-human", "review", "done"];
    RawBoard {
        api_version: ApiVersion::V1Alpha1,
        kind: BoardKind::Board,
        metadata: Metadata { name: "default".into(), creation_timestamp: None, labels: Default::default() },
        spec: RawBoardSpec {
            columns: columns.iter().map(|c| Column {
                id: c.parse().expect("static column id is valid"),
                title: title_case(c),
            }).collect(),
            cards: columns.iter().map(|c| (c.parse().unwrap(), Vec::new())).collect(),
        },
    }
}

fn title_case(s: &str) -> String {
    s.split('-').map(|w| {
        let mut chars = w.chars();
        match chars.next() {
            Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    }).collect::<Vec<_>>().join(" ")
}

pub fn load_task(root: &Path, id: TaskId) -> anyhow::Result<Task> {
    let text = fs::read_to_string(task_file(root, id))?;
    Ok(serde_yml::from_str(&text)?)
}

pub fn save_task(root: &Path, task: &Task) -> anyhow::Result<()> {
    let id: TaskId = task.metadata.name.parse()
        .map_err(|_| anyhow::anyhow!("task metadata.name is not a valid task id: {}", task.metadata.name))?;
    let text = serde_yml::to_string(task)?;
    atomic_write(&task_file(root, id), &text)
}

pub fn load_all_tasks(root: &Path) -> anyhow::Result<Vec<Task>> {
    let mut out = Vec::new();
    let dir = tasks_dir(root);
    if dir.exists() {
        for entry in fs::read_dir(&dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if let Ok(id) = name.parse::<TaskId>() {
                out.push(load_task(root, id)?);
            }
        }
    }
    Ok(out)
}

pub fn archive_task(root: &Path, id: TaskId) -> anyhow::Result<()> {
    let from = task_dir(root, id);
    let to = root.join("archive").join(id.to_string());
    fs::create_dir_all(root.join("archive"))?;
    fs::rename(from, to)?;
    Ok(())
}
```

**Step 4:** `./x cargo test --lib store` → PASS. Then full `--lib`.

**Step 5:** Commit: `feat(store): workspace init, Task CRUD, archive`.

---

## Task 3: unknown-field warning at the load boundary (deferred M1 item)

Permissive parsing stays; warn (via `tracing`) when a loaded file has fields we don't recognize, so typos surface.

**Files:** `src/model/mod.rs` (add catch-all maps to the `Raw*` types + a tiny accessor), `src/controller/store.rs` (warn on load).

**Step 1: Failing test** (in `store.rs` tests):
```rust
#[test]
fn load_board_warns_but_succeeds_on_unknown_field() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    init_workspace(&root).unwrap();
    // inject an unknown top-level field into the saved board.yaml
    let mut text = fs::read_to_string(board_path(&root)).unwrap();
    text.push_str("bogusField: 99\n");
    fs::write(board_path(&root), text).unwrap();
    // still loads (permissive); the unknown field is captured, not fatal
    let board = load_board_checked(&root).unwrap();
    assert!(!board.columns().is_empty());
}
```

**Step 2:** `./x cargo test --lib load_board_warns` → FAIL.

**Step 3: Implement.** In `src/model/mod.rs`, add a flattened catch-all to `RawBoard` and expose it:
```rust
// inside RawBoard:
#[serde(flatten)]
pub unknown: BTreeMap<String, serde_yml::Value>,
```
Add `unknown: Default::default()` everywhere `RawBoard { .. }` is constructed (default_board, tests). In `store.rs`, add a checked loader that deserializes the `RawBoard`, warns on unknowns, then converts:
```rust
pub fn load_board_checked(root: &Path) -> anyhow::Result<Board> {
    let text = fs::read_to_string(board_path(root))?;
    let raw: RawBoard = serde_yml::from_str(&text)?;
    if !raw.unknown.is_empty() {
        let keys: Vec<_> = raw.unknown.keys().cloned().collect();
        tracing::warn!(file = %board_path(root).display(), ?keys, "ignoring unknown field(s) in board.yaml");
    }
    Ok(Board::try_from(raw)?)
}
```
(Keep the existing `load_board` for the simple path; `apply`/the daemon use `load_board_checked`.)

**Step 4:** `./x cargo test --lib load_board_warns` → PASS; full `--lib` green (existing round-trip tests must still pass — the empty `unknown` map flattens to nothing on serialize).

**Step 5:** Commit: `feat(store): warn on unknown YAML fields (permissive load)`.

> Note: scope this to `RawBoard` for M2. The same pattern extends to other `Raw*`/resource types later if needed; don't over-apply now.

---

## Task 4: `apply` — the intent-application core (the meat)

A function that takes the workspace root + an `Intent` and performs it against the store, returning a `Response`. Pure orchestration over the store; fully unit-tested with temp dirs, no HTTP.

**Files:** `src/controller/apply.rs`.

**Step 1: Failing tests** (in `src/controller/apply.rs`):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskId;

    fn setup() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        crate::controller::store::init_workspace(&dir.path().join(".kanban")).unwrap();
        dir
    }

    #[test]
    fn create_task_allocates_id_and_places_on_board() {
        let dir = setup();
        let root = dir.path().join(".kanban");
        let r = apply(&root, Intent::CreateTask { title: "First".into(), summary: "s".into(), column: "inbox".parse().unwrap() }).unwrap();
        assert_eq!(r, Response::Ok { task: Some(TaskId::new(1)) });
        // it is on the board in inbox, and the task file exists
        let board = crate::controller::store::load_board(&root).unwrap();
        assert_eq!(board.cards().get(&"inbox".parse().unwrap()).unwrap(), &vec![TaskId::new(1)]);
        assert!(crate::controller::store::load_task(&root, TaskId::new(1)).is_ok());
    }

    #[test]
    fn get_board_returns_snapshot_with_tasks() {
        let dir = setup();
        let root = dir.path().join(".kanban");
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: "inbox".parse().unwrap() }).unwrap();
        match apply(&root, Intent::GetBoard).unwrap() {
            Response::Snapshot { tasks, .. } => assert_eq!(tasks.len(), 1),
            other => panic!("expected snapshot, got {other:?}"),
        }
    }

    #[test]
    fn move_card_moves_between_columns() {
        let dir = setup();
        let root = dir.path().join(".kanban");
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: "inbox".parse().unwrap() }).unwrap();
        apply(&root, Intent::MoveCard { task: TaskId::new(1), to_column: "doing".parse().unwrap(), position: None }).unwrap();
        let board = crate::controller::store::load_board(&root).unwrap();
        assert!(board.cards().get(&"inbox".parse().unwrap()).unwrap().is_empty());
        assert_eq!(board.cards().get(&"doing".parse().unwrap()).unwrap(), &vec![TaskId::new(1)]);
    }

    #[test]
    fn move_card_to_unknown_column_errors() {
        let dir = setup();
        let root = dir.path().join(".kanban");
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: "inbox".parse().unwrap() }).unwrap();
        let r = apply(&root, Intent::MoveCard { task: TaskId::new(1), to_column: "ghost".parse().unwrap(), position: None }).unwrap();
        assert!(matches!(r, Response::Error { .. }));
    }

    #[test]
    fn archive_removes_card_and_task() {
        let dir = setup();
        let root = dir.path().join(".kanban");
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: "inbox".parse().unwrap() }).unwrap();
        apply(&root, Intent::ArchiveTask { task: TaskId::new(1) }).unwrap();
        let board = crate::controller::store::load_board(&root).unwrap();
        assert!(board.cards().values().all(|v| v.is_empty()));
        assert!(crate::controller::store::load_task(&root, TaskId::new(1)).is_err());
    }
}
```

**Step 2:** `./x cargo test --lib apply` → FAIL.

**Step 3: Implement** `src/controller/apply.rs`. The board is rebuilt by editing the `RawBoard` (mutable), re-validating via `Board::try_from`, and saving. `Response::Error` is returned for domain errors (not `Err`), so the daemon always replies; `Err` is reserved for I/O failures.
```rust
use crate::controller::store;
use crate::model::proto::{Intent, Response};
use crate::model::*;
use std::path::Path;

pub fn apply(root: &Path, intent: Intent) -> anyhow::Result<Response> {
    match intent {
        Intent::InitWorkspace => { store::init_workspace(root)?; Ok(Response::Ok { task: None }) }
        Intent::GetBoard => {
            let board = store::load_board_checked(root)?;
            let tasks = store::load_all_tasks(root)?;
            Ok(Response::Snapshot { board, tasks })
        }
        Intent::CreateTask { title, summary, column } => create_task(root, title, summary, column),
        Intent::EditTask { task, title, summary } => edit_task(root, task, title, summary),
        Intent::MoveCard { task, to_column, position } => move_card(root, task, to_column, position),
        Intent::ReorderCard { task, position } => reorder_card(root, task, position),
        Intent::ArchiveTask { task } => archive(root, task),
    }
}
```
Implement the helpers. Sketch of `create_task` (the others follow the same edit-raw-board-then-revalidate shape):
```rust
fn create_task(root: &Path, title: String, summary: String, column: ColumnId) -> anyhow::Result<Response> {
    let mut raw: RawBoard = store::load_board_checked(root)?.into();
    if !raw.spec.columns.iter().any(|c| c.id == column) {
        return Ok(Response::Error { message: format!("unknown column: {column}") });
    }
    let id = store::next_task_id(root)?;
    let task = Task {
        api_version: ApiVersion::V1Alpha1, kind: TaskKind::Task,
        metadata: Metadata { name: id.to_string(), creation_timestamp: None, labels: Default::default() },
        spec: TaskSpec {
            title, summary, color: None,
            description_ref: "description.md".into(), notes_ref: "notes.md".into(),
            acceptance_criteria: vec![], repo: None, jira: Default::default(), context: Default::default(),
        },
        status: Default::default(),
    };
    store::save_task(root, &task)?;
    raw.spec.cards.entry(column).or_default().push(id);
    let board = match Board::try_from(raw) {
        Ok(b) => b,
        Err(e) => return Ok(Response::Error { message: e.to_string() }),
    };
    store::save_board(root, &board)?;
    Ok(Response::Ok { task: Some(id) })
}
```
Implement `edit_task` (load task, set fields, save — error if missing), `move_card` (remove id from all card lists, validate target column exists, insert at `position` or end, revalidate+save; error if task not found on board or unknown column), `reorder_card` (find the column containing the task, move it to `position` within that column), `archive` (remove id from all card lists, save board, then `store::archive_task`). Use `Response::Error` for all domain-level problems; `?` only for genuine I/O errors.

**Step 4:** `./x cargo test --lib apply` → PASS. Full `--lib` green.

**Step 5:** Commit: `feat(controller): intent-application core (apply)`.

---

## Task 5: `axum` HTTP server

Thin layer: one `POST /v1/intent` route that deserializes an `Intent`, calls `apply`, returns the `Response` as JSON. Bind loopback.

**Files:** `src/controller/server.rs`.

**Step 1: Failing test** (integration, in `src/controller/server.rs` — uses a real ephemeral port + temp workspace + `reqwest`):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::proto::{Intent, Response};

    #[tokio::test]
    async fn post_intent_creates_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();

        // bind :0 for an ephemeral port, serve in the background
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(root.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });

        let client = reqwest::Client::new();
        let base = format!("http://{addr}/v1/intent");
        let create: Response = client.post(&base)
            .json(&Intent::CreateTask { title: "A".into(), summary: "s".into(), column: "inbox".parse().unwrap() })
            .send().await.unwrap().json().await.unwrap();
        assert!(matches!(create, Response::Ok { task: Some(_) }));

        let snap: Response = client.post(&base).json(&Intent::GetBoard)
            .send().await.unwrap().json().await.unwrap();
        match snap { Response::Snapshot { tasks, .. } => assert_eq!(tasks.len(), 1), o => panic!("{o:?}") }
    }
}
```

**Step 2:** `./x cargo test --lib server` → FAIL.

**Step 3: Implement** `src/controller/server.rs`:
```rust
use crate::controller::apply::apply;
use crate::model::proto::{Intent, Response};
use axum::{extract::State, routing::post, Json, Router};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
struct AppState { root: Arc<PathBuf> }

pub fn router(root: PathBuf) -> Router {
    Router::new()
        .route("/v1/intent", post(handle_intent))
        .with_state(AppState { root: Arc::new(root) })
}

async fn handle_intent(State(state): State<AppState>, Json(intent): Json<Intent>) -> Json<Response> {
    let root = (*state.root).clone();
    // store ops are blocking fs; run them off the async reactor
    let resp = tokio::task::spawn_blocking(move || apply(&root, intent))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    match resp {
        Ok(r) => Json(r),
        Err(message) => Json(Response::Error { message }),
    }
}

/// Bind and serve until the process is stopped. Used by `kanban daemon`.
pub async fn serve(root: PathBuf, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "controller listening");
    axum::serve(listener, router(root)).await?;
    Ok(())
}
```

**Step 4:** `./x cargo test --lib server` → PASS. Full `--lib` (note: this adds an integration-style test that opens a socket — fine). Then `./x cargo clippy --all-targets`.

**Step 5:** Commit: `feat(server): axum HTTP server for intents`.

---

## Task 6: `clap` dispatch — `kanban init` / `kanban daemon`

**Files:** `src/main.rs`.

**Step 1:** No unit test for `main` wiring (it's glue); instead this task is verified by `./x cargo build` + a manual smoke check. Replace `src/main.rs`:
```rust
use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "kanban")]
struct Cli {
    /// Workspace root (the .kanban directory).
    #[arg(long, default_value = ".kanban", global = true)]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the workspace layout and a default board.
    Init,
    /// Run the controller daemon.
    Daemon {
        #[arg(long, default_value = "127.0.0.1:7777")]
        addr: SocketAddr,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Init => {
            kanban::controller::store::init_workspace(&cli.root)?;
            println!("initialized workspace at {}", cli.root.display());
            Ok(())
        }
        Command::Daemon { addr } => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            rt.block_on(kanban::controller::server::serve(cli.root, addr))
        }
    }
}
```

**Step 2:** `./x cargo build` → clean. Smoke test (run inside the container):
```bash
./x bash -lc 'cd /workspaces/personal-orchestration-system && rm -rf /tmp/kt && cargo run -q -- --root /tmp/kt/.kanban init && ls /tmp/kt/.kanban'
```
Expected: prints "initialized workspace…" and lists `board.yaml`, `tasks`, `sessions`, `archive`.

**Step 3:** `./x cargo test` (full suite) → all green. `./x cargo clippy --all-targets` → clean.

**Step 4:** Commit: `feat(cli): init and daemon subcommands`.

---

## M2 Done — verification

- `./x cargo test` → all green (M1 + M2 suites).
- `./x cargo clippy --all-targets` → clean.
- Manual: `kanban init` then `kanban daemon`, and in another shell `curl -s 127.0.0.1:7777/v1/intent -d '{"type":"getBoard"}' -H 'content-type: application/json'` returns a JSON snapshot.

**Achieved:** a daemon that owns the store and serves board/task CRUD over HTTP/JSON, with the mutation logic (`apply`) unit-tested independent of the transport. M3 adds the ratatui TUI (a `reqwest` client of this API) and the SSE live-update stream.
