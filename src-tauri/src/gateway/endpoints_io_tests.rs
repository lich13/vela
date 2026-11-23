use super::super::{
    model::{Edit, Provider},
    replay, Gateway,
};
use super::{digest, now, pin_key, Pin, Service};
use crate::errors::code;
use hyper::{body::Incoming, Request, Response, StatusCode};
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioIo},
};
use std::{
    collections::{BTreeMap, HashMap},
    convert::Infallible,
    fs,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

fn fixture_provider(base_url: &str) -> Provider {
    Provider {
        id: "fixture-provider".into(),
        name: "fixture-provider".into(),
        base_url: base_url.into(),
        token: "fixture-token".into(),
        queued: true,
        version: "fixture-version".into(),
        max_concurrency: 0,
        max_rpm: 0,
        allowed_models: None,
        handoff_after_compaction: false,
        take_new_threads: false,
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

fn pin_snapshot(entries: &HashMap<String, Pin>) -> BTreeMap<String, (String, u64)> {
    entries
        .iter()
        .map(|(key, pin)| (key.clone(), (pin.endpoint.clone(), pin.updated)))
        .collect()
}

fn write_entries(path: &Path, entries: &HashMap<String, Pin>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec(entries).unwrap()).unwrap();
}

fn read_snapshot(path: &Path) -> BTreeMap<String, (String, u64)> {
    let entries: HashMap<String, Pin> = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    pin_snapshot(&entries)
}

#[test]
fn failed_pin_write_for_another_thread_does_not_block_existing_selection() {
    let primary = "https://primary.example.invalid/v1";
    let provider = fixture_provider(primary);
    let urls = vec![primary.to_owned()];
    let existing_thread = "fixture-existing-thread";
    let failing_thread = "fixture-failing-thread";
    let existing_key = pin_key(&provider, "thread", existing_thread);
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("state/endpoint-owners.json");
    let entries = HashMap::from([(
        existing_key,
        Pin {
            endpoint: digest(primary),
            updated: now(),
        },
    )]);
    write_entries(&path, &entries);
    let service = Service::new(path.clone());

    assert_eq!(
        service
            .select(&provider, &urls, Some(existing_thread), None, false)
            .unwrap(),
        primary
    );

    // An obstruction at the persistence target keeps every later write failing.
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let first_error = service
        .select(&provider, &urls, Some(failing_thread), None, false)
        .unwrap_err();
    assert_eq!(first_error.code, code::ENDPOINT_AFFINITY);

    let selected = service
        .select(&provider, &urls, Some(existing_thread), None, false)
        .expect("another thread's failed pin write must not block an existing pin");
    assert_eq!(selected, primary);
    // A failed refresh preserves the known endpoint and waits before retrying.
    let key = pin_key(&provider, "thread", existing_thread);
    service.pins.lock().get_mut(&key).unwrap().updated = now().saturating_sub(60_001);
    let before_refresh = now();
    assert_eq!(
        service
            .select(&provider, &urls, Some(existing_thread), None, false)
            .unwrap(),
        primary
    );
    let attempted = service.pins.lock()[&key].updated;
    assert!(attempted >= before_refresh);
    assert_eq!(
        service
            .select(&provider, &urls, Some(existing_thread), None, false)
            .unwrap(),
        primary
    );
    assert_eq!(service.pins.lock()[&key].updated, attempted);
    assert!(path.is_dir());
}

#[test]
fn successful_initial_pin_matches_the_persisted_entries() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("endpoint-owners.json");
    fs::write(&path, b"{corrupt").unwrap();
    let service = Service::new(path.clone());
    assert!(service.pins.lock().is_empty());
    let provider = fixture_provider("https://primary.example.invalid/v1");
    let thread = "fixture-thread";

    let selected = service
        .select(
            &provider,
            std::slice::from_ref(&provider.base_url),
            Some(thread),
            None,
            false,
        )
        .unwrap();

    assert_eq!(selected, provider.base_url);
    let entries = pin_snapshot(&service.pins.lock());
    let key = pin_key(&provider, "thread", thread);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[&key].0, digest(&provider.base_url));
    assert_eq!(read_snapshot(&path), entries);
}

