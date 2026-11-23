//! Search ownership regressions.
//!
//! The fixture starts an ordinary provider and a provider that only accepts
//! new threads or compaction handoffs.  Search requests still carry a reliable
//! thread id, but must use the existing owner reservation without consuming
//! the compaction boundary or changing ownership. With a provider set to take
//! web searches, a search is that queue's alone.
use super::*;
use crate::usage::{
    model::{Filter, Operation, Record},
    Service,
};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{body::Incoming, Request, Response, StatusCode};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use parking_lot::Mutex;
use serde_json::json;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, Semaphore};

const SEARCH_REQUEST: &[u8] = br#"{"model":"fixture-search-model","query":"fixture search"}"#;
const SEARCH_RESPONSE: &[u8] = br#"{"model":"fixture-search-model","results":[]}"#;
const ORDINARY_RESPONSE: &[u8] = br#"{"provider":"ordinary"}"#;
const METADATA_SEARCH_REQUEST: &[u8] = br#"{"id":"fixture-session-body-id","model":"gpt-5-codex","input":[{"role":"user","content":"fixture metadata search"}],"commands":[{"type":"web_search","query":"fixture"}]}"#;
const BODY_ID_SEARCH_REQUEST: &[u8] = br#"{"id":"fixture-v02513-metadata-thread","model":"gpt-5-codex","input":[{"role":"user","content":"fixture body id only"}],"commands":[{"type":"web_search","query":"fixture"}]}"#;
const METADATA_THREAD: &str = "fixture-v02513-metadata-thread";
const METADATA_HEADER: &str =
    r#"{"thread_id":"fixture-v02513-metadata-thread","turn_id":"fixture-turn","source":"codex"}"#;
const INVALID_METADATA: &str = "{\"thread_id\":\"fixture-invalid\"";

