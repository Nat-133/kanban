// http server — Task 5

use crate::controller::apply::apply;
use crate::model::proto::{Intent, Response};
use axum::response::sse::{Event, Sse};
use axum::routing::get;
use axum::{extract::State, routing::post, Json, Router};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

#[derive(Clone)]
struct AppState {
    root: Arc<PathBuf>,
    changes: broadcast::Sender<()>,
}

pub fn router(root: PathBuf, changes: broadcast::Sender<()>) -> Router {
    Router::new()
        .route("/v1/intent", post(handle_intent))
        .route("/v1/events", get(sse_events))
        .with_state(AppState { root: Arc::new(root), changes })
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
    if is_mutation && !matches!(resp, Response::Error { .. }) {
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
    // periodic reconcile: drain intake spools; notify subscribers on change
    {
        let root = root.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
            loop {
                tick.tick().await;
                let r = root.clone();
                if let Ok(Ok(true)) =
                    tokio::task::spawn_blocking(move || crate::controller::events::reconcile_all(&r))
                        .await
                {
                    let _ = tx.send(());
                }
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "controller listening");
    axum::serve(listener, router(root, tx)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::proto::{Intent, Response};

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
            .json(&Intent::CreateTask { title: "A".into(), summary: "s".into(), column: "inbox".parse().unwrap() })
            .send().await.unwrap().json().await.unwrap();
        assert!(matches!(create, Response::Ok { task: Some(_) }));

        let snap: Response = client.post(&url).json(&Intent::GetBoard)
            .send().await.unwrap().json().await.unwrap();
        match snap { Response::Snapshot { tasks, .. } => assert_eq!(tasks.len(), 1), o => panic!("{o:?}") }
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
            .json(&crate::model::proto::Intent::CreateTask { title: "A".into(), summary: "s".into(), column: "inbox".parse().unwrap() })
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
