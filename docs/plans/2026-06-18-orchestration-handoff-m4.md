# Personal Orchestration System — Milestone 4 (Worker Handoff) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use @superpowers:subagent-driven-development; each task uses @superpowers:test-driven-development.

**Goal:** Hand a task off to a worker: create a per-task session workspace (allowlist symlinks + base-dir grant + generated `handoff.md`), record a `WorkerSession`, and launch the configured worker command in a tmux session — driven by a `Handoff` intent and a TUI key.

**Architecture:** Build on M1–M3. The handoff *logic* is pure filesystem orchestration, unit-tested in a temp dir against a **`Launcher` trait** (fake in tests, `TmuxLauncher` in production). `config.yaml` (plain config, not a reconciled resource) holds `agents.baseDirs` + the `workers` adapter map. The daemon's `apply` gains a `Handoff` arm that constructs a real `TmuxLauncher` and calls the tested `handoff()` function. Same testability split as `apply`/`tui::app`: logic in tested functions, process-spawning behind a trait.

**Environment reality:** `tmux` and `claude` are NOT in the devcontainer. Tests never spawn them — they use a `FakeLauncher`. Task 0 adds `tmux` to the devcontainer so the Task 6 end-to-end smoke test can launch a **dummy worker** (`sleep`) and assert the tmux session exists. `claude` is never needed for tests.

**Scope (M4):** config load + default, base-dir/template expansion, session-workspace prep (symlinks + handoff.md), `Launcher` trait + `TmuxLauncher`, `Handoff` intent + apply wiring + task `workerSessionRef` update, TUI `c` key to hand off. **Deferred:** attach-to-session (`t`) and worker-state indicators → M5 (they pair with the hook/event work); arbitrary glob support beyond `~`-expansion + trailing `/*`.

**Reference:** `spec.md` — *Worker Handoff*, *Agent Access and Working Directory*, *Worker Integration*, *Worker Session Resource*, *Safety*. The `WorkerSession`/`WorkerSessionSpec` model types already exist (M1).

---

## Conventions (same as prior milestones)

- **Rust runs in the devcontainer — use `./x`** for all cargo commands. Plain `cargo` fails.
- **No `Co-Authored-By: Claude...` trailer** in commits (hook rejects). Commit via `git -c user.name='Nathaniel Manley' -c user.email='nat.manley@portswigger.net' commit ...`.
- TDD: test → fail → implement → pass → commit. Commit only the files a task changes.

---

## Task 0: Add tmux to the devcontainer

**Files:** `.devcontainer/devcontainer.json`.

**Step 1:** Update `.devcontainer/devcontainer.json` to install tmux on container create:
```json
{
  "name": "kanban",
  "image": "mcr.microsoft.com/devcontainers/rust:1",
  "postCreateCommand": "sudo apt-get update && sudo apt-get install -y tmux"
}
```

**Step 2:** Install it into the *currently running* container too (postCreateCommand only runs on (re)create):
```bash
./x bash -lc 'sudo apt-get update && sudo apt-get install -y tmux && tmux -V'
```
Expected: prints a tmux version.

**Step 3:** Commit (devcontainer.json only): `chore(m4): add tmux to devcontainer for worker launch`.

(No Rust changes; no test.)

---

## Task 1: `Config` resource (config.yaml)

**Files:** `src/model/mod.rs` (add `Config` + nested types), `src/controller/store.rs` (`load_config`, default written by `init_workspace`).

**Step 1: Failing tests.** In `src/model/mod.rs` tests, add a parse test:
```rust
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
```
In `src/controller/store.rs` tests:
```rust
#[test]
fn init_writes_loadable_config() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    init_workspace(&root).unwrap();
    let cfg = load_config(&root).unwrap();
    assert!(cfg.workers.contains_key("claude"));
}
```

**Step 2:** `./x cargo test --lib config` → FAIL.

