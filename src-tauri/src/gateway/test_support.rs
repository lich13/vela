//! Entry points on the gateway that only tests call, and the fixtures that
//! several test files share.
use super::{model_health::Models, replay::RequestHints, *};
use hyper::HeaderMap;
use std::io::Write;
impl Gateway {
    pub fn set_listener_port(&self, port: u16, expected: &str) -> Result<ListenerView> {
        let _lifecycle = self
            .0
            .registry
            .lifecycle
            .try_lock()
            .map_err(|_| AppError::new(code::GATEWAY_ACTIVE, "网关正在启停，请稍后再试"))?;
        let _mutation = self.0.registry.mutation.lock();
        for peer in self.0.registry.gateways() {
            let state = peer.inner.lock();
            for provider in &state.store.providers {
                for url in Self::endpoint_urls(&state.store, provider) {
                    model::base_url(&url, port)?;
                }
            }
        }
        self.0.registry.listener.set_port(port, expected)?;
        self.0.registry.update_ports()?;
        self.0.registry.changed();
        self.try_listener_view()
    }
    pub fn view(&self) -> View {
        self.try_view().expect("healthy test gateway")
    }
    pub fn listener_view(&self) -> ListenerView {
        self.try_listener_view().expect("healthy test listener")
    }
    pub async fn stop(&self) -> Result<View> {
        self.stop_checked(None).await
    }
    pub(crate) fn successful_user_agent(&self) -> Result<hyper::header::HeaderValue> {
        self.request_state()
            .store
            .successful_user_agent
            .as_deref()
            .and_then(|raw| hyper::header::HeaderValue::from_bytes(raw).ok())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                AppError::new(
                    code::STATE,
                    "请先通过该客户端完成一次正常请求，再运行供应商测试",
                )
            })
    }
    pub(super) fn remember(&self, id: &str, provider: &str) {
        self.remember_model(id, provider, None);
    }
    pub(crate) fn test_usage(&self) -> crate::usage::Service {
        self.0.usage.lock().clone().unwrap()
    }
    pub(crate) fn remember_test_user_agent(&self, raw: &[u8]) {
        self.remember_business_identity(testing::BusinessIdentity {
            user_agent: raw.to_vec(),
            codex: None,
        });
    }
    pub(crate) fn remember_test_identity(&self, headers: &HeaderMap) {
        if let Some(identity) = self.business_identity(headers) {
            self.remember_business_identity(identity);
        }
    }
}

/// A provider form as the interface sends it, with `fields` over a new
/// provider's.
pub(crate) fn form(fields: serde_json::Value) -> Edit {
    let mut edit = serde_json::json!({
        "op": "saveProvider",
        "id": null,
        "baseUrl": "https://one.example.invalid",
        "token": "fixture",
    });
    edit.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(edit).unwrap()
}

/// The stored form of a thread or window identifier.
pub(crate) fn digest(value: &str) -> String {
    crate::storage::digest(value.as_bytes())
}

/// Owned provider ids.
pub(crate) fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).into()).collect()
}

/// Tells a test that the value holding it was released.
pub(crate) struct NotifyOnDrop(pub std::sync::Arc<tokio::sync::Notify>);

impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

/// Polls until the condition holds and panics after three seconds.
pub(crate) async fn until(f: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("state did not settle");
}

/// Waits until no provider has a request in flight.
pub(crate) async fn settle(g: &Gateway) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while g.view().providers.iter().any(|p| p.active_requests != 0) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fixture request did not settle");
}

/// Like `settle`, with a five-second budget.
pub(crate) async fn settled(gateway: &Gateway) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while gateway
            .view()
            .providers
            .iter()
            .any(|provider| provider.active_requests != 0)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture request did not settle");
}

/// A queued provider with the given handoff and new-thread switches.
pub(crate) fn policy_provider(id: &str, handoff: bool, take: bool) -> Provider {
    Provider {
        id: id.into(),
        name: id.into(),
        base_url: format!("https://{id}.example.invalid/v1"),
        token: "fixture-token".into(),
        queued: true,
        version: format!("{id}-version"),
        max_concurrency: 0,
        max_rpm: 0,
        allowed_models: None,
        handoff_after_compaction: handoff,
        take_new_threads: take,
        end_on_message_stop: false,
        filter_count_tokens: false,
        buffer_complete_response: false,
        endless_mode: false,
        upstream_retry_seconds: 5,
        normal_retry_seconds: 3,
        risk_codes: None,
        retry_intensity: None,
        limit_retry_seconds: None,
        limit_rest_seconds: None,
    }
}

