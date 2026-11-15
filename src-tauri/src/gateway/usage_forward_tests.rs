//! Usage integration through isolated loopback gateways; all keys are fixtures.
use super::{
    connector::{BoxError, Connector},
    replay::{self, full, WireBody},
    ClientId, Edit, Gateway, HttpClient, Settings,
};
use crate::usage::{
    model::{safe_id, Filter, Operation, Record},
    Service,
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, StreamBody};
use hyper::{
    body::{Frame, Incoming},
    Request, Response,
};
use hyper_util::{client::legacy::Client, rt::TokioExecutor, rt::TokioIo};
use parking_lot::Mutex;
use serde_json::json;
use std::{
    convert::Infallible,
    future::Future,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(5);
const FIXTURE_KEY: &str = "fixture-only-key";
const HTTP_REQUEST: &[u8] = br#" { "model" : "gpt-request-fixture", "stream": false,
  "input":[{"role":"user","content":"fixture"}], "unknown":9007199254740993 } "#;
const HTTP_RESPONSE: &[u8] = br#" { "id":"fixture-http-response", "object":"response",
 "model":"gpt-response-fixture", "status":"completed", "unknown":9007199254740993,
 "usage":{"input_tokens":100,"output_tokens":20,"input_tokens_details":{"cached_tokens":40}} } "#;
const WS_DELTA: &str = r#"{"type":"response.output_text.delta","delta":"fixture"}"#;

#[derive(Clone, Default)]
struct TaskScope(Arc<Mutex<(bool, Vec<JoinHandle<()>>)>>);

impl TaskScope {
    fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
        let mut state = self.0.lock();
        if !state.0 {
            state.1.push(tokio::spawn(future));
        }
    }

    fn close(&self) {
        let mut state = self.0.lock();
        state.0 = true;
        for task in state.1.drain(..) {
            task.abort();
        }
    }
}

struct LoopbackServer {
    port: u16,
    tasks: TaskScope,
}

impl LoopbackServer {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.tasks.close();
    }
}

async fn server<F, Fut>(handler: F) -> LoopbackServer
where
    F: Fn(Request<Incoming>, TaskScope) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response<WireBody>> + Send + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let tasks = TaskScope::default();
    let accept_tasks = tasks.clone();
    let handler = Arc::new(handler);
    tasks.spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let handler = handler.clone();
            let connection_tasks = accept_tasks.clone();
            accept_tasks.spawn(async move {
                let service = hyper::service::service_fn(move |request| {
                    let handler = handler.clone();
                    let tasks = connection_tasks.clone();
                    async move { Ok::<_, Infallible>(handler(request, tasks).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .with_upgrades()
                    .await;
            });
        }
    });
    LoopbackServer { port, tasks }
}

struct Fixture {
    gateway: Gateway,
    usage: Service,
    home: PathBuf,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new(client: ClientId, urls: &[String]) -> Self {
        assert!(!urls.is_empty());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("client-home");
        std::fs::create_dir(&home).unwrap();
        let data = dir.path().join("gateway-data");
        let gateway = match client {
            ClientId::Dsh => panic!("DSH profile path is covered separately"),
            ClientId::Grok => {
                let config = format!("[models]\ndefault='fixture'\n[model.fixture]\nmodel='fixture-model'\napi_backend='responses'\nbase_url={}\napi_key={}\n", serde_json::to_string(&urls[0]).unwrap(), serde_json::to_string(FIXTURE_KEY).unwrap());
                std::fs::write(home.join("config.toml"), config).unwrap();
                Gateway::new_client(data, ClientId::Grok, None).unwrap()
            }
            ClientId::Codex => {
                let config = format!(
                    "model_provider='custom'\n[model_providers.custom]\nbase_url={}\nexperimental_bearer_token={}\nwire_api='responses'\n",
                    serde_json::to_string(&urls[0]).unwrap(),
                    serde_json::to_string(FIXTURE_KEY).unwrap()
                );
                std::fs::write(home.join("config.toml"), config).unwrap();
                Gateway::new(data).unwrap()
            }
            ClientId::Claude => {
                let settings = json!({"env": {
                    "ANTHROPIC_BASE_URL": urls[0], "ANTHROPIC_AUTH_TOKEN": FIXTURE_KEY
                }});
                std::fs::write(home.join("settings.json"), settings.to_string()).unwrap();
                Gateway::new(dir.path().join("codex-data"))
                    .unwrap()
                    .companion(data)
                    .unwrap()
            }
        };
        let usage = Service::new(&dir.path().join("usage-data"));
        assert!(usage.state().error.is_none());
        gateway.set_usage(usage.clone());
        let fixture = Self {
            gateway,
            usage,
            home,
            _dir: dir,
        };
        let reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        fixture.edit(Edit::Settings {
            settings: Settings {
                port: reservation.local_addr().unwrap().port(),
                max_retries: 1,
                first_byte_seconds: 5,
                idle_seconds: 5,
                connect_seconds: 2,
                queue_seconds: 2,
                ..Settings::default()
            },
        });
        for (index, url) in urls.iter().enumerate() {
            fixture.edit(Edit::SaveProvider {
                search_only: None,
                end_on_message_stop: None,
                quota_access_token: None,
                id: None,
                base_url: url.clone(),
                token: FIXTURE_KEY.into(),
                name: Some(format!("fixture-P{}", index + 1)),
            });
        }
        drop(reservation);
        fixture
    }

    fn edit(&self, edit: Edit) {
        self.gateway
            .edit(edit, &self.gateway.view().revision, &self.home)
            .unwrap();
    }

    async fn start(&self) {
        self.gateway
            .start(&self.gateway.view().revision, &self.home)
            .await
            .unwrap();
    }

    async fn stop(&self) {
        tokio::time::timeout(WAIT, self.gateway.stop())
            .await
            .unwrap()
            .unwrap();
    }
}

async fn request(gateway: &Gateway, path: &str, body: &[u8]) -> Response<Incoming> {
    let client: HttpClient =
        Client::builder(TokioExecutor::new()).build(Connector::new(Duration::from_secs(2), 0));
    let token = gateway.0.inner.lock().store.local_token.clone();
    let request = Request::builder()
        .method("POST")
        .uri(format!(
            "http://127.0.0.1:{}{path}",
            gateway.view().settings.port
        ))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(full(body.to_vec()))
        .unwrap();
    tokio::time::timeout(WAIT, client.request(request))
        .await
        .unwrap()
        .unwrap()
}

