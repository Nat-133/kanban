use crate::controller::store;
use crate::model::{
    ApiVersion, Metadata, Task, TaskId, WorkerConfig, WorkerSession, WorkerSessionKind,
    WorkerSessionSpec,
};
use std::path::{Path, PathBuf};

/// Launches a prepared worker session in a terminal/multiplexer.
pub trait Launcher {
    /// `session_name` is the resolved terminal session name (e.g. the tmux session).
    fn launch(&self, session: &WorkerSession, session_name: &str) -> anyhow::Result<()>;
    /// Best-effort teardown of a running session. Tearing down a session that is
    /// already gone is not an error.
    fn kill(&self, session_name: &str);
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
        cmd.args(&session.spec.command);
        let status = cmd.status()?;
        if !status.success() {
            anyhow::bail!("tmux new-session failed with status {status}");
        }
        Ok(())
    }

    fn kill(&self, session_name: &str) {
        let _ = std::process::Command::new("tmux")
            .arg("kill-session")
            .arg("-t")
            .arg(session_name)
            .status();
    }
}

/// Prepare the per-task worker session workspace and build the `WorkerSession` value.
///
/// Pure filesystem prep: creates the session directory and hook subdirectories,
/// symlinks each allowlisted context file into the session dir, writes `handoff.md`,
/// and assembles the launch command. Does NOT launch anything and does NOT persist
/// `session.yaml`.
pub fn prepare_session(
    root: &Path,
    task: &Task,
    worker_name: &str,
    worker: &WorkerConfig,
    base_dirs: &[String],
) -> anyhow::Result<WorkerSession> {
    let id: TaskId = task.metadata.name.parse().map_err(|_| {
        anyhow::anyhow!("task metadata.name is not a valid task id: {}", task.metadata.name)
    })?;
    let sdir = root.join("sessions").join(id.to_string());
    // The session dir is the control plane (state.yaml, session.yaml, hooks/) and is
    // NOT writable by the agent. The agent gets a nested work/ sandbox: its cwd and
    // the only path on its allowlist, holding the handoff and context it may touch.
    std::fs::create_dir_all(sdir.join("hooks"))?;
    std::fs::create_dir_all(sdir.join("work"))?;
    // Canonicalize to an absolute path: the worker runs with the work dir (or the
    // repo) as its cwd, so the `--settings` and `--add-dir` paths derived from it
    // must be absolute or they won't resolve. A relative `--root` would otherwise
    // launch a worker that dies instantly on a missing settings file.
    let sdir = std::fs::canonicalize(&sdir)?;
    let work = sdir.join("work");

    // symlink each allowlisted context file (relative to the task dir) into work/
    for entry in &task.spec.context.include {
        let src = store::task_dir(root, id).join(entry);
        let src_abs = match std::fs::canonicalize(&src) {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!(entry = %entry, "allowlisted context file not found; skipping symlink");
                continue;
            }
        };
        let name = match Path::new(entry).file_name() {
            Some(n) => n.to_owned(),
            None => continue,
        };
        let link = work.join(&name);
        let _ = std::fs::remove_file(&link); // idempotent: clear any stale link
        std::os::unix::fs::symlink(&src_abs, &link)?;
    }

    // handoff.md (a read material for the agent, so it lives in work/)
    std::fs::write(work.join("handoff.md"), render_handoff(task))?;

    // command: worker.command + substituted args + one --add-dir per expanded base dir
    let task_id = id.to_string();
    let repo = task.spec.repo.as_deref();
    let mut command = vec![worker.command.clone()];
    for arg in &worker.args {
        command.push(substitute(arg, &task_id, repo));
    }
    // Always allowlist the work/ sandbox by absolute path — and only that, never the
    // control-plane session dir. When a task has a repo the worker's cwd is the repo,
    // so a relative `--add-dir` would resolve there and the worker couldn't read its
    // own handoff.md / write notes.md.
    command.push("--add-dir".to_string());
    command.push(work.display().to_string());
    for entry in base_dirs {
        for path in expand_base_dir(entry) {
            command.push("--add-dir".to_string());
            command.push(path.display().to_string());
        }
    }

    // Per-session Claude Code hooks settings: each event calls back into `kanban hook`.
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "kanban".to_string());
    let root_abs = std::fs::canonicalize(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| root.display().to_string());
    let mk = |name: &str| {
        serde_json::json!([{
            "matcher": "",
            "hooks": [{
                "type": "command",
                "command": format!("{exe} hook {name} --root {root_abs} --session {id}"),
            }]
        }])
    };
    let settings = serde_json::json!({ "hooks": {
        "Notification": mk("notification"),
        "Stop": mk("stop"),
        "SessionStart": mk("session-start"),
        "UserPromptSubmit": mk("user-prompt-submit"),
        "SessionEnd": mk("session-end"),
    }});
    let settings_path = sdir.join("hooks/settings.json");
    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings)?)?;
    // inject the flag right after the worker command (index 0)
    command.insert(1, settings_path.display().to_string());
    command.insert(1, "--settings".to_string());

    // Seed Claude's first turn with the task, otherwise it launches into an idle
    // interactive session and never reads the handoff. Insert it right after the
    // `--settings <path>` pair (now at indices 1 and 2): a leading positional
    // could be mistaken for the `[command]` subcommand slot, while a trailing one
    // would be swallowed by the variadic `--add-dir <dirs...>` that follows.
    let prompt = format!(
        "Read the task handoff at {} and start working on the task it describes, \
         following the instructions in that file.",
        work.join("handoff.md").display()
    );
    command.insert(3, prompt);

    // workdir: the task's repo (tilde-expanded) if set, else the agent's work/ sandbox
    let workdir = match repo {
        Some(r) => PathBuf::from(expand_tilde(r)),
        None => work.clone(),
    };

    Ok(WorkerSession {
        api_version: ApiVersion::V1Alpha1,
        kind: WorkerSessionKind::WorkerSession,
        metadata: Metadata {
            name: format!("{id}-{worker_name}"),
            creation_timestamp: None,
            labels: Default::default(),
        },
        spec: WorkerSessionSpec {
            task_ref: id,
            worker: worker_name.to_string(),
            workspace: sdir,
            workdir: Some(workdir),
            command,
            session_name: Some(substitute(&worker.terminal.session_name, &id.to_string(), task.spec.repo.as_deref())),
        },
        status: Default::default(),
    })
}

