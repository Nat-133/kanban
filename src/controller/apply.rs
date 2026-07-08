// intent application — Task 4

use crate::controller::activity;
use crate::controller::handoff;
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
            let tasks: Vec<_> = store::load_all_tasks(root)?
                .into_iter()
                .filter(|t| !t.status.archived)
                .collect();
            // Load each live task's long-form description alongside it, so the
            // TUI can render full detail without a filesystem round-trip.
            let mut descriptions = std::collections::BTreeMap::new();
            for t in &tasks {
                if let Ok(id) = t.metadata.name.parse::<TaskId>() {
                    if let Some(text) = store::load_description(root, id, &t.spec.description_ref)? {
                        descriptions.insert(id, text);
                    }
                }
            }
            let mut sessions = Vec::new();
            for s in store::load_all_sessions(root)? {
                let phase = crate::controller::events::session_phase(root, s.spec.task_ref)?;
                sessions.push(crate::model::proto::SessionView {
                    task: s.spec.task_ref,
                    session_name: s.spec.session_name.clone().unwrap_or_default(),
                    phase,
                    needs_human_input: phase.needs_human_input(),
                });
            }
            Ok(Response::Snapshot { board, tasks, sessions, descriptions })
        }
        Intent::CreateTask { title, summary, column } => create_task(root, title, summary, column),
        Intent::EditTask { task, title, summary } => edit_task(root, task, title, summary),
        Intent::EditDescription { task, base, description } => edit_description(root, task, base, description),
        Intent::MoveCard { task, to_column, position } => move_card(root, task, to_column, position),
        Intent::ReorderCard { task, position } => reorder_card(root, task, position),
        Intent::ArchiveTask { task } => archive(root, task, &handoff::TmuxLauncher),
        Intent::SetProfile { task, profile } => set_profile(root, task, profile),
        Intent::Handoff { task, worker } => {
            match handoff::handoff(root, task, &worker, &handoff::TmuxLauncher) {
                Ok(()) => Ok(Response::Ok { task: Some(task) }),
                Err(e) => Ok(Response::Error { message: e.to_string() }),
            }
        }
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
            profile: None,
        },
        status: Default::default(),
    };
    store::save_task(root, &task)?;
    // Seed the description file so it exists for the worker handoff (which
    // points at `description.md`) and for anyone editing it externally. Prose
    // lives in this Markdown file, never inline in task.yaml.
    store::atomic_write(
        &store::task_dir(root, id).join(&task.spec.description_ref),
        &format!("# {}\n", task.spec.title),
    )?;
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