async fn response_bytes(response: Response<Incoming>) -> Bytes {
    tokio::time::timeout(WAIT, response.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes()
}

fn fragmented(bytes: &[u8]) -> WireBody {
    let frames = bytes
        .chunks(7)
        .map(|chunk| Ok::<_, BoxError>(Frame::data(Bytes::copy_from_slice(chunk))))
        .collect::<Vec<_>>();
    StreamBody::new(futures_util::stream::iter(frames)).boxed_unsync()
}

fn records(service: &Service) -> Vec<Record> {
    service
        .query(|store| {
            let page = store.logs(&Filter::default())?;
            assert_eq!(page.total, page.rows.len() as u64);
            Ok(page.rows)
        })
        .unwrap()
}

async fn recorded(service: &Service, count: usize) -> Vec<Record> {
    let mut events = service.subscribe();
    tokio::time::timeout(WAIT, async {
        loop {
            let rows = records(service);
            if rows.len() >= count {
                assert_eq!(rows.len(), count);
                assert!(service.state().error.is_none());
                return rows;
            }
            events.recv().await.expect("fixture usage writer stopped");
        }
    })
    .await
    .expect("fixture usage record was not written")
}

async fn no_new_records(service: &Service, count: usize) {
    let mut events = service.subscribe();
    assert_eq!(records(service).len(), count);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), events.recv())
            .await
            .is_err()
    );
    assert_eq!(records(service).len(), count);
}