/// A queued provider at `base_url` under a fixed id and version.
pub(crate) fn provider_at(base_url: &str) -> Provider {
    Provider {
        base_url: base_url.into(),
        version: "fixture-version".into(),
        ..policy_provider("fixture-provider", false, false)
    }
}

/// Request hints that carry a compacted-window boundary.
pub(crate) fn compacted_hints(boundary: &str) -> RequestHints {
    RequestHints {
        compacted_window: Some(boundary.to_owned()),
        ..Default::default()
    }
}

/// The bytes in the given content encoding.
pub(crate) fn encoded(encoding: &str, raw: &[u8]) -> Vec<u8> {
    match encoding {
        "identity" => raw.to_vec(),
        "gzip" => {
            let mut encoder = flate2::write::GzEncoder::new(vec![], flate2::Compression::fast());
            encoder.write_all(raw).unwrap();
            encoder.finish().unwrap()
        }
        "deflate" => {
            let mut encoder = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::fast());
            encoder.write_all(raw).unwrap();
            encoder.finish().unwrap()
        }
        "zstd" => zstd::encode_all(raw, 1).unwrap(),
        _ => panic!("unsupported fixture encoding"),
    }
}

/// A Codex route to the provider with fresh circuits.
pub(crate) fn route(provider: Provider) -> Route {
    let client =
        Client::builder(TokioExecutor::new()).build(Connector::new(Duration::from_secs(1), 0));
    Route {
        dsh_models: None,
        model_map: Default::default(),
        delegate: Default::default(),
        grok: None,
        client_id: ClientId::Codex,
        provider,
        client,
        provider_circuit: Circuit::default(),
        model_circuits: Models::default(),
    }
}

/// Loopback upstreams and a Codex gateway configured to use them.
pub(crate) mod loopback {
    use crate::gateway::{
        model::Edit,
        replay::{self, WireBody},
        Gateway, Settings,
    };
    use bytes::Bytes;
    use futures_util::StreamExt;
    use http_body_util::BodyExt;
    use hyper::{body::Incoming, Request, Response, StatusCode};
    use hyper_util::{client::legacy::Client, rt::TokioExecutor};
    use std::{convert::Infallible, future::Future, sync::Arc, time::Duration};
    use tokio::sync::oneshot;

    pub(crate) const CREATED: &str = r#"{"type":"response.created","response":{"id":"fixture-response","model":"fixture-model"}}"#;

    pub(crate) const COMPLETED: &str = r#"{"type":"response.completed","response":{"id":"fixture-response","model":"fixture-model","status":"completed"}}"#;

    pub(crate) const TEMPORARY_429: &str = r#"{"type":"error","error":{"code":"USAGE_LIMIT_EXCEEDED","message":"try again later","status":429}}"#;

    pub(crate) const CREATE: &str = r#"{"type":"response.create","model":"fixture-model","input":[{"role":"user","content":"fixture"}]}"#;

    pub(crate) struct LoopbackServer {
        pub(crate) port: u16,
        pub(crate) stop: Option<oneshot::Sender<()>>,
        pub(crate) task: Option<tokio::task::JoinHandle<()>>,
    }

    impl LoopbackServer {
        pub(crate) async fn stop(mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(task) = self.task.take() {
                let _ = task.await;
            }
        }
    }

