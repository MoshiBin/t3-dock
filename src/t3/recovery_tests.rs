//! The real client against a stand-in T3 on loopback, so recovery is testable without
//! restarting the T3 Code that the developer is working in.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::tungstenite::Message;

use super::*;

const TOKEN: &str = "invented-token-0123456789";

struct Fake {
    address: SocketAddr,
    accepts_token: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Fake {
    async fn start(thread: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepts_token = Arc::new(AtomicBool::new(true));
        let accepts = accepts_token.clone();
        let task = tokio::spawn(async move {
            // Dropping the set closes every connection, like T3 quitting.
            let mut connections = JoinSet::new();
            while let Ok((socket, _)) = listener.accept().await {
                connections.spawn(serve(socket, thread, accepts.clone()));
            }
        });
        Self {
            address,
            accepts_token,
            task,
        }
    }

    fn publish(&self, home: &Path) {
        let runtime = json!({"origin": format!("http://{}", self.address), "pid": 1});
        std::fs::write(
            home.join("userdata/server-runtime.json"),
            runtime.to_string(),
        )
        .unwrap();
    }

    async fn quit(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

async fn serve(mut socket: TcpStream, thread: &'static str, accepts: Arc<AtomicBool>) {
    let mut start = [0u8; 8];
    while socket
        .peek(&mut start)
        .await
        .is_ok_and(|read| read < start.len() && read > 0)
    {
        tokio::task::yield_now().await;
    }
    if start.starts_with(b"GET /ws") {
        return stream_shell(socket, thread).await;
    }

    let mut request = Vec::new();
    let mut part = [0u8; 4096];
    let head_end = loop {
        match socket.read(&mut part).await {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(&part[..read]),
        }
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:")?.trim().parse().ok())
        .unwrap_or(0);
    while request.len() < head_end + length {
        match socket.read(&mut part).await {
            Ok(0) | Err(_) => return,
            Ok(read) => request.extend_from_slice(&part[..read]),
        }
    }

    let authorized =
        accepts.load(Ordering::SeqCst) && head.contains(&format!("authorization: bearer {TOKEN}"));
    let body = if head.starts_with("get /.well-known/t3/environment ") {
        json!({"environmentId": "env-invented", "orchestrationProtocolVersion": 2,
               "capabilities": {"shellThreadFamilies": true, "shellOlderSettled": true}})
    } else if head.starts_with("get /api/auth/session ") {
        json!({"authenticated": authorized, "scopes": ["orchestration:read"],
               "expiresAt": "2030-01-01T00:00:00Z"})
    } else if head.starts_with("post /api/auth/websocket-ticket ") && authorized {
        json!({"ticket": "invented ticket+/="})
    } else {
        json!({})
    };
    let body = body.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

async fn stream_shell(socket: TcpStream, thread: &str) {
    let Ok(mut socket) = tokio_tungstenite::accept_async(socket).await else {
        return;
    };
    let Some(Ok(Message::Text(request))) = socket.next().await else {
        return;
    };
    let request: Value = serde_json::from_str(&request).unwrap();
    assert_eq!(request["tag"], "orchestration.subscribeShell");
    assert_eq!(request["payload"]["omitIdleSubagents"], true);

    let chunk = |item: Value| json!({"_tag": "Chunk", "requestId": "shell", "values": [item]});
    let snapshot = chunk(json!({"kind": "snapshot", "snapshot": {
        "snapshotSequence": 1, "threads": [{"id": thread, "title": "Before"}], "projects": []}}));
    if socket
        .send(Message::text(snapshot.to_string()))
        .await
        .is_err()
    {
        return;
    }
    // Like T3, send nothing more until the client acknowledges.
    let Some(Ok(Message::Text(ack))) = socket.next().await else {
        return;
    };
    let ack: Value = serde_json::from_str(&ack).unwrap();
    assert_eq!(ack["_tag"], "Ack");

    let update = json!([chunk(
        json!({"kind": "thread.updated", "sequence": 2, "location": "active",
        "thread": {"id": thread, "title": "After"}})
    )]);
    if socket
        .send(Message::text(update.to_string()))
        .await
        .is_err()
    {
        return;
    }
    while let Some(Ok(_)) = socket.next().await {}
}

async fn until(views: &mut watch::Receiver<View>, wanted: impl Fn(&View) -> bool) {
    timeout(Duration::from_secs(30), async {
        loop {
            if wanted(&views.borrow_and_update()) {
                return;
            }
            views.changed().await.unwrap();
        }
    })
    .await
    .expect("the client did not get there");
}

fn has_updated(view: &View, thread: &str) -> bool {
    view.phase == Phase::Live
        && view.threads.len() == 1
        && view
            .threads
            .get(thread)
            .is_some_and(|thread| thread.title.as_deref() == Some("After"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn survives_a_restart_on_another_port_and_a_renewed_token() {
    let root = std::env::temp_dir().join(format!("t3-dock-test-{}", std::process::id()));
    let (home, data) = (root.join("home"), root.join("data"));
    std::fs::create_dir_all(home.join("userdata")).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    let sealed = crate::win::protect(TOKEN.as_bytes()).unwrap();
    std::fs::write(data.join("token.dpapi"), &sealed).unwrap();

    let first = Fake::start("thread:first").await;
    first.publish(&home);
    let (views_tx, mut views) = watch::channel(View::default());
    let client = tokio::spawn(run(home.clone(), data.clone(), views_tx));
    until(&mut views, |view| has_updated(view, "thread:first")).await;

    first.quit().await;
    until(&mut views, |view| view.phase == Phase::Offline).await;

    let second = Fake::start("thread:second").await;
    second.accepts_token.store(false, Ordering::SeqCst);
    second.publish(&home);
    until(&mut views, |view| view.phase == Phase::NeedsToken).await;

    second.accepts_token.store(true, Ordering::SeqCst);
    until(&mut views, |view| has_updated(view, "thread:second")).await;

    client.abort();
    second.quit().await;
    std::fs::remove_dir_all(root).unwrap();
}
