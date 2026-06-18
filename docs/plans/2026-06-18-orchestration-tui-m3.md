# Personal Orchestration System — Milestone 3 (TUI + Live Updates) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use @superpowers:subagent-driven-development to execute task-by-task; each task uses @superpowers:test-driven-development.

**Goal:** A `ratatui` terminal UI — a pure client of the M2 daemon — that renders the board, supports vim navigation, moves/reorders/archives/creates cards via `Intent`s over HTTP, and live-refreshes when the daemon's state changes (SSE). Adds the SSE endpoint to the daemon (deferred from M2).

**Architecture:** The TUI owns no authoritative state (the principle we locked). It holds an in-memory `Snapshot` fetched from the daemon and re-renders it every frame (ratatui is immediate-mode). Key presses become `Intent`s POSTed to the daemon; after a mutation the TUI refetches the snapshot. A `GET /v1/events` SSE stream pushes a lightweight "changed" ping; on each ping the TUI refetches (the snapshot stays the single source of truth — no diff logic). Testability split (same as `apply`): UI state + key handling live in pure functions (`App::on_key`) unit-tested without a terminal; rendering is asserted with ratatui's `TestBackend`; the async select-loop is thin glue.

**Scope (M3):** render, navigate (`hjkl`), move card across columns (`H`/`L`), reorder within column (`J`/`K`), archive (`d`), add task (`a` → single-line title prompt), help (`?`), quit (`q`), SSE live-refresh, `kanban tui` subcommand. **Deferred to M6:** multi-field edit, search/filter, task-detail view, Jira-open, worker indicators (M4/M5).

**Tech Stack additions:** `ratatui` 0.29, `crossterm` 0.28 (feature `event-stream`), `reqwest` 0.12 (promote to a normal dependency, `json`), `reqwest-eventsource` 0.6, `futures-util` 0.3, `tokio-stream` 0.1; server side uses `axum`'s SSE + `tokio::sync::broadcast`.

**Reference:** `spec.md` (TUI Requirements, suggested shortcuts), M2 plan, and the existing `model::proto`, `controller::{apply,server,store}`.

---

## Conventions (same as before)

- **Rust runs in the devcontainer — use `./x`**: `./x cargo test`, `./x cargo test <name>`, `./x cargo clippy --all-targets`, `./x cargo build`. Plain `cargo` fails.
- **No `Co-Authored-By: Claude...` trailer** in commits (a hook rejects it). Commit via `git -c user.name='Nathaniel Manley' -c user.email='nat.manley@portswigger.net' commit ...`.
- TDD: test → fail → implement → pass → commit. Commit only the files a task changes.

---

## Task 0: Dependencies + module scaffold

**Files:** `Cargo.toml`, `src/lib.rs` (add `pub mod tui;`), new `src/tui/` module files.

**Step 1:** In `Cargo.toml`, move `reqwest` from `[dev-dependencies]` to `[dependencies]` (keep the `json` feature). Add to `[dependencies]`:
```toml
ratatui = "0.29"
crossterm = { version = "0.28", features = ["event-stream"] }
reqwest-eventsource = "0.6"
futures-util = "0.3"
tokio-stream = { version = "0.1", features = ["sync"] }
```
Add `tokio` features needed by the SSE broadcast/stream: ensure the main `tokio` dependency includes `sync` and `time` (merge into the existing feature list: `["rt", "macros", "net", "io-util", "signal", "sync", "time"]`). `reqwest` stays in `[dev-dependencies]` too only if needed — but since it's now a normal dep, remove the dev-dep duplicate (keep `tempfile`, and `tokio` dev features if any).

**Step 2:** `src/lib.rs`: becomes
```rust
pub mod model;
pub mod controller;
pub mod tui;
```

**Step 3:** Create the TUI module skeleton:
- `src/tui/mod.rs`:
```rust
pub mod app;
pub mod client;
pub mod ui;
pub mod run;
```
- `src/tui/app.rs` → `// app state + key handling — Task 3`
- `src/tui/client.rs` → `// http client — Task 2`
- `src/tui/ui.rs` → `// rendering — Task 4`
- `src/tui/run.rs` → `// async event loop — Task 5`

**Step 4:** `./x cargo build` → compiles (resolving ratatui/crossterm takes a while). If a version fails to resolve, STOP and report.

**Step 5:** Commit: `chore(m3): add TUI dependencies and module scaffold`.

---

## Task 1: Daemon SSE endpoint + change broadcast

Add a `tokio::sync::broadcast` channel to the server; broadcast a "changed" ping after any successful mutating intent; expose it as `GET /v1/events` (SSE).

