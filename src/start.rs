use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const READY_TIMEOUT: Duration = Duration::from_secs(10);
const READY_POLL: Duration = Duration::from_millis(50);

fn log_path(root: &Path) -> PathBuf {
    root.join("daemon.log")
}

/// Bring up a daemon on `addr` unless one is already listening, run the TUI
/// against it, and stop the daemon again once the TUI exits.
pub fn run(root: &Path, addr: SocketAddr) -> anyhow::Result<()> {
    crate::controller::store::ensure_workspace(root)?;
    let _daemon = if is_listening(addr) { None } else { Some(spawn_daemon(root, addr)?) };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(crate::tui::run::run(format!("http://{addr}")))
}

fn is_listening(addr: SocketAddr) -> bool {
    TcpStream::connect_timeout(&addr, READY_POLL).is_ok()
}

/// Kills the daemon on drop, so it goes down however the TUI exits.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_daemon(root: &Path, addr: SocketAddr) -> anyhow::Result<Daemon> {
    let log_path = log_path(root);
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)?;
    let child = Command::new(std::env::current_exe()?)
        .arg("--root")
        .arg(root)
        .arg("daemon")
        .arg("--addr")
        .arg(addr.to_string())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let mut daemon = Daemon(child);
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if let Some(status) = daemon.0.try_wait()? {
            anyhow::bail!("daemon exited ({status}) before listening; see {}", log_path.display());
        }
        if is_listening(addr) {
            return Ok(daemon);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("daemon not listening on {addr} after {READY_TIMEOUT:?}; see {}", log_path.display());
        }
        std::thread::sleep(READY_POLL);
    }
}
