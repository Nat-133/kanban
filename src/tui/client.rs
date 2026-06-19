// http client — Task 2

use crate::model::proto::{Intent, Response};
use crate::model::{Board, Task};

/// The board view the TUI renders (pulled from a `Response::Snapshot`).
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub board: Board,
    pub tasks: Vec<Task>,
}

pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base: String) -> Self {
        Self { base, http: reqwest::Client::new() }
    }

    pub async fn send(&self, intent: Intent) -> anyhow::Result<Response> {
        let resp = self.http
            .post(format!("{}/v1/intent", self.base))
            .json(&intent)
            .send().await?
            .json::<Response>().await?;
        Ok(resp)
    }

    pub async fn snapshot(&self) -> anyhow::Result<Snapshot> {
        match self.send(Intent::GetBoard).await? {
            Response::Snapshot { board, tasks } => Ok(Snapshot { board, tasks }),
            Response::Error { message } => Err(anyhow::anyhow!(message)),
            Response::Ok { .. } => Err(anyhow::anyhow!("unexpected Ok response to GetBoard")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::proto::Intent;

    #[tokio::test]
    async fn client_create_and_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".kanban");
        crate::controller::store::init_workspace(&root).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, crate::controller::server::router(root, tokio::sync::broadcast::channel(64).0)).await.unwrap(); });

        let client = Client::new(format!("http://{addr}"));
        client.send(Intent::CreateTask { title: "A".into(), summary: "s".into(), column: "inbox".parse().unwrap() }).await.unwrap();
        let snap = client.snapshot().await.unwrap();
        assert_eq!(snap.tasks.len(), 1);
    }
}