async fn settled(check: impl Fn() -> bool) {
    tokio::time::timeout(WAIT, async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fixture gateway did not settle");
}

async fn websocket(gateway: &Gateway) -> yawc::TcpWebSocket {
    let token = gateway.0.inner.lock().store.local_token.clone();
    tokio::time::timeout(
        WAIT,
        yawc::WebSocket::connect(
            format!(
                "ws://127.0.0.1:{}/v1/responses",
                gateway.view().settings.port
            )
            .parse()
            .unwrap(),
        )
        .with_request(
            yawc::HttpRequest::builder().header("authorization", format!("Bearer {token}")),
        ),
    )
    .await
    .unwrap()
    .unwrap()
}

async fn next_frame(socket: &mut yawc::TcpWebSocket) -> yawc::Frame {
    tokio::time::timeout(WAIT, socket.next())
        .await
        .unwrap()
        .expect("fixture websocket closed")
}

async fn next_text_frame(socket: &mut yawc::TcpWebSocket) -> yawc::Frame {
    tokio::time::timeout(WAIT, async {
        loop {
            let frame = socket
                .next()
                .await
                .expect("fixture websocket closed before text frame");
            match frame.opcode() {
                yawc::OpCode::Text => return frame,
                yawc::OpCode::Ping | yawc::OpCode::Pong => continue,
                yawc::OpCode::Close => panic!("fixture websocket sent Close before text frame"),
                yawc::OpCode::Binary => {
                    panic!("fixture websocket sent Binary instead of text frame")
                }
                opcode => panic!("fixture websocket sent unexpected opcode: {opcode:?}"),
            }
        }
    })
    .await
    .expect("fixture websocket did not send a text frame within WAIT")
}

fn completed(turn: u64) -> String {
    json!({"type":"response.completed","response":{
        "id": format!("fixture-ws-response-{turn}"),
        "model": format!("gpt-ws-{turn}-fixture"), "status":"completed",
        "usage":{"input_tokens":100 * turn,"output_tokens":10 * turn,
            "input_tokens_details":{"cached_tokens":40 * turn}}
    }})
    .to_string()
}

#[tokio::test]
async fn freeze_drains_http_and_releases_only_connected_requests() {
    use tokio::io::AsyncWriteExt;
    let hits = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let observed = hits.clone();
    let finish = release.clone();
    let upstream = server(move |_, _| {
        let first = observed.fetch_add(1, Ordering::SeqCst) == 0;
        let finish = finish.clone();
        async move {
            if !first {
                return Response::builder().header("content-type", "application/json")
                    .body(full(HTTP_RESPONSE)).unwrap();
            }
            let stream = async_stream::try_stream! {
                yield Frame::data(Bytes::from(format!("data: {WS_DELTA}\n\n")));
                let _permit = finish.acquire().await.unwrap();
                yield Frame::data(Bytes::from(format!("data: {}\n\ndata: [DONE]\n\n", completed(1))));
            };
            Response::builder().header("content-type", "text/event-stream")
                .body(StreamBody::new(stream).map_err(|e: std::io::Error| -> BoxError { e.into() }).boxed_unsync()).unwrap()
        }
    }).await;
    let fixture = Fixture::new(ClientId::Codex, &[upstream.url("/v1")]).await;
    fixture.edit(Edit::Settings {
        settings: Settings {
            retry_budget_seconds: 1,
            queue_seconds: 1,
            ..fixture.gateway.view().settings
        },
    });
    fixture.start().await;
    let gate = fixture.gateway.freeze_gate();
    let first = request(
        &fixture.gateway,
        "/v1/responses",
        br#"{"model":"gpt-request-fixture","stream":true,"input":"fixture"}"#,
    )
    .await;
    let mut body = first.into_body();
    let head = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(head.starts_with(b"data: "));
    settled(|| gate.view().streaming == 1).await;
    assert_eq!(gate.set(true).unwrap().draining, 1);
    assert!(gate.set(false).is_err());

    let gateway = fixture.gateway.clone();
    let queued = tokio::spawn(async move {
        response_bytes(request(&gateway, "/v1/responses", HTTP_REQUEST).await).await
    });
    settled(|| gate.view().queued == 1).await;
    let port = fixture.gateway.view().settings.port;
    let token = fixture.gateway.0.inner.lock().store.local_token.clone();
    let mut cancelled = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    super::tests::set_reset_on_drop(&cancelled);
    cancelled.write_all(format!("POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", HTTP_REQUEST.len()).as_bytes()).await.unwrap();
    cancelled.write_all(HTTP_REQUEST).await.unwrap();
    settled(|| gate.view().queued == 2).await;
    drop(cancelled);
    settled(|| gate.view().queued == 1).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(!queued.is_finished());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.gateway.view().providers[0].active_requests, 1);
    release.add_permits(1);
    let tail = body.collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&tail).contains("response.completed"));
    settled(|| gate.view().draining == 0).await;
    assert_eq!(gate.view().queued, 1);
    assert!(gate.view().frozen);
    gate.set(false).unwrap();
    assert_eq!(queued.await.unwrap(), HTTP_RESPONSE);
    settled(|| gate.view().draining == 0).await;
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn freeze_holds_websocket_next_turn_and_honors_cancel_and_disconnect() {
    let hits = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let observed = hits.clone();
    let finish = release.clone();
    let upstream = server(move |mut request, tasks| {
        let observed = observed.clone();
        let finish = finish.clone();
        async move {
            let (response, upgrade) = yawc::WebSocket::upgrade(&mut request).unwrap();
            tasks.spawn(async move {
                let mut socket = upgrade.await.unwrap();
                while let Some(frame) = socket.next().await {
                    if frame.opcode() == yawc::OpCode::Close {
                        break;
                    }
                    if frame.opcode() == yawc::OpCode::Ping {
                        socket
                            .send(yawc::Frame::pong(frame.payload().to_vec()))
                            .await
                            .unwrap();
                        continue;
                    }
                    if frame.opcode().is_control() {
                        continue;
                    }
                    let turn = observed.fetch_add(1, Ordering::SeqCst) + 1;
                    socket.send(yawc::Frame::text(WS_DELTA)).await.unwrap();
                    if turn == 1 {
                        let _permit = finish.acquire().await.unwrap();
                    }
                    socket
                        .send(yawc::Frame::text(completed(turn as u64)))
                        .await
                        .unwrap();
                }
            });
            response.map(|_| replay::empty())
        }
    })
    .await;
    let fixture = Fixture::new(ClientId::Codex, &[upstream.url("/v1")]).await;
    fixture.edit(Edit::Settings {
        settings: Settings {
            retry_budget_seconds: 1,
            queue_seconds: 1,
            ..fixture.gateway.view().settings
        },
    });
    fixture.start().await;
    let gate = fixture.gateway.freeze_gate();
    let mut socket = websocket(&fixture.gateway).await;
    let create = r#"{"type":"response.create","model":"gpt-ws-fixture","input":"fixture"}"#;
    socket.send(yawc::Frame::text(create)).await.unwrap();
    assert_eq!(
        next_text_frame(&mut socket).await.payload().as_ref(),
        WS_DELTA.as_bytes()
    );
    gate.set(true).unwrap();
    socket.send(yawc::Frame::text(create)).await.unwrap();
    settled(|| gate.view().queued == 1).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert_eq!(gate.view().draining, 1);
    release.add_permits(1);
    assert_eq!(
        next_text_frame(&mut socket).await.payload().as_ref(),
        completed(1).as_bytes()
    );
    settled(|| gate.view().draining == 0).await;
    assert_eq!(gate.view().queued, 1);
    gate.set(false).unwrap();
    assert_eq!(
        next_text_frame(&mut socket).await.payload().as_ref(),
        WS_DELTA.as_bytes()
    );
    assert_eq!(
        next_text_frame(&mut socket).await.payload().as_ref(),
        completed(2).as_bytes()
    );
    settled(|| gate.view().draining == 0).await;
    gate.set(true).unwrap();
    socket.send(yawc::Frame::text(create)).await.unwrap();
    settled(|| gate.view().queued == 1).await;
    socket
        .send(yawc::Frame::text(r#"{"type":"response.cancel"}"#))
        .await
        .unwrap();
    assert_eq!(
        next_text_frame(&mut socket).await.payload().as_ref(),
        br#"{"type":"response.cancelled"}"#
    );
    settled(|| gate.view().queued == 0).await;
    socket.send(yawc::Frame::text(create)).await.unwrap();
    settled(|| gate.view().queued == 1).await;
    socket
        .send(yawc::Frame::close(1000.into(), "fixture cancelled"))
        .await
        .unwrap();
    drop(socket);
    settled(|| gate.view().queued == 0).await;
    gate.set(false).unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    fixture.stop().await;
}

#[tokio::test]
async fn http_usage_preserves_request_and_response_bytes() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observed = seen.clone();
    let upstream = server(move |request, _| {
        let observed = observed.clone();
        async move {
            let (parts, body) = request.into_parts();
            let bytes = body.collect().await.unwrap().to_bytes();
            observed.lock().push((parts.uri.to_string(), bytes));
            Response::builder()
                .header("content-type", "application/json")
                .header("x-fixture", "preserved")
                .body(full(HTTP_RESPONSE))
                .unwrap()
        }
    })
    .await;
    let fixture = Fixture::new(ClientId::Codex, &[upstream.url("/v1")]).await;
    fixture.start().await;
    let response = request(
        &fixture.gateway,
        "/v1/responses?item=1&item=2",
        HTTP_REQUEST,
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-fixture"], "preserved");
    assert_eq!(response_bytes(response).await.as_ref(), HTTP_RESPONSE);
    assert_eq!(
        *seen.lock(),
        vec![(
            "/v1/responses?item=1&item=2".into(),
            Bytes::from_static(HTTP_REQUEST)
        )]
    );

    let rows = recorded(&fixture.usage, 1).await;
    let row = &rows[0];
    assert_eq!(row.client, "codex");
    assert_eq!(row.source, "proxy");
    assert!(row.completed);
    assert_eq!(row.attempts.len(), 1);
    let attempt = &row.attempts[0];
    assert_eq!(
        attempt.provider.as_deref(),
        Some(fixture.gateway.view().providers[0].id.as_str())
    );
    assert_eq!(
        attempt.requested_model.as_deref(),
        Some("gpt-request-fixture")
    );
    assert_eq!(
        attempt.response_model.as_deref(),
        Some("gpt-response-fixture")
    );
    assert_eq!(attempt.response_id, Some(safe_id("fixture-http-response")));
    assert_eq!(attempt.status, Some(200));
    assert_eq!(attempt.outcome, "success");
    assert_eq!(attempt.transport, "http");
    assert!(!attempt.stream);
    assert_eq!(attempt.tokens.input, Some(60));
    assert_eq!(attempt.tokens.cache_read, Some(40));
    assert_eq!(attempt.tokens.output, Some(20));
    assert_eq!(attempt.tokens.total(), Some(120));
    fixture.stop().await;
}

#[tokio::test]
async fn http_search_routes_charge_each_success_once_without_token_surcharge() {
    for path in ["/v1/alpha/search", "/alpha/search"] {
        for with_tokens in [false, true] {
            search_usage_case(path, 200, with_tokens).await;
        }
    }
}

#[tokio::test]
async fn http_search_routes_keep_failed_searches_unpriced_even_with_tokens() {
    for path in ["/v1/alpha/search", "/alpha/search"] {
        for with_tokens in [false, true] {
            search_usage_case(path, 400, with_tokens).await;
        }
    }
}

async fn search_usage_case(path: &str, status: u16, with_tokens: bool) {
    const SEARCH_REQUEST: &[u8] = br#"{"model":"fixture-search-model","query":"fixture search"}"#;
    let mut payload = json!({
        "id": "fixture-search-response",
        "model": "fixture-search-model",
        "results": []
    });
    if status >= 400 {
        payload["error"] = json!({
            "type": "invalid_request_error",
            "message": "fixture search rejected"
        });
    }
    if with_tokens {
        payload["usage"] = json!({"input_tokens": 100, "output_tokens": 20});
    }
    let expected_response = serde_json::to_vec(&payload).unwrap();
    let response_payload = expected_response.clone();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observed = seen.clone();
    let upstream = server(move |request, _| {
        let observed = observed.clone();
        let payload = response_payload.clone();
        async move {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            observed.lock().push(body);
            Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(full(payload))
                .unwrap()
        }
    })
    .await;
    let fixture = Fixture::new(ClientId::Codex, &[upstream.url("")]).await;
    let mut settings = fixture.gateway.view().settings;
    settings.max_retries = 0;
    fixture.edit(Edit::Settings { settings });
    let pricing = fixture.usage.prices().unwrap();
    let view = pricing.view();
    let mut config = view.config;
    config.auto_update = false;
    config.fixed.insert(
        "fixture-search-model".into(),
        json!({"input_cost_per_token": "0.1", "output_cost_per_token": "0.2"}),
    );
    pricing.configure(config, &view.revision).unwrap();
    fixture.start().await;

    let response = request(&fixture.gateway, path, SEARCH_REQUEST).await;
    assert_eq!(response.status().as_u16(), status, "{path}");
    assert_eq!(response_bytes(response).await.as_ref(), expected_response);
    assert_eq!(*seen.lock(), vec![Bytes::from_static(SEARCH_REQUEST)]);
    let rows = recorded(&fixture.usage, 1).await;
    let row = &rows[0];
    assert_eq!(row.client, "codex");
    assert_eq!(row.source, "proxy");
    assert_eq!(row.completed, status == 200);
    assert_eq!(row.attempts.len(), 1);
    let attempt = &row.attempts[0];
    assert_eq!(attempt.operation, Operation::WebSearch);
    assert_eq!(attempt.status, Some(status));
    assert_eq!(attempt.transport, "http");
    assert_eq!(attempt.tokens.total(), with_tokens.then_some(120));
    let totals = fixture
        .usage
        .query(|store| Ok(store.dashboard(&Filter::default())?.totals))
        .unwrap();
    assert_eq!(totals.requests, 1);
    assert_eq!(totals.attempts, 1);
    if status == 200 {
        assert_eq!(attempt.outcome, "success");
        let price = attempt.price.as_ref().unwrap();
        assert_eq!(price.source, "endpoint");
        assert_eq!(price.model, "web_search");
        assert_eq!(price.cost, "0.01");
        assert_eq!(row.cost().unwrap().to_string(), "0.01");
        assert_eq!(totals.cost, "0.01");
        assert_eq!(totals.unpriced, 0);
    } else {
        assert_ne!(attempt.outcome, "success");
        assert!(attempt.price.is_none());
        assert!(row.cost().is_none());
        assert_eq!(totals.cost, "0");
        assert_eq!(totals.unpriced, 1);
    }
    fixture.stop().await;
}

#[tokio::test]
async fn claude_sse_preserves_wire_bytes_and_normalizes_cache_usage() {
    let body = br#" {"model":"claude-request-fixture","stream":true,"max_tokens":32,
        "messages":[{"role":"user","content":"fixture"}],"unknown":9007199254740993} "#;
    let sse = concat!(
        ": fixture keepalive\r\n\r\n",
        "event: message_start\r\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"fixture-claude-response\",\"type\":\"message\",\"model\":\"claude-response-fixture\",\"usage\":{\"input_tokens\":11,\"output_tokens\":0,\"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":13,\"cache_creation\":{\"ephemeral_5m_input_tokens\":5,\"ephemeral_1h_input_tokens\":8}}}}\r\n\r\n",
        "event: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"fixture\"}}\r\n\r\n",
        "event: message_delta\r\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\r\n\r\n",
        "event: message_stop\r\ndata: {\"type\":\"message_stop\"}\r\n\r\n"
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observed = seen.clone();
    let upstream = server(move |request, _| {
        let observed = observed.clone();
        async move {
            let (parts, body) = request.into_parts();
            let bytes = body.collect().await.unwrap().to_bytes();
            observed.lock().push((parts.uri.to_string(), bytes));
            Response::builder()
                .header("content-type", "text/event-stream; charset=utf-8")
                .header("x-fixture", "sse-preserved")
                .body(fragmented(sse.as_bytes()))
                .unwrap()
        }
    })
    .await;
    let fixture = Fixture::new(ClientId::Claude, &[upstream.url("/deployment")]).await;
    fixture.start().await;
    let response = request(&fixture.gateway, "/v1/messages?beta=fixture", body).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "text/event-stream; charset=utf-8"
    );
    assert_eq!(response.headers()["x-fixture"], "sse-preserved");
    assert_eq!(response_bytes(response).await.as_ref(), sse.as_bytes());
    assert_eq!(
        *seen.lock(),
        vec![(
            "/deployment/v1/messages?beta=fixture".into(),
            Bytes::from_static(body)
        )]
    );

    let rows = recorded(&fixture.usage, 1).await;
    let row = &rows[0];
    assert_eq!(row.client, "claude");
    assert!(row.completed);
    assert_eq!(row.attempts.len(), 1);
    let attempt = &row.attempts[0];
    assert_eq!(attempt.transport, "http");
    assert!(attempt.stream);
    assert_eq!(attempt.status, Some(200));
    assert_eq!(attempt.outcome, "success");
    assert_eq!(
        attempt.requested_model.as_deref(),
        Some("claude-request-fixture")
    );
    assert_eq!(
        attempt.response_model.as_deref(),
        Some("claude-response-fixture")
    );
    assert_eq!(
        attempt.response_id,
        Some(safe_id("fixture-claude-response"))
    );
    assert_eq!(attempt.tokens.input, Some(11));
    assert_eq!(attempt.tokens.output, Some(7));
    assert_eq!(attempt.tokens.cache_read, Some(30));
    assert_eq!(attempt.tokens.cache_write, Some(13));
    assert_eq!(attempt.tokens.cache_write_5m, Some(5));
    assert_eq!(attempt.tokens.cache_write_1h, Some(8));
    assert_eq!(attempt.tokens.total(), Some(61));
    assert!(attempt.first_token_ms.is_some());
    fixture.stop().await;
}

#[tokio::test]
async fn failed_provider_then_success_keeps_two_attempts_in_one_record() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let first_seen = seen.clone();
    let first = server(move |request, _| {
        let first_seen = first_seen.clone();
        async move {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            first_seen.lock().push(("P1", body));
            Response::builder()
                .status(503)
                .header("content-type", "application/json")
                .body(full(
                    r#"{"error":{"type":"server_error","message":"fixture unavailable"},"usage":{"input_tokens":12,"output_tokens":2,"input_tokens_details":{"cached_tokens":5}}}"#,
                ))
                .unwrap()
        }
    })
    .await;
    let second_seen = seen.clone();
    let second = server(move |request, _| {
        let second_seen = second_seen.clone();
        async move {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            second_seen.lock().push(("P2", body));
            Response::builder()
                .header("content-type", "application/json")
                .body(full(HTTP_RESPONSE))
                .unwrap()
        }
    })
    .await;
    let fixture = Fixture::new(ClientId::Codex, &[first.url("/v1"), second.url("/v1")]).await;
    fixture.edit(Edit::Mode {
        mode: "auto".into(),
    });
    let provider_ids = fixture
        .gateway
        .view()
        .providers
        .iter()
        .map(|p| p.id.clone())
        .collect::<Vec<_>>();
    fixture.start().await;
    let response = request(&fixture.gateway, "/v1/responses", HTTP_REQUEST).await;
    assert_eq!(response.status(), 200);
    assert_eq!(response_bytes(response).await.as_ref(), HTTP_RESPONSE);
    assert_eq!(
        *seen.lock(),
        vec![
            ("P1", Bytes::from_static(HTTP_REQUEST)),
            ("P2", Bytes::from_static(HTTP_REQUEST))
        ]
    );
    let rows = recorded(&fixture.usage, 1).await;
    let row = &rows[0];
    assert!(row.completed);
    assert_eq!(row.attempts.len(), 2);
    assert_ne!(row.attempts[0].id, row.attempts[1].id);
    for (attempt, provider) in row.attempts.iter().zip(&provider_ids) {
        assert_eq!(attempt.provider.as_ref(), Some(provider));
        assert_eq!(
            attempt.requested_model.as_deref(),
            Some("gpt-request-fixture")
        );
    }
    assert_eq!(row.attempts[0].status, Some(503));
    assert_eq!(row.attempts[0].outcome, "rejected");
    assert_eq!(row.attempts[0].tokens.input, Some(7));
    assert_eq!(row.attempts[0].tokens.cache_read, Some(5));
    assert_eq!(row.attempts[0].tokens.output, Some(2));
    assert_eq!(row.attempts[0].tokens.total(), Some(14));
    assert_eq!(row.attempts[1].status, Some(200));
    assert_eq!(row.attempts[1].outcome, "success");
    assert_eq!(row.attempts[1].tokens.input, Some(60));
    assert_eq!(row.attempts[1].tokens.cache_read, Some(40));
    assert_eq!(row.attempts[1].tokens.output, Some(20));
    assert_eq!(row.tokens().input, Some(67));
    assert_eq!(row.tokens().cache_read, Some(45));
    assert_eq!(row.tokens().output, Some(22));
    assert_eq!(row.tokens().total(), Some(134));
    let totals = fixture
        .usage
        .query(|store| Ok(store.dashboard(&Filter::default())?.totals))
        .unwrap();
    assert_eq!(totals.requests, 1);
    assert_eq!(totals.success, 1);
    assert_eq!(totals.tokens.input, Some(67));
    assert_eq!(totals.tokens.cache_read, Some(45));
    assert_eq!(totals.tokens.output, Some(22));
    fixture.stop().await;
}

