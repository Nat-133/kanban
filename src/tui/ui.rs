// rendering — Task 4

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::model::TaskId;
use crate::tui::app::{App, Mode};

/// Render the full board: one bordered column per board column, a footer key
/// hint, and (when active) a centered add-task or help overlay.
pub fn render(f: &mut Frame, app: &App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());
    let board_area = outer[0];
    let footer_area = outer[1];

    let columns = app.columns();
    let n = columns.len();
    if n > 0 {
        let constraints: Vec<Constraint> =
            (0..n).map(|_| Constraint::Ratio(1, n as u32)).collect();
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(board_area);

        for (i, column) in columns.iter().enumerate() {
            let selected_col = app.selected_col() == i;
            let border_style = if selected_col {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(column.title.clone());

            let items: Vec<ListItem> = app
                .visible_cards(i)
                .iter()
                .enumerate()
                .map(|(row, task)| {
                    let mut label = card_title(app, *task);
                    let mut style = Style::default();
                    if let Some(sv) = app.session_for(*task) {
                        label = format!("{label} [{}]", phase_badge(sv.phase));
                        if sv.needs_human_input {
                            style = Style::default()
                                .fg(Color::Red)
                                .add_modifier(Modifier::BOLD);
                        }
                    }
                    if selected_col && app.selected_row() == row {
                        style = style.add_modifier(Modifier::REVERSED);
                    }
                    ListItem::new(label).style(style)
                })
                .collect();

            let list = List::new(items).block(block);
            f.render_widget(list, chunks[i]);
        }
    }

    let footer = if matches!(app.mode(), Mode::Search) || !app.filter().is_empty() {
        Paragraph::new(format!("/{}", app.filter())).style(Style::default().fg(Color::Yellow))
    } else {
        Paragraph::new(
            "h/l/j/k move · H/L move card · J/K reorder · a add · e edit · / search · c hand off · d archive · ? help · q quit",
        )
        .style(Style::default().fg(Color::DarkGray))
    };
    f.render_widget(footer, footer_area);

    match app.mode() {
        Mode::AddTask => {
            let area = centered_rect(60, 3, f.area());
            let block = Block::default()
                .borders(Borders::ALL)
                .title("New task")
                .border_style(Style::default().fg(Color::Yellow));
            let para =
                Paragraph::new(format!("New task: {}", app.input())).block(block);
            f.render_widget(Clear, area);
            f.render_widget(para, area);
        }
        Mode::EditTask => {
            let area = centered_rect(60, 3, f.area());
            let block = Block::default()
                .borders(Borders::ALL)
                .title("Edit task")
                .border_style(Style::default().fg(Color::Yellow));
            let para =
                Paragraph::new(format!("Edit title: {}", app.input())).block(block);
            f.render_widget(Clear, area);
            f.render_widget(para, area);
        }
        Mode::Help => {
            let lines = [
                "Keys:",
                "  h / l        move selection between columns",
                "  j / k        move selection between cards",
                "  H / L        move card to prev / next column",
                "  J / K        reorder card down / up",
                "  a            add task",
                "  c            hand off selected task",
                "  d            archive selected task",
                "  ?            toggle this help",
                "  q            quit",
                "",
                "Press any key to close.",
            ];
            let height = lines.len() as u16 + 2;
            let area = centered_rect(50, height, f.area());
            let block = Block::default()
                .borders(Borders::ALL)
                .title("Help")
                .border_style(Style::default().fg(Color::Yellow));
            let para = Paragraph::new(lines.join("\n")).block(block);
            f.render_widget(Clear, area);
            f.render_widget(para, area);
        }
        Mode::Normal | Mode::Search => {}
    }
}

/// Short badge text for a worker session's phase, shown next to a card title.
fn phase_badge(phase: crate::model::Phase) -> &'static str {
    use crate::model::Phase;
    match phase {
        Phase::Working => "working",
        Phase::WaitingHuman => "blocked",
        Phase::Idle => "idle",
        Phase::Completed => "done",
        Phase::Failed => "failed",
        Phase::Pending => "pending",
    }
}

/// Look up a card's display title from the snapshot, falling back to the id.
fn card_title(app: &App, task: TaskId) -> String {
    let name = task.to_string();
    app.snapshot()
        .tasks
        .iter()
        .find(|t| t.metadata.name == name)
        .map(|t| t.spec.title.clone())
        .unwrap_or(name)
}

/// A `Rect` of the given width (as a percentage) and absolute height, centered
/// inside `area`.
fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = area.width.saturating_mul(percent_x) / 100;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let height = height.min(area.height);
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width, height }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use ratatui::{backend::TestBackend, Terminal};

    fn snap() -> crate::tui::client::Snapshot {
        use crate::controller::{store, apply::apply};
        use crate::model::proto::Intent;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { title: "Buy milk".into(), summary: "".into(), column: "inbox".parse().unwrap() }).unwrap();
        crate::tui::client::Snapshot { board: store::load_board(&root).unwrap(), tasks: store::load_all_tasks(&root).unwrap(), sessions: vec![] }
    }

    #[test]
    fn renders_columns_and_card_titles() {
        let app = App::new(snap());
        let backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Inbox"), "missing column title Inbox");
        assert!(text.contains("Doing"), "missing column title Doing");
        assert!(text.contains("Buy milk"), "missing card title");
    }

    #[test]
    fn renders_worker_state_badge() {
        use crate::model::proto::SessionView;
        use crate::model::{Phase, TaskId};
        let mut s = snap(); // existing ui-test helper with a card "Buy milk" (task-0001) in inbox
        s.sessions = vec![SessionView { task: TaskId::new(1), session_name: "kanban-task-0001".into(), phase: Phase::WaitingHuman, needs_human_input: true }];
        let app = App::new(s);
        let backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app)).unwrap();
        let text: String = terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Buy milk"));
        assert!(text.contains("blocked")); // badge text for WaitingHuman
    }
}
