// http server — Task 5

use crate::controller::apply::apply;
use crate::controller::{events, store};
use crate::model::proto::{Intent, Response, WakeRequest};
use crate::model::TaskId;
use axum::response::sse::{Event, Sse};
use axum::routing::get;
use axum::{extract::State, routing::post, Json, Router};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

/// Backstop reconcile cadence. The `/v1/wake` poke is the primary path and is
/// effectively instant on localhost (the hook always resolves an address, falling
/// back to the default, so it can't be silently skipped). This periodic pass is the
/// safety net: it re-reads state, applies any card moves, and broadcasts if anything
/// the UI can see changed — covering the rare case where a poke can't be delivered
/// (no daemon was up at hook time, or it restarted on a different port).
const RECONCILE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// File under the workspace root holding the daemon's bound address (e.g.
/// `127.0.0.1:7777`). Written on startup so the hook process can find the daemon
/// to poke without being told where it is.
pub fn addr_path(root: &Path) -> PathBuf {
    root.join("daemon.addr")
}

/// Record the daemon's bound address so hooks can locate it.
pub fn write_daemon_addr(root: &Path, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    store::atomic_write(&addr_path(root), &addr.to_string())
}

/// The address the daemon binds by default (and the TUI connects to by default).
pub const DEFAULT_ADDR: &str = "127.0.0.1:7777";

/// The daemon's last-known address, if one was recorded. None when no daemon has
/// run in this workspace, or the file was removed.
pub fn read_daemon_addr(root: &Path) -> Option<String> {
    std::fs::read_to_string(addr_path(root))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The address a hook should poke: the recorded `daemon.addr`, or the default if
/// that file is missing. Always returns something so the instant poke path is
/// never silently skipped — a missing file must not disable it.
pub fn daemon_addr(root: &Path) -> String {
    read_daemon_addr(root).unwrap_or_else(|| DEFAULT_ADDR.to_string())
}

/// Best-effort doorbell: tell the daemon at `addr` to reconcile session `id`.
/// Errors are the caller's to ignore — a failed poke degrades to the backstop.
pub async fn poke(addr: &str, id: TaskId) -> anyhow::Result<()> {
    reqwest::Client::new()
        .post(format!("http://{addr}/v1/wake"))
        .json(&WakeRequest { task: id })
        .send()
        .await?;
    Ok(())
}

#[derive(Clone)]
struct AppState {
    root: Arc<PathBuf>,
    changes: broadcast::Sender<()>,
}

pub fn router(root: PathBuf, changes: broadcast::Sender<()>) -> Router {
    Router::new()
        .route("/v1/intent", post(handle_intent))
        .route("/v1/wake", post(handle_wake))
        .route("/v1/events", get(sse_events))
        .with_state(AppState { root: Arc::new(root), changes })
}

/// Doorbell handler: re-read one session's state, move its card to match, and
/// notify subscribers. The body carries only the id; the new state was already
/// written to the session's `state.yaml` by the hook.
///
/// A wake only fires because the hook recorded a *new* worker event, so it always
/// represents a real change — and the card's icon tracks phase, not just column.
/// So we always broadcast, even when the card stays put (e.g. working -> idle both
/// live in `doing`); otherwise the UI's spinner/warning would go stale.
async fn handle_wake(State(state): State<AppState>, Json(wake): Json<WakeRequest>) -> Json<Response> {
    let root = (*state.root).clone();
    let task = wake.task;
    let _ = tokio::task::spawn_blocking(move || events::ingest_session(&root, task)).await;
    let _ = state.changes.send(()); // ignore "no subscribers"
    Json(Response::Ok { task: Some(task) })
}

async fn handle_intent(State(state): State<AppState>, Json(intent): Json<Intent>) -> Json<Response> {
    let is_mutation = !matches!(intent, Intent::GetBoard);
    let root = (*state.root).clone();
    // the store does blocking filesystem I/O; keep it off the async reactor
    let result = tokio::task::spawn_blocking(move || apply(&root, intent)).await;
    let resp = match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Response::Error { message: e.to_string() },
        Err(join_err) => Response::Error { message: format!("internal task error: {join_err}") },
    };
    // Only announce a change when a mutation actually succeeded — a rejected
    // write (Error, or a Conflict from the optimistic-concurrency check) changed
    // nothing, so waking subscribers to re-fetch would be a phantom refresh.
    if is_mutation && matches!(resp, Response::Ok { .. }) {
        let _ = state.changes.send(()); // ignore "no subscribers"
    }
    Json(resp)
}