#[tokio::test]
async fn responses_websocket_records_each_turn_without_idle_records() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observed = seen.clone();
    let upstream = server(move |mut request, tasks| {
        let observed = observed.clone();
        async move {
            let (response, upgrade) = yawc::WebSocket::upgrade(&mut request).unwrap();
            tasks.spawn(async move {
                let mut socket = upgrade.await.unwrap();
                let mut turn = 0;
                while let Some(frame) = socket.next().await {
                    if frame.opcode() == yawc::OpCode::Close {
                        break;
                    }
                    if frame.opcode() == yawc::OpCode::Ping {
                        socket
                            .send(yawc::Frame::pong(frame.payload().to_vec()))
                            .await
                            .unwrap();
                        continue;
                    }
                    if frame.opcode().is_control() {
                        continue;
                    }
                    observed.lock().push(frame.payload().to_vec());
                    turn += 1;
                    socket.send(yawc::Frame::text(WS_DELTA)).await.unwrap();
                    socket
                        .send(yawc::Frame::text(completed(turn)))
                        .await
                        .unwrap();
                }
            });
            response.map(|_| replay::empty())
        }
    })
    .await;
    let fixture = Fixture::new(ClientId::Codex, &[upstream.url("/v1")]).await;
    fixture.start().await;
    let mut socket = websocket(&fixture.gateway).await;
    socket
        .send(yawc::Frame::ping(b"fixture-idle".to_vec()))
        .await
        .unwrap();
    assert_eq!(next_frame(&mut socket).await.opcode(), yawc::OpCode::Pong);
    no_new_records(&fixture.usage, 0).await;
    let mut sent = Vec::new();
    for turn in 1..=2u64 {
        let mut create = json!({"type":"response.create","model":format!("gpt-ws-{turn}-fixture"),
            "input":"fixture", "unknown":9007199254740993u64});
        if turn > 1 {
            create["previous_response_id"] = json!("fixture-ws-response-1");
        }
        let text = create.to_string();
        sent.push(text.as_bytes().to_vec());
        socket.send(yawc::Frame::text(text)).await.unwrap();
        assert_eq!(
            next_text_frame(&mut socket).await.payload().as_ref(),
            WS_DELTA.as_bytes()
        );
        assert_eq!(
            next_text_frame(&mut socket).await.payload().as_ref(),
            completed(turn).as_bytes()
        );
        let rows = recorded(&fixture.usage, turn as usize).await;
        let response_id = safe_id(&format!("fixture-ws-response-{turn}"));
        let row = rows
            .iter()
            .find(|row| row.final_attempt().unwrap().response_id.as_ref() == Some(&response_id))
            .unwrap();
        assert!(row.completed);
        assert_eq!(row.attempts.len(), 1);
        let attempt = &row.attempts[0];
        assert_eq!(attempt.transport, "websocket");
        assert_eq!(attempt.status, Some(101));
        assert_eq!(attempt.outcome, "success");
        assert_eq!(
            attempt.requested_model,
            Some(format!("gpt-ws-{turn}-fixture"))
        );
        assert_eq!(attempt.tokens.input, Some(60 * turn));
        assert_eq!(attempt.tokens.cache_read, Some(40 * turn));
        assert_eq!(attempt.tokens.output, Some(10 * turn));
        assert!(attempt.first_token_ms.is_some());
        settled(|| fixture.gateway.view().providers[0].active_requests == 0).await;
        no_new_records(&fixture.usage, turn as usize).await;
    }
    assert_eq!(*seen.lock(), sent);
    drop(socket);
    fixture.stop().await;
    no_new_records(&fixture.usage, 2).await;
}

