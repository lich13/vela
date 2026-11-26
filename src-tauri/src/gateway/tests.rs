// End-to-end transport and configuration tests live here; all credentials are fixtures.
#[path = "failover_diagnostics_tests.rs"]
mod failover_diagnostics_tests;
use super::*;
use crate::gateway::test_support::form;
fn capacity_message(status: hyper::StatusCode, body: &[u8]) -> bool {
    failure_policy::decision(
        Some(status.as_u16()),
        &failure_policy::test_support::http(status.as_u16(), &hyper::HeaderMap::new(), body).0,
    )
    .temporary
}
#[test]
fn codex_capacity_messages_are_detected_without_matching_normal_errors() {
    assert!(capacity_message(
        hyper::StatusCode::TOO_MANY_REQUESTS,
        b"any body"
    ));
    assert!(capacity_message(
        hyper::StatusCode::SERVICE_UNAVAILABLE,
        b"Selected model is at capacity. Please try a different model."
    ));
    assert!(capacity_message(
        hyper::StatusCode::BAD_GATEWAY,
        "模型当前限流".as_bytes()
    ));
    assert!(!capacity_message(
        hyper::StatusCode::BAD_REQUEST,
        b"model is at capacity"
    ));
}

#[test]
fn provider_secrets_never_appear_in_view() {
    let temp = tempfile::tempdir().unwrap();
    let g = Gateway::new(temp.path().to_path_buf()).unwrap();
    let update = |edit| g.edit(edit, &g.view().revision, temp.path()).unwrap();
    update(form(serde_json::json!({
        "baseUrl": "https://example.test/custom/v1",
        "token": "private-fixture-token",
        "quotaAccessToken": "fixture-account-access-token",
    })));
    let json = serde_json::to_string(&g.view()).unwrap();
    assert!(!json.contains("private-fixture"));
    assert!(!json.contains("fixture-account-access-token"));
    assert!(g.view().providers[0].has_quota_access_token);
    assert!(!json.contains("localToken"));
}
#[test]
fn url_suffix_and_hop_headers() {
    assert_eq!(
        forward::target(
            "https://example.test/api/v1/",
            &"/v1/responses/compact?x=1&x=2".parse().unwrap()
        )
        .unwrap()
        .to_string(),
        "https://example.test/api/v1/responses/compact?x=1&x=2"
    );
    let mut h = hyper::HeaderMap::new();
    h.insert("connection", "x-private, keep-alive".parse().unwrap());
    h.insert("x-private", "secret".parse().unwrap());
    h.append("x-repeat", "one".parse().unwrap());
    h.append("x-repeat", "two".parse().unwrap());
    forward::clean_headers(&mut h, false);
    assert!(!h.contains_key("x-private"));
    assert_eq!(h.get_all("x-repeat").iter().count(), 2);
}

use super::replay::{full, WireBody};
pub(super) use crate::gateway::test_support::loopback::serve as server;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, StreamBody};
use hyper::{
    body::{Frame, Incoming},
    Request, Response,
};
use hyper_util::rt::TokioIo;
use parking_lot::Mutex;
use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};
async fn fixture(urls: Vec<String>) -> (tempfile::TempDir, Gateway) {
    let t = tempfile::tempdir().unwrap();
    let first = urls
        .first()
        .cloned()
        .unwrap_or_else(|| "https://example.test/v1".into());
    let config=format!("# fixture\nmodel_provider='custom'\n[model_providers.custom]\nbase_url = {}\nexperimental_bearer_token = \"upstream-fixture-token\"\nwire_api='responses'\nsupports_websockets=false\n",serde_json::to_string(&first).unwrap());
    std::fs::write(t.path().join("config.toml"), config).unwrap();
    let g = Gateway::new(t.path().to_path_buf()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    update(
        &g,
        &t,
        Edit::Settings {
            settings: Settings {
                port,
                network_probe: false,
                ..Default::default()
            },
        },
    );
    for url in urls {
        update(
            &g,
            &t,
            form(serde_json::json!({"baseUrl": url, "token": "upstream-fixture-token"})),
        );
    }
    std::fs::write(t.path().join("auth.json"), "unchanged-auth").unwrap();
    (t, g)
}

#[tokio::test]
async fn stopping_releases_listener_before_restart_returns_to_caller() {
    let (t, g) = fixture(vec!["https://example.test/v1".into()]).await;
    let config = std::fs::read(t.path().join("config.toml")).ok();
    let auth = std::fs::read(t.path().join("auth.json")).ok();
    for _ in 0..4 {
        start(&g, &t).await;
        g.stop().await.unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", g.view().settings.port))
            .await
            .expect("stop must release the listening port before returning");
        drop(listener);
        assert_eq!(std::fs::read(t.path().join("config.toml")).ok(), config);
        assert_eq!(std::fs::read(t.path().join("auth.json")).ok(), auth);
    }
}

#[tokio::test]
async fn codex_capacity_429_fails_over_to_next_provider_before_returning() {
    let first_hits = Arc::new(AtomicUsize::new(0));
    let first_counter = first_hits.clone();
    let first = server(move |_| {
        first_counter.fetch_add(1, Ordering::SeqCst);
        async {
            Response::builder()
                .status(429)
                .body(full("Selected model is at capacity"))
                .unwrap()
        }
    })
    .await;
    let second = server(|_| async { Response::new(full("backup-ok")) }).await;
    let (t, g) = fixture(vec![
        format!("http://127.0.0.1:{first}/v1"),
        format!("http://127.0.0.1:{second}/v1"),
    ])
    .await;
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    start(&g, &t).await;
    let body = br#"{"model":"gpt-test","input":"preserve"}"#.to_vec();
    let response = request(
        &g,
        "/v1/responses",
        body,
        vec![("content-type", "application/json")],
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "backup-ok"
    );
    assert_eq!(first_hits.load(Ordering::SeqCst), 1);
    let provider = &g.view().providers[0];
    let model = provider
        .model_health
        .iter()
        .find(|entry| entry.model == "gpt-test")
        .expect("known model must have isolated health");
    assert_eq!(
        model.health.cooldown_reason.as_deref(),
        Some("capacity_retry")
    );
    assert_eq!(provider.health.cooldown_reason, None);
    g.stop().await.unwrap();
}

#[tokio::test]
async fn codex_single_provider_capacity_waits_and_never_opens() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let upstream = server(move |_| {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        async move {
            if n == 0 {
                Response::builder()
                    .status(429)
                    .body(full("Selected model is at capacity"))
                    .unwrap()
            } else {
                Response::new(full("recovered"))
            }
        }
    })
    .await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{upstream}/v1")]).await;
    let mut settings = g.view().settings;
    settings.capacity_retry_seconds = 1;
    settings.max_retries = 1;
    update(&g, &t, Edit::Settings { settings });
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    start(&g, &t).await;
    let response = request(
        &g,
        "/v1/responses",
        br#"{"model":"gpt-test"}"#.to_vec(),
        vec![("content-type", "application/json")],
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "recovered"
    );
    let provider = &g.view().providers[0];
    assert_eq!(provider.health.state, super::circuit::CircuitState::Closed);

    assert_eq!(hits.load(Ordering::SeqCst), 2);
    g.stop().await.unwrap();
}