async fn sse_events(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    use tokio_stream::StreamExt as _;
    let stream = BroadcastStream::new(state.changes.subscribe())
        .filter_map(|r| r.ok())
        .map(|()| Ok(Event::default().event("changed").data("")));
    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

/// Bind and serve until the process is stopped. Used by `kanban daemon`.
pub async fn serve(root: PathBuf, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    // Fail fast with a clear message rather than binding the port and serving
    // ENOENT on every request (e.g. when launched from the wrong directory).
    crate::controller::store::ensure_workspace(&root)?;

    let (tx, _rx) = broadcast::channel(64);
    // Periodic reconcile: re-read every session's state, move cards to match, and
    // broadcast whenever the observable state (board layout OR any session's phase)
    // changed since last tick. This is the reliable path — it catches phase-only
    // changes (e.g. working -> idle, both in `doing`) and needs no working poke, so
    // the UI stays live even if a hook's doorbell never arrives.
    {
        let root = root.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(RECONCILE_INTERVAL);
            let mut last_fp: Option<u64> = None;
            loop {
                tick.tick().await;
                let r = root.clone();
                let fp = tokio::task::spawn_blocking(move || {
                    let _ = crate::controller::events::reconcile_all(&r); // apply any column moves
                    crate::controller::events::observable_fingerprint(&r)
                })
                .await;
                if let Ok(Ok(fp)) = fp {
                    if last_fp != Some(fp) {
                        last_fp = Some(fp);
                        let _ = tx.send(());
                    }
                }
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    // Publish where we're listening so hook processes can poke us.
    write_daemon_addr(&root, bound)?;
    tracing::info!(addr = %bound, "controller listening");
    axum::serve(listener, router(root, tx)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::proto::{Intent, Response};

    #[test]
    fn daemon_addr_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();
        assert!(read_daemon_addr(&root).is_none()); // none before any daemon ran
        write_daemon_addr(&root, "127.0.0.1:7777".parse().unwrap()).unwrap();
        assert_eq!(read_daemon_addr(&root).as_deref(), Some("127.0.0.1:7777"));
    }

    #[test]
    fn daemon_addr_falls_back_to_default_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();
        // no daemon.addr file -> the poke must still resolve an address, not skip
        assert_eq!(daemon_addr(&root), DEFAULT_ADDR);
        write_daemon_addr(&root, "127.0.0.1:9999".parse().unwrap()).unwrap();
        assert_eq!(daemon_addr(&root), "127.0.0.1:9999");
    }

    #[tokio::test]
    async fn wake_ingests_session_and_moves_card() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(root.clone(), tokio::sync::broadcast::channel(64).0);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/v1/intent");
        client.post(&url)
            .json(&Intent::CreateTask { title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() })
            .send().await.unwrap();
        // the hook would write state then poke; simulate the state write directly
        let id = crate::model::TaskId::new(1);
        crate::controller::events::record_state(&root, id, "notification", "{\"notification_type\":\"idle_prompt\"}").unwrap();

        // poke and confirm the card landed in doing (needs-human is in-progress)
        poke(&addr.to_string(), id).await.unwrap();
        let snap: Response = client.post(&url).json(&Intent::GetBoard).send().await.unwrap().json().await.unwrap();
        match snap {
            Response::Snapshot { board, .. } =>
                assert!(board.cards().get(&"doing".parse().unwrap()).unwrap().contains(&id)),
            o => panic!("{o:?}"),
        }
    }

    #[tokio::test]
    async fn wake_emits_sse_event_on_phase_change_without_column_move() {
        // working -> stopped keeps the card in `doing` (the icon changes, not the
        // column). The UI must still be notified so the spinner flips to a warning.
        use futures_util::StreamExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();
        crate::controller::apply::apply(&root, Intent::CreateTask {
            title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() }).unwrap();
        let id = crate::model::TaskId::new(1);
        // place it in `doing` as Working first
        crate::controller::events::record_state(&root, id, "user-prompt-submit", "{}").unwrap();
        crate::controller::events::ingest_session(&root, id).unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server_root = root.clone();
        tokio::spawn(async move { axum::serve(listener, router(server_root, tokio::sync::broadcast::channel(64).0)).await.unwrap(); });
        let mut es = reqwest_eventsource::EventSource::get(format!("http://{addr}/v1/events"));
        loop { if let reqwest_eventsource::Event::Open = es.next().await.unwrap().unwrap() { break; } }

        // stop: phase Working -> Idle, still in `doing` (no column move), then poke
        crate::controller::events::record_state(&root, id, "stop", "{}").unwrap();
        poke(&addr.to_string(), id).await.unwrap();

        let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let reqwest_eventsource::Event::Message(m) = es.next().await.unwrap().unwrap() {
                    return m.event;
                }
            }
        }).await.unwrap();
        assert_eq!(got, "changed");
    }

    #[tokio::test]
    async fn wake_emits_sse_event_when_card_moves() {
        use futures_util::StreamExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();
        crate::controller::apply::apply(&root, Intent::CreateTask {
            title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() }).unwrap();
        let id = crate::model::TaskId::new(1);
        crate::controller::events::record_state(&root, id, "notification", "{\"notification_type\":\"idle_prompt\"}").unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(root, tokio::sync::broadcast::channel(64).0)).await.unwrap(); });

        let mut es = reqwest_eventsource::EventSource::get(format!("http://{addr}/v1/events"));
        loop {
            if let reqwest_eventsource::Event::Open = es.next().await.unwrap().unwrap() { break; }
        }
        poke(&addr.to_string(), id).await.unwrap();

        let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let reqwest_eventsource::Event::Message(m) = es.next().await.unwrap().unwrap() {
                    return m.event;
                }
            }
        }).await.unwrap();
        assert_eq!(got, "changed");
    }

    #[tokio::test]
    async fn post_intent_creates_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();

        // bind :0 for an ephemeral port, serve in the background
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(root.clone(), tokio::sync::broadcast::channel(64).0);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/v1/intent");
        let create: Response = client.post(&url)
            .json(&Intent::CreateTask { title: "A".into(), summary: "s".into(), column: "todo".parse().unwrap() })
            .send().await.unwrap().json().await.unwrap();
        assert!(matches!(create, Response::Ok { task: Some(_) }));

        let snap: Response = client.post(&url).json(&Intent::GetBoard)
            .send().await.unwrap().json().await.unwrap();
        match snap { Response::Snapshot { tasks, .. } => assert_eq!(tasks.len(), 1), o => panic!("{o:?}") }
    }

    #[tokio::test]
    async fn edit_description_conflict_travels_back_over_http() {
        // The optimistic-concurrency rejection must survive JSON round-tripping so
        // the TUI's run loop can act on it. A stale `base` -> `Response::Conflict`
        // carrying the current on-disk content.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = router(root.clone(), tokio::sync::broadcast::channel(64).0);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/v1/intent");
        client.post(&url)
            .json(&Intent::CreateTask { title: "A".into(), summary: "".into(), column: "todo".parse().unwrap() })
            .send().await.unwrap();

        let resp: Response = client.post(&url)
            .json(&Intent::EditDescription {
                task: crate::model::TaskId::new(1),
                base: Some("stale".into()), // create seeded "# A\n", so this is stale
                description: "clobber".into(),
            })
            .send().await.unwrap().json().await.unwrap();
        assert_eq!(resp, Response::Conflict { current: Some("# A\n".into()) });
    }

    #[tokio::test]
    async fn mutation_emits_sse_event() {
        use futures_util::StreamExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(root, tokio::sync::broadcast::channel(64).0)).await.unwrap(); });

        let mut es = reqwest_eventsource::EventSource::get(format!("http://{addr}/v1/events"));
        // wait until the stream is open
        loop {
            if let reqwest_eventsource::Event::Open = es.next().await.unwrap().unwrap() {
                break;
            }
        }
        let client = reqwest::Client::new();
        client.post(format!("http://{addr}/v1/intent"))
            .json(&crate::model::proto::Intent::CreateTask { title: "A".into(), summary: "s".into(), column: "todo".parse().unwrap() })
            .send().await.unwrap();

        let got = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let reqwest_eventsource::Event::Message(m) = es.next().await.unwrap().unwrap() {
                    return m.event;
                }
            }
        }).await.unwrap();
        assert_eq!(got, "changed");
    }
}