#[tokio::test]
async fn unavailable_usage_database_does_not_interrupt_http_or_sse() {
    let broken_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(broken_dir.path().join("usage/usage.sqlite")).unwrap();
    let broken_usage = Service::new(broken_dir.path());
    assert!(broken_usage.state().error.is_some());
    assert!(broken_usage
        .query(|store| store.logs(&Filter::default()))
        .is_err());
    let hits = Arc::new(AtomicUsize::new(0));
    let observed = hits.clone();
    let upstream = server(move |request, _| {
        let observed = observed.clone();
        async move {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
            observed.fetch_add(1, Ordering::SeqCst);
            if value["stream"] == true {
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(fragmented(format!("data: {}\n\n", completed(1)).as_bytes()))
                    .unwrap()
            } else {
                Response::builder()
                    .header("content-type", "application/json")
                    .body(full(HTTP_RESPONSE))
                    .unwrap()
            }
        }
    })
    .await;
    let fixture = Fixture::new(ClientId::Codex, &[upstream.url("/v1")]).await;
    fixture.gateway.set_usage(broken_usage.clone());
    fixture.start().await;
    let mut events = broken_usage.subscribe();
    for stream in [false, true] {
        let body =
            json!({"model":"gpt-ws-1-fixture","stream":stream,"input":"fixture"}).to_string();
        let response = request(&fixture.gateway, "/v1/responses", body.as_bytes()).await;
        assert_eq!(response.status(), 200);
        let expected = if stream {
            format!("data: {}\n\n", completed(1)).into_bytes()
        } else {
            HTTP_RESPONSE.to_vec()
        };
        assert_eq!(response_bytes(response).await.as_ref(), expected.as_slice());
        tokio::time::timeout(WAIT, events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(broken_usage.state().error.is_some());
        settled(|| fixture.gateway.view().providers[0].active_requests == 0).await;
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.gateway.view().providers[0].health.failures, 0);
    fixture.stop().await;
}

/// One protocol's wire fixtures for a stream that opens a reasoning block.
struct OpenedBlock {
    client: ClientId,
    path: &'static str,
    request: &'static [u8],
    /// Stream start up to an opened reasoning block that has no content yet.
    opened: &'static [u8],
    /// A non-empty generated delta.
    delta: &'static [u8],
    /// An in-stream service error event.
    error: &'static [u8],
    /// The complete stream of the backup provider.
    backup: &'static [u8],
}
const MESSAGES_BLOCK: OpenedBlock = OpenedBlock {
    client: ClientId::Claude,
    path: "/v1/messages",
    request: br#"{"model":"claude-request-fixture","stream":true,"max_tokens":32,"messages":[{"role":"user","content":"fixture"}]}"#,
    opened: b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_first\",\"type\":\"message\",\"model\":\"claude-response-fixture\",\"stop_reason\":null}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
    delta: b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"visible fixture\"}}\n\n",
    error: b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"fixture stream failure\"}}\n\n",
    backup: b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_backup\",\"type\":\"message\",\"model\":\"claude-response-fixture\",\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"backup\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
};
const RESPONSES_BLOCK: OpenedBlock = OpenedBlock {
    client: ClientId::Codex,
    path: "/v1/responses",
    request: br#"{"model":"gpt-request-fixture","stream":true,"input":[{"role":"user","content":"fixture"}]}"#,
    opened: b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_first\",\"status\":\"in_progress\"}}\n\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"reasoning\",\"id\":\"rs_fixture\"}}\n\n",
    delta: b"data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"visible fixture\"}\n\n",
    error: b"data: {\"type\":\"error\",\"error\":{\"code\":\"server_error\"}}\n\n",
    backup: b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"backup\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_backup\",\"status\":\"completed\",\"usage\":{\"input_tokens\":3,\"output_tokens\":2}}}\n\n",
};

/// What the first provider does after the reasoning block has opened.
#[derive(Clone, Copy)]
enum AfterOpenedBlock {
    /// An in-stream error event before any generated content.
    Error,
    /// A non-empty delta in its own frame, then the error event once released.
    DeltaThenError,
    /// The connection drops before any generated content.
    Reset,
}

/// Two providers: the first opens a reasoning block and then fails as `then`
/// says, the second completes. Returns which providers were asked.
async fn failing_pair(
    wire: &'static OpenedBlock,
    then: AfterOpenedBlock,
    release: Arc<tokio::sync::Notify>,
) -> (
    Fixture,
    Arc<Mutex<Vec<&'static str>>>,
    (LoopbackServer, LoopbackServer),
) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let first_seen = seen.clone();
    let first = server(move |_, _| {
        first_seen.lock().push("P1");
        let release = release.clone();
        async move {
            let stream = async_stream::try_stream! {
                yield Frame::data(Bytes::from_static(wire.opened));
                match then {
                    AfterOpenedBlock::Error => {
                        yield Frame::data(Bytes::from_static(wire.error));
                    }
                    AfterOpenedBlock::DeltaThenError => {
                        yield Frame::data(Bytes::from_static(wire.delta));
                        release.notified().await;
                        yield Frame::data(Bytes::from_static(wire.error));
                    }
                    AfterOpenedBlock::Reset => {
                        // Let the opened block reach the gateway first; an
                        // immediate error would drop it before it is sent.
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        Err::<(), std::io::Error>(std::io::Error::new(
                            std::io::ErrorKind::ConnectionReset,
                            "fixture disconnect",
                        ))?;
                    }
                }
            };
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(
                    StreamBody::new(stream)
                        .map_err(|error: std::io::Error| -> BoxError { error.into() })
                        .boxed_unsync(),
                )
                .unwrap()
        }
    })
    .await;
    let second_seen = seen.clone();
    let second = server(move |_, _| {
        second_seen.lock().push("P2");
        async move {
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(full(wire.backup))
                .unwrap()
        }
    })
    .await;
    let fixture = Fixture::new(wire.client, &[first.url("/a/v1"), second.url("/b/v1")]).await;
    fixture.edit(Edit::Mode {
        mode: "auto".into(),
    });
    fixture.start().await;
    (fixture, seen, (first, second))
}