/// Replace a task's long-form description, guarded by an optimistic-concurrency
/// check. `base` is what the caller last saw; if the on-disk content diverged
/// (a worker or external editor wrote it meanwhile), reject with the current
/// content rather than clobbering it. Content flows through the intent so the
/// client never needs the workspace path.
fn edit_description(root: &Path, task_id: TaskId, base: Option<String>, description: String) -> anyhow::Result<Response> {
    if !store::task_dir(root, task_id).exists() {
        return Ok(Response::Error { message: format!("task not found: {task_id}") });
    }
    let task = store::load_task(root, task_id)?;
    let current = store::load_description(root, task_id, &task.spec.description_ref)?;
    if current != base {
        return Ok(Response::Conflict { current });
    }
    store::atomic_write(
        &store::task_dir(root, task_id).join(&task.spec.description_ref),
        &description,
    )?;
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

fn archive(root: &Path, task_id: TaskId, launcher: &dyn handoff::Launcher) -> anyhow::Result<Response> {
    let mut task = match store::load_task(root, task_id) {
        Ok(t) => t,
        Err(_) => return Ok(Response::Error { message: format!("unknown task: {task_id}") }),
    };
    // Idempotent: archiving an already-archived task is a no-op.
    if task.status.archived {
        return Ok(Response::Ok { task: Some(task_id) });
    }
    // Tear down any worker: kill its terminal session and drop its runtime state
    // so the reconcile loop stops seeing it.
    if let Some(session) = store::load_session(root, task_id)? {
        if let Some(name) = session.spec.session_name.as_deref() {
            launcher.kill(name);
        }
    }
    store::archive_session_dir(root, task_id)?;
    // Flag the task archived (kept on disk, hidden from the board)…
    task.status.archived = true;
    store::save_task(root, &task)?;
    // …and take its card off the board.
    let mut raw: RawBoard = store::load_board_checked(root)?.into();
    remove_card(&mut raw, task_id);
    let board = match Board::try_from(raw) {
        Ok(b) => b,
        Err(e) => return Ok(Response::Error { message: e.to_string() }),
    };
    store::save_board(root, &board)?;
    Ok(Response::Ok { task: Some(task_id) })
}

/// Set a task's active profile and record the change as an activity fact.
///
/// The profile update is the durable state change; the `ProfileChanged` fact is
/// observability, so its append is best-effort (warned and swallowed on failure),
/// consistent with the human-involvement facts emitted in `events::record_state`.
fn set_profile(root: &Path, task_id: TaskId, profile: String) -> anyhow::Result<Response> {
    if !store::task_dir(root, task_id).exists() {
        return Ok(Response::Error { message: format!("task not found: {task_id}") });
    }
    let mut task = store::load_task(root, task_id)?;
    // Capture the previous profile BEFORE mutating so the fact records the transition.
    let from = task.spec.profile.clone();
    task.spec.profile = Some(profile.clone());
    store::save_task(root, &task)?;
    let ev = activity::ActivityEvent {
        observed_at: time::OffsetDateTime::now_utc(),
        task: task_id,
        // The new profile is the active one at emit time.
        profile: profile.clone(),
        kind: activity::ActivityKind::ProfileChanged { from, to: profile },
    };
    if let Err(e) = activity::append(root, &ev) {
        tracing::warn!(error = %e, "failed to append profileChanged activity event");
    }
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
        let resp = apply(&r, Intent::CreateTask { title: "First".into(), summary: "s".into(), column: col("todo") }).unwrap();
        assert_eq!(resp, Response::Ok { task: Some(TaskId::new(1)) });
        let board = crate::controller::store::load_board(&r).unwrap();
        assert_eq!(board.cards().get(&col("todo")).unwrap(), &vec![TaskId::new(1)]);
        assert!(crate::controller::store::load_task(&r, TaskId::new(1)).is_ok());
    }

    #[test]
    fn create_task_in_unknown_column_errors() {
        let d = setup(); let r = root(&d);
        let resp = apply(&r, Intent::CreateTask { title: "x".into(), summary: "s".into(), column: col("ghost") }).unwrap();
        assert!(matches!(resp, Response::Error { .. }));
    }

    #[test]
    fn create_task_seeds_description_file_and_get_board_returns_it() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "Write docs".into(), summary: "s".into(), column: col("todo") }).unwrap();
        // the description file is seeded on disk with the title heading
        let desc = crate::controller::store::load_description(&r, TaskId::new(1), "description.md").unwrap();
        assert_eq!(desc.as_deref(), Some("# Write docs\n"));
        // and the snapshot carries it keyed by task id
        match apply(&r, Intent::GetBoard).unwrap() {
            Response::Snapshot { descriptions, .. } => {
                assert_eq!(descriptions.get(&TaskId::new(1)).map(String::as_str), Some("# Write docs\n"));
            }
            o => panic!("expected snapshot, got {o:?}"),
        }
    }

    #[test]
    fn get_board_returns_snapshot_with_tasks() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("todo") }).unwrap();
        match apply(&r, Intent::GetBoard).unwrap() {
            Response::Snapshot { tasks, .. } => assert_eq!(tasks.len(), 1),
            o => panic!("expected snapshot, got {o:?}"),
        }
    }

    struct NoLaunch;
    impl crate::controller::handoff::Launcher for NoLaunch {
        fn launch(&self, _s: &crate::model::WorkerSession, _n: &str) -> anyhow::Result<()> { Ok(()) }
        fn kill(&self, _n: &str) {}
    }

    #[test]
    fn get_board_includes_sessions_with_phase() {
        let dir = setup(); let r = root(&dir);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("todo") }).unwrap();
        crate::controller::handoff::handoff(&r, TaskId::new(1), "claude", &NoLaunch).unwrap();
        crate::controller::events::record_state(&r, TaskId::new(1), "notification", "{\"notification_type\":\"permission_prompt\"}").unwrap();
        crate::controller::events::ingest_session(&r, TaskId::new(1)).unwrap();
        match apply(&r, Intent::GetBoard).unwrap() {
            Response::Snapshot { sessions, .. } => {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].task, TaskId::new(1));
                assert!(sessions[0].needs_human_input);
                assert_eq!(sessions[0].session_name, "kanban-task-0001");
            }
            o => panic!("expected snapshot, got {o:?}"),
        }
    }

    #[test]
    fn edit_task_updates_fields() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("todo") }).unwrap();
        apply(&r, Intent::EditTask { task: TaskId::new(1), title: Some("B".into()), summary: None }).unwrap();
        let t = crate::controller::store::load_task(&r, TaskId::new(1)).unwrap();
        assert_eq!(t.spec.title, "B");
        assert_eq!(t.spec.summary, "s"); // unchanged
    }

    #[test]
    fn edit_description_writes_when_base_matches() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("todo") }).unwrap();
        // create seeds "# A\n"; edit from that exact base
        let resp = apply(&r, Intent::EditDescription {
            task: TaskId::new(1),
            base: Some("# A\n".into()),
            description: "# A\nnew body\n".into(),
        }).unwrap();
        assert_eq!(resp, Response::Ok { task: Some(TaskId::new(1)) });
        let desc = store::load_description(&r, TaskId::new(1), "description.md").unwrap();
        assert_eq!(desc.as_deref(), Some("# A\nnew body\n"));
    }

    #[test]
    fn edit_description_conflict_when_base_is_stale() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("todo") }).unwrap();
        // base does not match what's on disk ("# A\n") -> reject, return current, leave file untouched
        let resp = apply(&r, Intent::EditDescription {
            task: TaskId::new(1),
            base: Some("something else".into()),
            description: "clobber".into(),
        }).unwrap();
        assert_eq!(resp, Response::Conflict { current: Some("# A\n".into()) });
        let desc = store::load_description(&r, TaskId::new(1), "description.md").unwrap();
        assert_eq!(desc.as_deref(), Some("# A\n"), "conflicting write must not touch the file");
    }

    #[test]
    fn edit_description_missing_task_errors() {
        let d = setup(); let r = root(&d);
        let resp = apply(&r, Intent::EditDescription {
            task: TaskId::new(99),
            base: None,
            description: "x".into(),
        }).unwrap();
        assert!(matches!(resp, Response::Error { .. }));
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
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("todo") }).unwrap();
        apply(&r, Intent::MoveCard { task: TaskId::new(1), to_column: col("doing"), position: None }).unwrap();
        let board = crate::controller::store::load_board(&r).unwrap();
        assert!(board.cards().get(&col("todo")).unwrap().is_empty());
        assert_eq!(board.cards().get(&col("doing")).unwrap(), &vec![TaskId::new(1)]);
    }

    #[test]
    fn move_card_to_unknown_column_errors() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("todo") }).unwrap();
        let resp = apply(&r, Intent::MoveCard { task: TaskId::new(1), to_column: col("ghost"), position: None }).unwrap();
        assert!(matches!(resp, Response::Error { .. }));
    }

    #[test]
    fn reorder_card_within_column() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("todo") }).unwrap();
        apply(&r, Intent::CreateTask { title: "B".into(), summary: "s".into(), column: col("todo") }).unwrap();
        // inbox = [task-0001, task-0002]; move task-0002 to position 0
        apply(&r, Intent::ReorderCard { task: TaskId::new(2), position: 0 }).unwrap();
        let board = crate::controller::store::load_board(&r).unwrap();
        assert_eq!(board.cards().get(&col("todo")).unwrap(), &vec![TaskId::new(2), TaskId::new(1)]);
    }

    #[derive(Default)]
    struct FakeLauncher {
        killed: std::sync::Mutex<Vec<String>>,
    }
    impl handoff::Launcher for FakeLauncher {
        fn launch(&self, _session: &WorkerSession, _session_name: &str) -> anyhow::Result<()> { Ok(()) }
        fn kill(&self, session_name: &str) { self.killed.lock().unwrap().push(session_name.to_string()); }
    }

    #[test]
    fn archive_flags_task_kills_session_and_archives_session_dir() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "".into(), column: col("todo") }).unwrap();
        let id = TaskId::new(1);
        // hand off (fake launcher) so a session.yaml + sessions/ dir exist
        handoff::handoff(&r, id, "claude", &FakeLauncher::default()).unwrap();
        assert!(store::session_dir(&r, id).join("session.yaml").exists());

        let fake = FakeLauncher::default();
        archive(&r, id, &fake).unwrap();

        // task kept on disk but flagged archived, card gone, session torn down
        assert!(store::load_task(&r, id).unwrap().status.archived);
        let board = store::load_board(&r).unwrap();
        assert!(board.cards().values().all(|v| v.is_empty()));
        assert!(!store::session_dir(&r, id).exists(), "session dir should be archived out of sessions/");
        assert!(store::archive_dir(&r).join(id.to_string()).join("session.yaml").exists(),
            "archived session record should be preserved under archive/sessions/");
        assert_eq!(&*fake.killed.lock().unwrap(), &["kanban-task-0001".to_string()]);
    }

    #[test]
    fn archived_task_is_hidden_from_snapshot() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "s".into(), column: col("todo") }).unwrap();
        apply(&r, Intent::ArchiveTask { task: TaskId::new(1) }).unwrap();
        // still on disk…
        assert!(store::load_task(&r, TaskId::new(1)).unwrap().status.archived);
        // …but absent from the board and the snapshot's task list
        match apply(&r, Intent::GetBoard).unwrap() {
            Response::Snapshot { board, tasks, .. } => {
                assert!(board.cards().values().all(|v| v.is_empty()));
                assert!(tasks.is_empty());
            }
            other => panic!("expected snapshot, got {other:?}"),
        }
    }

    #[test]
    fn set_profile_updates_task_and_logs_profile_changed() {
        use crate::controller::activity;
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "A".into(), summary: "".into(),
            column: "todo".parse().unwrap() }).unwrap();
        let id = TaskId::new(1);

        apply(&root, Intent::SetProfile { task: id, profile: "cluster-ops".into() }).unwrap();

        assert_eq!(store::load_task(&root, id).unwrap().spec.profile.as_deref(), Some("cluster-ops"));
        let acts = activity::load(&root).unwrap();
        // Assert the full None -> Some transition: `from` must be the OLD value
        // (None on a fresh task), which pins the "capture from before mutating" contract.
        assert!(acts.iter().any(|a| matches!(&a.kind,
            activity::ActivityKind::ProfileChanged { from, to }
                if from.is_none() && to == "cluster-ops")));
    }

    #[test]
    fn archive_is_idempotent() {
        let d = setup(); let r = root(&d);
        apply(&r, Intent::CreateTask { title: "A".into(), summary: "".into(), column: col("todo") }).unwrap();
        let id = TaskId::new(1);
        let fake = FakeLauncher::default();
        archive(&r, id, &fake).unwrap();
        // second call must not error and must leave the same end state
        archive(&r, id, &fake).unwrap();
        assert!(store::load_task(&r, id).unwrap().status.archived);
    }
}
