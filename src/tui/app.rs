// app state + key handling — Task 3

use crate::model::proto::Intent;
use crate::model::{Column, ColumnId, Task, TaskId};
use crate::tui::client::Snapshot;
use crossterm::event::{KeyCode, KeyEvent};

/// The result of handling a key: nothing, quit the loop, or send an intent.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Quit,
    Send(Intent),
    /// Open the named tmux session in the embedded terminal popup.
    OpenTerminal(String),
    /// Attach to the named tmux session full-screen (suspending the TUI).
    AttachFullscreen(String),
}

/// The current input mode of the app.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    AddTask,
    EditTask,
    Search,
    Help,
    Detail,
    /// The embedded terminal popup is open; key input is routed to the PTY.
    Terminal,
}

/// Pure TUI state: holds the latest snapshot, the cursor (column/row), the
/// input mode, and any in-progress text input. No terminal or I/O.
pub struct App {
    snapshot: Snapshot,
    col: usize,
    row: usize,
    mode: Mode,
    input: String,
    filter: String,
    editing: Option<TaskId>,
    pub status: Option<String>,
}

impl App {
    pub fn new(snapshot: Snapshot) -> Self {
        Self {
            snapshot,
            col: 0,
            row: 0,
            mode: Mode::Normal,
            input: String::new(),
            filter: String::new(),
            editing: None,
            status: None,
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn set_snapshot(&mut self, s: Snapshot) {
        self.snapshot = s;
        self.clamp();
    }

    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    pub fn input(&self) -> &str {
        &self.input
    }

    pub fn selected_col(&self) -> usize {
        self.col
    }

    pub fn selected_row(&self) -> usize {
        self.row
    }

    pub fn columns(&self) -> &[Column] {
        self.snapshot.board.columns()
    }

    fn column_ids(&self) -> Vec<ColumnId> {
        self.columns().iter().map(|c| c.id.clone()).collect()
    }

    pub fn column_cards(&self, col: usize) -> Vec<TaskId> {
        match self.columns().get(col) {
            Some(c) => self.snapshot.board.cards().get(&c.id).cloned().unwrap_or_default(),
            None => Vec::new(),
        }
    }

    pub fn session_for(&self, task: TaskId) -> Option<&crate::model::proto::SessionView> {
        self.snapshot().sessions.iter().find(|s| s.task == task)
    }

    /// The current filter text (active when in `Search` mode or non-empty).
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Look up a task's title from the snapshot by its id.
    fn task_title(&self, id: TaskId) -> Option<String> {
        let name = id.to_string();
        self.snapshot
            .tasks
            .iter()
            .find(|t| t.metadata.name == name)
            .map(|t| t.spec.title.clone())
    }

    /// Cards in a column after applying the case-insensitive title filter.
    /// An empty filter returns every card (identical to `column_cards`).
    pub fn visible_cards(&self, col: usize) -> Vec<TaskId> {
        let all = self.column_cards(col);
        if self.filter.is_empty() {
            return all;
        }
        let needle = self.filter.to_lowercase();
        all.into_iter()
            .filter(|t| {
                self.task_title(*t)
                    .map(|title| title.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            })
            .collect()
    }

    pub fn selected_task(&self) -> Option<TaskId> {
        self.visible_cards(self.col).get(self.row).copied()
    }

    /// The currently selected task's full record, looked up from the snapshot.
    pub fn detail_task(&self) -> Option<&Task> {
        let id = self.selected_task()?;
        let name = id.to_string();
        self.snapshot.tasks.iter().find(|t| t.metadata.name == name)
    }

    fn clamp(&mut self) {
        let ncols = self.columns().len();
        if ncols == 0 {
            self.col = 0;
            self.row = 0;
            return;
        }
        self.col = self.col.min(ncols - 1);
        let n = self.visible_cards(self.col).len();
        self.row = if n == 0 { 0 } else { self.row.min(n - 1) };
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        match self.mode {
            Mode::Normal => self.on_normal(key),
            Mode::AddTask => self.on_add(key),
            Mode::EditTask => self.on_edit(key),
            Mode::Search => self.on_search(key),
            Mode::Detail => self.on_detail(key),
            Mode::Help => {
                self.mode = Mode::Normal;
                Action::None
            }
            // Terminal input is routed straight to the PTY by the run loop, so
            // it never reaches `on_key`; handled here only for exhaustiveness.
            Mode::Terminal => Action::None,
        }
    }

    /// Enter terminal-popup mode (the run loop owns the live `TermSession`).
    pub fn enter_terminal(&mut self) {
        self.mode = Mode::Terminal;
    }

    /// Leave terminal-popup mode and return to the board.
    pub fn exit_terminal(&mut self) {
        self.mode = Mode::Normal;
    }

    fn on_detail(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
                self.mode = Mode::Normal;
            }
            _ => {}
        }
        Action::None
    }

    fn on_normal(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('q') => Action::Quit,
            KeyCode::Char('h') => {
                if self.col > 0 {
                    self.col -= 1;
                }
                self.clamp();
                Action::None
            }
            KeyCode::Char('l') => {
                if self.col + 1 < self.columns().len() {
                    self.col += 1;
                }
                self.clamp();
                Action::None
            }
            KeyCode::Char('j') => {
                let n = self.visible_cards(self.col).len();
                if n > 0 && self.row + 1 < n {
                    self.row += 1;
                }
                Action::None
            }
            KeyCode::Char('k') => {
                if self.row > 0 {
                    self.row -= 1;
                }
                Action::None
            }
            KeyCode::Char('H') => self.move_card(-1),
            KeyCode::Char('L') => self.move_card(1),
            KeyCode::Char('K') => self.reorder(-1),
            KeyCode::Char('J') => self.reorder(1),
            KeyCode::Char('d') => match self.selected_task() {
                Some(t) => Action::Send(Intent::ArchiveTask { task: t }),
                None => Action::None,
            },
            KeyCode::Char('c') => {
                if let Some(t) = self.selected_task() {
                    return Action::Send(Intent::Handoff { task: t, worker: "claude".into() });
                }
                Action::None
            }
            KeyCode::Char('t') => {
                if let Some(name) = self.selected_task().and_then(|t| self.session_for(t)).map(|s| s.session_name.clone()) {
                    return Action::OpenTerminal(name);
                }
                Action::None
            }
            KeyCode::Char('T') => {
                if let Some(name) = self.selected_task().and_then(|t| self.session_for(t)).map(|s| s.session_name.clone()) {
                    return Action::AttachFullscreen(name);
                }
                Action::None
            }
            KeyCode::Char('a') => {
                self.mode = Mode::AddTask;
                self.input.clear();
                Action::None
            }
            KeyCode::Char('e') => {
                if let Some(t) = self.selected_task() {
                    self.mode = Mode::EditTask;
                    self.editing = Some(t);
                    self.input = self.task_title(t).unwrap_or_default();
                }
                Action::None
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Search;
                Action::None
            }
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
                Action::None
            }
            KeyCode::Enter => {
                if self.selected_task().is_some() {
                    self.mode = Mode::Detail;
                }
                Action::None
            }
            _ => Action::None,
        }
    }

    fn move_card(&self, dir: isize) -> Action {
        let cols = self.column_ids();
        let target = self.col as isize + dir;
        if target < 0 || target as usize >= cols.len() {
            return Action::None;
        }
        match self.selected_task() {
            Some(task) => Action::Send(Intent::MoveCard {
                task,
                to_column: cols[target as usize].clone(),
                position: None,
            }),
            None => Action::None,
        }
    }

    fn reorder(&self, dir: isize) -> Action {
        let new = self.row as isize + dir;
        let n = self.visible_cards(self.col).len() as isize;
        if new < 0 || new >= n {
            return Action::None;
        }
        match self.selected_task() {
            Some(task) => Action::Send(Intent::ReorderCard {
                task,
                position: new as usize,
            }),
            None => Action::None,
        }
    }

    fn on_add(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.input.clear();
                Action::None
            }
            KeyCode::Enter => {
                let title = std::mem::take(&mut self.input);
                self.mode = Mode::Normal;
                if title.is_empty() {
                    return Action::None;
                }
                let column = self.columns()[self.col].id.clone();
                Action::Send(Intent::CreateTask {
                    title,
                    summary: String::new(),
                    column,
                })
            }
            KeyCode::Backspace => {
                self.input.pop();
                Action::None
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                Action::None
            }
            _ => Action::None,
        }
    }

    fn on_edit(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.editing = None;
                self.input.clear();
                Action::None
            }
            KeyCode::Enter => {
                let title = std::mem::take(&mut self.input);
                let task = self.editing.take();
                self.mode = Mode::Normal;
                if title.is_empty() {
                    return Action::None;
                }
                match task {
                    Some(task) => Action::Send(Intent::EditTask {
                        task,
                        title: Some(title),
                        summary: None,
                    }),
                    None => Action::None,
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
                Action::None
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                Action::None
            }
            _ => Action::None,
        }
    }

    fn on_search(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.filter.clear();
                self.clamp();
                Action::None
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.clamp();
                Action::None
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.clamp();
                Action::None
            }
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.clamp();
                Action::None
            }
            _ => Action::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::proto::Intent;
    use crate::model::TaskId;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use crate::tui::client::Snapshot;

    fn key(c: char) -> KeyEvent { KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE) }

    // snapshot: inbox has task-0001, task-0002; other columns empty
    fn snap() -> Snapshot {
        use crate::controller::{store, apply::apply};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "First".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
        apply(&root, Intent::CreateTask { title: "Second".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
        Snapshot { board: store::load_board(&root).unwrap(), tasks: store::load_all_tasks(&root).unwrap(), sessions: vec![] }
    }

    #[test]
    fn session_for_finds_matching_session() {
        use crate::model::proto::SessionView;
        use crate::model::Phase;
        let mut s = snap(); // existing helper: inbox has task-0001, task-0002
        s.sessions = vec![SessionView { task: TaskId::new(1), session_name: "kanban-task-0001".into(), phase: Phase::WaitingHuman, needs_human_input: true }];
        let app = App::new(s);
        assert_eq!(app.session_for(TaskId::new(1)).unwrap().phase, Phase::WaitingHuman);
        assert!(app.session_for(TaskId::new(2)).is_none());
    }

    #[test]
    fn j_k_move_card_selection() {
        let mut app = App::new(snap());
        assert_eq!(app.selected_row(), 0);
        app.on_key(key('j'));
        assert_eq!(app.selected_row(), 1);
        app.on_key(key('k'));
        assert_eq!(app.selected_row(), 0);
    }

    #[test]
    fn l_h_change_columns_and_clamp_row() {
        let mut app = App::new(snap());
        app.on_key(key('j')); // row 1 in inbox
        let before = app.selected_col();
        app.on_key(key('l')); // move to ready (empty)
        assert!(app.selected_col() > before);
        assert_eq!(app.selected_row(), 0); // clamps in empty column
    }

    #[test]
    fn q_quits() {
        let mut app = App::new(snap());
        assert_eq!(app.on_key(key('q')), Action::Quit);
    }

    #[test]
    fn shift_l_emits_move_card_intent() {
        let mut app = App::new(snap()); // selected task-0001 in inbox (col 0)
        let action = app.on_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        match action {
            Action::Send(Intent::MoveCard { task, to_column, .. }) => {
                assert_eq!(task, TaskId::new(1));
                assert_eq!(to_column.as_str(), "ready");
            }
            o => panic!("expected MoveCard, got {o:?}"),
        }
    }

    #[test]
    fn d_emits_archive_intent() {
        let mut app = App::new(snap());
        assert_eq!(app.on_key(key('d')), Action::Send(Intent::ArchiveTask { task: TaskId::new(1) }));
    }

    #[test]
    fn c_emits_handoff_intent() {
        let mut app = App::new(snap());
        let action = app.on_key(key('c'));
        assert_eq!(action, Action::Send(Intent::Handoff { task: TaskId::new(1), worker: "claude".into() }));
    }

    #[test]
    fn add_task_flow_emits_create_intent() {
        let mut app = App::new(snap());
        app.on_key(key('a'));
        assert!(matches!(app.mode(), Mode::AddTask));
        app.on_key(key('H')); app.on_key(key('i')); // type "Hi"
        let action = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            Action::Send(Intent::CreateTask { title, column, .. }) => {
                assert_eq!(title, "Hi");
                assert_eq!(column.as_str(), "inbox");
            }
            o => panic!("expected CreateTask, got {o:?}"),
        }
        assert!(matches!(app.mode(), Mode::Normal));
    }

    #[test]
    fn t_opens_terminal_when_session_present() {
        use crate::model::proto::SessionView;
        use crate::model::Phase;
        let mut s = snap();
        s.sessions = vec![SessionView { task: TaskId::new(1), session_name: "kanban-task-0001".into(), phase: Phase::Working, needs_human_input: false }];
        let mut app = App::new(s);
        assert_eq!(app.on_key(key('t')), Action::OpenTerminal("kanban-task-0001".into()));
    }

    #[test]
    fn t_without_session_is_noop() {
        let mut app = App::new(snap()); // no sessions
        assert_eq!(app.on_key(key('t')), Action::None);
    }

    #[test]
    fn shift_t_attaches_fullscreen_when_session_present() {
        use crate::model::proto::SessionView;
        use crate::model::Phase;
        let mut s = snap();
        s.sessions = vec![SessionView { task: TaskId::new(1), session_name: "kanban-task-0001".into(), phase: Phase::Working, needs_human_input: false }];
        let mut app = App::new(s);
        assert_eq!(app.on_key(key('T')), Action::AttachFullscreen("kanban-task-0001".into()));
    }

    #[test]
    fn shift_t_without_session_is_noop() {
        let mut app = App::new(snap()); // no sessions
        assert_eq!(app.on_key(key('T')), Action::None);
    }

    #[test]
    fn enter_and_exit_terminal_toggle_mode() {
        let mut app = App::new(snap());
        app.enter_terminal();
        assert!(matches!(app.mode(), Mode::Terminal));
        app.exit_terminal();
        assert!(matches!(app.mode(), Mode::Normal));
    }

    #[test]
    fn esc_cancels_add() {
        let mut app = App::new(snap());
        app.on_key(key('a'));
        app.on_key(key('x'));
        let action = app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, Action::None);
        assert!(matches!(app.mode(), Mode::Normal));
    }

    #[test]
    fn e_edits_selected_task_title() {
        let mut app = App::new(snap()); // task-0001 title "First" selected
        app.on_key(key('e'));
        assert!(matches!(app.mode(), Mode::EditTask));
        // modal pre-filled with current title -> backspace it out, type "New"
        for _ in 0.."First".len() { app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)); }
        app.on_key(key('N')); app.on_key(key('e')); app.on_key(key('w'));
        let action = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, Action::Send(Intent::EditTask { task: TaskId::new(1), title: Some("New".into()), summary: None }));
        assert!(matches!(app.mode(), Mode::Normal));
    }

    #[test]
    fn slash_filters_cards_by_title() {
        let mut app = App::new(snap()); // inbox: "First"(1), "Second"(2)
        app.on_key(key('/'));
        assert!(matches!(app.mode(), Mode::Search));
        app.on_key(key('S')); app.on_key(key('e'));
        assert_eq!(app.visible_cards(0), vec![TaskId::new(2)]); // col 0 = inbox; only "Second" matches "Se"
    }

    #[test]
    fn empty_filter_shows_all_cards() {
        let app = App::new(snap());
        assert_eq!(app.visible_cards(0), app.column_cards(0)); // no filter == all
    }

    #[test]
    fn enter_opens_detail_then_closes() {
        let mut app = App::new(snap());
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.mode(), Mode::Detail));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode(), Mode::Normal));
    }
}
