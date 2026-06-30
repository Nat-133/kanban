use crate::model::*;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Atomic write: sibling temp file + fsync + rename. Safe because the controller
/// is the single writer of any given file.
pub fn atomic_write(path: &Path, contents: &str) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let file_name = path.file_name()
        .ok_or_else(|| anyhow::anyhow!("path has no file name: {}", path.display()))?;
    let tmp = path.with_file_name(format!("{}.tmp", file_name.to_string_lossy()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn board_path(root: &Path) -> PathBuf {
    root.join("board.yaml")
}

pub fn config_path(root: &Path) -> PathBuf {
    root.join("config.yaml")
}

pub fn load_config(root: &Path) -> anyhow::Result<Config> {
    let text = fs::read_to_string(config_path(root))?;
    Ok(serde_yml::from_str(&text)?)
}

fn default_config_yaml() -> &'static str {
    "agents:\n  baseDirs:\n    - ~/vcs/*\nworkers:\n  claude:\n    command: claude\n    args:\n      - --add-dir\n      - .kanban/sessions/{task_id}\n    workdir: \"{repo}\"\n    terminal:\n      type: tmux\n      sessionName: kanban-{task_id}\n"
}

pub fn load_board(root: &Path) -> anyhow::Result<Board> {
    let text = fs::read_to_string(board_path(root))?;
    Ok(serde_yml::from_str(&text)?)
}

pub fn load_board_checked(root: &Path) -> anyhow::Result<Board> {
    let text = fs::read_to_string(board_path(root))?;
    let raw: RawBoard = serde_yml::from_str(&text)?;
    if !raw.unknown.is_empty() {
        let keys: Vec<_> = raw.unknown.keys().cloned().collect();
        tracing::warn!(file = %board_path(root).display(), ?keys, "ignoring unknown field(s) in board.yaml");
    }
    Ok(Board::try_from(raw)?)
}

pub fn save_board(root: &Path, board: &Board) -> anyhow::Result<()> {
    let text = serde_yml::to_string(board)?;
    atomic_write(&board_path(root), &text)
}

/// Verify `root` is an initialized workspace (its `board.yaml` exists),
/// returning a clear, actionable error if not. The daemon calls this at startup
/// so it fails fast instead of binding the port and serving ENOENT on every
/// request (e.g. when launched from the wrong directory).
pub fn ensure_workspace(root: &Path) -> anyhow::Result<()> {
    if !board_path(root).exists() {
        anyhow::bail!(
            "no kanban workspace at {} (board.yaml not found) — run `kanban init` or pass --root <dir>",
            root.display()
        );
    }
    Ok(())
}

/// Next sequential id. Collision-free because the controller is the single writer.
pub fn next_task_id(root: &Path) -> anyhow::Result<TaskId> {
    // Archived tasks stay in tasks/ (just flagged), so scanning tasks/ keeps ids
    // monotonic for the workspace lifetime — no archived id is ever freed.
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
    max.checked_add(1)
        .map(TaskId::new)
        .ok_or_else(|| anyhow::anyhow!("task id space exhausted"))
}

pub fn load_events(session_dir: &Path) -> anyhow::Result<Vec<WorkerEvent>> {
    let path = session_dir.join("events.yaml");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path)?;
    let list: WorkerEventList = serde_yml::from_str(&text)?;
    Ok(list.items)
}

pub fn tasks_dir(root: &Path) -> PathBuf { root.join("tasks") }
pub fn task_dir(root: &Path, id: TaskId) -> PathBuf { tasks_dir(root).join(id.to_string()) }
fn task_file(root: &Path, id: TaskId) -> PathBuf { task_dir(root, id).join("task.yaml") }

