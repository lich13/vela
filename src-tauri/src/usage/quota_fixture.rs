//! A loopback service for the tests of a quota asked of a client's own
//! service. It answers as the test tells it to and keeps what it was asked.
use parking_lot::Mutex;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) struct Seen {
    pub(super) path: String,
    pub(super) head: String,
    pub(super) body: String,
}
/// The status, any further header lines and the body. Status 0 hangs up
/// without an answer.
pub(super) type Reply = (u16, &'static str, Value);
pub(super) struct Service {
    pub(super) origin: String,
    pub(super) seen: Arc<Mutex<Vec<Seen>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Service {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Service {
    pub(super) fn paths(&self) -> Vec<String> {
        self.seen.lock().iter().map(|r| r.path.clone()).collect()
    }
}
pub(super) async fn serve(answer: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Service {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let request = loop {
                let mut chunk = [0; 4096];
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0, "the request ended early");
                bytes.extend_from_slice(&chunk[..count]);
                let text = String::from_utf8_lossy(&bytes);
                let Some((head, body)) = text.split_once("\r\n\r\n") else {
                    continue;
                };
                let length = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|length| length.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if body.len() >= length {
                    break Seen {
                        path: head.split_whitespace().nth(1).unwrap().to_owned(),
                        head: head.to_owned(),
                        body: body.to_owned(),
                    };
                }
            };
            let (status, extra, body) = answer(&request);
            log.lock().push(request);
            if status == 0 {
                continue;
            }
            let body = body.to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    Service { origin, seen, task }
}
