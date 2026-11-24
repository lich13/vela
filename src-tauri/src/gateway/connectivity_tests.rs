//! A refused loopback port stands in for a dead network; rebinding it brings
//! the network back.
use super::*;
use crate::events::{Change, Reason};
use crate::gateway::test_support::form;
use std::sync::atomic::AtomicBool;

const MESSAGE: &[u8] = br#"{"id":"msg_fixture","type":"message","model":"claude-fixture","stop_reason":"end_turn","content":[{"type":"text","text":"complete"}],"usage":{"input_tokens":5,"output_tokens":3}}"#;
const REQUEST: &[u8] = br#"{"model":"claude-fixture","stream":false,"max_tokens":32,"messages":[{"role":"user","content":"fixture"}]}"#;
const LONG: Duration = Duration::from_secs(15);

async fn free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

/// Answers the model list with a 401 and every message in full. With `cut`,
/// the first connection is closed before any response.
async fn upstream(port: u16, listings: Arc<AtomicUsize>, cut: bool) -> tokio::task::JoinHandle<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let cut = Arc::new(AtomicBool::new(cut));
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            if cut.swap(false, Ordering::SeqCst) {
                continue;
            }
            let listings = listings.clone();
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(socket),
                        hyper::service::service_fn(move |request: Request<Incoming>| {
                            let listings = listings.clone();
                            async move {
                                let listing = request.uri().path().ends_with("/models");
                                let _ = request.into_body().collect().await;
                                Ok::<_, Infallible>(if listing {
                                    listings.fetch_add(1, Ordering::SeqCst);
                                    Response::builder().status(401).body(full("{}")).unwrap()
                                } else {
                                    Response::builder()
                                        .header("content-type", "application/json")
                                        .body(full(MESSAGE))
                                        .unwrap()
                                })
                            }
                        }),
                    )
                    .await;
            });
        }
    })
}

async fn fixture(
    port: u16,
    endless: bool,
    probe: bool,
) -> (
    Fixture,
    tempfile::TempDir,
    tokio::sync::broadcast::Receiver<Change>,
) {
    let f = Fixture::new(ClientId::Claude, &[format!("http://127.0.0.1:{port}/v1")]).await;
    let provider = &f.gateway.view().providers[0];
    f.edit(form(serde_json::json!({
        "id": provider.id,
        "baseUrl": provider.base_url,
        "token": "",
        "endlessMode": endless,
        "upstreamRetrySeconds": 1,
        "normalRetrySeconds": 1,
        "limitRestSeconds": 0,
    })));
    let mut settings = f.gateway.view().settings;
    settings.retry_budget_seconds = 10;
    settings.network_probe = probe;
    settings.network_probe_attempts = 1;
    settings.hold_idle_seconds = 1;
    f.edit(Edit::Settings { settings });
    let dir = tempfile::tempdir().unwrap();
    let events = crate::events::Service::new(dir.path());
    let changes = events.subscribe();
    f.gateway.set_diagnostics(events);
    f.start().await;
    (f, dir, changes)
}

fn send(f: &Fixture) -> tokio::task::JoinHandle<String> {
    let gateway = f.gateway.clone();
    tokio::spawn(async move {
        let response = request(&gateway, "/v1/messages", REQUEST).await;
        String::from_utf8_lossy(&response.into_body().collect().await.unwrap().to_bytes())
            .into_owned()
    })
}