**Step 3: Implement.** In `src/model/mod.rs`:
```rust
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
```
In `src/controller/store.rs`: add `config_path`, `load_config`, and have `init_workspace` write a default config if absent:
```rust
pub fn config_path(root: &Path) -> PathBuf { root.join("config.yaml") }

pub fn load_config(root: &Path) -> anyhow::Result<Config> {
    let text = fs::read_to_string(config_path(root))?;
    Ok(serde_yml::from_str(&text)?)
}

fn default_config_yaml() -> &'static str {
    "agents:\n  baseDirs:\n    - ~/vcs/*\nworkers:\n  claude:\n    command: claude\n    args:\n      - --add-dir\n      - .kanban/sessions/{task_id}\n    workdir: \"{repo}\"\n    terminal:\n      type: tmux\n      sessionName: kanban-{task_id}\n"
}
```
And in `init_workspace`, after the board: `if !config_path(root).exists() { atomic_write(&config_path(root), default_config_yaml())?; }`.

**Step 4:** tests pass; full `--lib`.

**Step 5:** Commit (model + store): `feat(config): config.yaml resource (agents + workers)`.

---

## Task 2: Expansion helpers (templates + base dirs)

Pure functions: substitute `{task_id}`/`{repo}` in strings, and expand a base-dir entry (`~` + trailing `/*`) into concrete existing directories.

**Files:** `src/controller/handoff.rs` (new; declare `pub mod handoff;` in `src/controller/mod.rs`).

**Step 1: Failing tests** (`src/controller/handoff.rs`):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_task_id_and_repo() {
        assert_eq!(substitute(".kanban/sessions/{task_id}", "task-0001", Some("~/vcs/x")), ".kanban/sessions/task-0001");
        assert_eq!(substitute("{repo}", "task-0001", Some("/home/u/vcs/x")), "/home/u/vcs/x");
        assert_eq!(substitute("{repo}", "task-0001", None), "");
    }

    #[test]
    fn expands_base_dir_glob() {
        let dir = tempfile::tempdir().unwrap();
        let vcs = dir.path().join("vcs");
        std::fs::create_dir_all(vcs.join("repo-a")).unwrap();
        std::fs::create_dir_all(vcs.join("repo-b")).unwrap();
        std::fs::write(vcs.join("loose.txt"), "x").unwrap(); // not a dir -> excluded
        let pattern = format!("{}/*", vcs.display());
        let mut got = expand_base_dir(&pattern);
        got.sort();
        assert_eq!(got, vec![vcs.join("repo-a"), vcs.join("repo-b")]);
    }

    #[test]
    fn non_glob_base_dir_returns_itself_if_exists() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().to_path_buf();
        assert_eq!(expand_base_dir(&p.display().to_string()), vec![p]);
        assert!(expand_base_dir("/no/such/path/xyz").is_empty());
    }
}
```

**Step 2:** `./x cargo test --lib handoff` → FAIL.

**Step 3: Implement** (top of `src/controller/handoff.rs`):
```rust
use std::path::PathBuf;

/// Replace `{task_id}` and `{repo}` placeholders. Missing repo -> empty string.
pub fn substitute(template: &str, task_id: &str, repo: Option<&str>) -> String {
    template.replace("{task_id}", task_id).replace("{repo}", repo.unwrap_or(""))
}

fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    s.to_string()
}

