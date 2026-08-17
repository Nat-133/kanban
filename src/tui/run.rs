// async event loop — Task 5

use crate::model::proto::Response;
use crate::tui::app::{Action, App, Mode};
use crate::tui::client::Client;
use crate::tui::term::{
    encode_mouse, handle_prefixed_key, mouse_cell, popup_pty_size, TermAction, TermSession,
};
use crossterm::event::{DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
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

    // Teardown — runs on both Ok and Err. Mouse capture is normally released
    // when the popup closes; disable it again in case we exited with one open.
    set_mouse_capture(false);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Turn the outer terminal's mouse reporting on or off. Capture is held only
/// while the embedded popup is open, so the board keeps the terminal's own
/// click-drag text selection.
fn set_mouse_capture(on: bool) {
    let mut out = std::io::stdout();
    let _ = if on {
        execute!(out, EnableMouseCapture)
    } else {
        execute!(out, DisableMouseCapture)
    };
}

/// What to do after routing a key while the terminal popup is open. Computed
/// under a short borrow of the session so the loop can then mutate `term`.
enum Post {
    Nothing,
    Close,
    Fullscreen(String),
    /// Re-hand off the task behind the popup's session, then re-attach.
    Rehandoff(String),
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
            set_mouse_capture(false);
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
                                    TermAction::Rehandoff => Post::Rehandoff(t.name().to_string()),
                                }
                            } else {
                                Post::Nothing
                            };
                            match post {
                                Post::Nothing => {}
                                Post::Close => {
                                    term = None;
                                    set_mouse_capture(false);
                                    app.exit_terminal();
                                    refresh(&client, &mut app).await;
                                }
                                // Killing the terminal is what a re-handoff does,
                                // so drop the popup first: its PTY is attached to
                                // the session about to die.
                                Post::Rehandoff(name) => {
                                    term = None;
                                    set_mouse_capture(false);
                                    app.exit_terminal();
                                    match app.task_for_session(&name) {
                                        Some(task) => {
                                            match client.send(crate::model::proto::Intent::Rehandoff { task }).await {
                                                Ok(Response::Ok { .. }) => {
                                                    refresh(&client, &mut app).await;
                                                    app.status = Some(format!("re-handed off {task}"));
                                                    // The name is recomputed from
                                                    // config, so re-read it rather
                                                    // than reusing the dead one.
                                                    let name = app
                                                        .session_for(task)
                                                        .map(|s| s.session_name.clone())
                                                        .filter(|n| !n.is_empty());
                                                    if let Some(name) = name {
                                                        open_terminal_popup(&name, terminal, &mut term, &mut app, &redraw_tx);
                                                    }
                                                }
                                                Ok(Response::Error { message }) => app.status = Some(message),
                                                Ok(_) => refresh(&client, &mut app).await,
                                                Err(e) => app.status = Some(e.to_string()),
                                            }
                                        }
                                        None => app.status = Some(format!("no task owns session {name}")),
                                    }
                                }
                                Post::Fullscreen(name) => {
                                    // Close the popup and repaint the board once,
                                    // then hand off to the full-screen attach.
                                    term = None;
                                    set_mouse_capture(false);
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
                                    // A create leaves the cursor on the new card,
                                    // so it can be handed off or edited straight
                                    // away; the id comes back in the reply.
                                    let created = matches!(intent, crate::model::proto::Intent::CreateTask { .. });
                                    // Inspect the controller's reply, not just the
                                    // transport result: a `Response::Error`/`Conflict`
                                    // arrives as `Ok(resp)`, so matching on `Ok(_)`
                                    // would silently treat a rejection as success.
                                    match client.send(intent).await {
                                        Ok(Response::Ok { task }) => {
                                            // A committed description edit closes its
                                            // editor; a no-op for every other intent.
                                            app.close_editor();
                                            refresh(&client, &mut app).await;
                                            if let (true, Some(t)) = (created, task) {
                                                app.select_task(t);
                                            }
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
                                    open_terminal_popup(&name, terminal, &mut term, &mut app, &redraw_tx);
                                }
                                Action::AttachFullscreen(name) => {
                                    fullscreen_attach(terminal, &name);
                                    refresh(&client, &mut app).await;
                                }
                                // Opening a session a crash or shutdown killed:
                                // bring the agent back, then attach the way the
                                // operator asked. Only attach if the relaunch
                                // actually succeeded — attaching after a failed
                                // resume would just fail again, hiding the reason.
                                Action::ResumeAndOpen { task, fullscreen } => {
                                    match client.send(crate::model::proto::Intent::ResumeSession { task }).await {
                                        Ok(Response::Ok { .. }) => {
                                            refresh(&client, &mut app).await;
                                            let name = app
                                                .session_for(task)
                                                .map(|s| s.session_name.clone())
                                                .filter(|n| !n.is_empty());
                                            if let Some(name) = name {
                                                app.status = Some(format!("resumed {task}"));
                                                if fullscreen {
                                                    fullscreen_attach(terminal, &name);
                                                    refresh(&client, &mut app).await;
                                                } else {
                                                    open_terminal_popup(&name, terminal, &mut term, &mut app, &redraw_tx);
                                                }
                                            }
                                        }
                                        Ok(Response::Error { message }) => app.status = Some(message),
                                        Ok(_) => refresh(&client, &mut app).await,
                                        Err(e) => app.status = Some(e.to_string()),
                                    }
                                }
                                Action::None => {}
                            }
                        }
                    }
                    // Mouse — forwarded to the inner session while the popup is
                    // open (tmux with `mouse on` turns the wheel into scrollback).
                    Some(Ok(Event::Mouse(me))) => {
                        if let Some(t) = term.as_mut() {
                            let size = terminal.size()?;
                            if let Some((col, row)) = mouse_cell(size.width, size.height, me.column, me.row) {
                                let (mode, encoding) = {
                                    let p = t.parser().read().unwrap();
                                    let s = p.screen();
                                    (s.mouse_protocol_mode(), s.mouse_protocol_encoding())
                                };
                                if let Some(bytes) = encode_mouse(me, col, row, mode, encoding) {
                                    let _ = t.write_input(&bytes);
                                }
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

/// Attach the embedded terminal popup to the named tmux session, sizing the PTY
/// to the current viewport. Surfaces attach failures to the status line. Shared
/// by the `t` key (Action::OpenTerminal) and the open after a resume.
fn open_terminal_popup(
    name: &str,
    terminal: &Term,
    term: &mut Option<TermSession>,
    app: &mut App,
    redraw_tx: &tokio::sync::mpsc::UnboundedSender<()>,
) {
    let size = match terminal.size() {
        Ok(s) => s,
        Err(e) => {
            app.status = Some(e.to_string());
            return;
        }
    };
    let (rows, cols) = popup_pty_size(size.width, size.height);
    match TermSession::attach(name, rows, cols, redraw_tx.clone()) {
        Ok(t) => {
            *term = Some(t);
            // Take the wheel and clicks off the outer terminal for as long as
            // the popup is up, so they can be forwarded to the inner session.
            set_mouse_capture(true);
            app.enter_terminal();
        }
        Err(e) => app.status = Some(e.to_string()),
    }
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