async fn held(f: &Fixture, count: usize) {
    tokio::time::timeout(LONG, async {
        loop {
            let pending = f.gateway.view().pending_requests;
            if pending.len() == count
                && pending
                    .iter()
                    .all(|p| p.phase == "held" && p.remaining_seconds.is_none())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the request was not held");
}

/// The hold and restore reports up to the first `last`, with their notify flag.
async fn reports(
    changes: &mut tokio::sync::broadcast::Receiver<Change>,
    last: Reason,
) -> Vec<(Reason, bool)> {
    let mut seen = Vec::new();
    tokio::time::timeout(LONG, async {
        loop {
            let change = changes.recv().await.expect("diagnostic writer closed");
            let Some(record) = change.record else {
                continue;
            };
            if matches!(record.reason, Reason::NetworkHold | Reason::NetworkRestored) {
                seen.push((record.reason, change.notify));
                if record.reason == last {
                    return;
                }
            }
        }
    })
    .await
    .expect("the report did not arrive");
    seen
}

async fn released(f: &Fixture) {
    tokio::time::timeout(LONG, async {
        while !f.gateway.0.links.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the probe kept running");
}

#[tokio::test]
async fn a_dead_network_holds_the_request_and_its_return_sends_it_again() {
    for endless in [true, false] {
        let port = free_port().await;
        let (f, _dir, mut changes) = fixture(port, endless, true).await;
        let pending = send(&f);
        held(&f, 1).await;
        // Held time is outside the ten-second retry budget.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let listings = Arc::new(AtomicUsize::new(0));
        let server = upstream(port, listings.clone(), false).await;
        assert!(pending.await.unwrap().contains("complete"), "{endless}");
        assert_eq!(
            reports(&mut changes, Reason::NetworkRestored).await,
            [(Reason::NetworkHold, true), (Reason::NetworkRestored, true)]
        );
        assert_eq!(listings.load(Ordering::SeqCst), 1);
        released(&f).await;
        server.abort();
        f.stop().await;
    }
}

#[tokio::test]
async fn any_answer_to_the_probe_keeps_the_ordinary_retry_and_the_switch_turns_it_off() {
    for probe in [true, false] {
        let port = free_port().await;
        let listings = Arc::new(AtomicUsize::new(0));
        let server = upstream(port, listings.clone(), true).await;
        let (f, _dir, mut changes) = fixture(port, true, probe).await;
        assert!(send(&f).await.unwrap().contains("complete"));
        assert_eq!(listings.load(Ordering::SeqCst), usize::from(probe));
        released(&f).await;
        assert!(changes.try_recv().into_iter().all(|change| change
            .record
            .is_none_or(|record| record.reason == Reason::Network)));
        server.abort();
        f.stop().await;
    }
}

#[tokio::test]
async fn held_requests_share_one_probe_and_a_resend_is_held_again() {
    let port = free_port().await;
    let (f, _dir, mut changes) = fixture(port, true, true).await;
    let (first, second) = (send(&f), send(&f));
    held(&f, 2).await;
    assert_eq!(
        reports(&mut changes, Reason::NetworkHold).await,
        [(Reason::NetworkHold, true)]
    );
    // With the network still down, an interrupted request goes back through
    // the probe and is held under a new row instead of being answered.
    let interrupted = f.gateway.view().pending_requests;
    for pending in &interrupted {
        assert!(f.gateway.cancel_wait(&pending.request_id));
    }
    tokio::time::timeout(LONG, async {
        loop {
            let pending = f.gateway.view().pending_requests;
            if pending.len() == 2
                && pending.iter().all(|p| {
                    p.phase == "held"
                        && interrupted.iter().all(|old| old.request_id != p.request_id)
                })
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the interrupted requests were not held again");
    assert!(!first.is_finished() && !second.is_finished());
    // Once nobody waits for the endpoint, its probe ends.
    f.stop().await;
    first.await.unwrap();
    second.await.unwrap();
    released(&f).await;
    while let Ok(change) = changes.try_recv() {
        assert!(change
            .record
            .is_none_or(|record| record.reason != Reason::NetworkRestored));
    }
}

fn freeze(f: &Fixture, frozen: bool) {
    f.gateway
        .freeze_gate()
        .set_selected(Some(ClientId::Claude), frozen)
        .unwrap();
}

#[tokio::test]
async fn a_freeze_stops_the_probe_and_the_thaw_resumes_it() {
    let port = free_port().await;
    let (f, _dir, mut changes) = fixture(port, true, true).await;
    let pending = send(&f);
    held(&f, 1).await;
    assert_eq!(
        reports(&mut changes, Reason::NetworkHold).await,
        [(Reason::NetworkHold, true)]
    );
    freeze(&f, true);
    // A probe already on its way is refused before the network returns.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let listings = Arc::new(AtomicUsize::new(0));
    let server = upstream(port, listings.clone(), false).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(listings.load(Ordering::SeqCst), 0);
    assert!(!pending.is_finished());
    while let Ok(change) = changes.try_recv() {
        assert!(change
            .record
            .is_none_or(|record| record.reason != Reason::NetworkRestored));
    }
    freeze(&f, false);
    assert!(pending.await.unwrap().contains("complete"));
    assert_eq!(
        reports(&mut changes, Reason::NetworkRestored).await,
        [(Reason::NetworkRestored, true)]
    );
    assert_eq!(listings.load(Ordering::SeqCst), 1);
    released(&f).await;
    server.abort();
    f.stop().await;
}

#[tokio::test]
async fn a_request_awaiting_the_verdict_does_not_block_the_thaw() {
    let port = free_port().await;
    let (f, _dir, _changes) = fixture(port, true, true).await;
    // Twenty failed probes decide the network is down: two seconds of checking.
    let mut settings = f.gateway.view().settings;
    settings.network_probe_attempts = 20;
    f.edit(Edit::Settings { settings });
    let pending = send(&f);
    tokio::time::timeout(LONG, async {
        while !f
            .gateway
            .view()
            .pending_requests
            .iter()
            .any(|p| p.phase == "retry")
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the request did not reach the probe");
    freeze(&f, true);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(f.gateway.freeze_gate().view().draining, 0);
    let listings = Arc::new(AtomicUsize::new(0));
    let server = upstream(port, listings.clone(), false).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(listings.load(Ordering::SeqCst), 0);
    assert!(!pending.is_finished());
    freeze(&f, false);
    assert!(pending.await.unwrap().contains("complete"));
    assert_eq!(listings.load(Ordering::SeqCst), 1);
    released(&f).await;
    server.abort();
    f.stop().await;
}

#[tokio::test]
async fn a_resend_during_a_freeze_waits_at_admission_until_the_thaw() {
    let port = free_port().await;
    let (f, _dir, _changes) = fixture(port, true, true).await;
    let pending = send(&f);
    held(&f, 1).await;
    freeze(&f, true);
    let id = f.gateway.view().pending_requests[0].request_id.clone();
    assert!(f.gateway.cancel_wait(&id));
    // The request left the hold, so its probe ends; the freeze keeps it queued.
    released(&f).await;
    let listings = Arc::new(AtomicUsize::new(0));
    let server = upstream(port, listings.clone(), false).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!pending.is_finished());
    assert_eq!(f.gateway.view().pending_requests.len(), 1);
    freeze(&f, false);
    assert!(pending.await.unwrap().contains("complete"));
    assert_eq!(listings.load(Ordering::SeqCst), 0);
    server.abort();
    f.stop().await;
}

#[test]
fn settings_default_to_three_probes_and_bound_the_count() {
    let mut saved = serde_json::to_value(Settings::default()).unwrap();
    let fields = saved.as_object_mut().unwrap();
    fields.remove("networkProbe");
    fields.remove("networkProbeAttempts");
    let settings: Settings = serde_json::from_value(saved).unwrap();
    assert!(settings.network_probe);
    assert_eq!(settings.network_probe_attempts, 3);
    for (attempts, valid) in [(0, false), (1, true), (20, true), (21, false)] {
        let settings = Settings {
            network_probe_attempts: attempts,
            ..Settings::default()
        };
        assert_eq!(settings.validate().is_ok(), valid, "{attempts}");
    }
}

/// Accepts one connection, starts a response and goes silent. The listener is
/// gone by then, so the port refuses every connection after it.
fn stalling(listener: tokio::net::TcpListener) -> tokio::task::JoinHandle<tokio::net::TcpStream> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        drop(listener);
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = socket.read(&mut buffer).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&buffer[..read]);
        }
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n14\r\n{\"id\":\"msg_fixture\",\r\n",
            )
            .await
            .unwrap();
        socket
    })
}

#[tokio::test]
async fn a_response_gone_silent_joins_the_hold_when_its_endpoint_goes_down() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (f, _dir, mut changes) = fixture(port, true, true).await;
    let stalled = stalling(listener);
    let first = send(&f);
    let socket = stalled.await.unwrap();
    // The first response is under way and silent; the next request finds the
    // network gone, and that outage is what ends the first one's wait.
    let second = send(&f);
    held(&f, 2).await;
    drop(socket);
    let listings = Arc::new(AtomicUsize::new(0));
    let server = upstream(port, listings.clone(), false).await;
    assert!(first.await.unwrap().contains("complete"));
    assert!(second.await.unwrap().contains("complete"));
    let mut silenced = None;
    let mut seen = Vec::new();
    tokio::time::timeout(LONG, async {
        while seen.last() != Some(&Reason::NetworkRestored) {
            let change = changes.recv().await.expect("diagnostic writer closed");
            let Some(record) = change.record else {
                continue;
            };
            if record.details.timeout_phase.as_deref() == Some("hold_idle") {
                silenced = record.details.timeout_seconds;
            }
            if matches!(record.reason, Reason::NetworkHold | Reason::NetworkRestored) {
                seen.push(record.reason);
            }
        }
    })
    .await
    .expect("the report did not arrive");
    assert_eq!(seen, [Reason::NetworkHold, Reason::NetworkRestored]);
    assert_eq!(silenced, Some(1));
    released(&f).await;
    f.stop().await;
    server.abort();
}

#[tokio::test]
async fn a_silent_response_is_left_alone_while_its_endpoint_is_not_held() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let upstream = server(move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        async move {
            let frames = async_stream::stream! {
                yield Ok::<_, BoxError>(Frame::data(Bytes::from_static(&MESSAGE[..20])));
                tokio::time::sleep(Duration::from_millis(2500)).await;
                yield Ok::<_, BoxError>(Frame::data(Bytes::from_static(&MESSAGE[20..])));
            };
            Response::builder()
                .header("content-type", "application/json")
                .body(StreamBody::new(frames).boxed_unsync())
                .unwrap()
        }
    })
    .await;
    // Silent for longer than the one-second outage limit, with no outage.
    let (f, _dir, _changes) = fixture(upstream.port, true, true).await;
    assert!(send(&f).await.unwrap().contains("complete"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(f.gateway.0.links.is_empty());
    f.stop().await;
}