    impl Drop for LoopbackServer {
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(task) = self.task.take() {
                task.abort();
            }
        }
    }

    pub(crate) async fn server<F, Fut>(handler: F) -> LoopbackServer
    where
        F: Fn(Request<Incoming>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response<WireBody>> + Send + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            tokio::select! {
                _ = stopped => {}
                _ = accept(listener, handler) => {}
            }
        });
        LoopbackServer {
            port,
            stop: Some(stop),
            task: Some(task),
        }
    }

    /// An upstream that serves until the test's runtime ends, by its port.
    pub(crate) async fn serve<F, Fut>(handler: F) -> u16
    where
        F: Fn(Request<Incoming>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response<WireBody>> + Send + 'static,
    {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(accept(listener, handler));
        port
    }

    async fn accept<F, Fut>(listener: tokio::net::TcpListener, handler: F)
    where
        F: Fn(Request<Incoming>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response<WireBody>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        while let Ok((stream, _)) = listener.accept().await {
            let handler = handler.clone();
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        hyper_util::rt::TokioIo::new(stream),
                        hyper::service::service_fn(move |request| {
                            let handler = handler.clone();
                            async move { Ok::<_, Infallible>(handler(request).await) }
                        }),
                    )
                    .with_upgrades()
                    .await;
            });
        }
    }

    pub(crate) fn json_response(
        status: StatusCode,
        body: impl Into<Bytes>,
        retry_after: Option<&str>,
    ) -> Response<WireBody> {
        let mut response = Response::builder()
            .status(status)
            .header("content-type", "application/json");
        if let Some(retry_after) = retry_after {
            response = response.header("retry-after", retry_after);
        }
        response.body(replay::full(body.into())).unwrap()
    }

    /// A gateway over `urls` in automatic mode. Refused loopback ports stand
    /// for failed providers here, not for a dead network, so the network
    /// probe is off.
    pub(crate) async fn fixture(urls: Vec<String>) -> (tempfile::TempDir, Gateway) {
        build(urls, false).await
    }

    /// Like `fixture`, with the network probe as a new store has it.
    pub(crate) async fn probing_fixture(urls: Vec<String>) -> (tempfile::TempDir, Gateway) {
        build(urls, true).await
    }

    async fn build(urls: Vec<String>, network_probe: bool) -> (tempfile::TempDir, Gateway) {
        let dir = tempfile::tempdir().unwrap();
        let first = urls
            .first()
            .cloned()
            .unwrap_or_else(|| "https://fixture.example.invalid/v1".into());
        std::fs::write(
        dir.path().join("config.toml"),
        format!(
            "model_provider='custom'\n[model_providers.custom]\nbase_url = {}\nexperimental_bearer_token = \"fixture-token\"\nwire_api='responses'\nsupports_websockets=false\n",
            serde_json::to_string(&first).unwrap()
        ),
    )
    .unwrap();
        let gateway = Gateway::new(dir.path().to_path_buf()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut settings = gateway.view().settings;
        settings.port = port;
        if !network_probe {
            settings.network_probe = false;
        }
        update(&gateway, &dir, Edit::Settings { settings });
        for url in urls {
            update(
                &gateway,
                &dir,
                super::form(serde_json::json!({"baseUrl": url, "token": "fixture-token"})),
            );
        }
        update(
            &gateway,
            &dir,
            Edit::Mode {
                mode: "auto".into(),
            },
        );
        (dir, gateway)
    }

    pub(crate) fn update(gateway: &Gateway, dir: &tempfile::TempDir, edit: Edit) {
        gateway
            .edit(edit, &gateway.view().revision, dir.path())
            .unwrap();
    }

    pub(crate) fn configure(gateway: &Gateway, dir: &tempfile::TempDir) {
        update(
            gateway,
            dir,
            Edit::Settings {
                settings: Settings {
                    max_retries: 2,
                    capacity_retry_seconds: 1,
                    websocket_retry_seconds: 1,
                    queue_seconds: 1,
                    rate_limit_seconds: 1,
                    first_byte_seconds: 3,
                    idle_seconds: 3,
                    failure_threshold: 10,
                    transient_failure_threshold: 10,
                    ..gateway.view().settings
                },
            },
        );
        for provider in gateway.view().providers {
            update(
                gateway,
                dir,
                Edit::RpmProvider {
                    id: provider.id,
                    max_rpm: 100,
                },
            );
        }
    }

    pub(crate) async fn start(gateway: &Gateway, dir: &tempfile::TempDir) {
        gateway
            .start(&gateway.view().revision, dir.path())
            .await
            .unwrap();
    }

    pub(crate) async fn request(
        gateway: &Gateway,
        session: &str,
        body: impl AsRef<[u8]>,
    ) -> Response<Incoming> {
        let client: Client<_, WireBody> = Client::builder(TokioExecutor::new()).build(
            crate::gateway::connector::Connector::new(Duration::from_secs(3), 0),
        );
        let (token, port) = {
            let state = gateway.0.inner.lock();
            (state.store.local_token.clone(), state.store.settings.port)
        };
        let req = Request::builder()
            .method("POST")
            .uri(format!("http://127.0.0.1:{port}/v1/responses"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("session_id", session)
            .body(replay::full(Bytes::copy_from_slice(body.as_ref())))
            .unwrap();
        client.request(req).await.unwrap()
    }

    /// Posts `body` to `path` on the gateway under the thread id.
    pub(crate) async fn thread_request(
        gateway: &Gateway,
        path: &str,
        thread: &str,
        body: Vec<u8>,
    ) -> Response<Incoming> {
        let client: Client<_, WireBody> = Client::builder(TokioExecutor::new()).build(
            crate::gateway::connector::Connector::new(Duration::from_secs(3), 0),
        );
        let (token, port) = {
            let state = gateway.0.inner.lock();
            (state.store.local_token.clone(), state.store.settings.port)
        };
        let request = Request::builder()
            .method("POST")
            .uri(format!("http://127.0.0.1:{port}{path}"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header("thread-id", thread)
            .body(replay::full(body))
            .unwrap();
        client.request(request).await.unwrap()
    }

    pub(crate) async fn response_bytes(response: Response<Incoming>) -> Bytes {
        response.into_body().collect().await.unwrap().to_bytes()
    }

    pub(crate) async fn response_value(response: Response<Incoming>) -> serde_json::Value {
        serde_json::from_slice(&response_bytes(response).await).unwrap()
    }

    pub(crate) fn provider_ids(gateway: &Gateway) -> Vec<String> {
        gateway
            .view()
            .providers
            .iter()
            .map(|provider| provider.id.clone())
            .collect()
    }

    pub(crate) async fn ws_client(gateway: &Gateway, session: &str) -> yawc::TcpWebSocket {
        let (token, port) = {
            let state = gateway.0.inner.lock();
            (state.store.local_token.clone(), state.store.settings.port)
        };
        yawc::WebSocket::connect(
            format!("ws://127.0.0.1:{port}/v1/responses?future=keep")
                .parse()
                .unwrap(),
        )
        .with_options(yawc::Options::default().with_balanced_compression())
        .with_request(
            yawc::HttpRequest::builder()
                .header("authorization", format!("Bearer {token}"))
                .header("session_id", session),
        )
        .await
        .unwrap()
    }

    pub(crate) async fn next_frame(ws: &mut yawc::TcpWebSocket) -> yawc::Frame {
        tokio::time::timeout(Duration::from_secs(8), ws.next())
            .await
            .expect("fixture WebSocket timed out")
            .expect("fixture WebSocket closed")
    }
}

/// Loopback upstreams whose accept loop and connections end with their handle.
pub(crate) mod scoped {
    use crate::gateway::replay::WireBody;
    use hyper::{body::Incoming, server::conn::http1, Request, Response};
    use parking_lot::Mutex;
    use std::{convert::Infallible, future::Future, sync::Arc};
    use tokio::task::JoinHandle;

    /// Tasks aborted together; a closed scope starts no more.
    #[derive(Clone, Default)]
    pub(crate) struct TaskScope(Arc<Mutex<(bool, Vec<JoinHandle<()>>)>>);

    impl TaskScope {
        pub(crate) fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) {
            let mut state = self.0.lock();
            if !state.0 {
                state.1.push(tokio::spawn(future));
            }
        }

        pub(crate) fn close(&self) {
            let mut state = self.0.lock();
            state.0 = true;
            for task in state.1.drain(..) {
                task.abort();
            }
        }
    }

    pub(crate) struct LoopbackServer {
        pub(crate) port: u16,
        tasks: TaskScope,
    }

    impl LoopbackServer {
        pub(crate) fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }
    }

    impl Drop for LoopbackServer {
        fn drop(&mut self) {
            self.tasks.close();
        }
    }

    pub(crate) async fn server<F, Fut>(handler: F) -> LoopbackServer
    where
        F: Fn(Request<Incoming>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response<WireBody>> + Send + 'static,
    {
        server_with(http1::Builder::new(), move |request, _| handler(request)).await
    }

    /// Like `server`; the handler also gets the scope, for tasks of its own
    /// that must end with the server.
    pub(crate) async fn server_in_scope<F, Fut>(handler: F) -> LoopbackServer
    where
        F: Fn(Request<Incoming>, TaskScope) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response<WireBody>> + Send + 'static,
    {
        server_with(http1::Builder::new(), handler).await
    }

    /// Like `server_in_scope`, serving connections as `http` is set up.
    pub(crate) async fn server_with<F, Fut>(http: http1::Builder, handler: F) -> LoopbackServer
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
                let http = http.clone();
                accept_tasks.spawn(async move {
                    let service = hyper::service::service_fn(move |request| {
                        let handler = handler.clone();
                        let tasks = connection_tasks.clone();
                        async move { Ok::<_, Infallible>(handler(request, tasks).await) }
                    });
                    let _ = http
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .with_upgrades()
                        .await;
                });
            }
        });
        LoopbackServer { port, tasks }
    }
}