#[tokio::test]
async fn stream_error_after_an_opened_reasoning_block_still_fails_over() {
    for wire in [&MESSAGES_BLOCK, &RESPONSES_BLOCK] {
        let (fixture, seen, _servers) =
            failing_pair(wire, AfterOpenedBlock::Error, Default::default()).await;
        let response = request(&fixture.gateway, wire.path, wire.request).await;
        assert_eq!(response.status(), 200);
        // Nothing of the first provider's private prefix reaches the client.
        assert_eq!(response_bytes(response).await.as_ref(), wire.backup);
        assert_eq!(*seen.lock(), vec!["P1", "P2"]);
        let rows = recorded(&fixture.usage, 1).await;
        let row = &rows[0];
        assert!(row.completed);
        assert_eq!(row.attempts.len(), 2);
        assert_ne!(row.attempts[0].outcome, "success");
        assert_eq!(row.attempts[1].outcome, "success");
        // The opened block started the first-token clock without closing the
        // replay window; that separation is what let the request move on.
        assert!(row.attempts[0].first_token_ms.is_some());
        fixture.stop().await;
    }
}

#[tokio::test]
async fn stream_error_after_a_delivered_delta_is_not_replayed() {
    for wire in [&MESSAGES_BLOCK, &RESPONSES_BLOCK] {
        let release = Arc::new(tokio::sync::Notify::new());
        let (fixture, seen, _servers) =
            failing_pair(wire, AfterOpenedBlock::DeltaThenError, release.clone()).await;
        let response = request(&fixture.gateway, wire.path, wire.request).await;
        assert_eq!(response.status(), 200);
        let mut body = response.into_body();
        let mut delivered = Vec::new();
        while !String::from_utf8_lossy(&delivered).contains("visible fixture") {
            let frame = tokio::time::timeout(WAIT, body.frame())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            delivered.extend_from_slice(frame.data_ref().unwrap());
        }
        release.notify_one();
        let rest = tokio::time::timeout(WAIT, body.collect())
            .await
            .unwrap()
            .unwrap()
            .to_bytes();
        delivered.extend_from_slice(&rest);
        assert_eq!(delivered, [wire.opened, wire.delta, wire.error].concat());
        assert_eq!(*seen.lock(), vec!["P1"]);
        fixture.stop().await;
    }
}