async fn counter_upstream(hits: Arc<AtomicUsize>) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let hits = hits.clone();
            tokio::spawn(async move {
                let service = hyper::service::service_fn(move |_request: Request<Incoming>| {
                    let hits = hits.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, Infallible>(
                            Response::builder()
                                .status(StatusCode::NO_CONTENT)
                                .body(replay::empty())
                                .unwrap(),
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    port
}

#[tokio::test]
async fn first_affinity_persist_failure_returns_before_sending_to_upstream() {
    let hits = Arc::new(AtomicUsize::new(0));
    let upstream_port = counter_upstream(hits.clone()).await;
    let temp = tempfile::tempdir().unwrap();
    let base_url = format!("http://127.0.0.1:{upstream_port}/v1");
    let config = format!(
        "# fixture\nmodel_provider='custom'\n[model_providers.custom]\nbase_url = {}\nexperimental_bearer_token = \"fixture-token\"\nwire_api='responses'\nsupports_websockets=false\n",
        serde_json::to_string(&base_url).unwrap()
    );
    fs::write(temp.path().join("config.toml"), config).unwrap();
    let gateway = Gateway::new(temp.path().to_path_buf()).unwrap();

    let port_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_port = port_listener.local_addr().unwrap().port();
    drop(port_listener);
    let mut settings = gateway.view().settings;
    settings.port = gateway_port;
    gateway
        .edit(
            Edit::Settings { settings },
            &gateway.view().revision,
            temp.path(),
        )
        .unwrap();
    gateway
        .edit(
            Edit::SaveProvider {
                delegate: Default::default(),
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
                base_url: base_url.clone(),
                token: "fixture-token".into(),
                name: Some("fixture".into()),
            },
            &gateway.view().revision,
            temp.path(),
        )
        .unwrap();
    fs::write(temp.path().join("auth.json"), "fixture-auth").unwrap();
    gateway
        .start(&gateway.view().revision, temp.path())
        .await
        .unwrap();

    // The service was initialized while this path was absent. A directory at
    // the exact persistence target makes the first atomic replacement fail.
    fs::create_dir(temp.path().join("endpoint-owners.json")).unwrap();
    let (gateway_port, token) = {
        let inner = gateway.0.inner.lock();
        (inner.store.settings.port, inner.store.local_token.clone())
    };
    let client: super::super::HttpClient = Client::builder(TokioExecutor::new()).build(
        super::super::connector::Connector::new(Duration::from_secs(2), 0),
    );
    let request = Request::builder()
        .method("POST")
        .uri(format!("http://127.0.0.1:{gateway_port}/v1/responses"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("thread-id", "fixture-thread")
        .body(replay::full(
            br#"{"model":"gpt-fixture","input":"fixture"}"#.as_slice(),
        ))
        .unwrap();
    let response = client.request(request).await;
    let status = match response {
        Ok(response) => response.status(),
        Err(error) => {
            let _ = gateway.stop().await;
            panic!("loopback gateway request failed: {error}");
        }
    };
    let upstream_hits = hits.load(Ordering::SeqCst);
    let owner_count = gateway.0.endpoints.pins.lock().len();
    gateway.stop().await.unwrap();

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(upstream_hits, 0);
    assert_eq!(owner_count, 0);
}

#[tokio::test]
async fn frozen_queued_probes_preserve_complete_samples_and_manual_probes_still_run() {
    use super::super::ClientId;
    use super::{Batch, Measurement};
    use std::time::Instant;
    let hits = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let port = super::super::tests::server({
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
                Response::builder()
                    .status(204)
                    .body(replay::empty())
                    .unwrap()
            }
        }
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::new(dir.path().to_path_buf()).unwrap();
    let primary = format!("http://127.0.0.1:{port}/v1");
    gateway
        .edit(
            Edit::SaveProvider {
                delegate: Default::default(),
                id: None,
                name: None,
                base_url: primary.clone(),
                token: "fixture-token".into(),
                quota_access_token: None,
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
            },
            &gateway.view().revision,
            dir.path(),
        )
        .unwrap();
    let id = gateway.view().providers[0].id.clone();
    let urls = vec![primary, format!("http://127.0.0.1:{port}/secondary/v1")];
    gateway
        .edit(
            Edit::EndpointsProvider {
                id: id.clone(),
                base_urls: urls.clone(),
            },
            &gateway.view().revision,
            dir.path(),
        )
        .unwrap();
    let provider = gateway.0.inner.lock().store.providers[0].clone();
    let checked = Instant::now() - Duration::from_secs(600);
    gateway.0.endpoints.samples.lock().insert(
        id.clone(),
        Batch {
            config: Service::config(&provider, &urls),
            checked,
            rows: urls
                .iter()
                .map(|url| Measurement {
                    base_url: url.clone(),
                    latency_ms: Some(123),
                    status: Some(200),
                    error: None,
                    checked_at: 1,
                })
                .collect(),
        },
    );
    let permits = gateway.0.endpoints.permits.acquire_many(3).await.unwrap();
    let pending = tokio::spawn({
        let g = gateway.clone();
        let id = id.clone();
        async move { g.probe_endpoints(&id, false).await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    gateway
        .freeze_gate()
        .set_selected(Some(ClientId::Codex), true)
        .unwrap();
    release.add_permits(1);
    drop(permits);
    pending.await.unwrap().unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    {
        let samples = gateway.0.endpoints.samples.lock();
        let batch = &samples[&id];
        assert_eq!(batch.checked, checked);
        assert_eq!(batch.rows.len(), 2);
        assert!(batch.rows.iter().all(|r| r.checked_at == 1));
    }
    gateway
        .freeze_gate()
        .set_selected(Some(ClientId::Codex), false)
        .unwrap();
    gateway.probe_endpoints(&id, false).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 6);
    gateway
        .freeze_gate()
        .set_selected(Some(ClientId::Codex), true)
        .unwrap();
    gateway.probe_endpoints(&id, true).await.unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 10);
}
