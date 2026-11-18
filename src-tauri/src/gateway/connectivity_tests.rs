//! A refused loopback port stands in for a dead network; rebinding it brings
//! the network back.
use super::*;
use crate::events::{Change, Reason};
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
    f.edit(Edit::SaveProvider {
        id: Some(provider.id.clone()),
        base_url: provider.base_url.clone(),
        token: String::new(),
        name: None,
        quota_access_token: None,
        search_only: None,
        end_on_message_stop: None,
        filter_count_tokens: None,
        buffer_complete_response: None,
        endless_mode: Some(endless),
        upstream_retry_seconds: Some(1),
        normal_retry_seconds: Some(1),
    });
    let mut settings = f.gateway.view().settings;
    settings.retry_budget_seconds = 10;
    settings.network_probe = probe;
    settings.network_probe_attempts = 1;
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
async fn held_requests_share_one_probe_and_cancelling_them_ends_it() {
    let port = free_port().await;
    let (f, _dir, mut changes) = fixture(port, true, true).await;
    let (first, second) = (send(&f), send(&f));
    held(&f, 2).await;
    assert_eq!(
        reports(&mut changes, Reason::NetworkHold).await,
        [(Reason::NetworkHold, true)]
    );
    for pending in f.gateway.view().pending_requests {
        assert!(f.gateway.cancel_wait(&pending.request_id));
    }
    first.await.unwrap();
    second.await.unwrap();
    released(&f).await;
    while let Ok(change) = changes.try_recv() {
        assert!(change
            .record
            .is_none_or(|record| record.reason == Reason::Network));
    }
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