async fn search_request(
    gateway: &Gateway,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Response<Incoming> {
    let client: Client<_, super::super::replay::WireBody> = Client::builder(TokioExecutor::new())
        .build(super::super::connector::Connector::new(
            Duration::from_secs(3),
            0,
        ));
    let (token, port) = {
        let state = gateway.0.inner.lock();
        (state.store.local_token.clone(), state.store.settings.port)
    };
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("http://127.0.0.1:{port}{path}"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    for (key, value) in headers {
        request = request.header(*key, *value);
    }
    client
        .request(
            request
                .body(super::super::replay::full(body.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn response_bytes(response: Response<Incoming>) -> Bytes {
    tokio::time::timeout(Duration::from_secs(5), response.into_body().collect())
        .await
        .expect("fixture search response timed out")
        .unwrap()
        .to_bytes()
}

async fn assert_ordinary_search(gateway: &Gateway, headers: &[(&str, &str)], body: &[u8]) {
    let response = search_request(gateway, "/v1/alpha/search", headers, body).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_bytes(response).await, ORDINARY_RESPONSE);
}

fn metadata_headers(values: &[&str]) -> hyper::HeaderMap {
    let mut headers = hyper::HeaderMap::new();
    for value in values {
        headers.append(
            "x-codex-turn-metadata",
            hyper::header::HeaderValue::from_str(value).unwrap(),
        );
    }
    headers
}

fn usage_rows(usage: &Service) -> Vec<Record> {
    usage
        .query(|store| Ok(store.logs(&Filter::default())?.rows))
        .unwrap()
}

async fn wait_for_usage(usage: &Service, count: usize) -> Vec<Record> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = usage_rows(usage);
            if rows.len() >= count {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected {count} usage rows, got {:?}",
            usage_rows(usage)
                .iter()
                .map(|r| (
                    r.attempts.len(),
                    r.final_attempt().map(|a| (&a.operation, &a.response_id))
                ))
                .collect::<Vec<_>>()
        )
    })
}

fn update_max(max: &AtomicUsize, current: usize) {
    let mut previous = max.load(Ordering::Acquire);
    while current > previous {
        match max.compare_exchange(previous, current, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => break,
            Err(next) => previous = next,
        }
    }
}

fn configure_search_owner(gateway: &Gateway, dir: &tempfile::TempDir, owner: &str) {
    super::v022_update(
        gateway,
        dir,
        Edit::PolicyProvider {
            id: owner.into(),
            allowed_models: Some(vec!["fixture-model".into()]),
            handoff_after_compaction: Some(true),
            take_new_threads: Some(true),
        },
    );
    super::v022_update(
        gateway,
        dir,
        Edit::ConcurrencyProvider {
            id: owner.into(),
            max_concurrency: 1,
        },
    );
}

async fn seed_owner(
    dir: &tempfile::TempDir,
    gateway: &Gateway,
    thread: &str,
    owner: &str,
) -> (Service, String) {
    let usage = Service::new(dir.path());
    gateway.set_usage(usage.clone());
    super::v022_start(gateway, dir).await;

    let seed = super::v022_request(gateway, thread, super::v022_initial_body()).await;
    assert_eq!(seed.status(), StatusCode::OK);
    assert_eq!(
        super::v022_body(seed).await,
        super::v022_seed_response().as_bytes()
    );

    let thread_hash = crate::storage::digest(thread.as_bytes());
    assert_eq!(gateway.0.compaction.owner(&thread_hash), Some(owner.into()));
    assert!(gateway.0.compaction.boundary_matches(
        &thread_hash,
        Some(&crate::storage::digest(super::V022_WINDOW.as_bytes()))
    ));
    (usage, thread_hash)
}

#[tokio::test]
async fn v02513_search_paths_follow_handoff_owner_and_charge_once() {
    let owner_hits = Arc::new(AtomicUsize::new(0));
    let owner_bodies = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let owner_hit_count = owner_hits.clone();
    let owner_seen = owner_bodies.clone();
    let owner_port = super::v022_server(move |request| {
        let owner_hit_count = owner_hit_count.clone();
        let owner_seen = owner_seen.clone();
        async move {
            let path = request.uri().path().to_owned();
            let body = request.into_body().collect().await.unwrap().to_bytes();
            owner_hit_count.fetch_add(1, Ordering::SeqCst);
            owner_seen.lock().push(body.to_vec());
            if path.ends_with("/alpha/search") {
                super::v022_json_response(StatusCode::OK, Bytes::from_static(SEARCH_RESPONSE))
            } else {
                super::v022_json_response(StatusCode::OK, super::v022_seed_response())
            }
        }
    })
    .await;
    let ordinary_hits = Arc::new(AtomicUsize::new(0));
    let ordinary_hit_count = ordinary_hits.clone();
    let ordinary_port = super::v022_server(move |request| {
        let ordinary_hit_count = ordinary_hit_count.clone();
        async move {
            let _ = request.into_body().collect().await.unwrap();
            ordinary_hit_count.fetch_add(1, Ordering::SeqCst);
            super::v022_json_response(StatusCode::OK, Bytes::from_static(ORDINARY_RESPONSE))
        }
    })
    .await;

    let (dir, gateway) = super::v022_fixture(vec![
        format!("http://127.0.0.1:{ordinary_port}/v1"),
        format!("http://127.0.0.1:{owner_port}/v1"),
    ])
    .await;
    let ids = super::v022_ids(&gateway);
    let owner = ids[1].clone();
    configure_search_owner(&gateway, &dir, &owner);
    assert!(gateway
        .view()
        .providers
        .iter()
        .find(|provider| provider.id == owner)
        .is_some_and(|provider| {
            provider.handoff_after_compaction && provider.take_new_threads
        }));
    let thread = "fixture-v02513-search-owner";
    let (usage, thread_hash) = seed_owner(&dir, &gateway, thread, &owner).await;

    for (index, path) in ["/v1/alpha/search", "/alpha/search"]
        .into_iter()
        .enumerate()
    {
        let response =
            search_request(&gateway, path, &[("thread-id", thread)], SEARCH_REQUEST).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response_bytes(response).await, SEARCH_RESPONSE, "{path}");
        assert_eq!(
            gateway.0.compaction.owner(&thread_hash),
            Some(owner.clone()),
            "{path}"
        );
        assert!(
            gateway.0.compaction.boundary_matches(
                &thread_hash,
                Some(&crate::storage::digest(super::V022_WINDOW.as_bytes()))
            ),
            "{path}"
        );

        let rows = wait_for_usage(&usage, index + 2).await;
        let searches = rows
            .iter()
            .filter(|row| {
                row.final_attempt().is_some_and(|attempt| {
                    attempt.operation == Operation::WebSearch
                        && attempt.provider.as_deref() == Some(owner.as_str())
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(searches.len(), index + 1, "{path}");
        let search = searches.last().unwrap();
        let attempt = search.final_attempt().unwrap();
        assert_eq!(attempt.status, Some(200), "{path}");
        assert_eq!(
            attempt.price.as_ref().map(|price| price.model.as_str()),
            Some("web_search")
        );
        assert_eq!(search.cost().unwrap().to_string(), "0.01", "{path}");
    }

    assert_eq!(ordinary_hits.load(Ordering::SeqCst), 0);
    assert_eq!(owner_hits.load(Ordering::SeqCst), 3);
    assert_eq!(
        *owner_bodies.lock(),
        vec![
            super::v022_initial_body(),
            SEARCH_REQUEST.to_vec(),
            SEARCH_REQUEST.to_vec(),
        ]
    );
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn v02513_search_owner_requests_never_exceed_max_concurrency() {
    let events = Arc::new(Mutex::new(Vec::<(&'static str, Vec<u8>)>::new()));
    let (entered_tx, mut entered_rx) = mpsc::unbounded_channel::<&'static str>();
    let release = Arc::new(Semaphore::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let owner_events = events.clone();
    let owner_entered = entered_tx.clone();
    let owner_release = release.clone();
    let owner_active = active.clone();
    let owner_max = max_active.clone();
    let owner_port = super::v022_server(move |request| {
        let owner_events = owner_events.clone();
        let owner_entered = owner_entered.clone();
        let owner_release = owner_release.clone();
        let owner_active = owner_active.clone();
        let owner_max = owner_max.clone();
        async move {
            let path = request.uri().path().to_owned();
            let body = request.into_body().collect().await.unwrap().to_bytes();
            if path.ends_with("/alpha/search") {
                let current = owner_active.fetch_add(1, Ordering::AcqRel) + 1;
                update_max(&owner_max, current);
                owner_events.lock().push(("owner", body.to_vec()));
                owner_entered.send("owner").unwrap();
                owner_release.acquire().await.unwrap().forget();
                owner_active.fetch_sub(1, Ordering::AcqRel);
                super::v022_json_response(StatusCode::OK, Bytes::from_static(SEARCH_RESPONSE))
            } else {
                super::v022_json_response(StatusCode::OK, super::v022_seed_response())
            }
        }
    })
    .await;
    let ordinary_hits = Arc::new(AtomicUsize::new(0));
    let ordinary_events = events.clone();
    let ordinary_entered = entered_tx.clone();
    let ordinary_hit_count = ordinary_hits.clone();
    let ordinary_port = super::v022_server(move |request| {
        let ordinary_events = ordinary_events.clone();
        let ordinary_entered = ordinary_entered.clone();
        let ordinary_hit_count = ordinary_hit_count.clone();
        async move {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            ordinary_hit_count.fetch_add(1, Ordering::SeqCst);
            ordinary_events.lock().push(("ordinary", body.to_vec()));
            ordinary_entered.send("ordinary").unwrap();
            super::v022_json_response(StatusCode::OK, Bytes::from_static(ORDINARY_RESPONSE))
        }
    })
    .await;

    let (dir, gateway) = super::v022_fixture(vec![
        format!("http://127.0.0.1:{ordinary_port}/v1"),
        format!("http://127.0.0.1:{owner_port}/v1"),
    ])
    .await;
    let ids = super::v022_ids(&gateway);
    let ordinary = ids[0].clone();
    let owner = ids[1].clone();
    configure_search_owner(&gateway, &dir, &owner);
    let thread = "fixture-v02513-search-concurrency";
    let _ = seed_owner(&dir, &gateway, thread, &owner).await;

    let first_gateway = gateway.clone();
    let second_gateway = gateway.clone();
    let first = tokio::spawn(async move {
        search_request(
            &first_gateway,
            "/v1/alpha/search",
            &[("thread-id", thread)],
            SEARCH_REQUEST,
        )
        .await
    });
    let second = tokio::spawn(async move {
        search_request(
            &second_gateway,
            "/alpha/search",
            &[("thread-id", thread)],
            SEARCH_REQUEST,
        )
        .await
    });

    let first_route = tokio::time::timeout(Duration::from_secs(5), entered_rx.recv())
        .await
        .expect("first concurrent search did not reach a fixture")
        .expect("fixture event channel closed");
    assert_eq!(first_route, "owner");
    tokio::time::timeout(Duration::from_secs(5), async {
        while gateway.view().waiting_requests != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second search must queue behind the active owner request");
    assert_eq!(active.load(Ordering::Acquire), 1);
    release.add_permits(1);
    let second_route = tokio::time::timeout(Duration::from_secs(5), entered_rx.recv())
        .await
        .expect("second concurrent search did not reach a fixture")
        .expect("fixture event channel closed");
    assert_eq!(second_route, "owner");
    release.add_permits(1);

    let first_response = tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .expect("first concurrent search timed out")
        .unwrap();
    let second_response = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .expect("second concurrent search timed out")
        .unwrap();
    assert_eq!(first_response.status(), StatusCode::OK);
    assert_eq!(second_response.status(), StatusCode::OK);
    assert_eq!(events.lock().len(), 2);
    assert_eq!(ordinary, ids[0]);
    assert_eq!(ordinary_hits.load(Ordering::SeqCst), 0);
    assert!(max_active.load(Ordering::Acquire) <= 1);
    assert!(events
        .lock()
        .iter()
        .all(|(provider, body)| *provider == "owner" && body.as_slice() == SEARCH_REQUEST));
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn v02513_search_without_thread_or_with_conflicting_thread_does_not_borrow_reserved_owner() {
    let owner_hits = Arc::new(AtomicUsize::new(0));
    let owner_hit_count = owner_hits.clone();
    let owner_port = super::v022_server(move |request| {
        let owner_hit_count = owner_hit_count.clone();
        async move {
            let path = request.uri().path().to_owned();
            let _ = request.into_body().collect().await.unwrap();
            owner_hit_count.fetch_add(1, Ordering::SeqCst);
            if path.ends_with("/alpha/search") {
                super::v022_json_response(StatusCode::OK, Bytes::from_static(SEARCH_RESPONSE))
            } else {
                super::v022_json_response(StatusCode::OK, super::v022_seed_response())
            }
        }
    })
    .await;
    let ordinary_hits = Arc::new(AtomicUsize::new(0));
    let ordinary_hit_count = ordinary_hits.clone();
    let ordinary_port = super::v022_server(move |request| {
        let ordinary_hit_count = ordinary_hit_count.clone();
        async move {
            let _ = request.into_body().collect().await.unwrap();
            ordinary_hit_count.fetch_add(1, Ordering::SeqCst);
            super::v022_json_response(StatusCode::OK, Bytes::from_static(ORDINARY_RESPONSE))
        }
    })
    .await;

    let (dir, gateway) = super::v022_fixture(vec![
        format!("http://127.0.0.1:{ordinary_port}/v1"),
        format!("http://127.0.0.1:{owner_port}/v1"),
    ])
    .await;
    let ids = super::v022_ids(&gateway);
    let ordinary = ids[0].clone();
    let owner = ids[1].clone();
    configure_search_owner(&gateway, &dir, &owner);
    let thread = "fixture-v02513-search-negative";
    let (usage, thread_hash) = seed_owner(&dir, &gateway, thread, &owner).await;

    let no_thread = search_request(&gateway, "/v1/alpha/search", &[], SEARCH_REQUEST).await;
    assert_eq!(no_thread.status(), StatusCode::OK);
    assert_eq!(response_bytes(no_thread).await, ORDINARY_RESPONSE);

    let conflicting = search_request(
        &gateway,
        "/alpha/search",
        &[
            ("thread-id", thread),
            ("thread-id", "fixture-conflicting-thread"),
        ],
        SEARCH_REQUEST,
    )
    .await;
    assert_eq!(conflicting.status(), StatusCode::OK);
    assert_eq!(response_bytes(conflicting).await, ORDINARY_RESPONSE);

    assert_eq!(owner_hits.load(Ordering::SeqCst), 1);
    assert_eq!(ordinary_hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        gateway.0.compaction.owner(&thread_hash),
        Some(owner.clone())
    );
    assert!(gateway.0.compaction.boundary_matches(
        &thread_hash,
        Some(&crate::storage::digest(super::V022_WINDOW.as_bytes()))
    ));

    let rows = wait_for_usage(&usage, 3).await;
    let searches = rows
        .iter()
        .filter(|row| {
            row.final_attempt().is_some_and(|attempt| {
                attempt.operation == Operation::WebSearch
                    && attempt.provider.as_deref() == Some(ordinary.as_str())
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(searches.len(), 2);
    for search in searches {
        assert_eq!(search.cost().unwrap().to_string(), "0.01");
    }
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn v02513_search_failure_falls_back_without_moving_owner_or_retaining_fallback() {
    let owner_hits = Arc::new(AtomicUsize::new(0));
    let seen = owner_hits.clone();
    let owner_port = super::v022_server(move |request| {
        let seen = seen.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            let search = request.uri().path().ends_with("/alpha/search");
            let _ = request.into_body().collect().await.unwrap();
            if search {
                super::v022_json_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({"error":{"reason":"DAILY_LIMIT_EXCEEDED"}}).to_string(),
                )
            } else {
                super::v022_json_response(StatusCode::OK, super::v022_seed_response())
            }
        }
    })
    .await;
    let fallback_hits = Arc::new(AtomicUsize::new(0));
    let seen = fallback_hits.clone();
    let fallback_port = super::v022_server(move |request| {
        let seen = seen.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                request.into_body().collect().await.unwrap().to_bytes(),
                SEARCH_REQUEST
            );
            super::v022_json_response(StatusCode::OK, Bytes::from_static(SEARCH_RESPONSE))
        }
    })
    .await;
    let (dir, gateway) = super::v022_fixture(vec![
        format!("http://127.0.0.1:{fallback_port}/v1"),
        format!("http://127.0.0.1:{owner_port}/v1"),
    ])
    .await;
    let ids = super::v022_ids(&gateway);
    configure_search_owner(&gateway, &dir, &ids[1]);
    let thread = "fixture-v02513-search-fallback";
    let (usage, digest) = seed_owner(&dir, &gateway, thread, &ids[1]).await;
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        search_request(
            &gateway,
            "/v1/alpha/search",
            &[("thread-id", thread)],
            SEARCH_REQUEST,
        ),
    )
    .await
    .expect("explicit quota rejection must not enter capacity waiting");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_bytes(response).await, SEARCH_RESPONSE);
    let rows = wait_for_usage(&usage, 2).await;
    let record = rows
        .iter()
        .find(|r| {
            r.final_attempt()
                .is_some_and(|a| a.operation == Operation::WebSearch)
        })
        .unwrap();
    assert_eq!(record.attempts.len(), 2);
    assert_eq!(
        record.attempts[0].provider.as_deref(),
        Some(ids[1].as_str())
    );
    assert_eq!(
        record.attempts[1].provider.as_deref(),
        Some(ids[0].as_str())
    );
    assert_eq!(owner_hits.load(Ordering::SeqCst), 2);
    assert_eq!(fallback_hits.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.0.compaction.owner(&digest), Some(ids[1].clone()));
    assert!(gateway.0.compaction.boundary_matches(
        &digest,
        Some(&crate::storage::digest(super::V022_WINDOW.as_bytes()))
    ));
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn v02513_metadata_only_search_follows_owner_and_preserves_wire_metadata() {
    let owner_hits = Arc::new(AtomicUsize::new(0));
    let owner_bodies = Arc::new(Mutex::new(Vec::<(Vec<u8>, Vec<Vec<u8>>)>::new()));
    let owner_hit_count = owner_hits.clone();
    let owner_seen = owner_bodies.clone();
    let owner_port = super::v022_server(move |request| {
        let owner_hit_count = owner_hit_count.clone();
        let owner_seen = owner_seen.clone();
        async move {
            let path = request.uri().path().to_owned();
            let metadata = request
                .headers()
                .get_all("x-codex-turn-metadata")
                .iter()
                .map(|value| value.as_bytes().to_vec())
                .collect::<Vec<_>>();
            let body = request.into_body().collect().await.unwrap().to_bytes();
            owner_hit_count.fetch_add(1, Ordering::SeqCst);
            owner_seen.lock().push((body.to_vec(), metadata));
            if path.ends_with("/alpha/search") {
                super::v022_json_response(StatusCode::OK, Bytes::from_static(SEARCH_RESPONSE))
            } else {
                super::v022_json_response(StatusCode::OK, super::v022_seed_response())
            }
        }
    })
    .await;
    let ordinary_hits = Arc::new(AtomicUsize::new(0));
    let ordinary_hit_count = ordinary_hits.clone();
    let ordinary_port = super::v022_server(move |request| {
        let ordinary_hit_count = ordinary_hit_count.clone();
        async move {
            let _ = request.into_body().collect().await.unwrap();
            ordinary_hit_count.fetch_add(1, Ordering::SeqCst);
            super::v022_json_response(StatusCode::OK, Bytes::from_static(ORDINARY_RESPONSE))
        }
    })
    .await;

    let (dir, gateway) = super::v022_fixture(vec![
        format!("http://127.0.0.1:{ordinary_port}/v1"),
        format!("http://127.0.0.1:{owner_port}/v1"),
    ])
    .await;
    let ids = super::v022_ids(&gateway);
    let owner = ids[1].clone();
    configure_search_owner(&gateway, &dir, &owner);
    let (usage, thread_hash) = seed_owner(&dir, &gateway, METADATA_THREAD, &owner).await;

    let response = search_request(
        &gateway,
        "/v1/alpha/search",
        &[("x-codex-turn-metadata", METADATA_HEADER)],
        METADATA_SEARCH_REQUEST,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_bytes(response).await, SEARCH_RESPONSE);

    assert_eq!(
        gateway.0.compaction.owner(&thread_hash),
        Some(owner.clone())
    );
    assert!(gateway.0.compaction.boundary_matches(
        &thread_hash,
        Some(&crate::storage::digest(super::V022_WINDOW.as_bytes()))
    ));

    assert_eq!(ordinary_hits.load(Ordering::SeqCst), 0);
    assert_eq!(owner_hits.load(Ordering::SeqCst), 2);
    assert_eq!(
        *owner_bodies.lock(),
        vec![
            (super::v022_initial_body(), Vec::new()),
            (
                METADATA_SEARCH_REQUEST.to_vec(),
                vec![METADATA_HEADER.as_bytes().to_vec()]
            ),
        ]
    );

    let rows = wait_for_usage(&usage, 2).await;
    let searches = rows
        .iter()
        .filter(|row| {
            row.final_attempt().is_some_and(|attempt| {
                attempt.operation == Operation::WebSearch
                    && attempt.provider.as_deref() == Some(owner.as_str())
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(searches.len(), 1);
    let search = searches[0];
    let attempt = search.final_attempt().unwrap();
    assert_eq!(attempt.status, Some(200));
    assert_eq!(
        attempt.price.as_ref().map(|price| price.model.as_str()),
        Some("web_search")
    );
    assert_eq!(search.cost().unwrap().to_string(), "0.01");
    assert_eq!(
        attempt
            .routing_evidence
            .as_ref()
            .and_then(|evidence| evidence.identity),
        Some(crate::usage::model::ThreadIdentity::Reliable)
    );
    gateway.stop().await.unwrap();
}

#[test]
fn v02513_search_metadata_identity_rejects_unsafe_shapes() {
    use crate::usage::model::ThreadIdentity;

    let mut cases = vec![
        (
            "reliable-header-conflict",
            metadata_headers(&[r#"{"thread_id":"fixture-other-thread"}"#]),
        ),
        (
            "duplicate-json-thread-id",
            metadata_headers(&[r#"{"thread_id":"fixture-a","thread_id":"fixture-b"}"#]),
        ),
        (
            "contradictory-metadata-headers",
            metadata_headers(&[
                r#"{"thread_id":"fixture-a"}"#,
                r#"{"thread_id":"fixture-b"}"#,
            ]),
        ),
        (
            "non-string-thread-id",
            metadata_headers(&[r#"{"thread_id":42}"#]),
        ),
        (
            "empty-thread-id",
            metadata_headers(&[r#"{"thread_id":""}"#]),
        ),
        (
            "control-character-thread-id",
            metadata_headers(&[r#"{"thread_id":"fixture-\u0001"}"#]),
        ),
        ("invalid-json", metadata_headers(&[INVALID_METADATA])),
    ];
    cases[0].1.insert(
        "thread-id",
        hyper::header::HeaderValue::from_static(METADATA_THREAD),
    );
    let oversized_thread = "x".repeat(64 * 1024);
    let oversized_metadata = format!(r#"{{"thread_id":"{oversized_thread}"}}"#);
    cases.push((
        "oversized-metadata-header",
        metadata_headers(&[oversized_metadata.as_str()]),
    ));
    let oversized_thread_id = "x".repeat(257);
    let oversized_thread_id_metadata = format!(r#"{{"thread_id":"{oversized_thread_id}"}}"#);
    cases.push((
        "oversized-thread-id",
        metadata_headers(&[oversized_thread_id_metadata.as_str()]),
    ));

    for (name, headers) in cases {
        assert_eq!(
            crate::gateway::compaction::search_identity(&headers),
            (None, ThreadIdentity::Conflict),
            "metadata case {name} must not produce a reliable identity"
        );
    }

    let mut metadata_with_legacy = metadata_headers(&[METADATA_HEADER]);
    metadata_with_legacy.insert(
        "session_id",
        hyper::header::HeaderValue::from_static("fixture-legacy-session"),
    );
    assert_eq!(
        crate::gateway::compaction::search_identity(&metadata_with_legacy),
        (
            Some(crate::storage::digest(METADATA_THREAD.as_bytes())),
            ThreadIdentity::Reliable
        )
    );

    let mut modern_session = hyper::HeaderMap::new();
    modern_session.insert(
        "session-id",
        hyper::header::HeaderValue::from_static("fixture-modern-session"),
    );
    assert_eq!(
        crate::gateway::compaction::search_identity(&modern_session),
        (None, ThreadIdentity::Missing)
    );

    let mut legacy_session = hyper::HeaderMap::new();
    legacy_session.insert(
        "session_id",
        hyper::header::HeaderValue::from_static("fixture-legacy-session"),
    );
    assert_eq!(
        crate::gateway::compaction::search_identity(&legacy_session),
        (
            Some(crate::storage::digest(b"fixture-legacy-session")),
            ThreadIdentity::LegacySession
        )
    );
}

#[tokio::test]
async fn v02513_invalid_search_metadata_and_body_id_never_borrow_owner() {
    let owner_hits = Arc::new(AtomicUsize::new(0));
    let owner_hit_count = owner_hits.clone();
    let owner_port = super::v022_server(move |request| {
        let owner_hit_count = owner_hit_count.clone();
        async move {
            let path = request.uri().path().to_owned();
            let _ = request.into_body().collect().await.unwrap();
            owner_hit_count.fetch_add(1, Ordering::SeqCst);
            if path.ends_with("/alpha/search") {
                super::v022_json_response(StatusCode::OK, Bytes::from_static(SEARCH_RESPONSE))
            } else {
                super::v022_json_response(StatusCode::OK, super::v022_seed_response())
            }
        }
    })
    .await;
    let ordinary_hits = Arc::new(AtomicUsize::new(0));
    let ordinary_hit_count = ordinary_hits.clone();
    let ordinary_port = super::v022_server(move |request| {
        let ordinary_hit_count = ordinary_hit_count.clone();
        async move {
            let _ = request.into_body().collect().await.unwrap();
            ordinary_hit_count.fetch_add(1, Ordering::SeqCst);
            super::v022_json_response(StatusCode::OK, Bytes::from_static(ORDINARY_RESPONSE))
        }
    })
    .await;

    let (dir, gateway) = super::v022_fixture(vec![
        format!("http://127.0.0.1:{ordinary_port}/v1"),
        format!("http://127.0.0.1:{owner_port}/v1"),
    ])
    .await;
    let ids = super::v022_ids(&gateway);
    let owner = ids[1].clone();
    configure_search_owner(&gateway, &dir, &owner);
    let (_usage, thread_hash) = seed_owner(&dir, &gateway, METADATA_THREAD, &owner).await;

    assert_ordinary_search(
        &gateway,
        &[
            ("thread-id", METADATA_THREAD),
            (
                "x-codex-turn-metadata",
                r#"{"thread_id":"fixture-other-thread"}"#,
            ),
        ],
        METADATA_SEARCH_REQUEST,
    )
    .await;
    assert_ordinary_search(
        &gateway,
        &[(
            "x-codex-turn-metadata",
            r#"{"thread_id":"fixture-a","thread_id":"fixture-b"}"#,
        )],
        METADATA_SEARCH_REQUEST,
    )
    .await;
    assert_ordinary_search(
        &gateway,
        &[
            ("x-codex-turn-metadata", r#"{"thread_id":"fixture-a"}"#),
            ("x-codex-turn-metadata", r#"{"thread_id":"fixture-b"}"#),
        ],
        METADATA_SEARCH_REQUEST,
    )
    .await;
    assert_ordinary_search(
        &gateway,
        &[("x-codex-turn-metadata", r#"{"thread_id":42}"#)],
        METADATA_SEARCH_REQUEST,
    )
    .await;
    assert_ordinary_search(
        &gateway,
        &[("x-codex-turn-metadata", r#"{"thread_id":""}"#)],
        METADATA_SEARCH_REQUEST,
    )
    .await;
    assert_ordinary_search(
        &gateway,
        &[("x-codex-turn-metadata", INVALID_METADATA)],
        METADATA_SEARCH_REQUEST,
    )
    .await;
    assert_ordinary_search(&gateway, &[], BODY_ID_SEARCH_REQUEST).await;

    assert_eq!(owner_hits.load(Ordering::SeqCst), 1);
    assert_eq!(ordinary_hits.load(Ordering::SeqCst), 7);
    assert_eq!(gateway.0.compaction.owner(&thread_hash), Some(owner));
    assert!(gateway.0.compaction.boundary_matches(
        &thread_hash,
        Some(&crate::storage::digest(super::V022_WINDOW.as_bytes()))
    ));
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn v02513_search_lease_is_readonly_and_rejects_changed_owner_before_send() {
    let (_dir, gateway) = super::v022_fixture(vec![
        "https://owner.example.invalid/v1".into(),
        "https://receiver.example.invalid/v1".into(),
    ])
    .await;
    let ids = super::v022_ids(&gateway);
    let thread = crate::storage::digest(b"fixture-search-stale-lease");
    let boundary = crate::storage::digest(b"fixture-search-boundary");
    assert!(gateway
        .0
        .compaction
        .prepare_search(thread.clone())
        .unwrap()
        .is_none());
    let seed = gateway
        .0
        .compaction
        .prepare(thread.clone(), None, true, &ids)
        .unwrap();
    seed.complete(&ids[0], Some(&boundary));
    drop(seed);
    let search = gateway
        .0
        .compaction
        .prepare_search(thread.clone())
        .unwrap()
        .unwrap();
    assert!(search.request_only());
    assert!(!search.handoff);
    search.output_started(&ids[0]);
    search.complete(
        &ids[1],
        Some(&crate::storage::digest(b"untrusted-search-boundary")),
    );
    assert_eq!(gateway.0.compaction.owner(&thread), Some(ids[0].clone()));
    assert!(gateway
        .0
        .compaction
        .boundary_matches(&thread, Some(&boundary)));
    let transfer = gateway
        .0
        .compaction
        .prepare(
            thread.clone(),
            Some(&boundary),
            true,
            &[ids[1].clone(), ids[0].clone()],
        )
        .unwrap();
    assert!(transfer.handoff);
    transfer.complete(&ids[1], None);
    let error = search
        .admit_request(
            &gateway,
            &gateway.route(&ids[0]).unwrap().provider,
            &super::super::routing::Requirement::Resource,
            false,
        )
        .unwrap_err();
    assert_eq!(error.code, crate::errors::code::CONVERSATION_OWNERSHIP);
    drop(search);
    assert_eq!(gateway.0.compaction.owner(&thread), Some(ids[1].clone()));
}

#[tokio::test]
async fn a_search_taken_over_leaves_the_conversation_and_its_owner_alone() {
    let owner_hits = Arc::new(AtomicUsize::new(0));
    let seen = owner_hits.clone();
    let owner_port = super::v022_server(move |request| {
        let seen = seen.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            assert!(
                !request.uri().path().ends_with("/alpha/search"),
                "the owner takes no searches"
            );
            let _ = request.into_body().collect().await.unwrap();
            super::v022_json_response(StatusCode::OK, super::v022_seed_response())
        }
    })
    .await;
    let taker_hits = Arc::new(AtomicUsize::new(0));
    let seen = taker_hits.clone();
    let taker_port = super::v022_server(move |request| {
        let seen = seen.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                request.into_body().collect().await.unwrap().to_bytes(),
                SEARCH_REQUEST
            );
            super::v022_json_response(StatusCode::OK, Bytes::from_static(SEARCH_RESPONSE))
        }
    })
    .await;
    let (dir, gateway) = super::v022_fixture(vec![
        format!("http://127.0.0.1:{owner_port}/v1"),
        format!("http://127.0.0.1:{taker_port}/v1"),
    ])
    .await;
    let ids = super::v022_ids(&gateway);
    super::v022_update(&gateway, &dir, searching(&gateway, &ids[1], false));
    configure_search_owner(&gateway, &dir, &ids[0]);
    let thread = "fixture-v02513-search-fallback";
    let (usage, digest) = seed_owner(&dir, &gateway, thread, &ids[0]).await;
    for _ in 0..2 {
        let response = search_request(
            &gateway,
            "/v1/alpha/search",
            &[("thread-id", thread)],
            SEARCH_REQUEST,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_bytes(response).await, SEARCH_RESPONSE);
    }
    let rows = wait_for_usage(&usage, 3).await;
    let searches: Vec<_> = rows
        .iter()
        .flat_map(|row| &row.attempts)
        .filter(|attempt| attempt.operation == Operation::WebSearch)
        .collect();
    assert_eq!(searches.len(), 2);
    for attempt in searches {
        assert_eq!(attempt.provider.as_deref(), Some(ids[1].as_str()));
        assert_eq!(
            attempt.routing_decision,
            Some(crate::usage::model::RoutingDecision::WebSearchTakeover)
        );
    }
    assert_eq!(owner_hits.load(Ordering::SeqCst), 1);
    assert_eq!(taker_hits.load(Ordering::SeqCst), 2);
    assert_eq!(gateway.0.compaction.owner(&digest), Some(ids[0].clone()));
    assert!(gateway.0.compaction.boundary_matches(
        &digest,
        Some(&crate::storage::digest(super::V022_WINDOW.as_bytes()))
    ));
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn search_safety_rejection_is_not_sent_to_another_provider() {
    let owner = super::v022_server(|request| async move {
        if !request.uri().path().ends_with("/alpha/search") {
            return super::v022_json_response(StatusCode::OK, super::v022_seed_response());
        }
        super::v022_json_response(
            StatusCode::FORBIDDEN,
            json!({"error":{"code":"content_policy_violation"}}).to_string(),
        )
    })
    .await;
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let other = super::v022_server(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        async { super::v022_json_response(StatusCode::OK, Bytes::from_static(SEARCH_RESPONSE)) }
    })
    .await;
    // No provider takes searches: the search is the ordinary queue's.
    let (dir, gateway) = super::v022_fixture(vec![
        format!("http://127.0.0.1:{owner}/v1"),
        format!("http://127.0.0.1:{other}/v1"),
    ])
    .await;
    let ids = super::v022_ids(&gateway);
    configure_search_owner(&gateway, &dir, &ids[0]);
    let thread = "fixture-search-safety";
    seed_owner(&dir, &gateway, thread, &ids[0]).await;
    let response = search_request(
        &gateway,
        "/v1/alpha/search",
        &[("thread-id", thread)],
        SEARCH_REQUEST,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn a_search_visits_its_takers_in_list_order_and_never_the_ordinary_queue() {
    for all_fail in [false, true] {
        let hits = Arc::new(Mutex::new(vec![0usize; 3]));
        let mut bases = vec![];
        for index in 0..3 {
            let hits = hits.clone();
            let port = super::v022_server(move |request| {
                let hits = hits.clone();
                async move {
                    hits.lock()[index] += 1;
                    let search = request.uri().path().ends_with("/alpha/search");
                    let _ = request.into_body().collect().await.unwrap();
                    assert!(
                        index != 0 || !search,
                        "the ordinary queue received a search"
                    );
                    if !search || (index == 2 && !all_fail) {
                        super::v022_json_response(
                            StatusCode::OK,
                            Bytes::from_static(SEARCH_RESPONSE),
                        )
                    } else {
                        super::v022_json_response(StatusCode::BAD_GATEWAY, "{}")
                    }
                }
            })
            .await;
            bases.push(format!("http://127.0.0.1:{port}/v1"));
        }
        let (dir, gateway) = super::v022_fixture(bases).await;
        let ids = super::v022_ids(&gateway);
        for id in &ids[1..] {
            super::v022_update(&gateway, &dir, searching(&gateway, id, false));
        }
        let mut settings = gateway.0.inner.lock().store.settings.clone();
        settings.max_retries = 0;
        super::v022_update(&gateway, &dir, Edit::Settings { settings });
        super::v022_start(&gateway, &dir).await;
        let response = search_request(&gateway, "/alpha/search", &[], SEARCH_REQUEST).await;
        assert_eq!(
            response.status(),
            if all_fail {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::OK
            }
        );
        let _ = response_bytes(response).await;
        assert_eq!(*hits.lock(), [0, 1, 1]);
        // A provider that takes searches is still one of the ordinary queue:
        // with the first provider out of it, the conversation is theirs.
        super::v022_update(
            &gateway,
            &dir,
            Edit::QueueProvider {
                id: ids[0].clone(),
                queued: false,
            },
        );
        let response = search_request(
            &gateway,
            "/v1/responses",
            &[],
            br#"{"model":"fixture-model","input":"fixture"}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let _ = response_bytes(response).await;
        assert_eq!(*hits.lock(), [0, 2, 1]);
        gateway.stop().await.unwrap();
    }
}

#[tokio::test]
async fn an_endless_search_taker_retries_a_rate_limit_where_it_was() {
    let hits = Arc::new(Mutex::new(vec![0usize; 3]));
    let mut bases = vec![];
    for index in 0..3 {
        let hits = hits.clone();
        let port = super::v022_server(move |request| {
            let hits = hits.clone();
            async move {
                let n = {
                    let mut hits = hits.lock();
                    hits[index] += 1;
                    hits[index]
                };
                let _ = request.into_body().collect().await.unwrap();
                if index == 1 && n == 1 {
                    super::v022_json_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        json!({"error":{"type":"rate_limit_error","message":"rate limit"}})
                            .to_string(),
                    )
                } else {
                    super::v022_json_response(StatusCode::OK, Bytes::from_static(SEARCH_RESPONSE))
                }
            }
        })
        .await;
        bases.push(format!("http://127.0.0.1:{port}/v1"));
    }
    let (dir, gateway) = super::v022_fixture(bases).await;
    let ids = super::v022_ids(&gateway);
    super::v022_update(&gateway, &dir, searching(&gateway, &ids[1], true));
    super::v022_update(&gateway, &dir, searching(&gateway, &ids[2], false));
    super::v022_start(&gateway, &dir).await;
    let started = std::time::Instant::now();
    let response = search_request(&gateway, "/alpha/search", &[], SEARCH_REQUEST).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_bytes(response).await, SEARCH_RESPONSE);
    // It waited its interval at the first taker: neither the next taker nor
    // the ordinary queue was asked.
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert_eq!(*hits.lock(), [0, 2, 0]);
    gateway.stop().await.unwrap();
}

/// The edit that sets provider `id` to take web searches, in endless mode
/// with a second between retries when `endless`.
fn searching(gateway: &Gateway, id: &str, endless: bool) -> Edit {
    let provider = gateway
        .view()
        .providers
        .into_iter()
        .find(|p| p.id == id)
        .unwrap();
    Edit::SaveProvider {
        delegate: super::super::model::DelegateEdit {
            take_web_search: Some(true),
            ..Default::default()
        },
        id: Some(id.to_owned()),
        base_url: provider.base_url,
        token: String::new(),
        quota_access_token: None,
        name: None,
        end_on_message_stop: None,
        filter_count_tokens: None,
        buffer_complete_response: None,
        endless_mode: Some(endless),
        upstream_retry_seconds: Some(1),
        normal_retry_seconds: Some(1),
        risk_codes: None,
        retry_intensity: Some(0),
        limit_retry_seconds: Some(1),
        limit_rest_seconds: Some(0),
    }
}