/// A gateway with a usage service attached, edited and started from the
/// client home it was given. Each test file adds the constructors it needs.
pub(crate) struct UsageFixture {
    pub(crate) gateway: Gateway,
    pub(crate) usage: crate::usage::Service,
    pub(crate) home: std::path::PathBuf,
    pub(crate) _dir: tempfile::TempDir,
}

impl UsageFixture {
    /// The fixture of `gateway`, recording usage under `dir`.
    pub(crate) fn over(dir: tempfile::TempDir, home: std::path::PathBuf, gateway: Gateway) -> Self {
        let usage = crate::usage::Service::new(&dir.path().join("usage-data"));
        assert!(usage.state().error.is_none());
        gateway.set_usage(usage.clone());
        Self {
            gateway,
            usage,
            home,
            _dir: dir,
        }
    }

    pub(crate) fn edit(&self, edit: Edit) -> View {
        self.gateway
            .edit(edit, &self.gateway.view().revision, &self.home)
            .unwrap()
    }

    pub(crate) async fn start(&self) {
        self.gateway
            .start(&self.gateway.view().revision, &self.home)
            .await
            .unwrap();
    }

    pub(crate) async fn stop(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.gateway.stop())
            .await
            .unwrap()
            .unwrap();
    }
}

/// A gateway driven over raw loopback TCP from the client side.
pub(crate) mod downstream {
    use crate::{
        events::Service as EventService,
        gateway::{Edit, Gateway},
        usage::{
            model::{Filter as UsageFilter, Record as UsageRecord},
            Service as UsageService,
        },
    };
    use std::io;
    use tokio::{io::AsyncReadExt, net::TcpStream};