fn update(g: &Gateway, t: &tempfile::TempDir, edit: Edit) {
    g.edit(edit, &g.view().revision, t.path()).unwrap();
}
async fn start(g: &Gateway, t: &tempfile::TempDir) {
    g.start(&g.view().revision, t.path()).await.unwrap();
}
async fn request(
    g: &Gateway,
    path: &str,
    body: Vec<u8>,
    headers: Vec<(&str, &str)>,
) -> Response<Incoming> {
    let client: HttpClient =
        Client::builder(TokioExecutor::new()).build(Connector::new(Duration::from_secs(2), 0));
    let (token, port) = {
        let s = g.0.inner.lock();
        (s.store.local_token.clone(), s.store.settings.port)
    };
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("http://127.0.0.1:{port}{path}"))
        .header("authorization", format!("Bearer {token}"));
    for (k, v) in headers {
        req = req.header(k, v);
    }
    client.request(req.body(full(body)).unwrap()).await.unwrap()
}
pub(super) type ResetSender = hyper::client::conn::http1::SendRequest<WireBody>;

// Dropping a response by itself closes with FIN. To model a full cancellation,
// abort and await the socket-owning connection task before dropping the response.
#[allow(deprecated)]
pub(super) async fn request_with_reset(
    g: &Gateway,
    path: &str,
    body: Vec<u8>,
    headers: Vec<(&str, &str)>,
) -> (Response<Incoming>, ResetSender, tokio::task::JoinHandle<()>) {
    let (token, port) = {
        let state = g.0.inner.lock();
        (state.store.local_token.clone(), state.store.settings.port)
    };
    let socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    socket.set_linger(Some(Duration::ZERO)).unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .unwrap();
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    sender.ready().await.unwrap();
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("host", format!("127.0.0.1:{port}"))
        .header("authorization", format!("Bearer {token}"));
    for (key, value) in headers {
        req = req.header(key, value);
    }
    let response = sender
        .send_request(req.body(full(body)).unwrap())
        .await
        .unwrap();
    (response, sender, task)
}

pub(super) async fn reset_and_drop<T>(
    task: tokio::task::JoinHandle<()>,
    sender: ResetSender,
    value: T,
) {
    task.abort();
    let _ = task.await;
    drop(sender);
    drop(value);
}

#[allow(deprecated)]
pub(super) fn set_reset_on_drop(stream: &tokio::net::TcpStream) {
    stream.set_linger(Some(Duration::ZERO)).unwrap();
}

