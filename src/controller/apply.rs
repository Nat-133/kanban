// intent application — Task 4

use crate::controller::store;
use crate::model::proto::{Intent, Response};
use crate::model::*;
use std::path::Path;

/// Apply an intent against the workspace at `root`.
///
/// Domain-level problems (unknown column, missing task, validation failures)
/// are returned as `Ok(Response::Error { .. })` so the daemon always replies.
/// Genuine I/O failures propagate as `Err` via `?`.
pub fn apply(root: &Path, intent: Intent) -> anyhow::Result<Response> {
    match intent {
        Intent::InitWorkspace => {
            store::init_workspace(root)?;
            Ok(Response::Ok { task: None })
        }
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

/// Remove `id` from every column's card list, keeping the board duplicate-free.
fn remove_card(raw: &mut RawBoard, id: TaskId) {
    for v in raw.spec.cards.values_mut() {
        v.retain(|t| *t != id);
    }
}

fn create_task(root: &Path, title: String, summary: String, column: ColumnId) -> anyhow::Result<Response> {
    let mut raw: RawBoard = store::load_board_checked(root)?.into();
    if !raw.spec.columns.iter().any(|c| c.id == column) {
        return Ok(Response::Error { message: format!("unknown column: {column}") });
    }
    let id = store::next_task_id(root)?;
    let task = Task {
        api_version: ApiVersion::V1Alpha1,
        kind: TaskKind::Task,
        metadata: Metadata { name: id.to_string(), creation_timestamp: None, labels: Default::default() },
        spec: TaskSpec {
            title,
            summary,
            color: None,
            description_ref: "description.md".into(),
            notes_ref: "notes.md".into(),
            acceptance_criteria: vec![],
            repo: None,
            jira: Default::default(),
            context: Default::default(),
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

fn edit_task(root: &Path, task_id: TaskId, title: Option<String>, summary: Option<String>) -> anyhow::Result<Response> {
    if !store::task_dir(root, task_id).exists() {
        return Ok(Response::Error { message: format!("task not found: {task_id}") });
    }
    let mut task = store::load_task(root, task_id)?;
    if let Some(title) = title {
        task.spec.title = title;
    }
    if let Some(summary) = summary {
        task.spec.summary = summary;
    }
    store::save_task(root, &task)?;
    Ok(Response::Ok { task: Some(task_id) })
}

fn move_card(root: &Path, task_id: TaskId, to_column: ColumnId, position: Option<usize>) -> anyhow::Result<Response> {
    let mut raw: RawBoard = store::load_board_checked(root)?.into();
    if !raw.spec.columns.iter().any(|c| c.id == to_column) {
        return Ok(Response::Error { message: format!("unknown column: {to_column}") });
    }
    remove_card(&mut raw, task_id);
    let list = raw.spec.cards.entry(to_column).or_default();
    match position {
        Some(pos) => {
            let idx = pos.min(list.len());
            list.insert(idx, task_id);
        }
        None => list.push(task_id),
    }
    let board = match Board::try_from(raw) {
        Ok(b) => b,
        Err(e) => return Ok(Response::Error { message: e.to_string() }),
    };
    store::save_board(root, &board)?;
    Ok(Response::Ok { task: Some(task_id) })
}

fn reorder_card(root: &Path, task_id: TaskId, position: usize) -> anyhow::Result<Response> {
    let mut raw: RawBoard = store::load_board_checked(root)?.into();
    let Some(column) = raw
        .spec
        .cards
        .iter()
        .find(|(_, tasks)| tasks.contains(&task_id))
        .map(|(c, _)| c.clone())
    else {
        return Ok(Response::Error { message: format!("card not on board: {task_id}") });
    };
    remove_card(&mut raw, task_id);
    let list = raw.spec.cards.entry(column).or_default();
    let idx = position.min(list.len());
    list.insert(idx, task_id);
    let board = match Board::try_from(raw) {
        Ok(b) => b,
        Err(e) => return Ok(Response::Error { message: e.to_string() }),
    };
    store::save_board(root, &board)?;
    Ok(Response::Ok { task: Some(task_id) })
}

fn archive(root: &Path, task_id: TaskId) -> anyhow::Result<Response> {
    let mut raw: RawBoard = store::load_board_checked(root)?.into();
    remove_card(&mut raw, task_id);
    let board = match Board::try_from(raw) {
        Ok(b) => b,
        Err(e) => return Ok(Response::Error { message: e.to_string() }),
    };
    store::save_board(root, &board)?;
    store::archive_task(root, task_id)?;
    Ok(Response::Ok { task: Some(task_id) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskId;

    fn setup() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        crate::controller::store::init_workspace(&dir.path().join(".kanban")).unwrap();
        dir
    }
    fn root(d: &tempfile::TempDir) -> std::path::PathBuf { d.path().join(".kanban") }
    fn col(s: &str) -> crate::model::ColumnId { s.parse().unwrap() }

    #[test]
    fn create_task_allocates_id_and_places_on_board() {
        let d = setup(); let r = root(&d);
        let resp = apply(&r, Intent::CreateTask { title: "First".into(), summary: "s".into(), column: col("inbox") }).unwrap();
        assert_eq!(resp, Response::Ok { task: Some(TaskId::new(1)) });
        let board = crate::controller::store::load_board(&r).unwrap();
        assert_eq!(board.cards().get(&col("inbox")).unwrap(), &vec![TaskId::new(1)]);
        assert!(crate::controller::store::load_task(&r, TaskId::new(1)).is_ok());
    }

    #[test]
    fn create_task_in_unknown_column_errors() {
        let d = setup(); let r = root(&d);
        let resp = apply(&r, Intent::CreateTask { title: "x".into(), summary: "s".into(), column: col("ghost") }).unwrap();
        assert!(matches!(resp, Response::Error { .. }));
    }

    #[test]
    fn get_board_returns_snapshot_with_tasks() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("inbox") }).unwrap();
        match apply(&r, Intent::GetBoard).unwrap() {
            Response::Snapshot { tasks, .. } => assert_eq!(tasks.len(), 1),
            o => panic!("expected snapshot, got {o:?}"),
        }
    }

    #[test]
    fn edit_task_updates_fields() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("inbox") }).unwrap();
        apply(&r, Intent::EditTask { task: TaskId::new(1), title: Some("B".into()), summary: None }).unwrap();
        let t = crate::controller::store::load_task(&r, TaskId::new(1)).unwrap();
        assert_eq!(t.spec.title, "B");
        assert_eq!(t.spec.summary, "s"); // unchanged
    }

    #[test]
    fn edit_missing_task_errors() {
        let d = setup(); let r = root(&d);
        let resp = apply(&r, Intent::EditTask { task: TaskId::new(99), title: Some("B".into()), summary: None }).unwrap();
        assert!(matches!(resp, Response::Error { .. }));
    }

    #[test]
    fn move_card_moves_between_columns() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("inbox") }).unwrap();
        apply(&r, Intent::MoveCard { task: TaskId::new(1), to_column: col("doing"), position: None }).unwrap();
        let board = crate::controller::store::load_board(&r).unwrap();
        assert!(board.cards().get(&col("inbox")).unwrap().is_empty());
        assert_eq!(board.cards().get(&col("doing")).unwrap(), &vec![TaskId::new(1)]);
    }

    #[test]
    fn move_card_to_unknown_column_errors() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("inbox") }).unwrap();
        let resp = apply(&r, Intent::MoveCard { task: TaskId::new(1), to_column: col("ghost"), position: None }).unwrap();
        assert!(matches!(resp, Response::Error { .. }));
    }

    #[test]
    fn reorder_card_within_column() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("inbox") }).unwrap();
        apply(&r, Intent::CreateTask { title: "B".into(), summary: "s".into(), column: col("inbox") }).unwrap();
        // inbox = [task-0001, task-0002]; move task-0002 to position 0
        apply(&r, Intent::ReorderCard { task: TaskId::new(2), position: 0 }).unwrap();
        let board = crate::controller::store::load_board(&r).unwrap();
        assert_eq!(board.cards().get(&col("inbox")).unwrap(), &vec![TaskId::new(2), TaskId::new(1)]);
    }

    #[test]
    fn archive_removes_card_and_task() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("inbox") }).unwrap();
        apply(&r, Intent::ArchiveTask { task: TaskId::new(1) }).unwrap();
        let board = crate::controller::store::load_board(&r).unwrap();
        assert!(board.cards().values().all(|v| v.is_empty()));
        assert!(crate::controller::store::load_task(&r, TaskId::new(1)).is_err());
    }
}