    pub(crate) const REQUEST_BODY: &[u8] =
        br#"{"model":"fixture-model","input":"fixture","stream":true}"#;

    pub(crate) fn edit(gateway: &Gateway, dir: &tempfile::TempDir, edit: Edit) {
        gateway
            .edit(edit, &gateway.view().revision, dir.path())
            .unwrap();
    }

    pub(crate) fn local_auth(gateway: &Gateway) -> (u16, String) {
        let port = gateway.view().settings.port;
        let token = gateway.0.inner.lock().store.local_token.clone();
        (port, token)
    }

    pub(crate) fn attach_observers(
        dir: &tempfile::TempDir,
        gateway: &Gateway,
    ) -> (UsageService, EventService) {
        let usage = UsageService::new(&dir.path().join("usage"));
        let events = EventService::new(&dir.path().join("events"));
        gateway.set_usage(usage.clone());
        gateway.set_diagnostics(events.clone());
        (usage, events)
    }

    pub(crate) fn usage_records(service: &UsageService) -> Vec<UsageRecord> {
        service
            .query(|store| {
                let page = store.logs(&UsageFilter::default())?;
                assert_eq!(page.total, page.rows.len() as u64);
                Ok(page.rows)
            })
            .unwrap()
    }

    pub(crate) async fn read_http_request(stream: &mut TcpStream) -> io::Result<()> {
        let mut header = Vec::new();
        let mut byte = [0u8; 1];
        while !header.windows(4).any(|window| window == b"\r\n\r\n") {
            if stream.read(&mut byte).await? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "request ended in headers",
                ));
            }
            header.push(byte[0]);
            if header.len() > 32 * 1024 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "request headers too large",
                ));
            }
        }
        let header = String::from_utf8_lossy(&header).to_ascii_lowercase();
        let length = header
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        if length != 0 {
            let mut body = vec![0; length];
            stream.read_exact(&mut body).await?;
        }
        Ok(())
    }

    pub(crate) async fn downstream_websocket(gateway: &Gateway) -> yawc::TcpWebSocket {
        let (port, token) = local_auth(gateway);
        yawc::WebSocket::connect(
            format!("ws://127.0.0.1:{port}/v1/responses")
                .parse()
                .unwrap(),
        )
        .with_options(yawc::Options::default().with_balanced_compression())
        .with_request(
            yawc::HttpRequest::builder().header("authorization", format!("Bearer {token}")),
        )
        .await
        .unwrap()
    }
}
