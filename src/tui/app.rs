// app state + key handling — Task 3

use crate::model::proto::Intent;
use crate::model::{Column, ColumnId, TaskId};
use crate::tui::client::Snapshot;
use crossterm::event::{KeyCode, KeyEvent};

/// The result of handling a key: nothing, quit the loop, or send an intent.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Quit,
    Send(Intent),
}

/// The current input mode of the app.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    AddTask,
    Help,
}

/// Pure TUI state: holds the latest snapshot, the cursor (column/row), the
/// input mode, and any in-progress text input. No terminal or I/O.
pub struct App {
    snapshot: Snapshot,
    col: usize,
    row: usize,
    mode: Mode,
    input: String,
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

    pub fn selected_task(&self) -> Option<TaskId> {
        self.column_cards(self.col).get(self.row).copied()
    }

    fn clamp(&mut self) {
        let ncols = self.columns().len();
        if ncols == 0 {
            self.col = 0;
            self.row = 0;
            return;
        }
        self.col = self.col.min(ncols - 1);
        let n = self.column_cards(self.col).len();
        self.row = if n == 0 { 0 } else { self.row.min(n - 1) };
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        match self.mode {
            Mode::Normal => self.on_normal(key),
            Mode::AddTask => self.on_add(key),
            Mode::Help => {
                self.mode = Mode::Normal;
                Action::None
            }
        }
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
                let n = self.column_cards(self.col).len();
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
            KeyCode::Char('a') => {
                self.mode = Mode::AddTask;
                self.input.clear();
                Action::None
            }
            KeyCode::Char('?') => {
                self.mode = Mode::Help;
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
        let n = self.column_cards(self.col).len() as isize;
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
    fn esc_cancels_add() {
        let mut app = App::new(snap());
        app.on_key(key('a'));
        app.on_key(key('x'));
        let action = app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, Action::None);
        assert!(matches!(app.mode(), Mode::Normal));
    }
}
