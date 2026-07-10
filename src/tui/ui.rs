// rendering — Task 4

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::model::{Phase, TaskId};
use crate::tui::app::{App, Mode};

/// `Working` spinner frames — one entry per render tick. A braille "comet": one
/// dot at the start, a tail that grows behind the rotating head to four dots,
/// then unwinds back to one. Baked from a 1-revolution / tail-4 envelope with
/// eased timing (slow at the ends, fast through the middle, via repeated frames),
/// so rendering is a plain table index — no per-frame math.
const SPINNER: [char; 16] = [
    '⠁', '⠁', '⠁', '⠉', '⠉', '⠙', '⠙', '⠹',
    '⢹', '⣰', '⣰', '⣄', '⣄', '⠆', '⠆', '⠆',
];

/// Shown when a worker needs a human: idle, waiting on a permission prompt, or failed.
const WARNING: char = '\u{f071}'; // nf-fa-warning

fn spinner_frame(tick: u64) -> char {
    SPINNER[(tick % SPINNER.len() as u64) as usize]
}

/// Render the full board: one bordered column per board column, a footer key
/// hint, and (when active) a centered overlay. When the terminal popup is open,
/// `term_screen` carries the live PTY screen to render.
pub fn render(f: &mut Frame, app: &App, term_screen: Option<&tui_term::vt100::Screen>) {
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
                    let mut title = card_title(app, *task);
                    if has_jira(app, *task) {
                        title = format!("{title} [J]");
                    }
                    let selected = selected_col && app.selected_row() == row;
                    // Leading status icon: spinner while working, warning when it
                    // needs a human, nothing otherwise. A fixed two-cell prefix
                    // keeps titles aligned across all cards. The warning is red,
                    // but on the highlighted card REVERSED would turn that red
                    // into a red background, so drop the colour when selected.
                    let icon = match app.session_for(*task).map(|s| s.phase) {
                        Some(Phase::Working) => {
                            Span::raw(format!("{} ", spinner_frame(app.spinner_tick())))
                        }
                        Some(Phase::Idle | Phase::WaitingHuman | Phase::Failed) => {
                            let icon_style = if selected {
                                Style::default()
                            } else {
                                Style::default().fg(Color::Red)
                            };
                            Span::styled(format!("{WARNING} "), icon_style)
                        }
                        _ => Span::raw("  "),
                    };
                    let mut style = Style::default();
                    if selected {
                        style = style.add_modifier(Modifier::REVERSED);
                    }
                    ListItem::new(Line::from(vec![icon, Span::raw(title)])).style(style)
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
            // Combined title/summary box: first line is the title, the body after
            // a blank line is the summary (git commit-message style).
            let height = (f.area().height.saturating_mul(40) / 100).max(6);
            render_editor_overlay(f, app, "New task — title, blank line, then summary".to_string(), 60, height);
        }
        Mode::EditTask => {
            let title = app
                .detail_task()
                .map(|t| format!("Edit task — {}", t.spec.title))
                .unwrap_or_else(|| "Edit task".to_string());
            let height = (f.area().height.saturating_mul(40) / 100).max(6);
            render_editor_overlay(f, app, title, 60, height);
        }
        Mode::Help => {
            let lines = [
                "Keys:",
                "  h / l        move selection between columns",
                "  j / k        move selection between cards",
                "  H / L        move card to prev / next column",
                "  J / K        reorder card down / up",
                "  a            add task",
                "  Enter        open task detail (e edits description)",
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
        Mode::Detail => {
            if let Some(task) = app.detail_task() {
                let spec = &task.spec;
                let mut lines: Vec<String> = Vec::new();
                lines.push(spec.title.clone());
                lines.push(String::new());
                if !spec.summary.is_empty() {
                    lines.push(spec.summary.clone());
                    lines.push(String::new());
                }
                // Long-form description from the task's description.md. Rendered
                // verbatim (Markdown source), one board line per source line.
                if let Some(desc) = app.detail_description().map(str::trim).filter(|d| !d.is_empty()) {
                    lines.push("Description:".to_string());
                    for line in desc.lines() {
                        lines.push(format!("  {line}"));
                    }
                    lines.push(String::new());
                }
                if !spec.acceptance_criteria.is_empty() {
                    lines.push("Acceptance criteria:".to_string());
                    for ac in &spec.acceptance_criteria {
                        lines.push(format!("  - {ac}"));
                    }
                    lines.push(String::new());
                }
                if let Some(repo) = &spec.repo {
                    lines.push(format!("Repo: {repo}"));
                }
                if let Some(key) = &spec.jira.key {
                    lines.push(format!("Jira: {key}"));
                }
                if let Some(url) = &spec.jira.url {
                    lines.push(format!("Jira URL: {url}"));
                }
                lines.push(String::new());
                lines.push("Press e to edit description · Esc / Enter / q to close.".to_string());

                let height = lines.len() as u16 + 2;
                let area = centered_rect(60, height, f.area());
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title("Task detail")
                    .border_style(Style::default().fg(Color::Yellow));
                let para = Paragraph::new(lines.join("\n")).block(block).wrap(Wrap { trim: false });
                f.render_widget(Clear, area);
                f.render_widget(para, area);
            }
        }
        Mode::EditDescription => {
            let title = app
                .detail_task()
                .map(|t| format!("Edit description — {}", t.spec.title))
                .unwrap_or_else(|| "Edit description".to_string());
            // A tall overlay: descriptions are long-form prose.
            let height = (f.area().height.saturating_mul(60) / 100).max(3);
            render_editor_overlay(f, app, title, 70, height);
        }
        Mode::Terminal => {
            if let Some(screen) = term_screen {
                let title = app
                    .selected_task()
                    .and_then(|t| app.session_for(t))
                    .map(|s| s.session_name.clone())
                    .unwrap_or_else(|| "terminal".to_string());
                crate::tui::term::render_terminal_popup(f, f.area(), screen, &title);
            }
        }
        Mode::Normal | Mode::Search => {}
    }
}