#[tokio::test]
async fn unknown_paths_query_duplicate_headers_and_original_payloads() {
    let seen = Arc::new(Mutex::new(vec![]));
    let copy = seen.clone();
    let port = server(move |req| {
        let copy = copy.clone();
        async move {
            let (parts, body) = req.into_parts();
            let bytes = body.collect().await.unwrap().to_bytes();
            assert_eq!(
                parts.headers["authorization"],
                "Bearer upstream-fixture-token"
            );
            assert!(!parts.headers.contains_key("proxy-authorization"));
            copy.lock().push((
                parts.uri.to_string(),
                parts.headers.get_all("x-repeat").iter().count(),
                bytes.clone(),
            ));
            Response::builder()
                .header("x-repeat", "one")
                .header("x-repeat", "two")
                .body(full(bytes))
                .unwrap()
        }
    })
    .await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}/sub/v1")]).await;
    start(&g, &t).await;
    use std::io::Write;
    let mut gzip = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    gzip.write_all(b"{\"unknown\":true,\"model\":\"untouched\"}")
        .unwrap();
    let cases=vec![(vec![0,255,0,128,10],"application/octet-stream","identity"),
        (b"--boundary\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\nraw\0\xff\r\n--boundary--\r\n".to_vec(),"multipart/form-data; boundary=boundary","identity"),
        (gzip.finish().unwrap(),"application/json","gzip"),
        (vec![7u8;3*1024*1024],"application/octet-stream","identity")];
    for (data, content, encoding) in cases {
        let response = request(
            &g,
            "/v1/future/path?x=1&x=2&raw=%2F",
            data.clone(),
            vec![
                ("content-type", content),
                ("content-encoding", encoding),
                ("x-repeat", "a"),
                ("x-repeat", "b"),
            ],
        )
        .await;
        assert_eq!(response.headers().get_all("x-repeat").iter().count(), 2);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            data
        );
    }
    assert!(seen
        .lock()
        .iter()
        .all(|(path, headers, _)| path == "/sub/v1/future/path?x=1&x=2&raw=%2F" && *headers == 2));
    assert_eq!(std::fs::read_dir(g.0.spool.path()).unwrap().count(), 0);
    g.stop().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(t.path().join("auth.json")).unwrap(),
        "unchanged-auth"
    );
}
#[tokio::test]
async fn queue_four_attempt_limit_terminal_errors_and_original_last_error() {
    let hits = Arc::new(Mutex::new(vec![]));
    let mut urls = vec![];
    for i in 0..5 {
        let hits = hits.clone();
        let port = server(move |req| {
            let hits = hits.clone();
            async move {
                let path = req.uri().path().to_owned();
                let _ = req.into_body().collect().await;
                hits.lock().push(i);
                Response::builder()
                    .status(if path.ends_with("terminal") { 422 } else { 503 })
                    .header("x-source", i.to_string())
                    .body(full(format!("upstream-{i}")))
                    .unwrap()
            }
        })
        .await;
        urls.push(format!("http://127.0.0.1:{port}"));
    }
    let (t, g) = fixture(urls).await;
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    start(&g, &t).await;
    let response = request(&g, "/v1/unknown", vec![], vec![]).await;
    assert_eq!(response.status(), 503);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "upstream-3"
    );
    assert_eq!(*hits.lock(), vec![0, 1, 2, 3]);
    hits.lock().clear();
    let response = request(&g, "/v1/terminal", vec![], vec![]).await;
    assert_eq!(response.status(), 422);
    response.into_body().collect().await.unwrap();
    assert_eq!(*hits.lock(), vec![0]);
    assert_eq!(g.view().providers[0].health.failures, 0);
    g.stop().await.unwrap();
}
#[tokio::test]
async fn streaming_first_byte_timeout_no_retry_and_cancel_releases_activity() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits2 = hits.clone();
    let slow=server(|_|async{Response::builder().header("content-type","text/event-stream").body(StreamBody::new(async_stream::try_stream!{
        tokio::time::sleep(Duration::from_secs(2)).await;yield Frame::data(Bytes::from_static(b"data: late\n\n"));
    }).boxed_unsync()).unwrap()}).await;
    let fast=server(move|_|{hits2.fetch_add(1,Ordering::Relaxed);async{Response::builder().header("content-type","text/event-stream").body(StreamBody::new(async_stream::try_stream!{
        yield Frame::data(Bytes::from_static(b"data: {\"response\":{\"id\":\"resp_test\"}}\n\n"));
        tokio::time::sleep(Duration::from_millis(20)).await;yield Frame::data(Bytes::from_static(b"data: tail\n\n"));
    }).boxed_unsync()).unwrap()}}).await;
    let (t, g) = fixture(vec![
        format!("http://127.0.0.1:{slow}"),
        format!("http://127.0.0.1:{fast}"),
    ])
    .await;
    update(
        &g,
        &t,
        Edit::Settings {
            settings: Settings {
                first_byte_seconds: 1,
                ..g.view().settings
            },
        },
    );
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    start(&g, &t).await;
    let timeout_started = Instant::now();
    let response = request(&g, "/v1/responses", vec![], vec![]).await;
    assert_eq!(response.status(), 200);
    assert!(response.into_body().collect().await.is_err());
    let timeout_elapsed = timeout_started.elapsed();
    assert!(timeout_elapsed >= Duration::from_millis(800));
    assert!(timeout_elapsed < Duration::from_secs(3));
    assert_eq!(
        hits.load(Ordering::Relaxed),
        0,
        "a dispatched request that times out before its first event must not be retried"
    );

    // This next case is a connection failure; let its deadline win on Windows.
    update(
        &g,
        &t,
        Edit::Settings {
            settings: Settings {
                connect_seconds: 1,
                first_byte_seconds: 5,
                ..g.view().settings
            },
        },
    );
    let closed_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_port = closed_listener.local_addr().unwrap().port();
    drop(closed_listener);
    let first = g.view().providers[0].id.clone();
    update(
        &g,
        &t,
        form(serde_json::json!({
            "id": first,
            "baseUrl": format!("http://127.0.0.1:{closed_port}/v1"),
            "token": "",
        })),
    );
    let response = request(&g, "/v1/responses", vec![], vec![]).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "data: {\"response\":{\"id\":\"resp_test\"}}\n\ndata: tail\n\n"
    );
    assert_eq!(hits.load(Ordering::Relaxed), 1);

    let response = request(
        &g,
        "/v1/responses",
        b"{\"previous_response_id\":\"resp_test\"}".to_vec(),
        vec![("content-type", "application/json")],
    )
    .await;
    response.into_body().collect().await.unwrap();

    assert_eq!(hits.load(Ordering::Relaxed), 2);
    let never=server(|_|async{Response::builder().header("content-type","text/event-stream").body(StreamBody::new(async_stream::try_stream!{
        yield Frame::data(Bytes::from_static(b"data: first\n\n"));tokio::time::sleep(Duration::from_secs(20)).await;yield Frame::data(Bytes::new());
    }).boxed_unsync()).unwrap()}).await;
    update(
        &g,
        &t,
        form(serde_json::json!({
            "id": first,
            "baseUrl": format!("http://127.0.0.1:{never}"),
            "token": "",
        })),
    );
    let (mut response, sender, task) =
        request_with_reset(&g, "/v1/responses", vec![], vec![]).await;
    response.body_mut().frame().await.unwrap().unwrap();
    reset_and_drop(task, sender, response).await;
    for _ in 0..20 {
        if g.view().active_connections == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(g.view().active_connections, 0);
    assert_eq!(hits.load(Ordering::Relaxed), 2);
    g.stop().await.unwrap();
}
#[tokio::test]
async fn port_conflict_startup_recovery_and_single_candidate_breaker() {
    let port = server(|_| async {
        Response::builder()
            .status(502)
            .header("retry-after", "120")
            .body(full("upstream-502"))
            .unwrap()
    })
    .await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}")]).await;
    let mut settings = g.view().settings;
    settings.failure_threshold = 1;
    settings.transient_failure_threshold = 1;
    settings.max_retries = 0;
    settings.queue_seconds = 1;
    update(&g, &t, Edit::Settings { settings });
    let occupied = tokio::net::TcpListener::bind(("127.0.0.1", g.view().settings.port))
        .await
        .unwrap();
    assert!(g.start(&g.view().revision, t.path()).await.is_err());
    assert!(takeover::tests::read(t.path()).is_ok());
    drop(occupied);
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    start(&g, &t).await;
    let body = br#"{"model":"fixture-model"}"#.to_vec();
    let first_status = tokio::time::timeout(
        Duration::from_secs(5),
        request(
            &g,
            "/v1/a",
            body.clone(),
            vec![("content-type", "application/json")],
        ),
    )
    .await
    .expect("upstream failure response timed out")
    .status();
    assert_eq!(first_status, 502);
    let cooldown_status = tokio::time::timeout(
        Duration::from_secs(5),
        request(
            &g,
            "/v1/a",
            body,
            vec![("content-type", "application/json")],
        ),
    )
    .await
    .expect("cooldown response timed out")
    .status();
    assert_eq!(cooldown_status, 503);
    let view = g.view();
    assert_eq!(view.providers[0].health.failures, 0);
    let health = view.providers[0]
        .model_health
        .iter()
        .find(|entry| entry.model == "fixture-model")
        .expect("known model must have isolated health")
        .health
        .clone();
    assert!(health.retry_in >= 118);
    assert_eq!(health.failures, 1);
    assert_eq!(health.state, super::circuit::CircuitState::Closed);
    assert_eq!(
        health.cooldown_reason.as_deref(),
        Some("single_provider_protected")
    );

    g.stop().await.unwrap();
    let (_, pair) = takeover::tests::read(t.path()).unwrap();
    takeover::tests::attach(
        t.path(),
        t.path(),
        g.view().settings.port,
        "fixture-crash",
        pair,
        &takeover::tests::read(t.path()).unwrap().0,
    )
    .unwrap();
    let recovered = Gateway::new(t.path().to_path_buf()).unwrap();
    assert!(!recovered.guarded_home());
    assert!(takeover::tests::read(t.path()).is_ok());
}