**Files:** `src/controller/server.rs`.

**Step 1: Failing test** (integration, in `server.rs` tests) — a mutation POST causes an SSE event to arrive:
```rust
#[tokio::test]
async fn mutation_emits_sse_event() {
    use futures_util::StreamExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    crate::controller::store::init_workspace(&root).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router(root)).await.unwrap(); });

    // open the SSE stream
    let mut es = reqwest_eventsource::EventSource::get(format!("http://{addr}/v1/events"));
    // wait for the Open event first
    loop {
        match es.next().await.unwrap().unwrap() {
            reqwest_eventsource::Event::Open => break,
            _ => {}
        }
    }
    // trigger a mutation
    let client = reqwest::Client::new();
    client.post(format!("http://{addr}/v1/intent"))
        .json(&crate::model::proto::Intent::CreateTask { title: "A".into(), summary: "s".into(), column: "inbox".parse().unwrap() })
        .send().await.unwrap();
    // expect a message event
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let reqwest_eventsource::Event::Message(m) = es.next().await.unwrap().unwrap() {
                return m.event;
            }
        }
    }).await.unwrap();
    assert_eq!(got, "changed");
}
```

**Step 2:** `./x cargo test --lib mutation_emits_sse` → FAIL.

**Step 3: Implement.** Extend `AppState` with a `tokio::sync::broadcast::Sender<()>`; add the SSE route and broadcast on mutation:
```rust
use axum::response::sse::{Event, Sse};
use axum::routing::get;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;

#[derive(Clone)]
struct AppState {
    root: Arc<PathBuf>,
    changes: broadcast::Sender<()>,
}

pub fn router(root: PathBuf) -> Router {
    let (tx, _rx) = broadcast::channel(64);
    Router::new()
        .route("/v1/intent", post(handle_intent))
        .route("/v1/events", get(sse_events))
        .with_state(AppState { root: Arc::new(root), changes: tx })
}

async fn handle_intent(State(state): State<AppState>, Json(intent): Json<Intent>) -> Json<Response> {
    let is_mutation = !matches!(intent, Intent::GetBoard);
    let root = (*state.root).clone();
    let result = tokio::task::spawn_blocking(move || apply(&root, intent)).await;
    let resp = match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Response::Error { message: e.to_string() },
        Err(join_err) => Response::Error { message: format!("internal task error: {join_err}") },
    };
    // notify subscribers only on a successful mutation
    if is_mutation && !matches!(resp, Response::Error { .. }) {
        let _ = state.changes.send(()); // ignore "no subscribers"
    }
    Json(resp)
}

async fn sse_events(State(state): State<AppState>) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let stream = BroadcastStream::new(state.changes.subscribe())
        .filter_map(|r| r.ok())
        .map(|()| Ok(Event::default().event("changed").data("")));
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}
```
(`reqwest`/`reqwest_eventsource`/`futures_util` are now normal deps, usable from tests.)

**Step 4:** `./x cargo test --lib mutation_emits_sse` → PASS; full `./x cargo test`; `./x cargo clippy --all-targets`.

**Step 5:** Commit: `feat(server): SSE change stream (/v1/events)`.

---

## Task 2: TUI HTTP client

A thin typed client wrapping `reqwest`.

**Files:** `src/tui/client.rs`.

**Step 1: Failing test** (integration against the in-process router):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::proto::Intent;

    #[tokio::test]
    async fn client_create_and_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, crate::controller::server::router(root)).await.unwrap(); });

        let client = Client::new(format!("http://{addr}"));
        client.send(Intent::CreateTask { title: "A".into(), summary: "s".into(), column: "inbox".parse().unwrap() }).await.unwrap();
        let snap = client.snapshot().await.unwrap();
        assert_eq!(snap.tasks.len(), 1);
    }
}
```
Note: `router` must be `pub` (it already is). This test needs a `Snapshot` type — define a small struct the client returns (see below).

**Step 2:** `./x cargo test --lib client` → FAIL.

**Step 3: Implement** `src/tui/client.rs`:
```rust
use crate::model::proto::{Intent, Response};
use crate::model::{Board, Task};

/// The board view the TUI renders.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub board: Board,
    pub tasks: Vec<Task>,
}

pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base: String) -> Self {
        Self { base, http: reqwest::Client::new() }
    }

    pub async fn send(&self, intent: Intent) -> anyhow::Result<Response> {
        let resp = self.http.post(format!("{}/v1/intent", self.base))
            .json(&intent).send().await?
            .json::<Response>().await?;
        Ok(resp)
    }

    pub async fn snapshot(&self) -> anyhow::Result<Snapshot> {
        match self.send(Intent::GetBoard).await? {
            Response::Snapshot { board, tasks } => Ok(Snapshot { board, tasks }),
            Response::Error { message } => Err(anyhow::anyhow!(message)),
            Response::Ok { .. } => Err(anyhow::anyhow!("unexpected Ok response to GetBoard")),
        }
    }
}
```

**Step 4:** `./x cargo test --lib client` → PASS; full suite.

**Step 5:** Commit: `feat(tui): HTTP client (snapshot + send intent)`.

---

## Task 3: App state + key handling (pure, unit-tested)

The heart of the TUI. `App` holds the snapshot + selection + mode; `on_key` mutates selection or returns an `Action` (an intent to send, quit, or nothing). No terminal, no I/O — fully unit-tested.

**Files:** `src/tui/app.rs`.

**Step 1: Failing tests** (`src/tui/app.rs`):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::proto::Intent;
    use crate::model::TaskId;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use crate::tui::client::Snapshot;

    fn key(c: char) -> KeyEvent { KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE) }

    // a snapshot with two columns; inbox has task-0001, task-0002; doing empty
    fn snap() -> Snapshot {
        use crate::controller::store;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        // create two tasks via apply for realism
        crate::controller::apply::apply(&root, Intent::CreateTask { title: "First".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
        crate::controller::apply::apply(&root, Intent::CreateTask { title: "Second".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
        let board = store::load_board(&root).unwrap();
        let tasks = store::load_all_tasks(&root).unwrap();
        Snapshot { board, tasks }
    }

    #[test]
    fn j_k_move_card_selection() {
        let mut app = App::new(snap());
        assert_eq!(app.selected_row(), 0);
        app.on_key(key('j'));
        assert_eq!(app.selected_row(), 1);
        app.on_key(key('k'));
        assert_eq!(app.selected_row(), 0);
    }

    #[test]
    fn l_h_change_columns_and_clamp_row() {
        let mut app = App::new(snap());
        // start in inbox (col 0); move down to row 1, then right to an empty column -> row clamps to 0
        app.on_key(key('j'));
        let before = app.selected_col();
        app.on_key(key('l'));
        assert!(app.selected_col() > before);
        assert_eq!(app.selected_row(), 0); // empty/short column clamps
    }

    #[test]
    fn q_quits() {
        let mut app = App::new(snap());
        let action = app.on_key(key('q'));
        assert_eq!(action, Action::Quit);
    }

    #[test]
    fn shift_l_emits_move_card_intent() {
        let mut app = App::new(snap()); // selected = task-0001 in inbox (col 0)
        let action = app.on_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        match action {
            Action::Send(Intent::MoveCard { task, to_column, .. }) => {
                assert_eq!(task, TaskId::new(1));
                assert_eq!(to_column.as_str(), "ready"); // column to the right of inbox
            }
            o => panic!("expected MoveCard, got {o:?}"),
        }
    }

    #[test]
    fn d_emits_archive_intent() {
        let mut app = App::new(snap());
        let action = app.on_key(key('d'));
        assert_eq!(action, Action::Send(Intent::ArchiveTask { task: TaskId::new(1) }));
    }

    #[test]
    fn add_task_flow_emits_create_intent() {
        let mut app = App::new(snap());
        app.on_key(key('a'));            // enter add mode
        assert!(matches!(app.mode(), Mode::AddTask));
        app.on_key(key('H')); app.on_key(key('i'));   // type "Hi" (chars; Shift irrelevant in input for the test)
        let action = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            Action::Send(Intent::CreateTask { title, column, .. }) => {
                assert_eq!(title, "Hi");
                assert_eq!(column.as_str(), "inbox");
            }
            o => panic!("expected CreateTask, got {o:?}"),
        }
        assert!(matches!(app.mode(), Mode::Normal));
    }
}
```

**Step 2:** `./x cargo test --lib tui::app` → FAIL.

