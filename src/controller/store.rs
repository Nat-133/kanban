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
    let tmp = path.with_extension("tmp");
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
    Ok(TaskId::new(max + 1))
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
}
