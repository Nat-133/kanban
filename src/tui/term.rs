//! Embedded terminal popup: spawns a tmux session inside a PTY and renders it
//! as a `ratatui` overlay (see Phase 0 walking skeleton).

use std::io::{Read, Write};
use std::sync::{Arc, RwLock};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Clear};
use ratatui::Frame;
use tokio::sync::mpsc::UnboundedSender;
use tui_term::vt100::Parser;
use tui_term::widget::PseudoTerminal;

/// A command running inside a pseudo-terminal whose output is parsed into a
/// shared `vt100` screen. A background thread drains the PTY into the parser so
/// the render loop can snapshot the screen each frame.
pub struct TermSession {
    /// The tmux session name, used by the full-screen fallback. Empty for
    /// sessions spawned directly via [`TermSession::spawn`].
    name: String,
    parser: Arc<RwLock<Parser>>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    size: (u16, u16),
    /// Whether the `Ctrl+G` prefix is currently armed (awaiting a command key).
    pub prefix_armed: bool,
}

impl TermSession {
    /// Attach to the tmux session `name` inside a PTY. `$TMUX` is removed so
    /// the attach works even when the kanban TUI itself runs inside tmux.
    pub fn attach(
        name: &str,
        rows: u16,
        cols: u16,
        redraw: UnboundedSender<()>,
    ) -> anyhow::Result<Self> {
        let argv = tmux_attach_argv(name);
        let mut cmd = CommandBuilder::new(&argv[0]);
        cmd.args(&argv[1..]);
        cmd.env_remove("TMUX");
        Self::build(cmd, name, rows, cols, redraw)
    }

    /// Spawn an arbitrary command in a fresh PTY. Primarily for tests; real use
    /// goes through [`TermSession::attach`].
    pub fn spawn(
        cmd: CommandBuilder,
        rows: u16,
        cols: u16,
        redraw: UnboundedSender<()>,
    ) -> anyhow::Result<Self> {
        Self::build(cmd, "", rows, cols, redraw)
    }

    fn build(
        cmd: CommandBuilder,
        name: &str,
        rows: u16,
        cols: u16,
        redraw: UnboundedSender<()>,
    ) -> anyhow::Result<Self> {
        let pair = native_pty_system().openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let child = pair.slave.spawn_command(cmd)?;
        // Drop the slave so the master sees EOF once the child exits.
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let parser = Arc::new(RwLock::new(Parser::new(rows, cols, 0)));

        let sink = Arc::clone(&parser);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut p) = sink.write() {
                            p.process(&buf[..n]);
                        }
                        // Wake the render loop; ignore a closed receiver.
                        let _ = redraw.send(());
                    }
                }
            }
        });

        Ok(Self {
            name: name.to_string(),
            parser,
            writer,
            child,
            master: pair.master,
            size: (rows, cols),
            prefix_armed: false,
        })
    }

    /// The shared parser, for rendering the screen each frame.
    pub fn parser(&self) -> &Arc<RwLock<Parser>> {
        &self.parser
    }

    /// The tmux session name (empty if spawned directly).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Write bytes (typically encoded keystrokes) to the PTY.
    pub fn write_input(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    /// Whether the child process is still running.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Resize the PTY and the parser's screen to `rows`×`cols`.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        if (rows, cols) == self.size {
            return;
        }
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.write() {
            p.screen_mut().set_size(rows, cols);
        }
        self.size = (rows, cols);
    }

    /// The current visible screen contents as plain text.
    pub fn screen_text(&self) -> String {
        self.parser.read().unwrap().screen().contents()
    }
}

impl Drop for TermSession {
    fn drop(&mut self) {
        // Killing the attach client detaches it; the tmux session persists.
        let _ = self.child.kill();
    }
}

/// A `Rect` sized to `pct_x`% of `area`'s width and `pct_y`% of its height,
/// centered inside `area`. Used to size the terminal popup (90×90).
pub fn centered_rect_pct(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let width = area.width.saturating_mul(pct_x) / 100;
    let height = area.height.saturating_mul(pct_y) / 100;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width, height }
}

