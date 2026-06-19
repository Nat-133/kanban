# Personal Orchestration System — Milestone 6 (TUI Polish) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use @superpowers:subagent-driven-development; each task uses @superpowers:test-driven-development.

**Goal:** Round out the TUI: live worker-state indicators on cards, attach-to-session (`t`), task edit (`e`), search/filter (`/`), and a task detail overlay (Enter) with Jira info. This completes the spec's TUI requirements.

**Architecture:** Build on M1–M5. One data enabler: the board snapshot gains a per-task worker view (session name + derived phase + needs-human flag), computed by the daemon from each session's event stream (reusing M1 `derive`). Everything else is TUI-side: `App` mode/selection logic stays pure and unit-tested; rendering is asserted via `TestBackend`; only the attach action (which suspends the TUI to run `tmux attach`) is run-loop glue.

**Scope (M6):** snapshot sessions + derived phase; card indicators; attach (`t`); edit (`e`); search/filter (`/`); detail overlay (Enter) + Jira indicator. **Deferred/out of scope:** opening Jira URLs in a browser (shelling to `open`/`xdg-open` — untestable here; the detail view shows the URL), multi-field edit beyond title/summary.

**Reference:** `spec.md` — *TUI Requirements*, *suggested shortcuts*, *Human Input Flow*, *Jira*, *Safety (waiting-for-human indicators)*. Existing: `model::proto`, `controller::{apply,store,handoff,events,server}`, `tui::{app,ui,client,run}`.

---

## Conventions

- **Rust in devcontainer — use `./x`** for cargo. Plain `cargo` fails.
- zoxide `cd` hook: run git as `git -C /Users/nathaniel.manley/Projects/personal-orchestration-system ...` (no `cd`).
- **No `Co-Authored-By: Claude...` trailer**. Commit as `Nathaniel Manley <nat.manley@portswigger.net>`.
- TDD: test → fail → implement → pass → commit. Commit only a task's files.

---

## Task 1: Snapshot includes worker sessions + derived phase

The enabler. `Response::Snapshot` gains `sessions: Vec<SessionView>`; the daemon builds it from each session's `session.yaml` + derived phase. Handoff records the resolved tmux session name on the `WorkerSession` so the TUI can attach.

**Files:** `src/model/mod.rs` (add `session_name` to `WorkerSessionSpec`), `src/model/proto.rs` (`SessionView` + extend `Snapshot`), `src/controller/store.rs` (`load_all_sessions`), `src/controller/apply.rs` (build sessions in `GetBoard`), `src/controller/handoff.rs` (set `session_name`).

**Step 1: Failing tests.**
- proto round-trip (`proto.rs` tests):
```rust
#[test]
fn session_view_round_trips() {
    let sv = SessionView { task: TaskId::new(1), session_name: "kanban-task-0001".into(), phase: crate::model::Phase::WaitingHuman, needs_human_input: true };
    let back: SessionView = serde_json::from_str(&serde_json::to_string(&sv).unwrap()).unwrap();
    assert_eq!(sv, back);
}
```
- apply (`apply.rs` tests): after create + handoff (via FakeLauncher? apply uses real TmuxLauncher — instead simulate a session by writing one), GetBoard returns a session view. Simpler test: write a session via `handoff` with a fake launcher is in handoff.rs; here, test `GetBoard` includes a session by constructing one through the store:
```rust
#[test]
fn get_board_includes_sessions_with_phase() {
    let dir = setup(); let r = root(&dir);
    apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("inbox") }).unwrap();
    // hand off using the fake launcher path
    let fake = crate::controller::handoff::tests_fake(); // see note
    crate::controller::handoff::handoff_with(&r, TaskId::new(1), "claude", &fake).unwrap(); // OR reuse handoff() with a fake
    // record a permission_prompt event so phase derives to WaitingHuman
    crate::controller::events::record_intake(&r, TaskId::new(1), "notification", "{\"notification_type\":\"permission_prompt\"}").unwrap();
    crate::controller::events::ingest_session(&r, TaskId::new(1)).unwrap();
    match apply(&r, Intent::GetBoard).unwrap() {
        Response::Snapshot { sessions, .. } => {
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].task, TaskId::new(1));
            assert!(sessions[0].needs_human_input);
        }
        o => panic!("{o:?}"),
    }
}
```
NOTE on the fake: `handoff()` takes `&dyn Launcher`. The `FakeLauncher` currently lives in `handoff.rs`'s test module. For this apply test, the simplest path that avoids cross-module test visibility is: in the test, build the session directly — call `crate::controller::handoff::handoff(&r, TaskId::new(1), "claude", &NoLaunch)` where you define a local no-op `Launcher` in the apply test module. Define:
```rust
struct NoLaunch;
impl crate::controller::handoff::Launcher for NoLaunch {
    fn launch(&self, _s: &crate::model::WorkerSession, _n: &str) -> anyhow::Result<()> { Ok(()) }
}
```
(The `Launcher` trait is `pub`.) Use `&NoLaunch` so no tmux is spawned. Adjust the test accordingly.

