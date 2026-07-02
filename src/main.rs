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
        Command::Hook { event, session } => {
            use std::io::Read;
            let id: kanban::model::TaskId = session.parse()
                .map_err(|_| anyhow::anyhow!("invalid --session task id: {session}"))?;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).ok();
            // Write the durable state first; only ring the daemon's doorbell if this
            // firing actually changed the state. A failed poke is fine — the daemon's
            // backstop reconcile will pick the change up on its next pass.
            let changed = kanban::controller::events::record_state(&cli.root, id, &event, &buf)?;
            if changed {
                // Ring the daemon's doorbell. Always resolve an address (recorded,
                // else default) so a missing daemon.addr never silently disables the
                // poke. If no daemon is up it fails harmlessly and the reconcile
                // backstop covers it.
                let addr = kanban::controller::server::daemon_addr(&cli.root);
                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                let _ = rt.block_on(kanban::controller::server::poke(&addr, id));
            }
            Ok(())
        }
    }
}
