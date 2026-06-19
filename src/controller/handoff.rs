use crate::controller::store;
use crate::model::{
    ApiVersion, Metadata, Task, TaskId, WorkerConfig, WorkerSession, WorkerSessionKind,
    WorkerSessionSpec,
};
use std::path::{Path, PathBuf};

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
    std::fs::create_dir_all(sdir.join("hooks/intake"))?;
    std::fs::create_dir_all(sdir.join("hooks/processed"))?;

    // symlink each allowlisted context file (relative to the task dir) into the session dir
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
        let link = sdir.join(&name);
        let _ = std::fs::remove_file(&link); // idempotent: clear any stale link
        std::os::unix::fs::symlink(&src_abs, &link)?;
    }

    // handoff.md
    std::fs::write(sdir.join("handoff.md"), render_handoff(task))?;

    // command: worker.command + substituted args + one --add-dir per expanded base dir
    let task_id = id.to_string();
    let repo = task.spec.repo.as_deref();
    let mut command = vec![worker.command.clone()];
    for arg in &worker.args {
        command.push(substitute(arg, &task_id, repo));
    }
    for entry in base_dirs {
        for path in expand_base_dir(entry) {
            command.push("--add-dir".to_string());
            command.push(path.display().to_string());
        }
    }

    // workdir: the task's repo (tilde-expanded) if set, else the session workspace
    let workdir = match repo {
        Some(r) => PathBuf::from(expand_tilde(r)),
        None => sdir.clone(),
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
        },
        status: Default::default(),
    })
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
        assert!(sdir.join("hooks/intake").is_dir());
        assert!(sdir.join("hooks/processed").is_dir());
        assert!(sdir.join("handoff.md").exists());
        let link = sdir.join("notes.md");
        assert!(std::fs::symlink_metadata(&link).is_ok(), "notes.md should be symlinked");
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "hi");
        assert_eq!(session.spec.command.first().unwrap(), "claude");
        assert!(session.spec.command.iter().any(|a| a.contains(&format!("sessions/{id}"))));
        assert_eq!(session.spec.task_ref, id);
        assert_eq!(session.metadata.name, format!("{id}-claude"));
    }

    fn sample_task(id: crate::model::TaskId, title: &str) -> crate::model::Task {
        use crate::model::*;
        Task { api_version: ApiVersion::V1Alpha1, kind: TaskKind::Task,
            metadata: Metadata { name: id.to_string(), creation_timestamp: None, labels: Default::default() },
            spec: TaskSpec { title: title.into(), summary: String::new(), color: None,
                description_ref: "description.md".into(), notes_ref: "notes.md".into(),
                acceptance_criteria: vec![], repo: None, jira: Default::default(), context: Default::default() },
            status: Default::default() }
    }
}