/// Tie handoff together: prepare the session, launch it, persist `session.yaml`,
/// and record the session ref on the task. Unknown worker -> error.
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

fn render_handoff(task: &Task) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Task handoff: {}\n\n", task.metadata.name));
    s.push_str(&format!("## Title\n\n{}\n\n", task.spec.title));
    s.push_str(&format!("## Summary\n\n{}\n\n", task.spec.summary));
    s.push_str("## Description\n\nSee `description.md`.\n\n");
    s.push_str("## Acceptance criteria\n\n");
    for c in &task.spec.acceptance_criteria {
        s.push_str(&format!("- {c}\n"));
    }
    s.push_str("\n## Allowed context\n\n");
    for c in &task.spec.context.include {
        s.push_str(&format!("- {c}\n"));
    }
    s.push_str("\n## Instructions\n\nWork only on this task unless explicitly asked otherwise.\nDo not inspect unrelated task directories.\nUpdate `notes.md` with useful findings.\n");
    s
}

/// Replace `{task_id}` and `{repo}` placeholders. Missing repo -> empty string.
pub fn substitute(template: &str, task_id: &str, repo: Option<&str>) -> String {
    template.replace("{task_id}", task_id).replace("{repo}", repo.unwrap_or(""))
}

/// Expand `~/` to $HOME (only a leading `~/`).
pub(crate) fn expand_tilde(s: &str) -> String {
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

    #[test]
    fn prepare_session_creates_workspace_symlinks_and_command() {
        use crate::controller::store;
        use crate::model::*;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
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
        // control-plane files stay at the top; the agent works inside work/
        let work = sdir.join("work");
        assert!(work.is_dir(), "agent work/ sandbox should exist");
        assert!(work.join("handoff.md").exists());
        let link = work.join("notes.md");
        assert!(std::fs::symlink_metadata(&link).is_ok(), "notes.md should be symlinked into work/");
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "hi");
        assert_eq!(session.spec.command.first().unwrap(), "claude");
        assert!(session.spec.command.iter().any(|a| a.contains(&format!("sessions/{id}"))));
        assert_eq!(session.spec.task_ref, id);
        assert_eq!(session.metadata.name, format!("{id}-claude"));
        // no repo set -> workdir is the session dir, and it must be absolute so the
        // relative-path-from-root launch bug can't recur
        assert!(session.spec.workdir.as_ref().unwrap().is_absolute());
    }

    #[test]
    fn prepare_session_allowlists_work_dir_not_control_plane() {
        use crate::controller::store;
        use crate::model::TaskId;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        let id = TaskId::new(1);
        let mut task = sample_task(id, "Do the thing");
        // A repo means cwd is the repo, so the agent's work/ sandbox (holding
        // handoff.md) must be allowlisted by absolute path.
        task.spec.repo = Some("~/vcs/whatever".into());
        store::save_task(&root, &task).unwrap();
        let cfg = store::load_config(&root).unwrap();
        let session = prepare_session(&root, &task, "claude", cfg.workers.get("claude").unwrap(), &cfg.agents.base_dirs).unwrap();

        let work = session.spec.workspace.join("work").display().to_string();
        let sdir = session.spec.workspace.display().to_string();
        let cmd = &session.spec.command;
        let idx = cmd.iter().position(|a| a == &work)
            .unwrap_or_else(|| panic!("work dir {work} not allowlisted; cmd={cmd:?}"));
        assert_eq!(cmd[idx - 1], "--add-dir", "work dir must follow --add-dir; cmd={cmd:?}");
        // the control-plane session dir must NOT be allowlisted on its own
        assert!(!cmd.iter().any(|a| a == &sdir), "control-plane dir must not be allowlisted; cmd={cmd:?}");
    }

    #[test]
    fn prepare_session_seeds_an_initial_prompt_pointing_at_handoff() {
        use crate::controller::store;
        use crate::model::TaskId;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        let id = TaskId::new(1);
        let task = sample_task(id, "Do the thing");
        store::save_task(&root, &task).unwrap();
        let cfg = store::load_config(&root).unwrap();
        let session = prepare_session(&root, &task, "claude", cfg.workers.get("claude").unwrap(), &cfg.agents.base_dirs).unwrap();

        // Without an initial prompt the worker launches into an idle interactive
        // session: some command arg must point it at its handoff brief.
        let cmd = &session.spec.command;
        let handoff = session.spec.workspace.join("work").join("handoff.md").display().to_string();
        let idx = cmd.iter().position(|a| a.contains(&handoff))
            .unwrap_or_else(|| panic!("no initial prompt referencing {handoff}; cmd={cmd:?}"));
        // It must not be the final token: a trailing positional would be eaten by
        // the variadic `--add-dir <dirs...>` that precedes it.
        assert!(idx < cmd.len() - 1, "prompt must not be the last arg; cmd={cmd:?}");
    }

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

        let settings = root.join("sessions/task-0001/hooks/settings.json");
        assert!(settings.exists());
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        let cmd = v["hooks"]["Notification"][0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("hook notification"));
        assert!(cmd.contains("--session task-0001"));
        let idx = session.spec.command.iter().position(|a| a == "--settings").expect("--settings present");
        // command stores the canonicalized (absolute) path so it resolves from any cwd
        let settings_canon = std::fs::canonicalize(&settings).unwrap();
        assert_eq!(session.spec.command[idx + 1], settings_canon.display().to_string());
        assert!(settings_canon.is_absolute());
    }

    #[derive(Default)]
    struct FakeLauncher {
        launched: std::sync::Mutex<Vec<(crate::model::WorkerSession, String)>>,
    }
    impl super::Launcher for FakeLauncher {
        fn launch(&self, session: &crate::model::WorkerSession, session_name: &str) -> anyhow::Result<()> {
            self.launched.lock().unwrap().push((session.clone(), session_name.to_string()));
            Ok(())
        }
        fn kill(&self, _session_name: &str) {}
    }

    #[test]
    fn handoff_writes_session_and_sets_task_ref() {
        use crate::controller::store;
        use crate::model::TaskId;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        crate::controller::apply::apply(&root, crate::model::proto::Intent::CreateTask {
            title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() }).unwrap();

        let fake = FakeLauncher::default();
        handoff(&root, TaskId::new(1), "claude", &fake).unwrap();

        assert!(root.join("sessions/task-0001/session.yaml").exists());
        let task = store::load_task(&root, TaskId::new(1)).unwrap();
        assert_eq!(task.status.worker_session_ref.as_deref(), Some("task-0001-claude"));
        let launched = fake.launched.lock().unwrap();
        assert_eq!(launched.len(), 1);
        assert_eq!(launched[0].1, "kanban-task-0001"); // resolved tmux session name
    }

    #[test]
    fn handoff_unknown_worker_errors() {
        use crate::controller::store;
        use crate::model::TaskId;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        crate::controller::apply::apply(&root, crate::model::proto::Intent::CreateTask {
            title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() }).unwrap();
        let fake = FakeLauncher::default();
        assert!(handoff(&root, TaskId::new(1), "nope", &fake).is_err());
    }

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
                session_name: None,
            },
            status: Default::default(),
        };
        let fake = FakeLauncher::default();
        fake.launch(&session, "kanban-task-0001").unwrap();
        let launched = fake.launched.lock().unwrap();
        assert_eq!(launched.len(), 1);
        assert_eq!(launched[0].1, "kanban-task-0001");
    }

    fn sample_task(id: crate::model::TaskId, title: &str) -> crate::model::Task {
        use crate::model::*;
        Task { api_version: ApiVersion::V1Alpha1, kind: TaskKind::Task,
            metadata: Metadata { name: id.to_string(), creation_timestamp: None, labels: Default::default() },
            spec: TaskSpec { title: title.into(), summary: String::new(), color: None,
                description_ref: "description.md".into(), notes_ref: "notes.md".into(),
                acceptance_criteria: vec![], repo: None, jira: Default::default(), context: Default::default(), profile: None },
            status: Default::default() }
    }
}
