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
    }
}