**Step 3: Implement** `src/tui/app.rs`. Sketch (fill in fully):
```rust
use crate::model::proto::Intent;
use crate::model::TaskId;
use crate::tui::client::Snapshot;
use crossterm::event::{KeyCode, KeyEvent};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Quit,
    Send(Intent),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    AddTask,
    Help,
}

pub struct App {
    snapshot: Snapshot,
    col: usize,
    row: usize,
    mode: Mode,
    input: String,
    pub status: Option<String>,
}

impl App {
    pub fn new(snapshot: Snapshot) -> Self {
        Self { snapshot, col: 0, row: 0, mode: Mode::Normal, input: String::new(), status: None }
    }

    pub fn snapshot(&self) -> &Snapshot { &self.snapshot }
    pub fn set_snapshot(&mut self, s: Snapshot) { self.snapshot = s; self.clamp(); }
    pub fn mode(&self) -> &Mode { &self.mode }
    pub fn input(&self) -> &str { &self.input }
    pub fn selected_col(&self) -> usize { self.col }
    pub fn selected_row(&self) -> usize { self.row }

    fn columns(&self) -> &[crate::model::Column] { self.snapshot.board.columns() }
    fn column_ids(&self) -> Vec<crate::model::ColumnId> {
        self.columns().iter().map(|c| c.id.clone()).collect()
    }
    fn current_column_cards(&self) -> Vec<TaskId> {
        let id = &self.columns()[self.col].id;
        self.snapshot.board.cards().get(id).cloned().unwrap_or_default()
    }
    pub fn selected_task(&self) -> Option<TaskId> {
        self.current_column_cards().get(self.row).copied()
    }

    fn clamp(&mut self) {
        if self.columns().is_empty() { self.col = 0; self.row = 0; return; }
        self.col = self.col.min(self.columns().len() - 1);
        let n = self.current_column_cards().len();
        self.row = if n == 0 { 0 } else { self.row.min(n - 1) };
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        match self.mode {
            Mode::Normal => self.on_key_normal(key),
            Mode::AddTask => self.on_key_add(key),
            Mode::Help => { self.mode = Mode::Normal; Action::None }
        }
    }

    fn on_key_normal(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('h') => { if self.col > 0 { self.col -= 1; } self.clamp(); }
            KeyCode::Char('l') => { if self.col + 1 < self.columns().len() { self.col += 1; } self.clamp(); }
            KeyCode::Char('j') => { let n = self.current_column_cards().len(); if n > 0 && self.row + 1 < n { self.row += 1; } }
            KeyCode::Char('k') => { if self.row > 0 { self.row -= 1; } }
            KeyCode::Char('H') => return self.move_card(-1),
            KeyCode::Char('L') => return self.move_card(1),
            KeyCode::Char('J') => return self.reorder(1),
            KeyCode::Char('K') => return self.reorder(-1),
            KeyCode::Char('d') => { if let Some(t) = self.selected_task() { return Action::Send(Intent::ArchiveTask { task: t }); } }
            KeyCode::Char('a') => { self.mode = Mode::AddTask; self.input.clear(); }
            KeyCode::Char('?') => { self.mode = Mode::Help; }
            _ => {}
        }
        Action::None
    }

    fn move_card(&mut self, dir: isize) -> Action {
        let cols = self.column_ids();
        let target = self.col as isize + dir;
        if target < 0 || target as usize >= cols.len() { return Action::None; }
        match self.selected_task() {
            Some(task) => Action::Send(Intent::MoveCard { task, to_column: cols[target as usize].clone(), position: None }),
            None => Action::None,
        }
    }

    fn reorder(&mut self, dir: isize) -> Action {
        let new = self.row as isize + dir;
        let n = self.current_column_cards().len() as isize;
        if new < 0 || new >= n { return Action::None; }
        match self.selected_task() {
            Some(task) => Action::Send(Intent::ReorderCard { task, position: new as usize }),
            None => Action::None,
        }
    }

    fn on_key_add(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => { self.mode = Mode::Normal; self.input.clear(); Action::None }
            KeyCode::Enter => {
                let title = std::mem::take(&mut self.input);
                self.mode = Mode::Normal;
                if title.is_empty() { return Action::None; }
                let column = self.columns()[self.col].id.clone();
                Action::Send(Intent::CreateTask { title, summary: String::new(), column })
            }
            KeyCode::Backspace => { self.input.pop(); Action::None }
            KeyCode::Char(c) => { self.input.push(c); Action::None }
            _ => Action::None,
        }
    }
}
```
(Adjust until the tests pass. Note: the `shift_l_emits_move_card_intent` test sends `KeyCode::Char('L')` — crossterm reports the shifted char as uppercase `'L'`, so matching `'H'`/`'L'`/`'J'`/`'K'` on the char is correct.)

**Step 4:** `./x cargo test --lib tui::app` → PASS; full suite.

**Step 5:** Commit: `feat(tui): app state and key handling`.

---

## Task 4: Rendering (ratatui + TestBackend)

Render the board; assert with `TestBackend` that columns and card titles appear.

**Files:** `src/tui/ui.rs`.

