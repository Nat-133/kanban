use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "kanban")]
struct Cli {
    /// Workspace root (the .kanban directory).
    #[arg(long, default_value = ".kanban", global = true)]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the workspace layout and a default board.
    Init,
    /// Run the controller daemon.
    Daemon {
        #[arg(long, default_value = "127.0.0.1:7777")]
        addr: SocketAddr,
    },
    /// Launch the terminal UI (connects to a running daemon).
    Tui {
        #[arg(long, default_value = "http://127.0.0.1:7777")]
        daemon: String,
    },
    /// Show the human-involvement activity log and summary counts.
    Activity,
    /// Internal: supervise an agent process and report its state.
    Agent {
        /// The task id whose session this agent belongs to.
        #[arg(long)]
        session: String,
        /// The agent command, after `--`.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Internal: called by Claude Code hooks to record a worker event.
    Hook {
        /// The event name (e.g. notification, stop, session-start).
        event: String,
        /// The task id whose session this hook belongs to.
        #[arg(long)]
        session: String,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Init => {
            kanban::controller::store::init_workspace(&cli.root)?;
            println!("initialized workspace at {}", cli.root.display());
            Ok(())
        }
        Command::Daemon { addr } => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            rt.block_on(kanban::controller::server::serve(cli.root, addr))
        }
        Command::Tui { daemon } => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
            rt.block_on(kanban::tui::run::run(daemon))
        }
        Command::Activity => {
            use kanban::controller::activity::ActivityKind;
            let evs = kanban::controller::activity::load(&cli.root)?;
            for e in &evs {
                let kind = match &e.kind {
                    ActivityKind::Interruption { reason } => format!("interruption ({reason:?})"),
                    ActivityKind::Steer => "steer".to_string(),
                    ActivityKind::ProfileChanged { from, to } => {
                        format!("profile-changed {} -> {to}", from.as_deref().unwrap_or("(none)"))
                    }
                };
                println!("{}  {}  {}  {kind}", e.observed_at, e.task, e.profile);
            }
            let summary = kanban::controller::activity::summary(&evs);
            println!("\ninterruptions per task:");
            if summary.interruptions_per_task.is_empty() {
                println!("  (none)");
            }
            for (task, count) in &summary.interruptions_per_task {
                println!("  {task}: {count}");
            }
            Ok(())
        }
        Command::Agent { session, command } => {
            let id: kanban::model::TaskId = session.parse()
                .map_err(|_| anyhow::anyhow!("invalid --session task id: {session}"))?;
            let code = kanban::agent::supervise::run(&cli.root, id, &command)?;
            std::process::exit(code);
        }
        Command::Hook { event, session } => {
            use std::io::Read;
            let id: kanban::model::TaskId = session.parse()
                .map_err(|_| anyhow::anyhow!("invalid --session task id: {session}"))?;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).ok();
            // No doorbell: the agent owns state and pokes when it changes. A hook
            // only records the activity log and the session metadata, neither of
            // which the board renders.
            kanban::controller::events::record_hook(&cli.root, id, &event, &buf)
        }
    }
}
