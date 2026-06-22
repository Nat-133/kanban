// async event loop — Task 5

use crate::tui::app::{Action, App};
use crate::tui::client::Client;
use crossterm::event::KeyEventKind;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::Stdout;

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

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    base: String,
) -> anyhow::Result<()> {
    let client = Client::new(base.clone());
    let snap = client.snapshot().await?;
    let mut app = App::new(snap);

    let mut sse = reqwest_eventsource::EventSource::get(format!("{base}/v1/events"));
    let mut input = crossterm::event::EventStream::new();

    loop {
        terminal.draw(|f| crate::tui::ui::render(f, &app))?;

        tokio::select! {
            maybe = input.next() => {
                match maybe {
                    Some(Ok(crossterm::event::Event::Key(key))) => {
                        if key.kind == KeyEventKind::Press {
                            match app.on_key(key) {
                                Action::Quit => break,
                                Action::Send(intent) => {
                                    match client.send(intent).await {
                                        Ok(_) => match client.snapshot().await {
                                            Ok(s) => app.set_snapshot(s),
                                            Err(e) => app.status = Some(e.to_string()),
                                        },
                                        Err(e) => app.status = Some(e.to_string()),
                                    }
                                }
                                Action::Attach(name) => {
                                    // Suspend the TUI, attach to the tmux session (blocking),
                                    // then restore the terminal and refresh. Clearing $TMUX
                                    // lets `attach` work when the TUI itself runs inside tmux
                                    // (otherwise tmux refuses to nest); the worker just opens
                                    // as a nested client in this pane.
                                    let _ = disable_raw_mode();
                                    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
                                    let _ = std::process::Command::new("tmux")
                                        .arg("attach")
                                        .arg("-t")
                                        .arg(&name)
                                        .env_remove("TMUX")
                                        .status();
                                    let _ = enable_raw_mode();
                                    let _ = execute!(terminal.backend_mut(), EnterAlternateScreen);
                                    let _ = terminal.clear();
                                    if let Ok(s) = client.snapshot().await {
                                        app.set_snapshot(s);
                                    }
                                }
                                Action::None => {}
                            }
                        }
                    }
                    // Non-key terminal events (resize, mouse, etc.) — redraw.
                    Some(Ok(_)) => {}
                    // Input stream ended or errored — exit.
                    Some(Err(_)) | None => break,
                }
            }
            ev = sse.next() => {
                match ev {
                    Some(Ok(reqwest_eventsource::Event::Message(_))) => {
                        if let Ok(s) = client.snapshot().await {
                            app.set_snapshot(s);
                        }
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
