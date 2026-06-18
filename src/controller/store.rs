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

pub fn load_board(root: &Path) -> anyhow::Result<Board> {
    let text = fs::read_to_string(board_path(root))?;
    Ok(serde_yml::from_str(&text)?)
}

pub fn save_board(root: &Path, board: &Board) -> anyhow::Result<()> {
    let text = serde_yml::to_string(board)?;
    atomic_write(&board_path(root), &text)
}

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
        };
        let board = Board::try_from(raw).unwrap();
        save_board(dir.path(), &board).unwrap();
        assert_eq!(load_board(dir.path()).unwrap(), board);
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
    fn archive_task_moves_dir_out_of_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        init_workspace(&root).unwrap();
        save_task(&root, &sample_task(TaskId::new(1), "A")).unwrap();
        archive_task(&root, TaskId::new(1)).unwrap();
        assert!(!root.join("tasks/task-0001").exists());
        assert!(root.join("archive/task-0001").exists());
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