**Step 1: Failing test** (`src/tui/ui.rs`):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use ratatui::{backend::TestBackend, Terminal};

    fn snap() -> crate::tui::client::Snapshot {
        use crate::controller::{store, apply::apply};
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "Buy milk".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
        crate::tui::client::Snapshot { board: store::load_board(&root).unwrap(), tasks: store::load_all_tasks(&root).unwrap() }
    }

    #[test]
    fn renders_columns_and_card_titles() {
        let app = App::new(snap());
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Inbox"));
        assert!(text.contains("Doing"));
        assert!(text.contains("Buy milk"));
    }
}
```

**Step 2:** `./x cargo test --lib tui::ui` → FAIL.

**Step 3: Implement** `src/tui/ui.rs`: a `pub fn render(f: &mut ratatui::Frame, app: &App)`. Split the area into one horizontal chunk per column; render each column as a `Block` (titled with the column title) containing a `List` of card titles (look up each `TaskId` in `app.snapshot().tasks` by `metadata.name`); highlight the selected column's border and the selected row; draw a one-line footer with key hints; if `app.mode()` is `AddTask`, draw a small centered input box showing `app.input()`; if `Help`, draw a help overlay. Expose `render` as `pub`. Keep it straightforward — the test only asserts the column titles + a card title are present, but implement the selection highlight and footer too (they're cheap and needed by the real app). Add accessor(s) on `App` if rendering needs them (e.g. a way to get a column's ordered tasks with titles); prefer adding small read-only helpers on `App` over reaching into private fields.

**Step 4:** `./x cargo test --lib tui::ui` → PASS; full suite; `./x cargo clippy --all-targets`.

**Step 5:** Commit: `feat(tui): board rendering`.

---

## Task 5: Async run loop + `kanban tui` subcommand

Tie it together: fetch initial snapshot, then `select!` over terminal input, the SSE stream, and a tick; apply keys, send intents, refetch on mutation or SSE ping; redraw each iteration. Wire the `tui` subcommand.

**Files:** `src/tui/run.rs`, `src/main.rs`.

This task is integration glue — verified by build + clippy + a manual smoke note (no unit test for the loop itself; the pieces it calls are all tested).

**Step 1: Implement** `src/tui/run.rs` with `pub async fn run(base: String) -> anyhow::Result<()>`:
- Enter raw mode + alternate screen (crossterm), build `Terminal<CrosstermBackend>`. Ensure a teardown guard restores the terminal on exit/panic.
- `let client = Client::new(base.clone());` fetch initial snapshot → `App::new(snapshot)`.
- Open SSE: `reqwest_eventsource::EventSource::get(format!("{base}/v1/events"))`.
- Use `crossterm::event::EventStream` for async key input. Loop:
  ```text
  loop {
    terminal.draw(|f| render(f, &app))?;
    tokio::select! {
      maybe_key = input.next() => { handle key -> App::on_key -> match Action { Quit => break, Send(i) => { client.send(i).await?; app.set_snapshot(client.snapshot().await?); }, None => {} } }
      maybe_ev  = sse.next()   => { on a "changed" message, app.set_snapshot(client.snapshot().await?); ignore Open/errors }
    }
  }
  ```
- Restore terminal (leave alternate screen, disable raw mode) on the way out.

Keep error handling pragmatic: a failed `send`/`snapshot` should set `app.status = Some(err)` rather than crash the UI (don't `?` on transient request errors inside the loop; reserve `?` for setup/teardown). 

**Step 2:** Add the `tui` subcommand to `src/main.rs`:
```rust
// in enum Command:
/// Launch the terminal UI (connects to a running daemon).
Tui {
    #[arg(long, default_value = "http://127.0.0.1:7777")]
    daemon: String,
},
// in match:
Command::Tui { daemon } => {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(kanban::tui::run::run(daemon))
}
```

**Step 3:** `./x cargo build` → clean. `./x cargo clippy --all-targets` → clean. (Manual smoke is optional and awkward in headless CI — the daemon + client + app + render are all independently tested; the loop is thin. If you want, document the manual check in the commit body: run `kanban daemon` in one shell, `kanban tui` in another.)

**Step 4:** Full `./x cargo test` → all green.

**Step 5:** Commit: `feat(tui): async run loop and tui subcommand`.

---

## M3 Done — verification

- `./x cargo test` → all green; `./x cargo clippy --all-targets` → clean.
- Manual (interactive, optional): `kanban init`; `kanban daemon` in one terminal; `kanban tui` in another → board renders, `hjkl` navigates, `a` adds a task, `H`/`L` moves it across columns, `d` archives, and a change made via `curl` to the daemon refreshes the TUI live (SSE).

**Achieved:** a usable, live-updating Kanban TUI that is a pure client of the daemon. M4 adds worker handoff (tmux adapter, session workspace + symlinks + base-dir grants); M6 adds edit/search/detail/Jira polish.