/// Draw the embedded terminal as a centered 90×90 popup over `area`: clears the
/// background, then renders `screen` inside a bordered block titled `title`.
pub fn render_terminal_popup(f: &mut Frame, area: Rect, screen: &tui_term::vt100::Screen, title: &str) {
    let popup = centered_rect_pct(90, 90, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} — Ctrl+G q to close "));
    let term = PseudoTerminal::new(screen).block(block);
    f.render_widget(Clear, popup);
    f.render_widget(term, popup);
}

/// What the terminal popup should do with a key (after `Ctrl+G` prefix logic).
#[derive(Debug, Clone, PartialEq)]
pub enum TermAction {
    /// Write these bytes to the PTY.
    Forward(Vec<u8>),
    /// Close the popup and return to the board.
    Close,
    /// Detach the popup and re-attach full-screen (the fallback path).
    Fullscreen,
    /// Swallow the key (e.g. it just armed the prefix, or was unrecognised).
    None,
}

/// Translate a key event into the bytes to write to the PTY, or `None` for keys
/// we don't forward.
pub fn encode_key(key: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        // Ctrl-<letter> → the corresponding control byte (Ctrl-A = 0x01, …).
        KeyCode::Char(c) if ctrl => Some(vec![(c as u8).to_ascii_lowercase() & 0x1f]),
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            Some(c.encode_utf8(&mut buf).as_bytes().to_vec())
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        _ => None,
    }
}

/// Apply the `Ctrl+G` prefix state machine. Given whether the prefix is armed
/// and the next key, return the new armed state and the action to take.
pub fn handle_prefixed_key(armed: bool, key: KeyEvent) -> (bool, TermAction) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let is_prefix = ctrl && matches!(key.code, KeyCode::Char('g'));

    if armed {
        // The prefix is armed: interpret the next key as a popup command.
        match key.code {
            // Doubling the prefix sends a literal Ctrl+G to the inner session.
            KeyCode::Char('g') if ctrl => (false, TermAction::Forward(vec![0x07])),
            KeyCode::Char('q') | KeyCode::Esc => (false, TermAction::Close),
            KeyCode::Char('T') => (false, TermAction::Fullscreen),
            // Unrecognised command: disarm and swallow.
            _ => (false, TermAction::None),
        }
    } else if is_prefix {
        // Arm the prefix and swallow this key.
        (true, TermAction::None)
    } else {
        // Ordinary key: forward to the PTY if we know how to encode it.
        match encode_key(key) {
            Some(bytes) => (false, TermAction::Forward(bytes)),
            None => (false, TermAction::None),
        }
    }
}

/// Inner PTY dimensions `(rows, cols)` for a 90×90 popup over a `width`×`height`
/// terminal, accounting for the 1-cell border on each side.
pub fn popup_pty_size(width: u16, height: u16) -> (u16, u16) {
    let popup = centered_rect_pct(90, 90, Rect::new(0, 0, width, height));
    let rows = popup.height.saturating_sub(2).max(1);
    let cols = popup.width.saturating_sub(2).max(1);
    (rows, cols)
}