/// Expand one `agents.baseDirs` entry into concrete existing directories.
/// Supports `~` expansion and a trailing `/*` (immediate subdirectories).
/// Returns only paths that exist and are directories.
pub fn expand_base_dir(entry: &str) -> Vec<PathBuf> {
    let expanded = expand_tilde(entry);
    if let Some(parent) = expanded.strip_suffix("/*") {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(parent) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() { out.push(p); }
            }
        }
        out
    } else {
        let p = PathBuf::from(&expanded);
        if p.is_dir() { vec![p] } else { Vec::new() }
    }
}
```

**Step 4:** tests pass; full `--lib`.

**Step 5:** Commit (handoff.rs + controller/mod.rs): `feat(handoff): template and base-dir expansion helpers`.

---

## Task 3: Session-workspace preparation

Build the per-task session dir (intake/processed dirs, allowlist symlinks, `handoff.md`) and construct the `WorkerSession` (command = worker command/args with substitutions + one `--add-dir` per expanded base dir; workdir = repo or workspace). Pure filesystem + value construction — no launch.

**Files:** `src/controller/handoff.rs`.

**Step 1: Failing tests** (add to `handoff.rs` tests). Build a workspace + a task with context, call `prepare_session`, assert structure:
```rust
#[test]
fn prepare_session_creates_workspace_symlinks_and_command() {
    use crate::controller::store;
    use crate::model::*;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    store::init_workspace(&root).unwrap();
    // a task whose context includes notes.md (which we create in the task dir)
    let id = TaskId::new(1);
    let task_dir = store::task_dir(&root, id);
    std::fs::create_dir_all(&task_dir).unwrap();
    std::fs::write(task_dir.join("notes.md"), "hi").unwrap();
    let mut task = sample_task(id, "Do it");
    task.spec.context.include = vec!["notes.md".into()];
    store::save_task(&root, &task).unwrap();

    let cfg = store::load_config(&root).unwrap();
    let worker = cfg.workers.get("claude").unwrap();
    let session = prepare_session(&root, &task, "claude", worker, &cfg.agents.base_dirs).unwrap();

    let sdir = root.join("sessions").join(id.to_string());
    assert!(sdir.join("hooks/intake").is_dir());
    assert!(sdir.join("handoff.md").exists());
    // notes.md symlinked into the session dir, pointing at the task's notes.md
    let link = sdir.join("notes.md");
    assert!(std::fs::symlink_metadata(&link).is_ok());
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "hi");
    // command starts with the worker command and contains the session --add-dir
    assert_eq!(session.spec.command.first().unwrap(), "claude");
    assert!(session.spec.command.iter().any(|a| a.contains(&format!("sessions/{id}"))));
    assert_eq!(session.spec.task_ref, id);

    // a helper for tests
    fn sample_task(id: TaskId, title: &str) -> Task {
        Task { api_version: ApiVersion::V1Alpha1, kind: TaskKind::Task,
            metadata: Metadata { name: id.to_string(), creation_timestamp: None, labels: Default::default() },
            spec: TaskSpec { title: title.into(), summary: String::new(), color: None,
                description_ref: "description.md".into(), notes_ref: "notes.md".into(),
                acceptance_criteria: vec![], repo: None, jira: Default::default(), context: Default::default() },
            status: Default::default() }
    }
}
```

**Step 2:** `./x cargo test --lib prepare_session` → FAIL.

**Step 3: Implement** `prepare_session(root, task, worker_name, worker: &WorkerConfig, base_dirs: &[String]) -> anyhow::Result<WorkerSession>`:
- `let id = task.metadata.name.parse::<TaskId>()?;` `let sdir = root.join("sessions").join(id.to_string());`
- `fs::create_dir_all(sdir.join("hooks/intake"))?; fs::create_dir_all(sdir.join("hooks/processed"))?;`
- For each `entry` in `task.spec.context.include`: resolve source = `store::task_dir(root, id).join(entry)` (entries are relative to the task dir). Canonicalize the source; if it doesn't exist, `tracing::warn!` and skip. Symlink name = the entry's file name (`Path::new(entry).file_name()`); target = the canonicalized absolute source. Use `std::os::unix::fs::symlink(&source_abs, sdir.join(name))`. (Remove a pre-existing symlink of the same name first to stay idempotent.)
- Write `handoff.md` (generate from the task — title, summary, acceptance criteria, the allowed-context list, and the standard instructions block from the spec).
- Build the command vec: start with `worker.command`, then `worker.args` each `substitute`d with `(task_id, repo)` where `repo = task.spec.repo.as_deref()`. Then for each `base_dirs` entry, for each `expand_base_dir(entry)` dir, push `"--add-dir"` and the dir's string. (Resulting `Vec<String>`.)
- `workdir`: if `task.spec.repo` is set, `Some(expand_tilde(repo))`; else the session workspace path. (Expose `expand_tilde` as `pub(crate)` or inline.)
- Construct and return the `WorkerSession`:
  ```
  WorkerSession {
    api_version: ApiVersion::V1Alpha1, kind: WorkerSessionKind::WorkerSession,
    metadata: Metadata { name: format!("{id}-{worker_name}"), ... },
    spec: WorkerSessionSpec {
      task_ref: id, worker: worker_name.into(),
      workspace: sdir.clone(), workdir: Some(...), command,
    },
    status: Default::default(),
  }
  ```
  (Do NOT write session.yaml here — that's the orchestration step in Task 5. This function only preps the workspace + builds the value.)

**Step 4:** tests pass; full `--lib`; `./x cargo clippy --all-targets`.

**Step 5:** Commit: `feat(handoff): session workspace preparation`.

---

## Task 4: `Launcher` trait + `TmuxLauncher` + `FakeLauncher`

**Files:** `src/controller/handoff.rs`.

**Step 1: Failing test** — a `FakeLauncher` records what it was asked to launch:
```rust
#[test]
fn fake_launcher_records_launch() {
    use crate::model::*;
    let session = WorkerSession {
        api_version: ApiVersion::V1Alpha1, kind: WorkerSessionKind::WorkerSession,
        metadata: Metadata { name: "task-0001-claude".into(), creation_timestamp: None, labels: Default::default() },
        spec: WorkerSessionSpec {
            task_ref: TaskId::new(1), worker: "claude".into(),
            workspace: "/tmp/x".into(), workdir: None,
            command: vec!["sleep".into(), "1".into()],
        },
        status: Default::default(),
    };
    let fake = FakeLauncher::default();
    fake.launch(&session, "kanban-task-0001").unwrap();
    assert_eq!(fake.launched.lock().unwrap().len(), 1);
}
```

**Step 2:** FAIL.

**Step 3: Implement:**
```rust
use crate::model::WorkerSession;

