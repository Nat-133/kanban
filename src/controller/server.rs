// http server — Task 5

use crate::controller::apply::apply;
use crate::model::proto::{Intent, Response};
use axum::{extract::State, routing::post, Json, Router};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
struct AppState {
    root: Arc<PathBuf>,
}

pub fn router(root: PathBuf) -> Router {
    Router::new()
        .route("/v1/intent", post(handle_intent))
        .with_state(AppState { root: Arc::new(root) })
}

async fn handle_intent(State(state): State<AppState>, Json(intent): Json<Intent>) -> Json<Response> {
    let root = (*state.root).clone();
    // the store does blocking filesystem I/O; keep it off the async reactor
    let result = tokio::task::spawn_blocking(move || apply(&root, intent)).await;
    let resp = match result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => Response::Error { message: e.to_string() },
        Err(join_err) => Response::Error { message: format!("internal task error: {join_err}") },
    };
    Json(resp)
}

/// Bind and serve until the process is stopped. Used by `kanban daemon`.
pub async fn serve(root: PathBuf, addr: std::net::SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "controller listening");
    axum::serve(listener, router(root)).await?;
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
        let app = router(root.clone());
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
}
