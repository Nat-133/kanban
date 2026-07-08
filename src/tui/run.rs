// async event loop — Task 5

use crate::model::proto::Response;
use crate::tui::app::{Action, App, Mode};
use crate::tui::client::Client;
use crate::tui::term::{handle_prefixed_key, popup_pty_size, TermAction, TermSession};
use crossterm::event::{Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use std::io::Stdout;
use std::time::Duration;

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Spinner animation cadence — how often the `Working` spinner advances a frame.
/// Only ticks while a worker is actually working (see `App::any_working`); the
/// loop otherwise blocks on real events, so an idle board costs nothing.
const SPINNER_TICK: Duration = Duration::from_millis(60);

pub async fn run(base: String) -> anyhow::Result<()> {
    // Terminal setup.
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Run the loop, always restoring the terminal afterwards.
    let result = run_loop(&mut terminal, base).await;

    // Teardown — runs on both Ok and Err.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// What to do after routing a key while the terminal popup is open. Computed
/// under a short borrow of the session so the loop can then mutate `term`.
enum Post {
    Nothing,
    Close,
    Fullscreen(String),
}

async fn run_loop(terminal: &mut Term, base: String) -> anyhow::Result<()> {
    let client = Client::new(base.clone());
    let snap = client.snapshot().await?;
    let mut app = App::new(snap);

    let mut sse = reqwest_eventsource::EventSource::get(format!("{base}/v1/events"));
    let mut input = crossterm::event::EventStream::new();

    // The reader thread of an active `TermSession` signals this channel whenever
    // the PTY produces output, waking the select loop to redraw.
    let (redraw_tx, mut redraw_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut term: Option<TermSession> = None;

    loop {
        // Reap a session whose child has exited (e.g. tmux detached/killed).
        if term.as_mut().map(|t| !t.is_alive()).unwrap_or(false) {
            term = None;
            app.exit_terminal();
        }

        // Draw. Hold a read lock on the parser only for the duration of the frame.
        {
            let guard = term.as_ref().map(|t| t.parser().read().unwrap());
            let screen = guard.as_deref().map(|p| p.screen());
            terminal.draw(|f| crate::tui::ui::render(f, &app, screen))?;
        }

        // Animate the spinner only while a worker is working; otherwise this arm
        // parks forever and the loop blocks on real events (no idle redraws).
        let animating = !matches!(app.mode(), Mode::Terminal) && app.any_working();

        tokio::select! {
            _ = async {
                if animating {
                    tokio::time::sleep(SPINNER_TICK).await
                } else {
                    std::future::pending::<()>().await
                }
            } => {
                app.advance_spinner();
            }
            maybe = input.next() => {
                match maybe {
                    Some(Ok(Event::Key(key))) => {
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        if matches!(app.mode(), Mode::Terminal) {
                            // Route the key through the Ctrl+G prefix machine.
                            let post = if let Some(t) = term.as_mut() {
                                let (armed, action) = handle_prefixed_key(t.prefix_armed, key);
                                t.prefix_armed = armed;
                                match action {
                                    TermAction::Forward(bytes) => {
                                        let _ = t.write_input(&bytes);
                                        Post::Nothing
                                    }
                                    TermAction::None => Post::Nothing,
                                    TermAction::Close => Post::Close,
                                    TermAction::Fullscreen => Post::Fullscreen(t.name().to_string()),
                                }
                            } else {
                                Post::Nothing
                            };
                            match post {
                                Post::Nothing => {}
                                Post::Close => {
                                    term = None;
                                    app.exit_terminal();
                                    refresh(&client, &mut app).await;
                                }
                                Post::Fullscreen(name) => {
                                    // Close the popup and repaint the board once,
                                    // then hand off to the full-screen attach.
                                    term = None;
                                    app.exit_terminal();
                                    terminal.draw(|f| crate::tui::ui::render(f, &app, None))?;
                                    fullscreen_attach(terminal, &name);
                                    refresh(&client, &mut app).await;
                                }
                            }
                        } else {
                            match app.on_key(key) {
                                Action::Quit => break,
                                Action::Send(intent) => {
                                    // Inspect the controller's reply, not just the
                                    // transport result: a `Response::Error`/`Conflict`
                                    // arrives as `Ok(resp)`, so matching on `Ok(_)`
                                    // would silently treat a rejection as success.
                                    match client.send(intent).await {
                                        Ok(Response::Ok { .. }) => {
                                            // A committed description edit closes its
                                            // editor; a no-op for every other intent.
                                            app.close_description_editor();
                                            refresh(&client, &mut app).await;
                                        }
                                        Ok(Response::Conflict { current }) => {
                                            app.on_description_conflict(current);
                                        }
                                        Ok(Response::Error { message }) => {
                                            app.status = Some(message);
                                        }
                                        Ok(Response::Snapshot { .. }) => {
                                            refresh(&client, &mut app).await;
                                        }
                                        Err(e) => app.status = Some(e.to_string()),
                                    }
                                }
                                Action::OpenTerminal(name) => {
                                    let size = terminal.size()?;
                                    let (rows, cols) = popup_pty_size(size.width, size.height);
                                    match TermSession::attach(&name, rows, cols, redraw_tx.clone()) {
                                        Ok(t) => {
                                            term = Some(t);
                                            app.enter_terminal();
                                        }
                                        Err(e) => app.status = Some(e.to_string()),
                                    }
                                }
                                Action::AttachFullscreen(name) => {
                                    fullscreen_attach(terminal, &name);
                                    refresh(&client, &mut app).await;
                                }
                                Action::None => {}
                            }
                        }
                    }
                    // Terminal resize — keep the PTY in sync with the popup.
                    Some(Ok(Event::Resize(w, h))) => {
                        if let Some(t) = term.as_mut() {
                            let (rows, cols) = popup_pty_size(w, h);
                            t.resize(rows, cols);
                        }
                    }
                    // Other terminal events — redraw on next loop.
                    Some(Ok(_)) => {}
                    // Input stream ended or errored — exit.
                    Some(Err(_)) | None => break,
                }
            }
            // PTY produced output: drain extra signals, then redraw.
            _ = redraw_rx.recv() => {
                while redraw_rx.try_recv().is_ok() {}
            }
            ev = sse.next() => {
                match ev {
                    Some(Ok(reqwest_eventsource::Event::Message(_))) => {
                        refresh(&client, &mut app).await;
                    }
                    // Open / transient error / stream end — keep the loop alive.
                    Some(Ok(reqwest_eventsource::Event::Open)) => {}
                    Some(Err(_)) | None => {}
                }
            }
        }
    }

    Ok(())
}

/// Refresh the app's snapshot from the controller, surfacing errors to status.
async fn refresh(client: &Client, app: &mut App) {
    match client.snapshot().await {
        Ok(s) => app.set_snapshot(s),
        Err(e) => app.status = Some(e.to_string()),
    }
}

/// Suspend the TUI and attach to a tmux session full-screen (the fallback from
/// the popup, via `Ctrl+G T`). Clearing `$TMUX` lets `attach` work when the TUI
/// itself runs inside tmux; on detach we restore the alternate screen.
fn fullscreen_attach(terminal: &mut Term, name: &str) {
    let _ = disable_raw_mode();
    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
    let _ = std::process::Command::new("tmux")
        .arg("attach")
        .arg("-t")
        .arg(name)
        .env_remove("TMUX")
        .status();
    let _ = enable_raw_mode();
    let _ = execute!(terminal.backend_mut(), EnterAlternateScreen);
    // Force a query-free full repaint. `Terminal::clear()` first queries the
    // cursor position over stdin, which races with the live crossterm
    // EventStream reader and silently no-ops (leaving tmux residue on screen);
    // `resize` clears the viewport and resets the back buffer without touching
    // stdin, so the next draw repaints every cell.
    if let Ok(size) = terminal.size() {
        let _ = terminal.resize(Rect::new(0, 0, size.width, size.height));
    }
}
