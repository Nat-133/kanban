# Personal Orchestration System — Milestone 5 (Hooks & Events) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use @superpowers:subagent-driven-development; each task uses @superpowers:test-driven-development.

**Goal:** Close the deterministic worker→controller loop. Claude Code hooks call `kanban hook`, which drops a payload into the session intake spool; the daemon drains the spool, appends `WorkerEvent`s, derives the worker phase (M1's `derive`), and moves the card to the matching column — then notifies the TUI via SSE. Handoff generates the Claude `--settings` hooks config so a real `claude` calls back.

**Architecture:** Build on M1–M4. New: `kanban hook <event>` writes intake payloads (dumb capture). `controller::events` ingests: drain `hooks/intake/*` → append to `events.yaml` → move to `hooks/processed/` (once-only via atomic rename) → `derive` phase → transition the card. The daemon runs a periodic reconcile that drains all sessions and broadcasts on change. Handoff writes a per-session hooks settings JSON and adds `--settings <path>` to the launch command. Same testability split: ingestion + mapping are pure functions tested with hand-placed intake files; the daemon timer and the real `claude` wiring are thin glue (the latter verified structurally + by simulating the hook).

**Confirmed Claude Code facts (from the guide):** `claude --settings <path|json>` injects hooks for one session (CLI-arg precedence, merges with file settings). Hook command JSON: `{"hooks":{"Notification":[{"matcher":"","hooks":[{"type":"command","command":"<cmd>"}]}], ...}}`. The configured command keeps our fixed args AND receives the event JSON on **stdin**; the Notification payload includes `notification_type` (`permission_prompt`/`idle_prompt`/...), `session_id`, `cwd`, `transcript_path`. Hooks can't block Notification/Stop.

**Scope (M5):** `kanban hook` subcommand + intake capture; store plumbing (append event, drain intake → processed); ingest + phase→column transition; handoff generates `--settings` hooks config + adds it to the command; daemon periodic reconcile + SSE broadcast; end-to-end smoke (simulate a hook → card moves — no real `claude` needed). **Deferred to M6:** attach-to-session (`t`), worker-state indicators on cards, edit/search/detail/Jira.

**Reference:** `spec.md` — *Deterministic Worker/Controller Communication*, *Claude Code Hook Integration*, *Event Stream*, *State Derivation*, *Human Input Flow*. M1 `derive`, M4 `handoff`/`prepare_session`.

---

## Conventions

- **Rust in devcontainer — use `./x`** for cargo. Plain `cargo` fails.
- The shell's zoxide `cd` hook can interfere with the build mount; run git with `git -C /Users/nathaniel.manley/Projects/personal-orchestration-system ...` and no `cd`.
- **No `Co-Authored-By: Claude...` trailer** (hook rejects). Commit as `Nathaniel Manley <nat.manley@portswigger.net>`.
- TDD: test → fail → implement → pass → commit. Commit only a task's files.

---

## Task 1: `kanban hook` — intake capture

`kanban hook <event> --root <abs> --session <task-id>` reads Claude's event JSON from stdin and writes one intake payload file. Dumb capture; mapping happens at ingest.

**Files:** `src/controller/events.rs` (new; `pub mod events;` in `src/controller/mod.rs`), `src/main.rs` (subcommand).

**Step 1: Failing test** in `src/controller/events.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskId;

    #[test]
    fn record_writes_intake_payload() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();
        let id = TaskId::new(1);
        std::fs::create_dir_all(root.join("sessions/task-0001/hooks/intake")).unwrap();

        record_intake(&root, id, "notification", "{\"notification_type\":\"permission_prompt\"}").unwrap();
        record_intake(&root, id, "stop", "{}").unwrap();

        let intake = root.join("sessions/task-0001/hooks/intake");
        let mut files: Vec<_> = std::fs::read_dir(&intake).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        files.sort();
        assert_eq!(files.len(), 2);
        // first file parses back to event=notification with the raw payload retained
        let first = std::fs::read_to_string(intake.join(&files[0])).unwrap();
        assert!(first.contains("notification"));
        assert!(first.contains("permission_prompt"));
    }
}
```

**Step 2:** `./x cargo test --lib events` → FAIL.

**Step 3: Implement** in `src/controller/events.rs` (top):
```rust
use crate::controller::store;
use crate::model::TaskId;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One captured hook event awaiting ingestion. `payload` is Claude's raw event JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntakePayload {
    pub event: String,
    pub payload: serde_json::Value,
}

fn intake_dir(root: &Path, id: TaskId) -> std::path::PathBuf {
    store::session_dir(root, id).join("hooks/intake")
}

/// Next zero-padded intake filename (count-based; hooks from one session fire serially).
fn next_intake_name(dir: &Path) -> String {
    let mut max = 0u32;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Some(stem) = e.path().file_stem().and_then(|s| s.to_str()) {
                if let Ok(n) = stem.parse::<u32>() { max = max.max(n); }
            }
        }
    }
    format!("{:04}.json", max + 1)
}

/// Write one intake payload. `raw_payload` is the JSON Claude delivered on stdin
/// (parsed leniently; if it isn't valid JSON we store it as a string).
pub fn record_intake(root: &Path, id: TaskId, event: &str, raw_payload: &str) -> anyhow::Result<()> {
    let dir = intake_dir(root, id);
    std::fs::create_dir_all(&dir)?;
    let payload: serde_json::Value = serde_json::from_str(raw_payload)
        .unwrap_or_else(|_| serde_json::Value::String(raw_payload.to_string()));
    let item = IntakePayload { event: event.to_string(), payload };
    let name = next_intake_name(&dir);
    store::atomic_write(&dir.join(name), &serde_json::to_string_pretty(&item)?)?;
    Ok(())
}
```
(`store::session_dir` and `store::atomic_write` are added/exist in M4/earlier; if `session_dir` isn't `pub`, make it `pub`.)

**Step 4 (subcommand):** add to `src/main.rs` `Command`:
```rust
/// Internal: called by Claude Code hooks to record a worker event.
Hook {
    /// The event name (e.g. notification, stop, session-start).
    event: String,
    /// The task id whose session this hook belongs to.
    #[arg(long)]
    session: String,
},
```
and the match arm (reads stdin, calls `record_intake`):
```rust
Command::Hook { event, session } => {
    use std::io::Read;
    let id: kanban::model::TaskId = session.parse()
        .map_err(|_| anyhow::anyhow!("invalid --session task id: {session}"))?;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf).ok();
    kanban::controller::events::record_intake(&cli.root, id, &event, &buf)
}
```
(`--root` is the existing global arg; the hook command bakes it in.)

**Step 5:** `./x cargo test --lib events` → PASS; full `./x cargo test`; `./x cargo clippy --all-targets`. Note: this task adds `serde_json` use in a lib module — it's already a dependency.

**Step 6:** Commit (`events.rs`, `controller/mod.rs`, `main.rs`): `feat(hook): kanban hook intake capture`.

---

## Task 2: Store plumbing — append event + drain intake

**Files:** `src/controller/store.rs`.

**Step 1: Failing tests** (store tests):
```rust
#[test]
fn append_worker_event_accumulates() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    init_workspace(&root).unwrap();
    let id = TaskId::new(1);
    std::fs::create_dir_all(session_dir(&root, id)).unwrap();
    let ev = WorkerEvent { kind: WorkerEventKind::Started, source: "controller".into(),
        observed_at: time::OffsetDateTime::UNIX_EPOCH, payload_ref: None };
    append_worker_event(&root, id, &ev).unwrap();
    append_worker_event(&root, id, &ev).unwrap();
    assert_eq!(load_events(&session_dir(&root, id)).unwrap().len(), 2);
}

#[test]
fn drain_intake_moves_files_to_processed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    init_workspace(&root).unwrap();
    let id = TaskId::new(1);
    let intake = session_dir(&root, id).join("hooks/intake");
    std::fs::create_dir_all(&intake).unwrap();
    std::fs::write(intake.join("0001.json"), "{}").unwrap();
    let files = list_intake(&root, id).unwrap();
    assert_eq!(files.len(), 1);
    mark_processed(&root, id, &files[0]).unwrap();
    assert!(!files[0].exists());
    assert!(session_dir(&root, id).join("hooks/processed/0001.json").exists());
}
```

**Step 2:** `./x cargo test --lib append_worker_event` / `drain_intake` → FAIL.

**Step 3: Implement** in `store.rs`:
```rust
pub fn events_path(root: &Path, id: TaskId) -> PathBuf { session_dir(root, id).join("events.yaml") }

pub fn append_worker_event(root: &Path, id: TaskId, event: &WorkerEvent) -> anyhow::Result<()> {
    let mut items = load_events(&session_dir(root, id)).unwrap_or_default();
    items.push(event.clone());
    let list = WorkerEventList {
        api_version: Some(ApiVersion::V1Alpha1),
        kind: Some(WorkerEventListKind::WorkerEventList),
        metadata: Some(Metadata { name: format!("{id}-events"), creation_timestamp: None, labels: Default::default() }),
        items,
    };
    atomic_write(&events_path(root, id), &serde_yml::to_string(&list)?)
}

/// Intake payload files for a session, sorted by name (ingest order).
pub fn list_intake(root: &Path, id: TaskId) -> anyhow::Result<Vec<PathBuf>> {
    let dir = session_dir(root, id).join("hooks/intake");
    let mut out = Vec::new();
    if dir.exists() {
        for e in fs::read_dir(&dir)? { out.push(e?.path()); }
    }
    out.sort();
    Ok(out)
}

/// Move a processed intake file into hooks/processed/ (once-only via rename).
pub fn mark_processed(root: &Path, id: TaskId, path: &Path) -> anyhow::Result<()> {
    let processed = session_dir(root, id).join("hooks/processed");
    fs::create_dir_all(&processed)?;
    let name = path.file_name().ok_or_else(|| anyhow::anyhow!("intake path has no file name"))?;
    fs::rename(path, processed.join(name))?;
    Ok(())
}
```

**Step 4:** tests pass; full `--lib`; clippy.

**Step 5:** Commit: `feat(store): append worker events and drain intake spool`.

---

## Task 3: Ingest + phase→column transition (the meat)

Map each intake payload → `WorkerEvent`, append it, mark processed; then `derive` the phase and move the card to the matching column. Per the State Derivation table: Working→`doing`, WaitingHuman→`blocked`, Idle→`waiting-human`, Completed/Failed→`review`, Pending→(no move). Non-input notifications (not permission/idle) produce no event.

**Files:** `src/controller/events.rs`.

**Step 1: Failing tests** (add to `events.rs` tests):
```rust
#[test]
fn ingest_permission_prompt_moves_card_to_blocked() {
    use crate::controller::{store, apply::apply};
    use crate::model::proto::Intent;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    store::init_workspace(&root).unwrap();
    apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
    let id = TaskId::new(1);
    record_intake(&root, id, "notification", "{\"notification_type\":\"permission_prompt\"}").unwrap();

    let changed = ingest_session(&root, id).unwrap();
    assert!(changed);
    // event recorded
    assert_eq!(store::load_events(&store::session_dir(&root, id)).unwrap().len(), 1);
    // card moved to blocked
    let board = store::load_board(&root).unwrap();
    assert!(board.cards().get(&"blocked".parse().unwrap()).unwrap().contains(&id));
    // intake drained
    assert!(store::list_intake(&root, id).unwrap().is_empty());
    assert!(store::session_dir(&root, id).join("hooks/processed/0001.json").exists());
}

#[test]
fn ingest_session_end_moves_to_review() {
    use crate::controller::{store, apply::apply};
    use crate::model::proto::Intent;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    store::init_workspace(&root).unwrap();
    apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
    let id = TaskId::new(1);
    record_intake(&root, id, "session-end", "{}").unwrap();
    ingest_session(&root, id).unwrap();
    let board = store::load_board(&root).unwrap();
    assert!(board.cards().get(&"review".parse().unwrap()).unwrap().contains(&id));
}

#[test]
fn ingest_is_idempotent_when_no_new_intake() {
    use crate::controller::store;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    store::init_workspace(&root).unwrap();
    let id = TaskId::new(1);
    std::fs::create_dir_all(store::session_dir(&root, id)).unwrap();
    assert!(!ingest_session(&root, id).unwrap()); // nothing to do
}
```

**Step 2:** FAIL.

**Step 3: Implement** in `events.rs`:
```rust
use crate::model::{Notification, Phase, WorkerEvent, WorkerEventKind};

/// Map a captured intake item to a worker event. Returns None for events we don't
/// track (e.g. a non-permission/idle Notification).
fn to_event(item: &IntakePayload, payload_ref: String) -> Option<WorkerEvent> {
    let kind = match item.event.as_str() {
        "session-start" => WorkerEventKind::Started,
        "user-prompt-submit" | "stop" => WorkerEventKind::Working,
        "session-end" => WorkerEventKind::Completed,
        "stop-failure" => WorkerEventKind::Failed,
        "notification" => {
            let nt = item.payload.get("notification_type").and_then(|v| v.as_str());
            match nt {
                Some("permission_prompt") => WorkerEventKind::HumanInputRequired(Notification::PermissionPrompt),
                Some("idle_prompt") => WorkerEventKind::HumanInputRequired(Notification::IdlePrompt),
                _ => return None, // auth_success, elicitation, etc. — not tracked
            }
        }
        _ => return None,
    };
    Some(WorkerEvent {
        kind,
        source: "claude-code-hook".to_string(),
        observed_at: time::OffsetDateTime::now_utc(),
        payload_ref: Some(payload_ref),
    })
}

/// Target board column for a derived phase. None = leave the card where it is.
fn phase_column(phase: Phase) -> Option<&'static str> {
    match phase {
        Phase::Working => Some("doing"),
        Phase::WaitingHuman => Some("blocked"),
        Phase::Idle => Some("waiting-human"),
        Phase::Completed | Phase::Failed => Some("review"),
        Phase::Pending => None,
    }
}

/// Drain a session's intake spool, append events, and transition the card to match
/// the derived phase. Returns true if anything changed.
pub fn ingest_session(root: &Path, id: TaskId) -> anyhow::Result<bool> {
    let intake = store::list_intake(root, id)?;
    if intake.is_empty() { return Ok(false); }
    let mut any = false;
    for path in intake {
        let text = std::fs::read_to_string(&path)?;
        let item: IntakePayload = serde_json::from_str(&text)?;
        let processed_ref = format!("hooks/processed/{}", path.file_name().unwrap().to_string_lossy());
        if let Some(event) = to_event(&item, processed_ref) {
            store::append_worker_event(root, id, &event)?;
            any = true;
        }
        store::mark_processed(root, id, &path)?;
    }
    if any {
        let phase = crate::controller::derive::derive(&store::load_events(&store::session_dir(root, id))?);
        if let Some(col) = phase_column(phase) {
            move_card_to(root, id, col)?;
        }
    }
    Ok(any)
}

/// Move a card to `column` on the board (remove from all columns first; re-validate).
fn move_card_to(root: &Path, id: TaskId, column: &str) -> anyhow::Result<()> {
    use crate::model::{Board, RawBoard};
    let mut raw: RawBoard = store::load_board(root)?.into();
    let col = column.parse().map_err(|_| anyhow::anyhow!("bad column id: {column}"))?;
    for v in raw.spec.cards.values_mut() { v.retain(|t| *t != id); }
    raw.spec.cards.entry(col).or_default().push(id);
    let board = Board::try_from(raw).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    store::save_board(root, &board)
}

/// Drain every session under the workspace. Returns true if anything changed.
pub fn reconcile_all(root: &Path) -> anyhow::Result<bool> {
    let sessions = root.join("sessions");
    let mut any = false;
    if sessions.exists() {
        for e in std::fs::read_dir(&sessions)? {
            let name = e?.file_name().to_string_lossy().into_owned();
            if let Ok(id) = name.parse::<TaskId>() {
                any |= ingest_session(root, id)?;
            }
        }
    }
    Ok(any)
}
```
(Note: `move_card_to` mirrors `apply`'s card-move; acceptable duplication for M5 — a later refactor could share it. If you prefer, factor a `store`-level helper, but keep this task self-contained.)

**Step 4:** tests pass; full `./x cargo test`; clippy.

**Step 5:** Commit: `feat(events): ingest intake, derive phase, transition card`.

---

## Task 4: Handoff generates Claude `--settings` hooks config

Make a real `claude` call back into `kanban hook`. Extend `prepare_session` to write a per-session hooks settings JSON and prepend `--settings <path>` to the launch command.

**Files:** `src/controller/handoff.rs`.

**Step 1: Failing test** (add to `handoff.rs` tests):
```rust
#[test]
fn prepare_session_writes_hook_settings_and_adds_flag() {
    use crate::controller::store;
    use crate::model::TaskId;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    store::init_workspace(&root).unwrap();
    let id = TaskId::new(1);
    let task = sample_task(id, "x");
    store::save_task(&root, &task).unwrap();
    let cfg = store::load_config(&root).unwrap();
    let session = prepare_session(&root, &task, "claude", cfg.workers.get("claude").unwrap(), &cfg.agents.base_dirs).unwrap();

    // settings file written
    let settings = root.join("sessions/task-0001/hooks/settings.json");
    assert!(settings.exists());
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    // has a Notification hook whose command references our session + hook subcommand
    let cmd = v["hooks"]["Notification"][0]["hooks"][0]["command"].as_str().unwrap();
    assert!(cmd.contains("hook notification"));
    assert!(cmd.contains("--session task-0001"));
    // command passes --settings <that file>
    let idx = session.spec.command.iter().position(|a| a == "--settings").expect("--settings present");
    assert_eq!(session.spec.command[idx + 1], settings.display().to_string());
}
```

**Step 2:** FAIL.

**Step 3: Implement.** In `prepare_session`, after creating the session dirs and before/while building the command:
- Determine the kanban binary path: `let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "kanban".into());`
- Build the hooks settings JSON with `serde_json::json!`, one entry per tracked event (matcher `""`), command = `format!("{exe} hook {name} --root {root_abs} --session {id}")`. Events → hook names: `Notification`→`notification`, `Stop`→`stop`, `SessionStart`→`session-start`, `UserPromptSubmit`→`user-prompt-submit`, `SessionEnd`→`session-end`. `root_abs` = `std::fs::canonicalize(root)` (fallback to root as-is).
  ```rust
  let mk = |name: &str| serde_json::json!([{ "matcher": "", "hooks": [{ "type": "command", "command": format!("{exe} hook {name} --root {root_abs} --session {id}") }] }]);
  let settings = serde_json::json!({ "hooks": {
      "Notification": mk("notification"),
      "Stop": mk("stop"),
      "SessionStart": mk("session-start"),
      "UserPromptSubmit": mk("user-prompt-submit"),
      "SessionEnd": mk("session-end"),
  }});
  let settings_path = sdir.join("hooks/settings.json");
  std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
  ```
- Prepend the flag to the command: insert `"--settings"` and `settings_path.display().to_string()` immediately after `worker.command` (i.e. `command = [claude, --settings, <path>, ...args, --add-dir ...]`). Easiest: build the command as before, then `command.insert(1, settings_path...); command.insert(1, "--settings".into());` (insert in reverse) — or construct in order.

**Step 4:** `./x cargo test --lib prepare_session` (both the M4 test and this new one) → PASS; full `./x cargo test`; clippy. NOTE: the M4 `prepare_session_creates_workspace_symlinks_and_command` test asserts `command.first() == "claude"` and that some arg contains `sessions/task-0001` — both still hold (command[0] is still `claude`; `--settings <path>` contains `sessions/task-0001`, and the `--add-dir` arg does too). Verify it still passes; if the M4 test's `command.first()` assumption is the only conflict, it isn't (we insert AFTER index 0).

**Step 5:** Commit: `feat(handoff): generate Claude --settings hooks config`.

---

## Task 5: Daemon periodic reconcile + SSE broadcast + end-to-end smoke

The daemon drains intake on a timer and notifies the TUI. Then prove the whole loop by simulating a hook.

**Files:** `src/controller/server.rs`.

**Step 1: Implement** the periodic reconcile in `serve` (glue — no unit test for the timer). In `serve`, before `axum::serve`, spawn a task that owns a clone of the root + the broadcast sender:
```rust
pub async fn serve(root: PathBuf, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    let (tx, _rx) = ... // NOTE: router currently makes its own channel; refactor so serve and router share one Sender (see below)
    ...
}
```
Refactor so the change-broadcast `Sender` is created in `serve`, passed into `router`, and also used by a reconcile task:
- Change `pub fn router(root: PathBuf) -> Router` to `pub fn router(root: PathBuf, changes: broadcast::Sender<()>) -> Router` (the existing `mutation_emits_sse_event` and other tests call `router(root)` — update them to `router(root, tokio::sync::broadcast::channel(64).0)`).
- In `serve`:
  ```rust
  let (tx, _rx) = tokio::sync::broadcast::channel(64);
  // periodic reconcile: drain intake spools, notify on change
  {
      let root = root.clone();
      let tx = tx.clone();
      tokio::spawn(async move {
          let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
          loop {
              tick.tick().await;
              let r = root.clone();
              if let Ok(Ok(true)) = tokio::task::spawn_blocking(move || crate::controller::events::reconcile_all(&r)).await {
                  let _ = tx.send(());
              }
          }
      });
  }
  let listener = tokio::net::TcpListener::bind(addr).await?;
  tracing::info!(%addr, "controller listening");
  axum::serve(listener, router(root, tx)).await?;
  Ok(())
  ```
- The `router` builds `AppState` from the passed-in `changes` sender instead of creating its own.

**Step 2:** Update the affected tests to the new `router(root, changes)` signature (the SSE test can pass a fresh `broadcast::channel(64).0`, or — better — assert reconcile-driven events too, but keep it minimal). `./x cargo test` → all green; `./x cargo clippy --all-targets` → clean.

**Step 3:** Commit: `feat(server): periodic intake reconcile with SSE notify`.

**Step 4: End-to-end smoke** (no real `claude` — simulate the hook by invoking `kanban hook` exactly as Claude would, piping a payload on stdin):
```bash
/Users/nathaniel.manley/Projects/personal-orchestration-system/x bash -lc '
set -e
cd /workspaces/personal-orchestration-system
rm -rf /tmp/kh5 && cargo build -q
EXE=$PWD/target/debug/kanban
$EXE --root /tmp/kh5/.kanban init >/dev/null
# dummy worker so handoff/tmux works without claude
cat > /tmp/kh5/.kanban/config.yaml <<EOF
agents:
  baseDirs: []
workers:
  claude:
    command: sleep
    args: ["300"]
    terminal:
      type: tmux
      sessionName: kanban-{task_id}
EOF
$EXE --root /tmp/kh5/.kanban daemon --addr 127.0.0.1:7821 &
DPID=$!; sleep 2
curl -s -X POST 127.0.0.1:7821/v1/intent -H "content-type: application/json" -d "{\"type\":\"createTask\",\"title\":\"t\",\"summary\":\"\",\"column\":\"inbox\"}" >/dev/null
curl -s -X POST 127.0.0.1:7821/v1/intent -H "content-type: application/json" -d "{\"type\":\"handoff\",\"task\":\"task-0001\",\"worker\":\"claude\"}" >/dev/null
# simulate Claude firing a permission_prompt Notification hook:
echo "{\"notification_type\":\"permission_prompt\",\"session_id\":\"x\"}" | $EXE --root /tmp/kh5/.kanban hook notification --session task-0001
sleep 1  # let the daemon reconcile tick run
echo "--- board after hook (expect task-0001 in blocked) ---"
curl -s -X POST 127.0.0.1:7821/v1/intent -H "content-type: application/json" -d "{\"type\":\"getBoard\"}" | python3 -c "import sys,json; b=json.load(sys.stdin)[\"board\"][\"spec\"][\"cards\"]; print(\"blocked:\", b[\"blocked\"]) "
echo "--- events.yaml ---"; cat /tmp/kh5/.kanban/sessions/task-0001/events.yaml
tmux kill-server 2>/dev/null || true; kill $DPID 2>/dev/null || true
'
```
Expected: after the simulated hook + a reconcile tick, `task-0001` is in `blocked`, and `events.yaml` contains a `human_input_required` event. Capture the actual output. (This proves the full loop: hook → intake → daemon ingest → derive → card transition.)

**Step 5:** Commit any smoke-driven tweak if needed (likely none).

---

## M5 Done — verification

- `./x cargo test` → all green; `./x cargo clippy --all-targets` → clean.
- The Step 4 smoke shows a simulated hook moving the card to `blocked` via the daemon's reconcile — the deterministic loop works end to end (and would work with real `claude` via the `--settings` config from Task 4).

**Achieved:** the deterministic worker→controller loop the spec is built around — Claude hooks (or anything) drop intake payloads, the daemon ingests them once-only, derives worker phase via M1's `derive`, and moves cards accordingly, pushing live updates to the TUI. M6 adds attach-to-session (`t`), worker-state indicators on cards, and the remaining TUI polish (edit/search/detail/Jira).