/// The argv for attaching to a tmux session by name.
pub fn tmux_attach_argv(session: &str) -> Vec<String> {
    vec![
        "tmux".to_string(),
        "attach".to_string(),
        "-t".to_string(),
        session.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn encode_plain_char_is_its_byte() {
        assert_eq!(encode_key(key('a')), Some(vec![b'a']));
    }

    #[test]
    fn encode_enter_is_carriage_return() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(vec![b'\r'])
        );
    }

    #[test]
    fn encode_ctrl_c_is_etx() {
        assert_eq!(encode_key(ctrl('c')), Some(vec![0x03]));
    }

    #[test]
    fn encode_up_arrow_is_escape_sequence() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            Some(b"\x1b[A".to_vec())
        );
    }

    #[test]
    fn prefix_arms_on_ctrl_g_and_swallows() {
        let (armed, act) = handle_prefixed_key(false, ctrl('g'));
        assert!(armed);
        assert_eq!(act, TermAction::None);
    }

    #[test]
    fn unprefixed_key_is_forwarded() {
        let (armed, act) = handle_prefixed_key(false, key('x'));
        assert!(!armed);
        assert_eq!(act, TermAction::Forward(vec![b'x']));
    }

    #[test]
    fn prefix_then_q_closes() {
        let (armed, act) = handle_prefixed_key(true, key('q'));
        assert!(!armed);
        assert_eq!(act, TermAction::Close);
    }

    #[test]
    fn prefix_then_capital_t_goes_fullscreen() {
        let (_, act) = handle_prefixed_key(true, key('T'));
        assert_eq!(act, TermAction::Fullscreen);
    }

    #[test]
    fn prefix_then_ctrl_g_sends_literal_bel() {
        let (armed, act) = handle_prefixed_key(true, ctrl('g'));
        assert!(!armed);
        assert_eq!(act, TermAction::Forward(vec![0x07]));
    }

    #[test]
    fn popup_pty_size_subtracts_the_border() {
        // 100×100 terminal → 90×90 popup → 88×88 inner area.
        assert_eq!(popup_pty_size(100, 100), (88, 88));
    }

    #[test]
    fn tmux_attach_argv_targets_named_session() {
        assert_eq!(
            tmux_attach_argv("kanban-task-0001"),
            vec!["tmux", "attach", "-t", "kanban-task-0001"]
        );
    }

    #[test]
    fn centered_rect_pct_90_in_100x100_is_inset_by_5() {
        let area = Rect::new(0, 0, 100, 100);
        let r = centered_rect_pct(90, 90, area);
        assert_eq!(r, Rect::new(5, 5, 90, 90));
    }

    #[test]
    fn popup_renders_screen_contents_and_titled_border() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut parser = tui_term::vt100::Parser::new(24, 80, 0);
        parser.process(b"hello-pty");

        let backend = TestBackend::new(100, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_terminal_popup(f, f.area(), parser.screen(), "kanban-task-0001"))
            .unwrap();

        let text: String = terminal.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("hello-pty"), "popup should display PTY output");
        assert!(text.contains("kanban-task-0001"), "popup border should show the session title");
    }

    fn sh(script: &str) -> portable_pty::CommandBuilder {
        let mut cmd = portable_pty::CommandBuilder::new("sh");
        cmd.arg("-c");
        cmd.arg(script);
        cmd
    }

    /// Poll `cond` up to ~2s, returning whether it became true.
    fn eventually(mut cond: impl FnMut() -> bool) -> bool {
        for _ in 0..100 {
            if cond() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn spawn_captures_command_output_into_screen() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let session = TermSession::spawn(sh("printf hello-from-pty"), 24, 80, tx).unwrap();
        assert!(
            eventually(|| session.screen_text().contains("hello-from-pty")),
            "PTY command output was not captured into the vt100 screen"
        );
    }

    #[test]
    fn spawn_signals_redraw_when_output_arrives() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _session = TermSession::spawn(sh("printf hi"), 24, 80, tx).unwrap();
        assert!(
            eventually(|| rx.try_recv().is_ok()),
            "reader thread should signal the redraw channel on output"
        );
    }

    #[test]
    fn write_input_reaches_the_command() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        // Read a line, then echo it back with a marker prefix.
        let mut session =
            TermSession::spawn(sh("read x; printf 'got:%s' \"$x\""), 24, 80, tx).unwrap();
        session.write_input(b"hello\n").unwrap();
        assert!(
            eventually(|| session.screen_text().contains("got:hello")),
            "input written to the PTY should reach the command"
        );
    }

    #[test]
    fn is_alive_becomes_false_after_command_exits() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = TermSession::spawn(sh("exit 0"), 24, 80, tx).unwrap();
        assert!(
            eventually(|| !session.is_alive()),
            "is_alive should report false once the command has exited"
        );
    }

    #[test]
    fn resize_updates_the_screen_dimensions() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = TermSession::spawn(sh("sleep 5"), 24, 80, tx).unwrap();
        session.resize(10, 40);
        assert_eq!(session.parser().read().unwrap().screen().size(), (10, 40));
    }
}