#[tokio::test]
async fn reset_after_an_opened_reasoning_block_is_not_replayed() {
    for wire in [&MESSAGES_BLOCK, &RESPONSES_BLOCK] {
        let (fixture, seen, _servers) =
            failing_pair(wire, AfterOpenedBlock::Reset, Default::default()).await;
        let response = request(&fixture.gateway, wire.path, wire.request).await;
        assert_eq!(response.status(), 200);
        let mut body = response.into_body();
        let mut delivered = Vec::new();
        let failed = loop {
            match tokio::time::timeout(WAIT, body.frame()).await.unwrap() {
                Some(Ok(frame)) => delivered.extend_from_slice(frame.data_ref().unwrap()),
                Some(Err(_)) => break true,
                None => break false,
            }
        };
        // The provider may already be working on the request: any response
        // byte closes the transport replay window, whatever the block holds.
        assert_eq!(delivered, wire.opened);
        assert!(failed, "the cut must reach the client as a stream error");
        settled(|| {
            fixture
                .gateway
                .view()
                .providers
                .iter()
                .all(|p| p.active_requests == 0)
        })
        .await;
        assert_eq!(*seen.lock(), vec!["P1"]);
        fixture.stop().await;
    }
}

/// How an upstream delivers a complete Messages stream it then leaves open.
#[derive(Clone, Copy, PartialEq)]
enum Lingering {
    /// One frame, no declared length.
    Whole,
    /// `message_stop` arrives in a later frame than the stream start.
    Split,
    /// So much precedes the message that the gateway inspects `message_stop`
    /// before it has relayed anything.
    Padded,
    /// The declared length covers bytes sent only after the release.
    Sized,
    /// Gzip, flushed after `message_stop` but not finished. The flush marker
    /// is split so the decoder hands the gateway the whole message.
    Gzip,
}