**Step 2:** FAIL.

**Step 3: Implement.**
- `WorkerSessionSpec`: add `#[serde(default, skip_serializing_if = "Option::is_none")] pub session_name: Option<String>,`. Update ALL `WorkerSessionSpec { .. }` literals (handoff `prepare_session`, the M4 launcher test, any others) with `session_name: ...` (in `prepare_session` set `session_name: Some(substitute(&worker.terminal.session_name, &id.to_string(), task.spec.repo.as_deref()))`; elsewhere `None`). Search `WorkerSessionSpec {` repo-wide.
- `proto.rs`: add
```rust
use crate::model::Phase;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub task: TaskId,
    pub session_name: String,
    pub phase: Phase,
    pub needs_human_input: bool,
}
```
and extend `Response::Snapshot` to `Snapshot { board: Board, tasks: Vec<Task>, sessions: Vec<SessionView> }`.
- `store.rs`: add `load_all_sessions(root) -> anyhow::Result<Vec<WorkerSession>>` (read `sessions/*/session.yaml`, parse each; skip dirs without a session.yaml). Add `load_session(root, id)` if convenient.
- `apply.rs` `GetBoard`: after loading board + tasks, build sessions:
```rust
let mut sessions = Vec::new();
for s in store::load_all_sessions(root)? {
    let phase = crate::controller::derive::derive(&store::load_events(&store::session_dir(root, s.spec.task_ref))?);
    sessions.push(SessionView {
        task: s.spec.task_ref,
        session_name: s.spec.session_name.clone().unwrap_or_default(),
        phase,
        needs_human_input: phase.needs_human_input(),
    });
}
Ok(Response::Snapshot { board, tasks, sessions })
```
(import `SessionView`.) Update the existing `get_board_returns_snapshot_with_tasks` test if it destructures `Snapshot { tasks, .. }` — `..` covers the new field, so it should still compile.

**Step 4:** all tests pass; full `./x cargo test`; clippy.

**Step 5:** Commit (all five files): `feat(snapshot): include worker sessions with derived phase`.

---

## Task 2: Card worker-state indicators

`tui::client::Snapshot` gains `sessions`; rendering shows a phase badge on cards that have a session (and highlights needs-human).

**Files:** `src/tui/client.rs` (add `sessions` to `Snapshot`), `src/tui/app.rs` (accessor `session_for(task) -> Option<&SessionView>`), `src/tui/ui.rs` (badge).

**Step 1: Failing tests.**
- `tui/ui.rs` TestBackend test: a task with a `WaitingHuman` session renders an indicator glyph (e.g. the card line contains a marker like `‼` or `[blocked]`); assert the chosen marker text appears.
- `tui/app.rs`: `session_for` returns the matching `SessionView`.

**Step 2:** FAIL.

