//! Real loopback TCP regressions for downstream cancellation.
use super::{Edit, Gateway};
use crate::gateway::test_support::downstream::{
    attach_observers, downstream_websocket, edit, local_auth, read_http_request, usage_records,
    REQUEST_BODY,
};
use crate::{
    events::{
        Action, Category, ClassifiedRecord, Filter as EventFilter, Level, Reason as EventReason,
        Service as EventService,
    },
    usage::{
        model::{EndCause, Record as UsageRecord},
        Service as UsageService,
    },
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use std::{
    io,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

const WAIT: Duration = Duration::from_secs(4);
const FAST_RELEASE: Duration = Duration::from_millis(300);
const USAGE_TAIL: Duration = Duration::from_millis(1500);
const USAGE_TAIL_TOLERANCE: Duration = Duration::from_millis(500);

async fn fixture(urls: Vec<String>) -> (tempfile::TempDir, Gateway) {
    let dir = tempfile::tempdir().unwrap();
    let primary = urls.first().cloned().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        format!(
            "# fixture\nmodel_provider='custom'\n[model_providers.custom]\nbase_url = {}\nexperimental_bearer_token = 'fixture-original-token'\nwire_api='responses'\nsupports_websockets=true\n",
            serde_json::to_string(&primary).unwrap()
        ),
    )
    .unwrap();
    let gateway = Gateway::new(dir.path().to_path_buf()).unwrap();
    let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let mut settings = gateway.view().settings;
    settings.port = port;
    settings.max_retries = 1;
    settings.first_byte_seconds = 1;
    settings.idle_seconds = 3;
    settings.connect_seconds = 2;
    settings.queue_seconds = 1;
    settings.websocket_retry_seconds = 1;
    edit(&gateway, &dir, Edit::Settings { settings });
    for url in urls {
        edit(
            &gateway,
            &dir,
            Edit::SaveProvider {
                delegate: Default::default(),
                search_only: None,
                end_on_message_stop: None,
                filter_count_tokens: None,
                buffer_complete_response: None,
                endless_mode: None,
                upstream_retry_seconds: None,
                normal_retry_seconds: None,
                risk_codes: None,
                retry_intensity: None,
                limit_retry_seconds: None,
                limit_rest_seconds: None,
                quota_access_token: None,
                id: None,
                base_url: url,
                token: "fixture-upstream-token".into(),
                name: Some("fixture-upstream".into()),
            },
        );
    }
    edit(
        &gateway,
        &dir,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    std::fs::write(dir.path().join("auth.json"), "fixture-only").unwrap();
    gateway
        .start(&gateway.view().revision, dir.path())
        .await
        .unwrap();
    (dir, gateway)
}

async fn wait_for_usage_record(service: &UsageService) -> UsageRecord {
    let mut events = service.subscribe();
    tokio::time::timeout(WAIT, async {
        loop {
            let rows = usage_records(service);
            if let Some(record) = rows.into_iter().next() {
                assert_eq!(usage_records(service).len(), 1);
                assert!(service.state().error.is_none());
                return record;
            }
            let _ = events.recv().await;
        }
    })
    .await
    .expect("canceled request usage record was not written")
}

fn interruption_records(service: &EventService) -> Vec<ClassifiedRecord> {
    service
        .query(EventFilter {
            reason: Some(EventReason::RequestInterrupted),
            ..EventFilter::default()
        })
        .items
}

async fn wait_for_interruption(service: &EventService) -> ClassifiedRecord {
    let mut events = service.subscribe();
    tokio::time::timeout(WAIT, async {
        loop {
            let rows = interruption_records(service);
            if let Some(record) = rows.into_iter().next() {
                assert_eq!(interruption_records(service).len(), 1);
                return record;
            }
            let _ = events.recv().await;
        }
    })
    .await
    .expect("downstream interruption diagnostic was not written")
}

async fn assert_downstream_diagnostic_policy(
    service: &EventService,
    receiver: &mut tokio::sync::broadcast::Receiver<crate::events::Change>,
    events_dir: &std::path::Path,
    downstream_error: Option<&str>,
) -> Option<ClassifiedRecord> {
    match downstream_error {
        Some(
            "DOWNSTREAM_CLOSED"
            | "DOWNSTREAM_PEER_RESET"
            | "DOWNSTREAM_WRITE_FAILED"
            | "DOWNSTREAM_CANCELLED",
        )
        | None => {
            super::diagnostic_test_support::assert_no_interruptions_after_barrier(
                service, receiver, events_dir,
            )
            .await;
            None
        }
        Some(
            "CLIENT_DISCONNECTED"
            | "RESPONSE_BODY_DROPPED"
            | "DOWNSTREAM_CONNECTION_ERROR"
            | "DOWNSTREAM_PROTOCOL_ERROR"
            | "DOWNSTREAM_WRITE_TIMEOUT",
        ) => {
            let record = wait_for_interruption(service).await;
            assert_eq!(record.reason, EventReason::RequestInterrupted);
            assert_eq!(record.level, Level::Warning);
            assert_eq!(record.category, Category::OrdinaryError);
            assert_eq!(record.details.local_code.as_deref(), downstream_error);
            assert_eq!(record.details.counted_failure, Some(false));
            assert!(!record.reason.notifiable(record.action));
            Some(record)
        }
        Some(code) => panic!("unclassified fixture downstream error: {code}"),
    }
}

async fn assert_neutral_health(gateway: &Gateway) {
    tokio::time::timeout(Duration::from_secs(3), async {
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
    .expect("canceled request kept provider activity open");
    let providers = gateway.view().providers;
    assert!(providers
        .iter()
        .all(|provider| { provider.health.failures == 0 && provider.health.retry_in == 0 }));
    assert!(!gateway.has_health_wait());
}

fn assert_not_successful_usage(record: &UsageRecord) {
    let attempt = record
        .final_attempt()
        .expect("request should have one attempt");
    assert_ne!(attempt.outcome, "success");
    assert_ne!(
        attempt.end.as_ref().map(|end| end.cause),
        Some(EndCause::Completed)
    );
}

async fn wait_for_disconnect(mut stream: TcpStream) -> Instant {
    let mut bytes = [0u8; 256];
    loop {
        match stream.read(&mut bytes).await {
            Ok(0) => return Instant::now(),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Instant::now(),
        }
    }
}

async fn downstream_http(gateway: &Gateway, body: &[u8]) -> TcpStream {
    let (port, token) = local_auth(gateway);
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    #[allow(deprecated)]
    stream.set_linger(Some(Duration::ZERO)).unwrap();
    let head = format!(
        "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    stream
}

async fn assert_no_retry(primary: &TcpListener, backup: &TcpListener, wait: Duration) {
    let accepted = tokio::time::timeout(wait, async {
        tokio::select! {
            result = primary.accept() => Some(("primary", result.unwrap())),
            result = backup.accept() => Some(("backup", result.unwrap())),
        }
    })
    .await;
    assert!(
        accepted.is_err(),
        "downstream cancellation opened another upstream connection"
    );
}

#[tokio::test]
async fn rst_before_upstream_headers_closes_the_real_upstream_socket_promptly() {
    let primary = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_port = primary.local_addr().unwrap().port();
    let backup = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backup_port = backup.local_addr().unwrap().port();
    let (seen_tx, seen_rx) = oneshot::channel();
    let (closed_tx, closed_rx) = oneshot::channel();
    let primary_task = tokio::spawn(async move {
        let (mut upstream, _) = primary.accept().await.unwrap();
        read_http_request(&mut upstream).await.unwrap();
        let _ = seen_tx.send(());
        let closed = wait_for_disconnect(upstream).await;
        let _ = closed_tx.send(closed);
        primary
    });
    let (_dir, gateway) = fixture(vec![
        format!("http://127.0.0.1:{primary_port}/v1"),
        format!("http://127.0.0.1:{backup_port}/v1"),
    ])
    .await;
    let (usage, events) = attach_observers(&_dir, &gateway);
    let mut diagnostic_events = events.subscribe();
    let client = downstream_http(&gateway, REQUEST_BODY).await;
    tokio::time::timeout(WAIT, seen_rx)
        .await
        .expect("upstream should receive the request before cancellation")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let cancelled_at = Instant::now();
    drop(client);
    let closed_at = tokio::time::timeout(FAST_RELEASE, closed_rx)
        .await
        .expect("upstream TCP connection stayed open after client RST")
        .unwrap();
    let close_elapsed = closed_at.duration_since(cancelled_at);
    eprintln!("HTTP pre-header RST closed upstream after {close_elapsed:?}");
    assert!(close_elapsed < FAST_RELEASE);
    let primary = primary_task.await.unwrap();
    assert_no_retry(&primary, &backup, Duration::from_secs(2)).await;
    let record = wait_for_usage_record(&usage).await;
    assert_not_successful_usage(&record);
    let attempt = record.final_attempt().unwrap();
    assert_eq!(
        attempt.end.as_ref().map(|end| end.cause),
        Some(EndCause::ClientDisconnected)
    );
    let transfer = attempt.transfer.as_ref();
    eprintln!(
        "HTTP pre-header RST usage: outcome={}, end={:?}, status={:?}, upstream_bytes={}, downstream_bytes={}, downstream_error={:?}",
        attempt.outcome,
        attempt.end.as_ref().map(|end| end.cause),
        attempt.status,
        transfer.map_or(0, |transfer| transfer.upstream_bytes),
        transfer.map_or(0, |transfer| transfer.downstream_bytes),
        transfer.and_then(|transfer| transfer.downstream_error.as_deref())
    );
    assert_neutral_health(&gateway).await;
    let _ = assert_downstream_diagnostic_policy(
        &events,
        &mut diagnostic_events,
        &_dir.path().join("events"),
        transfer.and_then(|transfer| transfer.downstream_error.as_deref()),
    )
    .await;
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn rst_after_two_sse_events_closes_the_real_upstream_without_retry() {
    let primary = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_port = primary.local_addr().unwrap().port();
    let backup = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backup_port = backup.local_addr().unwrap().port();
    let (seen_tx, seen_rx) = oneshot::channel();
    let (closed_tx, closed_rx) = oneshot::channel();
    let event_one = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"one\"}\n\n";
    let event_two = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"two\"}\n\n";
    let primary_task = tokio::spawn(async move {
        let (mut upstream, _) = primary.accept().await.unwrap();
        read_http_request(&mut upstream).await.unwrap();
        upstream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .unwrap();
        for event in [event_one.as_slice(), event_two.as_slice()] {
            let chunk = format!("{:X}\r\n", event.len());
            upstream.write_all(chunk.as_bytes()).await.unwrap();
            upstream.write_all(event).await.unwrap();
            upstream.write_all(b"\r\n").await.unwrap();
            upstream.flush().await.unwrap();
        }
        let _ = seen_tx.send(());
        let closed = wait_for_disconnect(upstream).await;
        let _ = closed_tx.send(closed);
        primary
    });
    let (_dir, gateway) = fixture(vec![
        format!("http://127.0.0.1:{primary_port}/v1"),
        format!("http://127.0.0.1:{backup_port}/v1"),
    ])
    .await;
    let (usage, events) = attach_observers(&_dir, &gateway);
    let mut diagnostic_events = events.subscribe();
    let (mut response, sender, task) = super::tests::request_with_reset(
        &gateway,
        "/v1/responses",
        REQUEST_BODY.to_vec(),
        vec![("content-type", "application/json")],
    )
    .await;
    assert_eq!(response.status(), 200);
    let mut body = Vec::new();
    tokio::time::timeout(WAIT, async {
        while String::from_utf8_lossy(&body)
            .matches("response.output_text.delta")
            .count()
            < 2
        {
            let frame = response
                .body_mut()
                .frame()
                .await
                .expect("SSE stream ended before two events")
                .expect("SSE frame should be readable");
            if let Some(data) = frame.data_ref() {
                body.extend_from_slice(data);
            }
            if String::from_utf8_lossy(&body)
                .matches("response.output_text.delta")
                .count()
                >= 2
            {
                break;
            }
        }
    })
    .await
    .expect("gateway should forward two SSE events");
    let text = String::from_utf8_lossy(&body);
    assert_eq!(text.matches("response.output_text.delta").count(), 2);
    tokio::time::timeout(WAIT, seen_rx)
        .await
        .expect("upstream should have written the two events")
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let cancelled_at = Instant::now();
    super::tests::reset_and_drop(task, sender, response).await;
    let closed_at = tokio::time::timeout(USAGE_TAIL + USAGE_TAIL_TOLERANCE, closed_rx)
        .await
        .expect("upstream TCP connection stayed open after streaming RST tail window")
        .unwrap();
    let close_elapsed = closed_at.duration_since(cancelled_at);
    eprintln!("HTTP two-event RST closed upstream after {close_elapsed:?}");
    assert!(close_elapsed <= USAGE_TAIL + USAGE_TAIL_TOLERANCE);
    let primary = primary_task.await.unwrap();
    assert_no_retry(&primary, &backup, Duration::from_secs(2)).await;
    let usage_record = wait_for_usage_record(&usage).await;
    assert_not_successful_usage(&usage_record);
    let usage_attempt = usage_record.final_attempt().unwrap();
    assert_eq!(usage_attempt.status, Some(200));
    assert!(matches!(
        usage_attempt.end.as_ref().map(|end| end.cause),
        Some(
            EndCause::ClientDisconnected | EndCause::DownstreamTransportError | EndCause::Unsettled
        )
    ));
    assert!(!usage_attempt.end.as_ref().unwrap().terminal_received);
    let transfer = usage_attempt.transfer.as_ref().unwrap();
    assert!(transfer.upstream_bytes > 0);
    assert!(transfer.downstream_bytes > 0);
    assert!(!transfer.terminal_handed_off);
    eprintln!(
        "HTTP two-event RST usage: outcome={}, end={:?}, upstream_bytes={}, downstream_bytes={}, downstream_error={:?}",
        usage_attempt.outcome,
        usage_attempt.end.as_ref().map(|end| end.cause),
        transfer.upstream_bytes,
        transfer.downstream_bytes,
        transfer.downstream_error
    );
    assert_neutral_health(&gateway).await;
    let _ = assert_downstream_diagnostic_policy(
        &events,
        &mut diagnostic_events,
        &_dir.path().join("events"),
        transfer.downstream_error.as_deref(),
    )
    .await;
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn fin_half_close_keeps_the_response_stream_readable() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_port = upstream.local_addr().unwrap().port();
    let payload = b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"fixture-finished\",\"status\":\"completed\"}}\n\n";
    let upstream_task = tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.unwrap();
        read_http_request(&mut socket).await.unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.write_all(payload).await.unwrap();
        socket.flush().await.unwrap();
    });
    let (_dir, gateway) = fixture(vec![format!("http://127.0.0.1:{upstream_port}/v1")]).await;
    let (port, token) = local_auth(&gateway);
    let mut client = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let head = format!(
        "POST /v1/responses HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        REQUEST_BODY.len()
    );
    client.write_all(head.as_bytes()).await.unwrap();
    client.write_all(REQUEST_BODY).await.unwrap();
    client.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(WAIT, client.read_to_end(&mut response))
        .await
        .expect("FIN half-close should still allow the response to arrive")
        .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200"));
    assert!(String::from_utf8_lossy(&response).contains("response.completed"));
    upstream_task.await.unwrap();
    gateway.stop().await.unwrap();
}

const CREATE: &str = r#"{"type":"response.create","model":"fixture-model","input":"fixture"}"#;

async fn assert_websocket_cancel_closes_upstream(explicit_close: bool, generation: bool) {
    let primary = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_port = primary.local_addr().unwrap().port();
    let backup = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backup_port = backup.local_addr().unwrap().port();
    let (request_tx, request_rx) = oneshot::channel();
    let (create_tx, create_rx) = oneshot::channel();
    let (closed_tx, closed_rx) = oneshot::channel();
    let primary_task = tokio::spawn(async move {
        let (socket, _) = primary.accept().await.unwrap();
        if !generation {
            let mut socket = socket;
            read_http_request(&mut socket).await.unwrap();
            let _ = request_tx.send(());
            let closed = wait_for_disconnect(socket).await;
            let _ = closed_tx.send(closed);
        } else {
            let mut websocket = tokio_tungstenite::accept_async(socket).await.unwrap();
            let _ = request_tx.send(());
            let first = tokio::time::timeout(WAIT, websocket.next())
                .await
                .expect("upstream should receive response.create")
                .expect("upstream WebSocket closed before response.create")
                .expect("upstream WebSocket frame should be valid");
            assert!(first.to_text().unwrap().contains("response.create"));
            let _ = create_tx.send(());
            websocket
                .send(tokio_tungstenite::tungstenite::Message::Ping(
                    vec![1, 2, 3].into(),
                ))
                .await
                .unwrap();
            loop {
                match websocket.next().await {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_)))
                    | None
                    | Some(Err(_)) => {
                        let _ = closed_tx.send(Instant::now());
                        break;
                    }
                    Some(Ok(_)) => {}
                }
            }
        }
        primary
    });
    let (_dir, gateway) = fixture(vec![
        format!("http://127.0.0.1:{primary_port}/v1"),
        format!("http://127.0.0.1:{backup_port}/v1"),
    ])
    .await;
    let (usage, events) = attach_observers(&_dir, &gateway);
    let mut diagnostic_events = events.subscribe();
    let mut client = downstream_websocket(&gateway).await;
    client
        .send(yawc::Frame::text(CREATE.to_owned()))
        .await
        .unwrap();
    tokio::time::timeout(WAIT, request_rx)
        .await
        .expect("primary upstream should receive a real WebSocket handshake")
        .unwrap();
    if generation {
        tokio::time::timeout(WAIT, create_rx)
            .await
            .expect("primary upstream should receive response.create")
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let cancelled_at = Instant::now();
    if explicit_close {
        client
            .send(yawc::Frame::close(1000.into(), "fixture cancel"))
            .await
            .unwrap();
    }
    drop(client);
    let close_window = if generation {
        USAGE_TAIL + USAGE_TAIL_TOLERANCE
    } else {
        // Allow scheduler/socket dispatch while still beating the configured
        // two-second upstream handshake timeout.
        Duration::from_secs(1)
    };
    let closed_at = tokio::time::timeout(close_window, closed_rx)
        .await
        .expect("upstream WebSocket stayed open after downstream Close/EOF")
        .unwrap();
    let close_elapsed = closed_at.duration_since(cancelled_at);
    eprintln!("WebSocket cancellation closed upstream after {close_elapsed:?}");
    assert!(close_elapsed <= close_window);
    let primary = primary_task.await.unwrap();
    assert_no_retry(&primary, &backup, Duration::from_secs(2)).await;
    if generation {
        let usage_record = wait_for_usage_record(&usage).await;
        assert_not_successful_usage(&usage_record);
        let attempt = usage_record.final_attempt().unwrap();
        assert_eq!(attempt.status, Some(101));
        assert!(!attempt.end.as_ref().unwrap().terminal_received);
        assert_neutral_health(&gateway).await;
        let _ = assert_downstream_diagnostic_policy(
            &events,
            &mut diagnostic_events,
            &_dir.path().join("events"),
            attempt
                .transfer
                .as_ref()
                .and_then(|transfer| transfer.downstream_error.as_deref()),
        )
        .await;
    }
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn websocket_close_while_upstream_handshake_is_pending_cancels_socket() {
    assert_websocket_cancel_closes_upstream(true, false).await;
}

#[tokio::test]
async fn websocket_eof_while_upstream_handshake_is_pending_cancels_socket() {
    assert_websocket_cancel_closes_upstream(false, false).await;
}

#[tokio::test]
async fn websocket_close_during_generation_closes_upstream_socket() {
    assert_websocket_cancel_closes_upstream(true, true).await;
}

#[tokio::test]
async fn websocket_eof_during_generation_closes_upstream_socket() {
    assert_websocket_cancel_closes_upstream(false, true).await;
}

#[path = "usage_tail_tests.rs"]
mod usage_tail_tests;

const RESENT: &[u8] = b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"fixture-resent\",\"status\":\"completed\"}}\n\n";

/// The next upstream connection, with its request read.
async fn accepted(upstream: &TcpListener) -> TcpStream {
    let (mut socket, _) = tokio::time::timeout(WAIT, upstream.accept())
        .await
        .expect("the request was not sent to the provider")
        .unwrap();
    read_http_request(&mut socket).await.unwrap();
    socket
}

/// Answers an upstream request with a finished response.
async fn complete(socket: &mut TcpStream) {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        RESENT.len()
    );
    socket.write_all(head.as_bytes()).await.unwrap();
    socket.write_all(RESENT).await.unwrap();
    socket.flush().await.unwrap();
}

/// Reads the client's answer up to the event `complete` wrote.
async fn read_resent(stream: &mut TcpStream) -> String {
    tokio::time::timeout(WAIT, async {
        let mut bytes = Vec::new();
        loop {
            let mut chunk = [0; 2048];
            let read = stream.read(&mut chunk).await.unwrap();
            bytes.extend_from_slice(&chunk[..read]);
            if read == 0 || String::from_utf8_lossy(&bytes).contains("fixture-resent") {
                return String::from_utf8_lossy(&bytes).into_owned();
            }
        }
    })
    .await
    .expect("the resent request was not answered")
}

/// The request's usage record once every attempt up to the last has ended.
async fn wait_for_attempts(service: &UsageService, attempts: usize) -> UsageRecord {
    let mut events = service.subscribe();
    tokio::time::timeout(WAIT, async {
        loop {
            let rows = usage_records(service);
            assert!(rows.len() <= 1);
            if let Some(record) = rows.into_iter().next().filter(|record| {
                record.attempts.len() == attempts
                    && record.attempts.iter().all(|attempt| attempt.end.is_some())
            }) {
                return record;
            }
            let _ = events.recv().await;
        }
    })
    .await
    .expect("the resent request's usage record was not written")
}

fn causes(record: &UsageRecord) -> Vec<Option<EndCause>> {
    record
        .attempts
        .iter()
        .map(|attempt| attempt.end.as_ref().map(|end| end.cause))
        .collect()
}

#[tokio::test]
async fn panel_cancel_before_headers_resends_to_the_same_provider() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (dir, gateway) = fixture(vec![format!(
        "http://{}/v1",
        upstream.local_addr().unwrap()
    )])
    .await;
    let (usage, _) = attach_observers(&dir, &gateway);
    let mut downstream = downstream_http(&gateway, REQUEST_BODY).await;
    let socket = accepted(&upstream).await;
    let pending = gateway.view().pending_requests;
    assert_eq!(pending.len(), 1);
    assert!(gateway.cancel_wait(&pending[0].request_id));
    assert!(!gateway.cancel_wait(&pending[0].request_id));
    // The interrupted attempt is dropped; the client is not answered.
    tokio::time::timeout(WAIT, wait_for_disconnect(socket))
        .await
        .unwrap();
    let mut socket = accepted(&upstream).await;
    complete(&mut socket).await;
    assert!(read_resent(&mut downstream)
        .await
        .starts_with("HTTP/1.1 200"));
    let record = wait_for_attempts(&usage, 2).await;
    assert_eq!(
        causes(&record),
        [Some(EndCause::ClientCancelled), Some(EndCause::Completed)]
    );
    assert_neutral_health(&gateway).await;
    assert!(gateway.view().pending_requests.is_empty());
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn panel_cancel_resends_past_the_retry_limit_and_back_to_its_provider() {
    let primary = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backup = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (dir, gateway) = fixture(vec![
        format!("http://{}/v1", primary.local_addr().unwrap()),
        format!("http://{}/v1", backup.local_addr().unwrap()),
    ])
    .await;
    let mut settings = gateway.view().settings;
    settings.max_retries = 0;
    edit(&gateway, &dir, Edit::Settings { settings });
    let mut downstream = downstream_http(&gateway, REQUEST_BODY).await;
    // Interrupted twice: neither attempt counts, and each resend returns to
    // the provider it was interrupted at.
    for _ in 0..2 {
        let socket = accepted(&primary).await;
        let pending = gateway.view().pending_requests;
        assert_eq!(pending.len(), 1);
        assert!(gateway.cancel_wait(&pending[0].request_id));
        tokio::time::timeout(WAIT, wait_for_disconnect(socket))
            .await
            .unwrap();
    }
    let mut socket = accepted(&primary).await;
    complete(&mut socket).await;
    assert!(read_resent(&mut downstream)
        .await
        .starts_with("HTTP/1.1 200"));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), backup.accept())
            .await
            .is_err()
    );
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn panel_cancel_queued_request_requeues_and_leaves_its_active_neighbor() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (dir, gateway) = fixture(vec![format!(
        "http://{}/v1",
        upstream.local_addr().unwrap()
    )])
    .await;
    edit(
        &gateway,
        &dir,
        Edit::ConcurrencyProvider {
            id: gateway.view().providers[0].id.clone(),
            max_concurrency: 1,
        },
    );
    let mut first = downstream_http(&gateway, REQUEST_BODY).await;
    let mut socket = accepted(&upstream).await;
    let first_id = gateway.view().pending_requests[0].request_id.clone();
    let mut second = downstream_http(&gateway, REQUEST_BODY).await;
    let mut changes = gateway.subscribe();
    let id = tokio::time::timeout(WAIT, async {
        loop {
            if gateway.view().waiting_requests == 1 {
                break gateway
                    .view()
                    .pending_requests
                    .into_iter()
                    .find(|p| p.request_id != first_id)
                    .unwrap()
                    .request_id;
            }
            let _ = changes.recv().await;
        }
    })
    .await
    .unwrap();
    assert!(gateway.cancel_wait(&id));
    // It is back in the queue under a new row; its neighbor is untouched.
    tokio::time::timeout(WAIT, async {
        loop {
            let view = gateway.view();
            if view.waiting_requests == 1
                && view
                    .pending_requests
                    .iter()
                    .any(|p| p.request_id != first_id && p.request_id != id)
            {
                break;
            }
            let _ = changes.recv().await;
        }
    })
    .await
    .expect("the interrupted request did not queue again");
    assert!(gateway
        .view()
        .pending_requests
        .iter()
        .any(|p| p.request_id == first_id));
    complete(&mut socket).await;
    assert!(read_resent(&mut first).await.starts_with("HTTP/1.1 200"));
    let mut socket = accepted(&upstream).await;
    complete(&mut socket).await;
    assert!(read_resent(&mut second).await.starts_with("HTTP/1.1 200"));
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn panel_cancel_websocket_turn_keeps_connection_for_next_turn() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (_dir, gateway) = fixture(vec![format!(
        "http://{}/v1",
        upstream.local_addr().unwrap()
    )])
    .await;
    let mut client = downstream_websocket(&gateway).await;
    client.send(yawc::Frame::text(CREATE)).await.unwrap();
    let (socket, _) = upstream.accept().await.unwrap();
    let mut server = tokio_tungstenite::accept_async(socket).await.unwrap();
    server.next().await.unwrap().unwrap();
    let id = gateway.view().pending_requests[0].request_id.clone();
    assert!(gateway.cancel_wait(&id));
    let cancelled = tokio::time::timeout(WAIT, client.next())
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(cancelled.payload()).contains("response.cancelled"));
    client.send(yawc::Frame::text(CREATE)).await.unwrap();
    let (socket, _) = tokio::time::timeout(WAIT, upstream.accept())
        .await
        .unwrap()
        .unwrap();
    let mut server2 = tokio_tungstenite::accept_async(socket).await.unwrap();
    server2.next().await.unwrap().unwrap();
    assert!(!gateway.cancel_wait(&id));
    assert_eq!(gateway.view().pending_requests.len(), 1);
    drop(server);
    drop(server2);
    drop(client);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn panel_cancel_sse_before_effective_output_resends_and_drops_the_first_stream() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (dir, gateway) = fixture(vec![format!(
        "http://{}/v1",
        upstream.local_addr().unwrap()
    )])
    .await;
    let (usage, _) = attach_observers(&dir, &gateway);
    let mut downstream = downstream_http(&gateway, REQUEST_BODY).await;
    let mut socket = accepted(&upstream).await;
    let event = b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"fixture-waiting\",\"status\":\"in_progress\"}}\n\n";
    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
    socket
        .write_all(format!("{:x}\r\n", event.len()).as_bytes())
        .await
        .unwrap();
    socket.write_all(event).await.unwrap();
    socket.write_all(b"\r\n").await.unwrap();
    let mut prefix = vec![0; 4096];
    assert!(
        tokio::time::timeout(Duration::from_millis(100), downstream.read(&mut prefix))
            .await
            .is_err()
    );
    let id = gateway.view().pending_requests[0].request_id.clone();
    assert!(gateway.cancel_wait(&id));
    tokio::time::timeout(WAIT, wait_for_disconnect(socket))
        .await
        .unwrap();
    let mut socket = accepted(&upstream).await;
    complete(&mut socket).await;
    let response = read_resent(&mut downstream).await;
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(!response.contains("fixture-waiting"));
    let record = wait_for_attempts(&usage, 2).await;
    assert_eq!(
        causes(&record),
        [Some(EndCause::ClientCancelled), Some(EndCause::Completed)]
    );
    assert_neutral_health(&gateway).await;
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn panel_cancel_capacity_backoff_keeps_the_cooldown_and_sends_when_it_ends() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (dir, gateway) = fixture(vec![format!(
        "http://{}/v1",
        upstream.local_addr().unwrap()
    )])
    .await;
    let mut settings = gateway.view().settings;
    settings.rate_limit_seconds = 60;
    edit(&gateway, &dir, Edit::Settings { settings });
    let mut downstream = downstream_http(&gateway, REQUEST_BODY).await;
    let mut socket = accepted(&upstream).await;
    let body = r#"{"error":{"code":"model_capacity_exceeded"}}"#;
    socket.write_all(format!("HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    drop(socket);
    let mut changes = gateway.subscribe();
    tokio::time::timeout(WAIT, async {
        while gateway.view().waiting_requests == 0 {
            let _ = changes.recv().await;
        }
    })
    .await
    .unwrap();
    let before = gateway.view();
    let id = before.pending_requests[0].request_id.clone();
    assert!(gateway.cancel_wait(&id));
    // The client is not answered: the request waits out the same cooldown
    // under a new row, and nothing is sent while it lasts.
    tokio::time::timeout(WAIT, async {
        loop {
            let view = gateway.view();
            if view.waiting_requests == 1
                && view.pending_requests.iter().all(|p| p.request_id != id)
            {
                break;
            }
            let _ = changes.recv().await;
        }
    })
    .await
    .expect("the interrupted request did not wait again");
    assert_eq!(
        gateway.view().providers[0].health.failures,
        before.providers[0].health.failures
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(1500), upstream.accept())
            .await
            .is_err()
    );
    assert_eq!(gateway.view().pending_requests.len(), 1);
    // Clearing the cooldown lets it through.
    edit(
        &gateway,
        &dir,
        Edit::Reset {
            id: before.providers[0].id.clone(),
        },
    );
    let mut socket = accepted(&upstream).await;
    complete(&mut socket).await;
    assert!(read_resent(&mut downstream)
        .await
        .starts_with("HTTP/1.1 200"));
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn panel_cancel_websocket_admission_keeps_neighbor_running() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (dir, gateway) = fixture(vec![format!(
        "http://{}/v1",
        upstream.local_addr().unwrap()
    )])
    .await;
    edit(
        &gateway,
        &dir,
        Edit::ConcurrencyProvider {
            id: gateway.view().providers[0].id.clone(),
            max_concurrency: 1,
        },
    );
    let first = downstream_http(&gateway, REQUEST_BODY).await;
    let (mut socket, _) = upstream.accept().await.unwrap();
    read_http_request(&mut socket).await.unwrap();
    let first_id = gateway.view().pending_requests[0].request_id.clone();
    let mut changes = gateway.subscribe();
    let mut client = downstream_websocket(&gateway).await;
    client.send(yawc::Frame::text(CREATE)).await.unwrap();
    tokio::time::timeout(WAIT, async {
        while gateway.view().waiting_requests == 0 {
            let _ = changes.recv().await;
        }
    })
    .await
    .unwrap();
    let id = gateway
        .view()
        .pending_requests
        .iter()
        .find(|p| p.request_id != first_id)
        .unwrap()
        .request_id
        .clone();
    assert!(gateway.cancel_wait(&id));
    let frame = tokio::time::timeout(WAIT, client.next())
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(frame.payload()).contains("response.cancelled"));
    assert_eq!(gateway.view().waiting_requests, 0);
    assert!(gateway
        .view()
        .pending_requests
        .iter()
        .any(|p| p.request_id == first_id));
    drop(client);
    drop(first);
    drop(socket);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn panel_cancel_websocket_backoff_preserves_operation_id_and_releases_wait() {
    let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (dir, gateway) = fixture(vec![format!(
        "http://{}/v1",
        upstream.local_addr().unwrap()
    )])
    .await;
    let mut settings = gateway.view().settings;
    settings.capacity_retry_seconds = 60;
    edit(&gateway, &dir, Edit::Settings { settings });
    let mut client = downstream_websocket(&gateway).await;
    client.send(yawc::Frame::text(CREATE)).await.unwrap();
    let (socket, _) = upstream.accept().await.unwrap();
    let mut server = tokio_tungstenite::accept_async(socket).await.unwrap();
    server.next().await.unwrap().unwrap();
    let id = gateway.view().pending_requests[0].request_id.clone();
    let mut changes = gateway.subscribe();
    server
        .send(tokio_tungstenite::tungstenite::Message::Text(
            r#"{"type":"error","error":{"code":"model_capacity_exceeded","message":"capacity"}}"#
                .into(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(WAIT, async {
        while !gateway
            .view()
            .pending_requests
            .iter()
            .any(|p| p.phase == "ws_wait")
        {
            let _ = changes.recv().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(gateway.view().pending_requests[0].request_id, id);
    let failures = gateway.view().providers[0].health.failures;
    assert!(gateway.cancel_wait(&id));
    let frame = tokio::time::timeout(WAIT, client.next())
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(frame.payload()).contains("response.cancelled"));
    assert!(gateway.view().pending_requests.is_empty());
    assert_eq!(gateway.view().waiting_requests, 0);
    assert_eq!(gateway.view().providers[0].health.failures, failures);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), upstream.accept())
            .await
            .is_err()
    );
    drop(server);
    drop(client);
    gateway.stop().await.unwrap();
}