#[tokio::test]
async fn priority_recovers_on_next_request_and_unknown_cursor_never_retries() {
    let hits = Arc::new(AtomicUsize::new(0));
    let x = hits.clone();
    let first = server(move |_| {
        let n = x.fetch_add(1, Ordering::Relaxed);
        async move {
            Response::builder()
                .status(if n == 0 { 503 } else { 200 })
                .body(full("first"))
                .unwrap()
        }
    })
    .await;
    let second = server(|_| async { Response::new(full("second")) }).await;
    let (t, g) = fixture(vec![
        format!("http://127.0.0.1:{first}"),
        format!("http://127.0.0.1:{second}"),
    ])
    .await;
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    start(&g, &t).await;
    assert_eq!(
        request(&g, "/v1/new", vec![], vec![])
            .await
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
        "second"
    );
    assert_eq!(
        request(&g, "/v1/new", vec![], vec![])
            .await
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes(),
        "first"
    );
    hits.store(0, Ordering::Relaxed);
    let r = request(
        &g,
        "/v1/responses",
        b"{\"previous_response_id\":\"unknown\"}".to_vec(),
        vec![("content-type", "application/json")],
    )
    .await;
    assert_eq!(r.status(), 503);
    assert_eq!(hits.load(Ordering::Relaxed), 1);
    g.stop().await.unwrap();
}
#[tokio::test]
async fn idle_timeout_after_commit_never_splices_backup_and_slow_consumer_is_bounded() {
    let first=server(|_|async{Response::builder().header("content-type","text/event-stream").body(StreamBody::new(async_stream::try_stream!{
        yield Frame::data(Bytes::from_static(b"data: original\n\n"));tokio::time::sleep(Duration::from_secs(3)).await;yield Frame::data(Bytes::from_static(b"data: late\n\n"));
    }).boxed_unsync()).unwrap()}).await;
    let hits = Arc::new(AtomicUsize::new(0));
    let copy = hits.clone();
    let second = server(move |_| {
        copy.fetch_add(1, Ordering::Relaxed);
        async { Response::new(full("backup")) }
    })
    .await;
    let (t, g) = fixture(vec![
        format!("http://127.0.0.1:{first}"),
        format!("http://127.0.0.1:{second}"),
    ])
    .await;
    update(
        &g,
        &t,
        Edit::Settings {
            settings: Settings {
                idle_seconds: 1,
                ..g.view().settings
            },
        },
    );
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    start(&g, &t).await;
    let mut r = request(&g, "/v1/stream", vec![], vec![]).await;
    assert_eq!(
        r.body_mut()
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap(),
        "data: original\n\n"
    );
    assert!(r.body_mut().frame().await.unwrap().is_err());
    assert_eq!(hits.load(Ordering::Relaxed), 0);
    g.stop().await.unwrap();

    let produced = Arc::new(AtomicUsize::new(0));
    let count = produced.clone();
    let port=server(move|_|{let count=count.clone();async move{Response::builder().header("content-type","application/octet-stream").body(StreamBody::new(async_stream::try_stream!{
        for _ in 0..1024{count.fetch_add(1,Ordering::Relaxed);yield Frame::data(Bytes::from(vec![1u8;64*1024]));}
    }).boxed_unsync()).unwrap()}}).await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}")]).await;
    start(&g, &t).await;
    let r = request(&g, "/v1/binary", vec![], vec![]).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        produced.load(Ordering::Relaxed) < 1024,
        "downstream backpressure must bound upstream reads"
    );
    drop(r);
    g.stop().await.unwrap();
}
#[tokio::test]
#[ignore = "Uses the user's installed Codex CLI with an isolated home and an explicitly supplied private fixture"]
async fn real_codex_cli_with_isolated_home() {
    let path = std::env::var_os("VELA_REAL_FIXTURE").expect("private fixture required");
    let private: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let cli = std::env::var_os("VELA_CODEX_CLI").expect("official CLI path required");
    let (t, g) = fixture(vec![]).await;
    update(
        &g,
        &t,
        form(serde_json::json!({
            "baseUrl": private["baseUrl"].as_str().unwrap(),
            "token": private["token"].as_str().unwrap(),
        })),
    );
    // A valid disposable auth file proves the CLI used the managed provider, never the real account.
    let auth = b"{\"auth_mode\":\"apikey\",\"OPENAI_API_KEY\":\"fixture-only\"}";
    storage::atomic_write(&t.path().join("auth.json"), auth, None).unwrap();
    let config=format!("model = {}\nmodel_provider = 'custom'\n[model_providers.custom]\nname = 'Original'\nwire_api = 'responses'\nsupports_websockets = false\nbase_url = {}\nexperimental_bearer_token = {}\n",serde_json::to_string(private["model"].as_str().unwrap()).unwrap(),serde_json::to_string(private["baseUrl"].as_str().unwrap()).unwrap(),serde_json::to_string(private["token"].as_str().unwrap()).unwrap());
    storage::atomic_write(&t.path().join("config.toml"), config.as_bytes(), None).unwrap();
    start(&g, &t).await;
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::process::Command::new(cli)
            .args([
                "exec",
                "--ephemeral",
                "--skip-git-repo-check",
                "--json",
                "Reply only: gateway-cli-ok. Do not use tools.",
            ])
            .env("CODEX_HOME", t.path())
            .env_remove("OPENAI_API_KEY")
            .current_dir(t.path())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    // Never print CLI output: it can contain private upstream data.
    assert!(
        result.status.success(),
        "isolated official CLI failed; output intentionally withheld"
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("gateway-cli-ok"),
        "CLI completion missing"
    );
    let view = g.view();

    assert_eq!(
        view.last_successful.as_deref(),
        Some(view.providers[0].id.as_str())
    );
    g.stop().await.unwrap();
    assert_eq!(
        storage::read_optional(&t.path().join("auth.json"))
            .unwrap()
            .unwrap(),
        auth
    );
    assert_eq!(
        storage::read_optional(&t.path().join("config.toml"))
            .unwrap()
            .unwrap(),
        config.as_bytes()
    );
    println!("Official Codex CLI completed with isolated home; auth/config restored");
}