**Step 3: Implement.**
- `client.rs`: `Snapshot { pub board: Board, pub tasks: Vec<Task>, pub sessions: Vec<crate::model::proto::SessionView> }`. The `snapshot()` mapping already destructures `Response::Snapshot { board, tasks }` — extend to `{ board, tasks, sessions }`.
- `app.rs`: `pub fn session_for(&self, task: TaskId) -> Option<&SessionView>` (find in `self.snapshot().sessions`).
- `ui.rs`: when rendering each card, if `app.session_for(id)` is `Some`, prefix/suffix a short badge derived from `phase` (e.g. `working`/`blocked`/`idle`/`done`), and if `needs_human_input`, style it attention-grabbing (e.g. red/bold). Keep the card's task title present (don't break Task-4 era assertions). Choose a concrete marker string and assert it in the test.

**Step 4:** tests pass; full suite; clippy.

**Step 5:** Commit: `feat(tui): worker-state indicators on cards`.

---

## Task 3: Attach to session (`t`)

`t` → `Action::Attach(session_name)`; the run loop suspends the TUI, runs `tmux attach`, and restores.

**Files:** `src/tui/app.rs` (Action variant + `t` handling), `src/tui/run.rs` (handle Attach).

**Step 1: Failing test** (`app.rs`): with a session present for the selected task, `t` yields `Action::Attach("kanban-task-0001")`; with no session, `Action::None`.
```rust
#[test]
fn t_attaches_when_session_present() {
    // build a snapshot whose sessions contains task-0001
    let mut app = App::new(snap_with_session());
    assert_eq!(app.on_key(key('t')), Action::Attach("kanban-task-0001".into()));
}
```
(Add a `snap_with_session()` helper that builds a `Snapshot` with one `SessionView { task: 1, session_name: "kanban-task-0001", phase: Working, needs_human_input: false }`.)

**Step 2:** FAIL.