/// A Claude gateway whose only provider sends the whole message, then keeps
/// the connection open until `release`; `closed` flips once the gateway lets
/// go of it. Also returns what the upstream sends before and with the wait.
async fn lingering(
    shape: Lingering,
    end_on_message_stop: bool,
) -> (
    Fixture,
    Arc<tokio::sync::Notify>,
    Arc<std::sync::atomic::AtomicBool>,
    (Vec<u8>, Vec<u8>),
    LoopbackServer,
) {
    struct Closed(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Closed {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let message = MESSAGES_BLOCK.backup;
    let (head, tail) = if shape == Lingering::Gzip {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(message).unwrap();
        encoder.flush().unwrap();
        let head = encoder.get_ref().clone();
        let all = encoder.finish().unwrap();
        let tail = all[head.len()..].to_vec();
        (head, tail)
    } else {
        let padding = if shape == Lingering::Padded {
            b": fixture padding\n\n".repeat(3 * 1024 * 1024 / 19)
        } else {
            Vec::new()
        };
        (
            [padding.as_slice(), message].concat(),
            b": fixture tail\n\n".to_vec(),
        )
    };
    let sent = (head.clone(), [head.as_slice(), tail.as_slice()].concat());
    let release = Arc::new(tokio::sync::Notify::new());
    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (released, dropped) = (release.clone(), closed.clone());
    let upstream = server(move |_, _| {
        let (head, tail) = (Bytes::from(head.clone()), Bytes::from(tail.clone()));
        let (released, guard) = (released.clone(), Closed(dropped.clone()));
        async move {
            let length = head.len() + tail.len();
            let stream = async_stream::stream! {
                let _guard = guard;
                if shape != Lingering::Whole && shape != Lingering::Sized {
                    let start = match shape {
                        Lingering::Gzip => head.len() - 1,
                        Lingering::Padded => head.len() - MESSAGES_BLOCK.backup.len(),
                        _ => head.len() / 2,
                    };
                    yield Ok::<_, BoxError>(Frame::data(head.slice(..start)));
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    yield Ok(Frame::data(head.slice(start..)));
                } else {
                    yield Ok(Frame::data(head));
                }
                released.notified().await;
                yield Ok(Frame::data(tail));
            };
            let mut response = Response::builder().header("content-type", "text/event-stream");
            if shape == Lingering::Sized {
                response = response.header("content-length", length);
            }
            if shape == Lingering::Gzip {
                response = response.header("content-encoding", "gzip");
            }
            response
                .body(StreamBody::new(stream).boxed_unsync())
                .unwrap()
        }
    })
    .await;
    let fixture = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    let provider = &fixture.gateway.view().providers[0];
    fixture.edit(Edit::SaveProvider {
        id: Some(provider.id.clone()),
        base_url: provider.base_url.clone(),
        token: String::new(),
        name: None,
        quota_access_token: None,
        search_only: None,
        end_on_message_stop: Some(end_on_message_stop),
    });
    fixture.start().await;
    (fixture, release, closed, sent, upstream)
}

#[tokio::test]
async fn message_stop_ends_the_response_while_the_upstream_lingers() {
    for shape in [Lingering::Whole, Lingering::Split, Lingering::Padded] {
        let (fixture, _release, closed, (head, _), _upstream) = lingering(shape, true).await;
        let response = request(
            &fixture.gateway,
            MESSAGES_BLOCK.path,
            MESSAGES_BLOCK.request,
        )
        .await;
        let bytes = tokio::time::timeout(Duration::from_secs(1), response.into_body().collect())
            .await
            .expect("the response must end at message_stop")
            .unwrap()
            .to_bytes();
        assert!(bytes.as_ref() == head.as_slice(), "the message was cut");
        settled(|| closed.load(Ordering::Acquire)).await;

        let rows = recorded(&fixture.usage, 1).await;
        assert!(rows[0].completed);
        let attempt = &rows[0].attempts[0];
        assert_eq!(attempt.outcome, "success");
        assert_eq!(attempt.tokens.input, Some(3));
        assert_eq!(attempt.tokens.output, Some(2));
        let transfer = attempt.transfer.as_ref().unwrap();
        assert!(!transfer.upstream_eof);
        assert_eq!(transfer.upstream_end, None);
        assert_eq!(transfer.terminal_event.as_deref(), Some("message_stop"));
        fixture.stop().await;
    }
}

#[tokio::test]
async fn a_lingering_upstream_is_awaited_unless_the_stream_may_end_at_message_stop() {
    for (shape, enabled) in [
        (Lingering::Whole, false),
        (Lingering::Sized, true),
        (Lingering::Gzip, true),
    ] {
        let (fixture, release, closed, (_, all), _upstream) = lingering(shape, enabled).await;
        let response = request(
            &fixture.gateway,
            MESSAGES_BLOCK.path,
            MESSAGES_BLOCK.request,
        )
        .await;
        let mut body = std::pin::pin!(response.into_body().collect());
        assert!(
            tokio::time::timeout(Duration::from_millis(400), &mut body)
                .await
                .is_err(),
            "the response ended before the upstream did"
        );
        assert!(!closed.load(Ordering::Acquire));
        release.notify_one();
        let bytes = tokio::time::timeout(WAIT, body)
            .await
            .unwrap()
            .unwrap()
            .to_bytes();
        assert_eq!(bytes.as_ref(), all.as_slice());

        let rows = recorded(&fixture.usage, 1).await;
        let attempt = &rows[0].attempts[0];
        assert_eq!(attempt.outcome, "success");
        assert!(attempt.transfer.as_ref().unwrap().upstream_eof);
        fixture.stop().await;
    }
}

#[tokio::test]
async fn message_stop_uses_the_failover_target_provider_setting() {
    for enabled in [false, true] {
        let (fixture, release, closed, (head, all), _target) =
            lingering(Lingering::Whole, enabled).await;
        let failed = server(|_, _| async {
            Response::builder()
                .status(503)
                .header("content-type", "application/json")
                .body(full(
                    br#"{"error":{"type":"api_error","message":"fixture unavailable"}}"#.as_slice(),
                ))
                .unwrap()
        })
        .await;
        fixture.edit(Edit::SaveProvider {
            id: None,
            name: Some("Failing provider".into()),
            base_url: failed.url("/v1"),
            token: FIXTURE_KEY.into(),
            quota_access_token: None,
            search_only: None,
            end_on_message_stop: Some(!enabled),
        });
        let providers = fixture.gateway.view().providers;
        let target_id = providers[0].id.clone();
        fixture.edit(Edit::Reorder {
            ids: vec![providers[1].id.clone(), target_id.clone()],
        });
        fixture.edit(Edit::Mode {
            mode: "auto".into(),
        });
        let response = request(
            &fixture.gateway,
            MESSAGES_BLOCK.path,
            MESSAGES_BLOCK.request,
        )
        .await;
        let mut body = std::pin::pin!(response.into_body().collect());
        if !enabled {
            assert!(tokio::time::timeout(Duration::from_millis(400), &mut body)
                .await
                .is_err());
            assert!(!closed.load(Ordering::Acquire));
            release.notify_one();
        }
        let bytes = tokio::time::timeout(WAIT, body)
            .await
            .unwrap()
            .unwrap()
            .to_bytes();
        assert_eq!(
            bytes.as_ref(),
            if enabled {
                head.as_slice()
            } else {
                all.as_slice()
            }
        );
        let rows = recorded(&fixture.usage, 1).await;
        assert_eq!(rows[0].attempts.len(), 2);
        assert_eq!(
            rows[0].final_attempt().unwrap().provider.as_deref(),
            Some(target_id.as_str())
        );
        assert_eq!(
            rows[0]
                .final_attempt()
                .unwrap()
                .transfer
                .as_ref()
                .unwrap()
                .upstream_eof,
            !enabled
        );
        fixture.stop().await;
    }
}