#[tokio::test]
async fn compressed_and_spooled_json_replay_exactly_across_failover() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut urls = Vec::new();
    for status in [503, 200] {
        let seen = seen.clone();
        let port = server(move |r| {
            let seen = seen.clone();
            async move {
                let encoding = r.headers().get("content-encoding").cloned();
                let bytes = r.into_body().collect().await.unwrap().to_bytes();
                seen.lock().push(storage::digest(&bytes));
                let mut response = Response::builder().status(status);
                if let Some(encoding) = encoding {
                    response = response.header("content-encoding", encoding);
                }
                response.body(full(bytes)).unwrap()
            }
        })
        .await;
        urls.push(format!("http://127.0.0.1:{port}/v1"));
    }
    let (t, g) = fixture(urls).await;
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    start(&g, &t).await;
    let json = serde_json::to_vec(
        &serde_json::json!({"input":"x".repeat(3*1024*1024),"unknown":{"keep":true}}),
    )
    .unwrap();
    let zstd = zstd::stream::encode_all(json.as_slice(), 1).unwrap();
    for (body, encoding) in [(json, "identity"), (zstd, "zstd")] {
        let digest = storage::digest(&body);
        let response = request(
            &g,
            "/v1/future",
            body.clone(),
            vec![
                ("content-type", "application/json"),
                ("content-encoding", encoding),
            ],
        )
        .await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-encoding"], encoding);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            body
        );
        assert_eq!(
            seen.lock().drain(..).collect::<Vec<_>>(),
            vec![digest.clone(), digest]
        );
    }
    assert_eq!(std::fs::read_dir(g.0.spool.path()).unwrap().count(), 0);
    g.stop().await.unwrap();
}