pub fn init_workspace(root: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(tasks_dir(root))?;
    fs::create_dir_all(root.join("sessions"))?;
    if !board_path(root).exists() {
        let board = Board::try_from(default_board())
            .map_err(|e| anyhow::anyhow!("default board invalid: {e}"))?;
        save_board(root, &board)?;
    }
    if !config_path(root).exists() {
        atomic_write(&config_path(root), default_config_yaml())?;
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
        unknown: Default::default(),
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

pub fn session_dir(root: &Path, id: TaskId) -> PathBuf { root.join("sessions").join(id.to_string()) }

pub fn save_session(root: &Path, session: &WorkerSession) -> anyhow::Result<()> {
    let id = session.spec.task_ref;
    let text = serde_yml::to_string(session)?;
    atomic_write(&session_dir(root, id).join("session.yaml"), &text)
}

pub fn load_all_sessions(root: &Path) -> anyhow::Result<Vec<WorkerSession>> {
    let mut out = Vec::new();
    let dir = root.join("sessions");
    if dir.exists() {
        for e in fs::read_dir(&dir)? {
            let p = e?.path().join("session.yaml");
            if p.exists() {
                out.push(serde_yml::from_str(&fs::read_to_string(&p)?)?);
            }
        }
    }
    Ok(out)
}

/// Load a single persisted worker session, if one exists for `id`.
pub fn load_session(root: &Path, id: TaskId) -> anyhow::Result<Option<WorkerSession>> {
    let p = session_dir(root, id).join("session.yaml");
    if !p.exists() {
        return Ok(None);
    }
    Ok(Some(serde_yml::from_str(&fs::read_to_string(&p)?)?))
}

/// Remove a task's session workspace. No-op if there is none. Called on archive
/// to drop the dead worker's runtime state so the reconcile loop stops seeing it.
pub fn remove_session_dir(root: &Path, id: TaskId) -> anyhow::Result<()> {
    let dir = session_dir(root, id);
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_workspace_errors_with_hint_when_uninitialized() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban"); // never initialized
        let err = ensure_workspace(&root).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no kanban workspace"), "got: {msg}");
        assert!(msg.contains("kanban init"), "should hint at remedy, got: {msg}");
    }

    #[test]
    fn ensure_workspace_ok_after_init() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        init_workspace(&root).unwrap();
        assert!(ensure_workspace(&root).is_ok());
    }

    #[test]
    fn atomic_write_creates_parent_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/board.yaml");
        atomic_write(&path, "hello: world\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello: world\n");
        assert!(!dir.path().join("nested/board.tmp").exists());
        assert!(!dir.path().join("nested/board.yaml.tmp").exists());
    }

    #[test]
    fn board_saves_and_loads_back_equal() {
        let dir = tempfile::tempdir().unwrap();
        let raw = RawBoard {
            api_version: ApiVersion::V1Alpha1,
            kind: BoardKind::Board,
            metadata: Metadata {
                name: "default".into(),
                creation_timestamp: None,
                labels: Default::default(),
            },
            spec: RawBoardSpec {
                columns: vec![Column {
                    id: "inbox".parse().unwrap(),
                    title: "Inbox".into(),
                }],
                cards: [("inbox".parse().unwrap(), vec![TaskId::new(1)])]
                    .into_iter()
                    .collect(),
            },
            unknown: Default::default(),
        };
        let board = Board::try_from(raw).unwrap();
        save_board(dir.path(), &board).unwrap();
        assert_eq!(load_board(dir.path()).unwrap(), board);
    }

    #[test]
    fn load_board_warns_but_succeeds_on_unknown_field() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        init_workspace(&root).unwrap();
        let mut text = fs::read_to_string(board_path(&root)).unwrap();
        text.push_str("bogusField: 99\n");
        fs::write(board_path(&root), text).unwrap();
        // still loads (permissive); unknown field captured, not fatal
        let board = load_board_checked(&root).unwrap();
        assert!(!board.columns().is_empty());
    }

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

    #[test]
    fn next_task_id_counts_archived_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        init_workspace(&root).unwrap();
        let mut t = sample_task(TaskId::new(2), "archived");
        t.status.archived = true;
        save_task(&root, &t).unwrap();
        // archived tasks stay in tasks/, so their ids are never reused.
        assert_eq!(next_task_id(&root).unwrap(), TaskId::new(3));
    }

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
        assert_eq!(
            e[1].kind,
            WorkerEventKind::HumanInputRequired(Notification::IdlePrompt)
        );
    }

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
        let all = load_all_tasks(&root).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn remove_session_dir_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        init_workspace(&root).unwrap();
        let id = TaskId::new(1);
        fs::create_dir_all(session_dir(&root, id).join("hooks")).unwrap();
        remove_session_dir(&root, id).unwrap();
        assert!(!session_dir(&root, id).exists());
        // a second call on a missing dir is a no-op, not an error
        remove_session_dir(&root, id).unwrap();
    }

    #[test]
    fn init_writes_loadable_config() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        init_workspace(&root).unwrap();
        let cfg = load_config(&root).unwrap();
        assert!(cfg.workers.contains_key("claude"));
    }

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
}