/// Whether a task has a Jira key set in the snapshot.
fn has_jira(app: &App, task: TaskId) -> bool {
    let name = task.to_string();
    app.snapshot()
        .tasks
        .iter()
        .find(|t| t.metadata.name == name)
        .map(|t| t.spec.jira.key.is_some())
        .unwrap_or(false)
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

/// Render a centered modal text-editor overlay (add task, edit task, edit
/// description) with a yellow border, a save/cancel hint, and any status message
/// surfaced in red along the bottom border. No-op if no editor is open.
fn render_editor_overlay(f: &mut Frame, app: &App, title: String, percent_x: u16, height: u16) {
    let Some(editor) = app.editor() else { return };
    let area = centered_rect(percent_x, height, f.area());
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom("Ctrl+S save · Esc cancel")
        .border_style(Style::default().fg(Color::Yellow));
    if let Some(status) = &app.status {
        block = block.title_bottom(Span::styled(
            format!(" {status} "),
            Style::default().fg(Color::Red),
        ));
    }
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    f.render_widget(editor, inner);
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
        apply(&root, Intent::CreateTask { text: "Buy milk".into(), column: "todo".parse().unwrap() }).unwrap();
        crate::tui::client::Snapshot { board: store::load_board(&root).unwrap(), tasks: store::load_all_tasks(&root).unwrap(), sessions: vec![], descriptions: Default::default() }
    }

    fn snap_detail() -> crate::tui::client::Snapshot {
        use crate::controller::{store, apply::apply};
        use crate::model::proto::Intent;
        use crate::model::TaskId;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        store::init_workspace(&root).unwrap();
        apply(&root, Intent::CreateTask { text: "Buy milk".into(), column: "todo".parse().unwrap() }).unwrap();
        let mut task = store::load_task(&root, TaskId::new(1)).unwrap();
        task.spec.summary = "from the shop".into();
        store::save_task(&root, &task).unwrap();
        // A description is authored during handoff, not at create time; simulate
        // a task that already has one so the detail overlay has prose to render.
        let mut descriptions = std::collections::BTreeMap::new();
        descriptions.insert(TaskId::new(1), "# Buy milk\n".to_string());
        crate::tui::client::Snapshot { board: store::load_board(&root).unwrap(), tasks: store::load_all_tasks(&root).unwrap(), sessions: vec![], descriptions }
    }

    #[test]
    fn detail_overlay_shows_title_and_summary() {
        let mut app = App::new(snap_detail());
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE));
        let backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app, None)).unwrap();
        let text: String = terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Buy milk"));
        assert!(text.contains("from the shop"));
    }

    #[test]
    fn detail_overlay_shows_description() {
        // the detail overlay must surface the description under a Description
        // section (here the task already has one, as it would post-handoff).
        let mut app = App::new(snap_detail());
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE));
        let backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app, None)).unwrap();
        let text: String = terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Description:"), "detail overlay should label the description");
        // the seeded body is the `# Buy milk` heading line
        assert!(text.contains("# Buy milk"), "detail overlay should render description prose");
    }

    #[test]
    fn edit_description_overlay_renders_editor_with_seeded_text_and_hint() {
        use crate::model::TaskId;
        let mut s = snap_detail(); // task-0001 with a seeded description ("# Buy milk\n")
        s.descriptions.insert(TaskId::new(1), "# Buy milk\nbody text".into());
        let mut app = App::new(s);
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE)); // Detail
        app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('e'), crossterm::event::KeyModifiers::NONE)); // EditDescription
        let backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app, None)).unwrap();
        let text: String = terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("body text"), "editor should show the seeded description");
        assert!(text.contains("Ctrl+S"), "overlay should hint how to save");
    }

    #[test]
    fn renders_columns_and_card_titles() {
        let app = App::new(snap());
        let backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app, None)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Todo"), "missing column title Todo");
        assert!(text.contains("Doing"), "missing column title Doing");
        assert!(text.contains("Buy milk"), "missing card title");
    }

    #[test]
    fn spinner_frame_indexes_table_and_wraps() {
        assert_eq!(spinner_frame(0), '⠁');
        assert_eq!(spinner_frame(7), '⠹');
        assert_eq!(spinner_frame(8), '⢹');
        assert_eq!(spinner_frame(16), spinner_frame(0)); // wraps
    }

    fn render_with_phase(phase: crate::model::Phase) -> String {
        use crate::model::proto::SessionView;
        use crate::model::TaskId;
        let mut s = snap(); // card "Buy milk" (task-0001) in inbox
        s.sessions = vec![SessionView { task: TaskId::new(1), session_name: "kanban-task-0001".into(), phase, needs_human_input: phase.needs_human_input() }];
        let app = App::new(s);
        let backend = TestBackend::new(160, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app, None)).unwrap();
        terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn renders_warning_icon_for_needs_human_phases() {
        for phase in [crate::model::Phase::WaitingHuman, crate::model::Phase::Idle, crate::model::Phase::Failed] {
            let text = render_with_phase(phase);
            assert!(text.contains("Buy milk"), "{phase:?}: title missing");
            assert!(text.contains(WARNING), "{phase:?}: warning icon missing");
        }
    }

    #[test]
    fn renders_spinner_for_working() {
        let text = render_with_phase(crate::model::Phase::Working);
        assert!(text.contains("Buy milk"));
        assert!(text.contains(spinner_frame(0)), "working should show a spinner frame");
        assert!(!text.contains(WARNING), "working must not show the warning icon");
    }

    #[test]
    fn renders_no_status_icon_when_done() {
        let text = render_with_phase(crate::model::Phase::Completed);
        assert!(text.contains("Buy milk"));
        assert!(!text.contains(WARNING), "done must not warn");
        assert!(!text.contains(spinner_frame(8)), "done must not spin"); // ⢹ is spinner-only
    }

    #[test]
    fn terminal_mode_renders_pty_popup_with_session_title() {
        use crate::model::proto::SessionView;
        use crate::model::{Phase, TaskId};
        let mut s = snap();
        s.sessions = vec![SessionView { task: TaskId::new(1), session_name: "kanban-task-0001".into(), phase: Phase::Working, needs_human_input: false }];
        let mut app = App::new(s);
        app.enter_terminal();

        let mut parser = tui_term::vt100::Parser::new(24, 80, 0);
        parser.process(b"live-shell-output");

        let backend = TestBackend::new(160, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, &app, Some(parser.screen()))).unwrap();
        let text: String = terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("live-shell-output"), "should render live PTY screen contents");
        assert!(text.contains("kanban-task-0001"), "popup title should show the session name");
    }
}