#[tokio::test]
async fn custom_only_switch_while_stopped_running_and_auto_stop() {
    let ok = server(|_| async { Response::new(full("ok")) }).await;
    let (t, g) = fixture(vec![
        "https://a.test/v1".into(),
        format!("http://127.0.0.1:{ok}"),
    ])
    .await;
    let b = g.view().providers[1].id.clone();
    let path = t.path().join("config.toml");
    let initial = std::fs::read_to_string(&path).unwrap();
    let old_rev = takeover::tests::read(t.path()).unwrap().0;
    assert!(g
        .edit_checked(
            Edit::Select { id: b.clone() },
            &g.view().revision,
            t.path(),
            Some("stale")
        )
        .is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), initial);
    g.edit_checked(
        Edit::Select { id: b.clone() },
        &g.view().revision,
        t.path(),
        Some(&old_rev),
    )
    .unwrap();
    assert_eq!(g.view().config_provider.as_deref(), Some(b.as_str()));
    let direct = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        direct,
        initial.replace("https://a.test/v1", &format!("http://127.0.0.1:{ok}/v1"))
    );
    start(&g, &t).await;
    let live = std::fs::read(&path).unwrap();
    let a = g.view().providers[0].id.clone();
    update(&g, &t, Edit::Select { id: a });
    assert_eq!(std::fs::read(&path).unwrap(), live);
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    update(
        &g,
        &t,
        Edit::QueueProvider {
            id: g.view().providers[0].id.clone(),
            queued: false,
        },
    );
    let response = request(&g, "/v1/new", vec![], vec![]).await;
    assert_eq!(response.status(), 200);
    response.into_body().collect().await.unwrap();
    let view = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let view = g.view();
            if view.last_successful.as_deref() == Some(b.as_str()) || view.error.is_some() {
                break view;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("successful provider state did not settle");
    assert_eq!(view.last_successful.as_deref(), Some(b.as_str()));
    assert!(
        view.error.is_none(),
        "successful provider state failed: {:?}",
        view.error
    );
    assert_eq!(std::fs::read(&path).unwrap(), live);
    g.stop().await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), direct);
    assert_eq!(
        std::fs::read_to_string(t.path().join("auth.json")).unwrap(),
        "unchanged-auth"
    );
}
#[tokio::test]
async fn codex_without_custom_reads_as_official_and_is_only_written_by_a_start() {
    for original in [None, Some("# mine\nmodel = 'keep'\n[other]\na = 1\n")] {
        let (t, g) = fixture(vec!["https://a.test/v1".into(), "https://b.test/v1".into()]).await;
        let path = t.path().join("config.toml");
        match original {
            Some(text) => std::fs::write(&path, text).unwrap(),
            None => std::fs::remove_file(&path).unwrap(),
        }
        let on_disk = || std::fs::read_to_string(&path).ok();
        let view = g.view();
        assert_eq!(view.current.state, current::Kind::Official);
        assert_eq!((view.config_error, view.config_provider), (None, None));

        let b = view.providers[1].id.clone();
        update(&g, &t, Edit::Select { id: b.clone() });
        let view = g.view();
        assert_eq!(view.selected.as_deref(), Some(b.as_str()));
        assert_eq!(view.current.state, current::Kind::Official);
        assert_eq!(on_disk().as_deref(), original);
        assert!(!t.path().join("gateway-recovery.json").exists());

        start(&g, &t).await;
        let view = g.view();
        assert!(view.running);
        assert_eq!(view.current.state, current::Kind::Gateway);
        assert_eq!(view.config_error, None);
        let live = on_disk().unwrap();
        assert!(live.starts_with("model_provider = \"custom\"\n"));
        assert!(live.contains(original.unwrap_or_default()));

        g.stop().await.unwrap();
        let view = g.view();
        assert_eq!(view.config_provider.as_deref(), Some(b.as_str()));
        assert_eq!(view.config_error, None);
        assert!(on_disk().unwrap().contains("[model_providers.custom]"));

        // With `custom` in place a choice made while stopped is written again.
        let a = view.providers[0].id.clone();
        update(&g, &t, Edit::Select { id: a.clone() });
        assert_eq!(g.view().config_provider.as_deref(), Some(a.as_str()));

        // Another provider chosen by the user is neither read as official nor replaced.
        let other = "model_provider = 'other'\n";
        std::fs::write(&path, other).unwrap();
        assert!(g.view().config_error.is_some());
        assert!(g
            .edit(Edit::Select { id: b }, &g.view().revision, t.path())
            .is_err());
        assert!(g.start(&g.view().revision, t.path()).await.is_err());
        assert_eq!(on_disk().as_deref(), Some(other));
    }
}
#[tokio::test]
async fn codex_config_removed_while_running_is_an_error_and_is_not_recreated_by_stop() {
    let (t, g) = fixture(vec!["https://a.test/v1".into()]).await;
    let path = t.path().join("config.toml");
    std::fs::remove_file(&path).unwrap();
    start(&g, &t).await;
    assert_eq!(g.view().current.state, current::Kind::Gateway);
    std::fs::remove_file(&path).unwrap();
    let view = g.view();
    assert!(matches!(view.current.state, current::Kind::Unknown { .. }));
    assert!(view.config_error.is_some());
    assert_eq!(g.stop().await.err().unwrap().code, "CONFLICT");
    assert!(g.view().running);
    assert!(!path.exists());
    assert!(t.path().join("gateway-recovery.json").exists());
}
#[tokio::test]
async fn codex_config_that_lost_custom_is_left_alone_by_resume_and_connection_choice() {
    let (t, g) = fixture(vec!["https://a.test/v1".into()]).await;
    let path = t.path().join("config.toml");
    start(&g, &t).await;
    g.stop_for_exit().await.unwrap();
    for text in [Some("model = 'keep'\n"), None] {
        match text {
            Some(text) => std::fs::write(&path, text).unwrap(),
            None => std::fs::remove_file(&path).unwrap(),
        }
        assert_eq!(
            g.resume(t.path()).await.err().unwrap().code,
            "RESUME_CONFLICT"
        );
        assert!(!g.view().running);
        update(
            &g,
            &t,
            Edit::Connection {
                mode: "bearer".into(),
            },
        );
        assert_eq!(std::fs::read_to_string(&path).ok().as_deref(), text);
    }
}
#[tokio::test]
async fn crash_while_running_writes_the_provider_selected_last() {
    let (t, g) = fixture(vec!["https://a.test/v1".into(), "https://b.test/v1".into()]).await;
    start(&g, &t).await;
    let a = g.view().providers[0].id.clone();
    let b = g.view().providers[1].id.clone();
    update(&g, &t, Edit::Select { id: b.clone() });
    update(&g, &t, Edit::DeleteProvider { id: a });
    takeover::recover(t.path()).unwrap();
    assert_eq!(
        takeover::tests::read(t.path())
            .unwrap()
            .1
            .base_url
            .as_deref(),
        Some("https://b.test/v1")
    );
    g.stop().await.unwrap();
}
#[tokio::test]
async fn quota_sub2_priority_singleflight_cache_and_health_isolation() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    let port = server(move |req| {
        let hits = counted.clone();
        async move {
            hits.fetch_add(1, Ordering::Relaxed);
            assert_eq!(req.uri().path(), "/prefix/v1/usage");
            assert_eq!(
                req.headers()["authorization"],
                "Bearer upstream-fixture-token"
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
            Response::new(full(
                r#"{"isValid":true,"quota":{"limit":10,"used":2,"remaining":8}}"#,
            ))
        }
    })
    .await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}/prefix/v1")]).await;
    let p = g.view().providers[0].id.clone();
    let config = std::fs::read(t.path().join("config.toml")).unwrap();
    let (a, b) = tokio::join!(g.query_quota(&p, true), g.query_quota(&p, true));
    assert_eq!(a.unwrap().plans[0].remaining, Some(8.));
    assert_eq!(b.unwrap().source.as_deref(), Some("sub2api"));
    assert_eq!(hits.load(Ordering::Relaxed), 1);
    g.query_quota(&p, false).await.unwrap();
    assert_eq!(hits.load(Ordering::Relaxed), 1);
    assert_eq!(g.view().providers[0].health.failures, 0);
    assert_eq!(g.view().providers[0].active_requests, 0);
    assert_eq!(g.view().last_successful, None);

    assert_eq!(std::fs::read(t.path().join("config.toml")).unwrap(), config);
}
#[tokio::test]
async fn quota_newapi_fallback_and_public_unit_request_has_no_key() {
    let paths = Arc::new(Mutex::new(vec![]));
    let seen = paths.clone();
    let port=server(move|r|{let seen=seen.clone();async move{
        let path=r.uri().path().to_owned();seen.lock().push(path.clone());
        match path.as_str(){
            "/site/v1/usage"=>Response::builder().status(404).body(full("missing")).unwrap(),
            "/site/api/usage/token/"=>{assert!(r.headers().contains_key("authorization"));Response::new(full(r#"{"code":true,"data":{"object":"token_usage","total_available":400,"total_granted":1000,"total_used":600,"unlimited_quota":false,"expires_at":0}}"#))},
            "/site/api/status"=>{assert!(!r.headers().contains_key("authorization"));Response::new(full(r#"{"success":true,"data":{"quota_per_unit":100,"quota_display_type":"CNY","usd_exchange_rate":7}}"#))},
            _=>panic!("unexpected quota endpoint"),
        }
    }}).await;
    let (_t, g) = fixture(vec![format!("http://127.0.0.1:{port}/site/v1")]).await;
    let result = g
        .query_quota(&g.view().providers[0].id, true)
        .await
        .unwrap();
    assert_eq!(result.source.as_deref(), Some("newapi"));
    assert_eq!(result.plans[0].remaining, Some(28.));
    assert_eq!(result.plans[0].unit, "CNY");
    assert_eq!(paths.lock().len(), 3);
}
#[tokio::test]
async fn quota_retry_after_keeps_last_success_and_does_not_fallback() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let port = server(move |_| {
        let seen = seen.clone();
        async move {
            if seen.fetch_add(1, Ordering::Relaxed) == 0 {
                Response::new(full(r#"{"balance":4.25}"#))
            } else {
                Response::builder()
                    .status(429)
                    .header("retry-after", "120")
                    .body(full("limited"))
                    .unwrap()
            }
        }
    })
    .await;
    let (_t, g) = fixture(vec![format!("http://127.0.0.1:{port}")]).await;
    let id = g.view().providers[0].id.clone();
    g.query_quota(&id, true).await.unwrap();
    let result = g.query_quota(&id, true).await.unwrap();
    assert!(result.stale);
    assert_eq!(result.state, "error");
    assert_eq!(result.plans[0].remaining, Some(4.25));
    assert!(result.retry_at.unwrap() >= quota::now() + 119);
    g.query_quota(&id, true).await.unwrap();
    assert_eq!(hits.load(Ordering::Relaxed), 2);
}
#[tokio::test]
async fn quota_redirect_oversize_auth_and_unsupported_are_distinct() {
    for (status, body, expected) in [
        (302, "redirect".to_owned(), "error"),
        (200, "x".repeat(2_000_001), "error"),
        (401, "invalid key".to_owned(), "error"),
        (200, "<html>login</html>".to_owned(), "unsupported"),
    ] {
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let port = server(move |_| {
            let seen = seen.clone();
            let body = body.clone();
            async move {
                seen.fetch_add(1, Ordering::Relaxed);
                Response::builder()
                    .status(status)
                    .header("location", "https://elsewhere.invalid/steal")
                    .body(full(body))
                    .unwrap()
            }
        })
        .await;
        let (_t, g) = fixture(vec![format!("http://127.0.0.1:{port}")]).await;
        let result = g
            .query_quota(&g.view().providers[0].id, true)
            .await
            .unwrap();
        assert_eq!(result.state, expected);
        assert!(result.plans.is_empty());
        assert_eq!(
            hits.load(Ordering::Relaxed),
            if status == 302 || result.error.as_ref().is_some_and(|e| e.contains("2 MB")) {
                1
            } else {
                2
            }
        );
    }
}

#[tokio::test]
async fn quota_late_response_is_discarded_after_provider_edit() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let e = entered.clone();
    let gate2 = gate.clone();
    let port = server(move |_| {
        let e = e.clone();
        let gate = gate2.clone();
        async move {
            e.notify_one();
            gate.notified().await;
            Response::new(full(r#"{"balance":123}"#))
        }
    })
    .await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}")]).await;
    let id = g.view().providers[0].id.clone();
    let g2 = g.clone();
    let id2 = id.clone();
    let task = tokio::spawn(async move { g2.query_quota(&id2, true).await });
    entered.notified().await;
    update(
        &g,
        &t,
        form(serde_json::json!({
            "id": id,
            "baseUrl": format!("http://127.0.0.1:{port}"),
            "token": "changed-key",
        })),
    );
    gate.notify_one();
    assert_eq!(
        task.await
            .unwrap()
            .err()
            .expect("stale result must be rejected")
            .code,
        "STALE"
    );
    assert!(g.view().providers[0].quota.is_none());
}

#[tokio::test]
async fn quota_concurrency_limit_and_echoed_secrets_are_filtered() {
    let active = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let a = active.clone();
    let m = max.clone();
    let port = server(move |_| {
        let a = a.clone();
        let m = m.clone();
        async move {
            let current = a.fetch_add(1, Ordering::SeqCst) + 1;
            m.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(80)).await;
            a.fetch_sub(1, Ordering::SeqCst);
            Response::new(full(r#"{"balance":5,"unit":"upstream-fixture-token"}"#))
        }
    })
    .await;
    let (_t, g) = fixture(
        (0..6)
            .map(|i| format!("http://127.0.0.1:{port}/site{i}"))
            .collect(),
    )
    .await;
    let ids: Vec<_> = g.view().providers.iter().map(|p| p.id.clone()).collect();
    let results =
        futures_util::future::join_all(ids.iter().map(|id| g.query_quota(id, true))).await;
    assert_eq!(max.load(Ordering::SeqCst), 3);
    for result in results {
        let view = result.unwrap();
        assert_eq!(view.state, "ok");
        assert!(!serde_json::to_string(&view)
            .unwrap()
            .contains("upstream-fixture-token"));
    }
}

#[tokio::test]
async fn quota_timeout_does_not_probe_another_protocol() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let port = server(move |_| {
        let seen = seen.clone();
        async move {
            seen.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_secs(20)).await;
            Response::new(full(r#"{"balance":5}"#))
        }
    })
    .await;
    let (_t, g) = fixture(vec![format!("http://127.0.0.1:{port}")]).await;
    let start = Instant::now();
    let result = g
        .query_quota(&g.view().providers[0].id, true)
        .await
        .unwrap();
    assert_eq!(result.error.as_deref(), Some("额度查询超时"));
    assert!(start.elapsed() < Duration::from_secs(12));
    assert_eq!(hits.load(Ordering::Relaxed), 1);
    let view = g.view();

    assert_eq!(view.providers[0].health.failures, 0);
    assert_eq!(view.providers[0].active_requests, 0);
    assert_eq!(view.waiting_requests, 0);
}

#[tokio::test]
async fn recognized_expired_sub2_stops_protocol_probing() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let port = server(move |_| {
        let seen = seen.clone();
        async move {
            seen.fetch_add(1, Ordering::Relaxed);
            Response::builder()
                .status(403)
                .body(full(r#"{"status":"expired","isValid":false}"#))
                .unwrap()
        }
    })
    .await;
    let (_t, g) = fixture(vec![format!("http://127.0.0.1:{port}")]).await;
    let result = g
        .query_quota(&g.view().providers[0].id, true)
        .await
        .unwrap();
    assert_eq!(result.source.as_deref(), Some("sub2api"));
    assert_eq!(result.key_status.as_deref(), Some("已到期"));
    assert_eq!(hits.load(Ordering::Relaxed), 1);
}

#[path = "concurrency_tests.rs"]
mod concurrency;
#[path = "entry_key_tests.rs"]
mod entry_keys;
#[path = "revision_tests.rs"]
mod revisions;

#[path = "model_tests.rs"]
mod models;

#[path = "failover_transport_tests.rs"]
mod failover_transport_tests;

#[path = "import_tests.rs"]
mod imports;

#[path = "claude_gateway_tests.rs"]
mod claude_gateway_tests;

#[path = "capacity_tests.rs"]
mod capacity;
#[path = "claude_tests.rs"]
mod claude_tests;
#[path = "providerless_tests.rs"]
mod providerless;

#[path = "model_error_tests.rs"]
mod model_errors;

#[path = "quota_account_tests.rs"]
mod quota_account;
#[path = "quota_tests.rs"]
mod quota_tests;

#[path = "../events_tests.rs"]
mod events_tests;

#[path = "websocket_reconnect_tests.rs"]
mod websocket_reconnect_tests;

#[path = "capacity_retry_tests.rs"]
mod capacity_retry_tests;

#[path = "compaction_handoff_tests.rs"]
mod compaction_handoff_tests;

#[path = "compaction_execution_tests.rs"]
mod compaction_execution;

#[test]
fn view_reads_without_routing_lock_and_retries_only_once_on_configuration_changes() {
    let temp = tempfile::tempdir().unwrap();
    let gateway = Gateway::new(temp.path().to_path_buf()).unwrap();
    let reads = std::cell::Cell::new(0);
    let view = gateway
        .try_view_read(|gateway, view, home, store| {
            let mut state = gateway
                .0
                .inner
                .try_lock()
                .expect("config reads must release routing lock");
            if reads.get() == 0 {
                state.editable_mut().store.mode = "auto".into();
            }
            reads.set(reads.get() + 1);
            drop(state);
            gateway.read_view_config(view, home, store);
        })
        .unwrap();
    assert_eq!(reads.get(), 2);
    assert_eq!(view.mode, "auto");
    assert_eq!(view.revision, gateway.view().revision);
    reads.set(0);
    let error = gateway
        .try_view_read(|gateway, _, _, _| {
            let mut state = gateway.0.inner.try_lock().unwrap();
            let next_mode = if state.store.mode == "auto" {
                "manual"
            } else {
                "auto"
            };
            state.editable_mut().store.mode = next_mode.into();
            reads.set(reads.get() + 1);
        })
        .err()
        .unwrap();
    assert_eq!(error.code, code::CONFLICT);
    assert_eq!(reads.get(), 2);
}

#[tokio::test]
async fn quota_freeze_suppresses_automatic_queries_but_preserves_manual_and_started_queries() {
    let hits = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let port = server({
        let hits = hits.clone();
        let entered = entered.clone();
        let release = release.clone();
        move |_| {
            let hits = hits.clone();
            let entered = entered.clone();
            let release = release.clone();
            async move {
                if hits.fetch_add(1, Ordering::SeqCst) == 0 {
                    entered.notify_one();
                    release.acquire().await.unwrap().forget();
                }
                Response::new(full(
                    r#"{"isValid":true,"quota":{"limit":10,"used":2,"remaining":8}}"#,
                ))
            }
        }
    })
    .await;
    let (_dir, g) = fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
    let id = g.view().providers[0].id.clone();
    let gate = g.freeze_gate();
    gate.set_selected(Some(ClientId::Codex), true).unwrap();
    assert_eq!(g.query_quota(&id, false).await.unwrap().state, "idle");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    gate.set_selected(Some(ClientId::Codex), false).unwrap();
    let pending = tokio::spawn({
        let g = g.clone();
        let id = id.clone();
        async move { g.query_quota(&id, false).await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    gate.set_selected(Some(ClientId::Codex), true).unwrap();
    release.add_permits(1);
    let result = pending.await.unwrap().unwrap();
    assert_eq!(result.plans[0].remaining, Some(8.));
    assert_eq!(
        g.query_quota(&id, false).await.unwrap().checked_at,
        result.checked_at
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    g.query_quota(&id, true).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}