**Step 3: Implement.**
- `app.rs`: add `Attach(String)` to `Action`. In `on_normal`, `KeyCode::Char('t') => if let Some(s) = self.selected_task().and_then(|t| self.session_for(t)) { return Action::Attach(s.session_name.clone()); }`.
- `run.rs`: handle `Action::Attach(name)` in the input branch:
```rust
Action::Attach(name) => {
    // suspend the TUI, hand the terminal to tmux, then restore
    crossterm::terminal::disable_raw_mode().ok();
    crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen).ok();
    let _ = std::process::Command::new("tmux").arg("attach").arg("-t").arg(&name).status();
    crossterm::terminal::enable_raw_mode().ok();
    crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen).ok();
    terminal.clear().ok();
    // refresh after returning
    if let Ok(s) = client.snapshot().await { app.set_snapshot(s); }
}
```
(blocks until the user detaches; that's intended.)

**Step 4:** `./x cargo test` (app test passes); `./x cargo build` (run.rs compiles); clippy clean. (No automated test for the run-loop attach — it's glue; the app-level Action is tested.)

**Step 5:** Commit: `feat(tui): attach to worker session with 't'`.

---

## Task 4: Edit (`e`) and search/filter (`/`)

Two `App` mode additions. Edit pre-fills the selected task's title and emits `EditTask`. Search filters visible cards by title substring (TUI-local; no daemon call).

**Files:** `src/tui/app.rs` (modes + keys + filter), `src/tui/ui.rs` (filter box + edit modal reuse).

**Step 1: Failing tests** (`app.rs`):
```rust
#[test]
fn e_edits_selected_task_title() {
    let mut app = App::new(snap()); // task-0001 "First" selected
    app.on_key(key('e'));
    assert!(matches!(app.mode(), Mode::EditTask));
    // input pre-filled with current title; clear + type "New"
    // (implement so the modal starts with the current title; for the test, simulate replacing)
    for _ in 0.."First".len() { app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)); }
    app.on_key(key('N')); app.on_key(key('e')); app.on_key(key('w'));
    let action = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(action, Action::Send(Intent::EditTask { task: TaskId::new(1), title: Some("New".into()), summary: None }));
}

#[test]
fn slash_filters_cards_by_title() {
    let mut app = App::new(snap()); // inbox: "First"(1), "Second"(2)
    app.on_key(key('/'));
    assert!(matches!(app.mode(), Mode::Search));
    app.on_key(key('S')); app.on_key(key('e')); // "Se"
    // visible cards in inbox now only those whose title contains "Se"
    let visible = app.visible_cards(0); // column 0 = inbox
    assert_eq!(visible, vec![TaskId::new(2)]);
}
```

**Step 2:** FAIL.

**Step 3: Implement.**
- Add `Mode::EditTask` and `Mode::Search`; add `filter: String` to `App`.
- `e`: enter `EditTask` mode, pre-fill `self.input` with the selected task's current title (look it up in `snapshot().tasks`); store the editing task id. On Enter: emit `Intent::EditTask { task, title: Some(input), summary: None }` (skip if input empty → None action), return to Normal. Esc cancels.
- `/`: enter `Search` mode; chars/backspace edit `self.filter`; Enter or Esc returns to Normal (keep the filter applied; a later `/` then Esc with empty clears — keep simple: Esc clears filter, Enter keeps it). Define `pub fn visible_cards(&self, col: usize) -> Vec<TaskId>`: the column's cards filtered by `self.filter` (case-insensitive substring on the task title; empty filter = all). Navigation (`j/k`) and rendering should use `visible_cards`; update `selected_task`/clamp to operate over visible cards so selection stays consistent. (Keep the change contained; adjust `column_cards`-based logic to filter.)
- `ui.rs`: render only `visible_cards`; show a filter line when `mode == Search` or filter non-empty; reuse the input overlay for `EditTask` (prompt "Edit title:").

**Step 4:** tests pass; full suite; clippy. Re-verify earlier app tests (navigation/move) still hold — if filtering changed `selected_task`, ensure empty-filter behavior is identical to before.

**Step 5:** Commit: `feat(tui): edit task title and search/filter`.

---

## Task 5: Task detail overlay (Enter) + Jira indicator

Enter opens a detail overlay for the selected task; cards with a Jira key show a marker.

**Files:** `src/tui/app.rs` (`Mode::Detail`, Enter handling), `src/tui/ui.rs` (overlay + jira marker).

**Step 1: Failing tests.**
- `app.rs`: Enter on a selected task → `Mode::Detail`; any key (or Esc/Enter) returns to Normal.
```rust
#[test]
fn enter_opens_detail_then_closes() {
    let mut app = App::new(snap());
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(app.mode(), Mode::Detail));
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(app.mode(), Mode::Normal));
}
```
- `ui.rs` TestBackend: in `Detail` mode the overlay shows the selected task's title + summary.

**Step 2:** FAIL.

**Step 3: Implement.**
- `app.rs`: add `Mode::Detail`; in Normal mode `KeyCode::Enter` → if a task is selected, `Mode::Detail`. In `Detail` mode, Esc/Enter/`q` → Normal (consume keys). Expose the selected task lookup for rendering (`pub fn selected_task_detail(&self) -> Option<&Task>`).
- `ui.rs`: when `mode == Detail`, draw a centered overlay (Clear + bordered Paragraph) with the task's title, summary, acceptance criteria, repo, and Jira key/url if present. Card rendering: if a task's `spec.jira.key` is `Some`, append a small marker (e.g. ` [J]`). Assert the marker / overlay text in tests.

**Step 4:** tests pass; full `./x cargo test`; `./x cargo clippy --all-targets` clean.

**Step 5:** Commit: `feat(tui): task detail overlay and Jira indicator`.

---

## M6 Done — verification

- `./x cargo test` → all green; `./x cargo clippy --all-targets` → clean.
- Manual (interactive): `kanban daemon` + `kanban tui` → cards show worker-state badges that update live as hooks fire; `e` edits a title; `/` filters; Enter shows detail; `t` attaches to a handed-off task's tmux session (and returns cleanly on detach).

**Achieved:** the TUI now covers the spec's requirements — navigation, create/edit/move/reorder/archive, handoff, search, detail, attach, and live worker-state indicators driven by the deterministic event loop. With M1–M6 the system is feature-complete against the v1 spec.