/// Launches a prepared worker session in a terminal/multiplexer.
pub trait Launcher {
    /// `session_name` is the resolved terminal session name (e.g. tmux session).
    fn launch(&self, session: &WorkerSession, session_name: &str) -> anyhow::Result<()>;
}

/// Real launcher: starts a detached tmux session running the worker command.
pub struct TmuxLauncher;

impl Launcher for TmuxLauncher {
    fn launch(&self, session: &WorkerSession, session_name: &str) -> anyhow::Result<()> {
        let mut cmd = std::process::Command::new("tmux");
        cmd.arg("new-session").arg("-d").arg("-s").arg(session_name);
        if let Some(workdir) = &session.spec.workdir {
            cmd.arg("-c").arg(workdir);
        }
        // tmux runs the remaining args as the command
        cmd.args(&session.spec.command);
        let status = cmd.status()?;
        if !status.success() {
            anyhow::bail!("tmux new-session failed with status {status}");
        }
        Ok(())
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct FakeLauncher {
    pub launched: std::sync::Mutex<Vec<(WorkerSession, String)>>,
}

#[cfg(test)]
impl Launcher for FakeLauncher {
    fn launch(&self, session: &WorkerSession, session_name: &str) -> anyhow::Result<()> {
        self.launched.lock().unwrap().push((session.clone(), session_name.to_string()));
        Ok(())
    }
}
```

**Step 4:** tests pass; full `--lib`; clippy.

**Step 5:** Commit: `feat(handoff): Launcher trait with tmux and fake implementations`.

---

## Task 5: `handoff()` orchestration + `Handoff` intent + apply wiring

Tie it together: load config + task, prepare the session, launch, write `session.yaml`, set the task's `workerSessionRef`. Then wire `Intent::Handoff` into `apply` using a real `TmuxLauncher`.

**Files:** `src/model/proto.rs` (add `Handoff`), `src/controller/handoff.rs` (the `handoff` fn), `src/controller/apply.rs` (dispatch), `src/controller/store.rs` (`save_session` if not present).

**Step 1: Failing tests.** In `handoff.rs`, test the orchestration with the `FakeLauncher`:
```rust
#[test]
fn handoff_writes_session_and_sets_task_ref() {
    use crate::controller::store;
    use crate::model::*;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    store::init_workspace(&root).unwrap();
    // create a task on the board
    crate::controller::apply::apply(&root, crate::model::proto::Intent::CreateTask {
        title: "A".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();

    let fake = FakeLauncher::default();
    handoff(&root, TaskId::new(1), "claude", &fake).unwrap();

    // session.yaml written
    let sess_path = root.join("sessions/task-0001/session.yaml");
    assert!(sess_path.exists());
    // task.status.workerSessionRef set
    let task = store::load_task(&root, TaskId::new(1)).unwrap();
    assert_eq!(task.status.worker_session_ref.as_deref(), Some("task-0001-claude"));
    // launcher invoked with the resolved tmux session name
    let launched = fake.launched.lock().unwrap();
    assert_eq!(launched.len(), 1);
    assert_eq!(launched[0].1, "kanban-task-0001");
}

#[test]
fn handoff_unknown_worker_errors() {
    use crate::controller::store;
    use crate::model::TaskId;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".kanban");
    store::init_workspace(&root).unwrap();
    crate::controller::apply::apply(&root, crate::model::proto::Intent::CreateTask {
        title: "A".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
    let fake = FakeLauncher::default();
    assert!(handoff(&root, TaskId::new(1), "nope", &fake).is_err());
}
```
In `proto.rs` tests, add a round-trip for the new intent:
```rust
#[test]
fn handoff_intent_round_trips() {
    let i = Intent::Handoff { task: TaskId::new(1), worker: "claude".into() };
    let back: Intent = serde_json::from_str(&serde_json::to_string(&i).unwrap()).unwrap();
    assert_eq!(i, back);
}
```

**Step 2:** FAIL.

**Step 3: Implement.**
- `proto.rs`: add `Handoff { task: TaskId, worker: String }` to `Intent`.
- `store.rs`: add `save_session` (write `sessions/<task_id>/session.yaml` via `atomic_write` + `serde_yml`) and `session_dir(root, id)` helper if not present. Resolve the session path from `session.spec.task_ref`.
- `handoff.rs`:
```rust
pub fn handoff(root: &Path, task_id: TaskId, worker_name: &str, launcher: &dyn Launcher) -> anyhow::Result<()> {
    let cfg = store::load_config(root)?;
    let worker = cfg.workers.get(worker_name)
        .ok_or_else(|| anyhow::anyhow!("unknown worker: {worker_name}"))?;
    let mut task = store::load_task(root, task_id)?;
    let session = prepare_session(root, &task, worker_name, worker, &cfg.agents.base_dirs)?;
    let session_name = substitute(&worker.terminal.session_name, &task_id.to_string(), task.spec.repo.as_deref());
    launcher.launch(&session, &session_name)?;
    store::save_session(root, &session)?;
    task.status.worker_session_ref = Some(session.metadata.name.clone());
    store::save_task(root, &task)?;
    Ok(())
}
```
- `apply.rs`: add the dispatch arm:
```rust
Intent::Handoff { task, worker } => {
    match handoff::handoff(root, task, &worker, &handoff::TmuxLauncher) {
        Ok(()) => Ok(Response::Ok { task: Some(task) }),
        Err(e) => Ok(Response::Error { message: e.to_string() }),
    }
}
```
(import `use crate::controller::handoff;`). Domain errors → `Response::Error`; only true I/O bubbles via `?` inside `handoff` — but since the apply arm maps `Err` to `Response::Error`, a missing tmux at runtime surfaces as an error response rather than crashing the daemon. That's acceptable for M4.

**Step 4:** tests pass; full `./x cargo test`; clippy.

**Step 5:** Commit: `feat(handoff): handoff orchestration and Handoff intent`.

---

## Task 6: TUI `c` handoff + end-to-end smoke

**Files:** `src/tui/app.rs` (handle `c`), and a smoke verification.

**Step 1: Failing test** in `app.rs` tests:
```rust
#[test]
fn c_emits_handoff_intent() {
    let mut app = App::new(snap());
    let action = app.on_key(key('c'));
    assert_eq!(action, Action::Send(Intent::Handoff { task: TaskId::new(1), worker: "claude".into() }));
}
```

**Step 2:** FAIL.

**Step 3: Implement.** In `App::on_normal`, add:
```rust
KeyCode::Char('c') => if let Some(t) = self.selected_task() { return Action::Send(Intent::Handoff { task: t, worker: "claude".into() }); },
```
(For M4 the worker is hardcoded to `"claude"`; a worker picker is later polish.) Update the footer hint in `ui.rs` to mention `c hand off` (optional, cheap).

**Step 4:** `./x cargo test` → all green; `./x cargo clippy --all-targets` → clean.

**Step 5: End-to-end smoke** (real tmux, dummy worker — verifies the `TmuxLauncher` path without `claude`). Run inside the container:
```bash
./x bash -lc '
set -e
cd /workspaces/personal-orchestration-system
rm -rf /tmp/kh && cargo build -q
./target/debug/kanban --root /tmp/kh/.kanban init >/dev/null
# point the claude worker at a dummy long-running command so tmux has something to run
sed -i "s/command: claude/command: sleep/" /tmp/kh/.kanban/config.yaml
sed -i "/- --add-dir/d; /- .kanban\/sessions/d" /tmp/kh/.kanban/config.yaml
# (simplest: rewrite config to a trivial worker)
cat > /tmp/kh/.kanban/config.yaml <<EOF
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
./target/debug/kanban --root /tmp/kh/.kanban daemon --addr 127.0.0.1:7811 &
DPID=$!; sleep 2
curl -s -X POST 127.0.0.1:7811/v1/intent -H "content-type: application/json" -d "{\"type\":\"createTask\",\"title\":\"t\",\"summary\":\"\",\"column\":\"inbox\"}" >/dev/null
curl -s -X POST 127.0.0.1:7811/v1/intent -H "content-type: application/json" -d "{\"type\":\"handoff\",\"task\":\"task-0001\",\"worker\":\"claude\"}"
echo; echo "--- tmux sessions ---"; tmux ls 2>&1 || echo "(none)"
echo "--- session.yaml ---"; cat /tmp/kh/.kanban/sessions/task-0001/session.yaml
tmux kill-server 2>/dev/null || true; kill $DPID 2>/dev/null || true
'
```
Expected: the handoff returns `{"type":"ok",...}`, `tmux ls` shows `kanban-task-0001`, and `session.yaml` records the worker session.

**Step 6:** Commit (`app.rs` + any `ui.rs` hint): `feat(tui): hand off selected task with 'c'`.

---

## M4 Done — verification

- `./x cargo test` → all green; `./x cargo clippy --all-targets` → clean.
- The Task 6 smoke test shows a real tmux session created by a real handoff (dummy worker).

**Achieved:** a task can be handed off to a worker — session workspace with allowlist symlinks + base-dir grant + `handoff.md`, a recorded `WorkerSession`, the task's `workerSessionRef` set, and the worker command launched in tmux — all driven by the `Handoff` intent and the TUI `c` key, with the orchestration logic fully tested behind a `Launcher` trait. M5 adds `kanban hook`, the intake-spool ingestion, and worker-state → card transitions (wiring the M1 derivation to real events), plus attach-to-session.
