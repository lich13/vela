// 覆盖模型白名单、健康作用域与探测隔离。
//! v0.22 ownership, model-health, and first-turn regression fixtures.
//!
//! Every upstream in this module is a loopback HTTP fixture and every token,
//! thread ID, model, and opaque window is synthetic.
use super::{
    model::Edit,
    replay::{self, RequestHints, TurnEvidence, WireBody},
    *,
};
use crate::gateway::test_support::loopback::{
    json_response, probing_fixture, provider_ids, response_bytes, serve, start, thread_request,
    update,
};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{Response, StatusCode};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[path = "model_detour_http_tests.rs"]
mod model_detour_http_tests;

const WINDOW: &str = "fixture-v022-window";
const CAPACITY: &[u8] = b"data: {\"type\":\"error\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Selected model is at capacity\"}}\n\n";
const TEMPORARY_429: &[u8] =
    br#"{"error":{"code":"rate_limit_exceeded","message":"selected model is at capacity"}}"#;
const BALANCE_429: &[u8] =
    br#"{"error":{"code":"INSUFFICIENT_BALANCE","message":"fixture balance exhausted"}}"#;
const USAGE_LIMIT_429: &[u8] =
    br#"{"error":{"code":"USAGE_LIMIT_EXCEEDED","message":"fixture usage limit"}}"#;

fn sse_response(body: impl Into<Bytes>) -> Response<WireBody> {
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(replay::full(body))
        .unwrap()
}

fn compacted_body() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "model": "fixture-model",
        "stream": true,
        "input": [
            {"type": "compaction", "encrypted_content": WINDOW},
            {"role": "user", "content": "fixture-after-compaction"}
        ]
    }))
    .unwrap()
}

fn initial_body() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "model": "fixture-model",
        "input": [{"role": "user", "content": "fixture-initial"}]
    }))
    .unwrap()
}

fn seed_response() -> String {
    json!({
        "id": "fixture-v022-seed",
        "object": "response",
        "status": "completed",
        "model": "fixture-model",
        "output": [{"type": "compaction", "encrypted_content": WINDOW}]
    })
    .to_string()
}

#[tokio::test]
async fn capacity_before_first_output_returns_to_compaction_owner() {
    let owner_hits = Arc::new(AtomicUsize::new(0));
    let owner_count = owner_hits.clone();
    let owner_body = seed_response();
    let owner_response_body = owner_body.clone();
    let owner_port = serve(move |_| {
        let owner_body = owner_response_body.clone();
        owner_count.fetch_add(1, Ordering::SeqCst);
        async move { json_response(StatusCode::OK, owner_body, None) }
    })
    .await;
    let receiver_hits = Arc::new(AtomicUsize::new(0));
    let receiver_count = receiver_hits.clone();
    let receiver_port = serve(move |_| {
        receiver_count.fetch_add(1, Ordering::SeqCst);
        async { json_response(StatusCode::TOO_MANY_REQUESTS, CAPACITY, None) }
    })
    .await;

    let (dir, gateway) = probing_fixture(vec![
        format!("http://127.0.0.1:{owner_port}/v1"),
        format!("http://127.0.0.1:{receiver_port}/v1"),
    ])
    .await;
    let ids = provider_ids(&gateway);
    update(
        &gateway,
        &dir,
        Edit::TakeoverProvider {
            id: ids[1].clone(),
            handoff_after_compaction: Some(true),
            take_new_threads: Some(false),
        },
    );
    start(&gateway, &dir).await;

    let session = "fixture-v022-capacity-owner";
    let seed = thread_request(&gateway, "/v1/responses", session, initial_body()).await;
    assert_eq!(seed.status(), StatusCode::OK);
    let seed_bytes = response_bytes(seed).await;
    assert_eq!(seed_bytes, owner_body.as_bytes());

    update(
        &gateway,
        &dir,
        Edit::Reorder {
            ids: vec![ids[1].clone(), ids[0].clone()],
        },
    );
    let response = thread_request(&gateway, "/v1/responses", session, compacted_body()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_bytes(response).await, owner_body.as_bytes());
    assert_eq!(receiver_hits.load(Ordering::SeqCst), 1);
    assert_eq!(owner_hits.load(Ordering::SeqCst), 2);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn compaction_output_closes_the_single_retakeover_opportunity() {
    let owner_hits = Arc::new(AtomicUsize::new(0));
    let owner_count = owner_hits.clone();
    let owner_body = seed_response();
    let owner_response_body = owner_body.clone();
    let owner_port = serve(move |_| {
        let owner_body = owner_response_body.clone();
        owner_count.fetch_add(1, Ordering::SeqCst);
        async move { json_response(StatusCode::OK, owner_body, None) }
    })
    .await;
    let receiver_hits = Arc::new(AtomicUsize::new(0));
    let receiver_count = receiver_hits.clone();
    let receiver_port = serve(move |_| {
        receiver_count.fetch_add(1, Ordering::SeqCst);
        async {
            sse_response(Bytes::from_static(
                b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"fixture-v022-created\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
            ))
        }
    })
    .await;
    let (dir, gateway) = probing_fixture(vec![
        format!("http://127.0.0.1:{owner_port}/v1"),
        format!("http://127.0.0.1:{receiver_port}/v1"),
    ])
    .await;
    let ids = provider_ids(&gateway);
    update(
        &gateway,
        &dir,
        Edit::TakeoverProvider {
            id: ids[1].clone(),
            handoff_after_compaction: Some(true),
            take_new_threads: Some(false),
        },
    );
    start(&gateway, &dir).await;
    let session = "fixture-v022-output-started";
    let seed = thread_request(&gateway, "/v1/responses", session, initial_body()).await;
    assert_eq!(seed.status(), StatusCode::OK);
    let _ = response_bytes(seed).await;
    update(
        &gateway,
        &dir,
        Edit::Reorder {
            ids: vec![ids[1].clone(), ids[0].clone()],
        },
    );
    let response = thread_request(&gateway, "/v1/responses", session, compacted_body()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let expected = b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"fixture-v022-created\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n";
    assert_eq!(response_bytes(response).await, expected.as_slice());
    assert_eq!(receiver_hits.load(Ordering::SeqCst), 1);
    assert_eq!(owner_hits.load(Ordering::SeqCst), 1);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn model_capacity_isolated_so_same_provider_accepts_astra() {
    let luna_hits = Arc::new(AtomicUsize::new(0));
    let astra_hits = Arc::new(AtomicUsize::new(0));
    let luna_first = Arc::new(tokio::sync::Notify::new());
    let luna_count = luna_hits.clone();
    let astra_count = astra_hits.clone();
    let first_signal = luna_first.clone();
    let port = serve(move |request| {
        let luna_count = luna_count.clone();
        let astra_count = astra_count.clone();
        let first_signal = first_signal.clone();
        async move {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            let value: Value = serde_json::from_slice(&body).unwrap();
            let model = value["model"].as_str().unwrap_or_default();
            if model == "fixture-luna" {
                let attempt = luna_count.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt == 1 {
                    first_signal.notify_waiters();
                }
                if attempt <= 2 {
                    return json_response(StatusCode::TOO_MANY_REQUESTS, CAPACITY, None);
                }
            } else if model == "fixture-astra" {
                astra_count.fetch_add(1, Ordering::SeqCst);
            }
            json_response(
                StatusCode::OK,
                json!({"id":"fixture-v022-model","status":"completed","model":model}).to_string(),
                None,
            )
        }
    })
    .await;
    let (dir, gateway) = probing_fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
    let mut settings = gateway.view().settings;
    settings.max_retries = 2;
    settings.capacity_retry_seconds = 1;
    update(&gateway, &dir, Edit::Settings { settings });
    start(&gateway, &dir).await;

    let luna_request = {
        let gateway = gateway.clone();
        tokio::spawn(async move {
            thread_request(
                &gateway,
                "/v1/responses",
                "fixture-v022-luna",
                br#"{"model":"fixture-luna","input":[{"role":"user","content":"luna"}]}"#.to_vec(),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), luna_first.notified())
        .await
        .unwrap();
    let astra = thread_request(
        &gateway,
        "/v1/responses",
        "fixture-v022-astra",
        br#"{"model":"fixture-astra","input":[{"role":"user","content":"astra"}]}"#.to_vec(),
    )
    .await;
    assert_eq!(astra.status(), StatusCode::OK);
    assert_eq!(astra_hits.load(Ordering::SeqCst), 1);
    let luna = tokio::time::timeout(Duration::from_secs(6), luna_request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(luna.status(), StatusCode::OK);
    assert_eq!(luna_hits.load(Ordering::SeqCst), 3);
    let provider = &gateway.view().providers[0];
    assert!(provider.health.available);
    gateway.stop().await.unwrap();
}

#[test]
fn permanent_429_markers_are_provider_scope_and_usage_limit_stays_temporary() {
    let chinese_balance = serde_json::to_vec(&json!({
        "error": {"message": "\u{4f59}\u{989d}\u{8017}\u{5c3d}"}
    }))
    .unwrap();
    for body in [
        br#"{"error":{"code":"API_KEY_QUOTA_EXHAUSTED"}}"#.as_slice(),
        br#"{"error":{"code":"INSUFFICIENT_BALANCE"}}"#.as_slice(),
        chinese_balance.as_slice(),
    ] {
        assert!(
            super::failure_policy::test_support::permanent_rejection(body),
            "expected explicit balance or key marker in {:?}",
            std::str::from_utf8(body).unwrap()
        );
        let details = super::failure_policy::test_support::details_http(body);
        assert_eq!(
            details.health_scope,
            Some(crate::events::HealthScope::Provider)
        );
    }

    for body in [
        br#"{"error":{"code":"USAGE_LIMIT_EXCEEDED"}}"#.as_slice(),
        br#"{"errorKey":"USAGE_LIMIT_EXCEEDED","errorMessage":"fixture quota"}"#.as_slice(),
    ] {
        assert!(
            !super::failure_policy::test_support::permanent_rejection(body),
            "usage-limit marker should remain temporary: {:?}",
            std::str::from_utf8(body).unwrap()
        );
        let details = super::failure_policy::test_support::details_http(body);
        assert_eq!(details.health_scope, None);
        assert_eq!(
            details.upstream_code.as_deref(),
            Some("USAGE_LIMIT_EXCEEDED")
        );
    }

    let temporary = super::failure_policy::test_support::details_http(TEMPORARY_429);
    assert!(!super::failure_policy::test_support::permanent_rejection(
        TEMPORARY_429
    ));
    assert_eq!(temporary.health_scope, None);
    assert_eq!(
        temporary.upstream_code.as_deref(),
        Some("rate_limit_exceeded")
    );
}

#[tokio::test]
async fn usage_limit_429_fails_over_to_p2_without_capacity_wait() {
    let p1_hits = Arc::new(AtomicUsize::new(0));
    let p1_count = p1_hits.clone();
    let p1_port = serve(move |_| {
        p1_count.fetch_add(1, Ordering::SeqCst);
        async { json_response(StatusCode::TOO_MANY_REQUESTS, USAGE_LIMIT_429, None) }
    })
    .await;
    let p2_hits = Arc::new(AtomicUsize::new(0));
    let p2_count = p2_hits.clone();
    let p2_port = serve(move |_| {
        p2_count.fetch_add(1, Ordering::SeqCst);
        async {
            json_response(
                StatusCode::OK,
                br#"{"provider":"fixture-p2"}"#.as_slice(),
                None,
            )
        }
    })
    .await;
    let (dir, gateway) = probing_fixture(vec![
        format!("http://127.0.0.1:{p1_port}/v1"),
        format!("http://127.0.0.1:{p2_port}/v1"),
    ])
    .await;
    let mut settings = gateway.view().settings;
    settings.max_retries = 3;
    settings.capacity_retry_seconds = 60;
    settings.queue_seconds = 2;
    settings.first_byte_seconds = 1;
    settings.idle_seconds = 1;
    update(&gateway, &dir, Edit::Settings { settings });
    start(&gateway, &dir).await;

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        thread_request(
            &gateway, "/v1/responses",
            "fixture-v022-usage-limit-429-failover",
            br#"{"model":"fixture-model","input":[{"role":"user","content":"previous"},{"role":"assistant","content":"answer"},{"role":"user","content":"followup"}]}"#.to_vec(),
        ),
    )
    .await;
    let response = match result {
        Ok(response) => response,
        Err(_) => {
            gateway.stop().await.unwrap();
            panic!("usage-limit 429 failover waited for the 60-second capacity delay");
        }
    };
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_bytes(response).await,
        br#"{"provider":"fixture-p2"}"#.as_slice()
    );
    assert_eq!(p1_hits.load(Ordering::SeqCst), 1);
    assert_eq!(p2_hits.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < Duration::from_secs(2));
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn insufficient_balance_429_with_one_provider_returns_original_body_without_waiting() {
    let p1_hits = Arc::new(AtomicUsize::new(0));
    let p1_count = p1_hits.clone();
    let p1_port = serve(move |_| {
        p1_count.fetch_add(1, Ordering::SeqCst);
        async { json_response(StatusCode::TOO_MANY_REQUESTS, BALANCE_429, None) }
    })
    .await;
    let (dir, gateway) = probing_fixture(vec![format!("http://127.0.0.1:{p1_port}/v1")]).await;
    let mut settings = gateway.view().settings;
    settings.max_retries = 3;
    settings.capacity_retry_seconds = 60;
    settings.queue_seconds = 2;
    settings.first_byte_seconds = 1;
    settings.idle_seconds = 1;
    update(&gateway, &dir, Edit::Settings { settings });
    start(&gateway, &dir).await;

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        thread_request(
            &gateway, "/v1/responses",
            "fixture-v022-balance-429-single",
            br#"{"model":"fixture-model","input":[{"role":"user","content":"previous"},{"role":"assistant","content":"answer"},{"role":"user","content":"followup"}]}"#.to_vec(),
        ),
    )
    .await;
    let response = match result {
        Ok(response) => response,
        Err(_) => {
            gateway.stop().await.unwrap();
            panic!("single-provider balance rejection waited for the capacity delay");
        }
    };
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response_bytes(response).await, BALANCE_429);
    assert_eq!(p1_hits.load(Ordering::SeqCst), 1);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn temporary_capacity_then_hard_429_does_not_revive_denied_provider() {
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let port = serve(move |_| {
        let attempt = count.fetch_add(1, Ordering::SeqCst) + 1;
        async move {
            match attempt {
                1 => json_response(StatusCode::TOO_MANY_REQUESTS, TEMPORARY_429, None),
                2 => json_response(StatusCode::TOO_MANY_REQUESTS, BALANCE_429, None),
                _ => json_response(
                    StatusCode::OK,
                    br#"{"provider":"fixture-revived"}"#.as_slice(),
                    None,
                ),
            }
        }
    })
    .await;
    let (dir, gateway) = probing_fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
    let mut settings = gateway.view().settings;
    settings.max_retries = 3;
    settings.capacity_retry_seconds = 1;
    settings.queue_seconds = 2;
    settings.first_byte_seconds = 1;
    settings.idle_seconds = 1;
    update(&gateway, &dir, Edit::Settings { settings });
    start(&gateway, &dir).await;

    let response = tokio::time::timeout(
        Duration::from_secs(5),
        thread_request(
            &gateway, "/v1/responses",
            "fixture-v022-capacity-then-hard",
            br#"{"model":"fixture-model","input":[{"role":"user","content":"previous"},{"role":"assistant","content":"answer"},{"role":"user","content":"followup"}]}"#.to_vec(),
        ),
    )
    .await
    .expect("mixed capacity and hard rejection did not settle");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response_bytes(response).await, BALANCE_429);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn first_turn_history_and_empty_warmup_do_not_misclassify_new_threads() {
    let ordinary_hits = Arc::new(AtomicUsize::new(0));
    let new_hits = Arc::new(AtomicUsize::new(0));
    let ordinary_count = ordinary_hits.clone();
    let new_count = new_hits.clone();
    let ordinary_port = serve(move |_| {
        ordinary_count.fetch_add(1, Ordering::SeqCst);
        async {
            json_response(
                StatusCode::OK,
                br#"{"provider":"ordinary"}"#.as_slice(),
                None,
            )
        }
    })
    .await;
    let new_port = serve(move |_| {
        new_count.fetch_add(1, Ordering::SeqCst);
        async {
            json_response(
                StatusCode::OK,
                br#"{"provider":"new-thread"}"#.as_slice(),
                None,
            )
        }
    })
    .await;
    let (dir, gateway) = probing_fixture(vec![
        format!("http://127.0.0.1:{ordinary_port}/v1"),
        format!("http://127.0.0.1:{new_port}/v1"),
    ])
    .await;
    let ids = provider_ids(&gateway);
    update(
        &gateway,
        &dir,
        Edit::TakeoverProvider {
            id: ids[1].clone(),
            handoff_after_compaction: Some(false),
            take_new_threads: Some(true),
        },
    );
    start(&gateway, &dir).await;

    let first = thread_request(
        &gateway, "/v1/responses",
        "fixture-v022-first",
        br#"{"model":"fixture-model","input":[{"role":"system","content":"policy"},{"role":"developer","content":"rules"},{"role":"user","content":"first"},{"role":"user","content":"second"}]}"#.to_vec(),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        response_bytes(first).await,
        br#"{"provider":"new-thread"}"#.as_slice()
    );

    let warmup = thread_request(
        &gateway,
        "/v1/responses",
        "fixture-v022-warmup",
        br#"{"model":"fixture-model","generate":false,"input":[]}"#.to_vec(),
    )
    .await;
    assert_eq!(warmup.status(), StatusCode::OK);
    assert_eq!(
        response_bytes(warmup).await,
        br#"{"provider":"ordinary"}"#.as_slice()
    );

    let history = thread_request(
        &gateway, "/v1/responses",
        "fixture-v022-history",
        br#"{"model":"fixture-model","input":[{"role":"user","content":"old"},{"role":"assistant","content":"answer"},{"role":"user","content":"next"}]}"#.to_vec(),
    )
    .await;
    assert_eq!(history.status(), StatusCode::OK);
    assert_eq!(
        response_bytes(history).await,
        br#"{"provider":"ordinary"}"#.as_slice()
    );
    assert_eq!(new_hits.load(Ordering::SeqCst), 1);
    assert_eq!(ordinary_hits.load(Ordering::SeqCst), 2);
    gateway.stop().await.unwrap();
}

#[test]
fn turn_evidence_keeps_first_history_and_auxiliary_distinct() {
    let first = RequestHints::from_value(
        json!({
            "model":"fixture-model",
            "input":[
                {"role":"system","content":"policy"},
                {"role":"developer","content":"rules"},
                {"role":"user","content":"first"},
                {"role":"user","content":"second"}
            ]
        }),
        false,
    )
    .unwrap();
    assert!(first.first_turn);
    assert_eq!(first.turn, TurnEvidence::First);

    for input in [json!([]), json!("")] {
        let warmup = RequestHints::from_value(
            json!({"model":"fixture-model","generate":false,"input":input}),
            false,
        )
        .unwrap();
        assert!(warmup.auxiliary);
        assert!(!warmup.first_turn);
        assert_eq!(warmup.turn, TurnEvidence::Unknown);
    }

    let history = RequestHints::from_value(
        json!({
            "model":"fixture-model",
            "input":[
                {"role":"user","content":"old"},
                {"role":"assistant","content":"answer"},
                {"role":"user","content":"next"}
            ]
        }),
        false,
    )
    .unwrap();
    assert!(!history.first_turn);
    assert_eq!(history.turn, TurnEvidence::History);
}

#[tokio::test]
async fn model_server_errors_are_isolated_and_other_models_remain_admitted() {
    let port = serve(|request| async move {
        let body = request.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let model = value["model"].as_str().unwrap_or_default();
        if matches!(model, "fixture-luna" | "fixture-unknown") {
            return json_response(
                StatusCode::BAD_GATEWAY,
                r#"{"error":{"code":"server_error","message":"fixture backend unavailable"}}"#,
                None,
            );
        }
        json_response(
            StatusCode::OK,
            json!({"id":"fixture-v022-astra","status":"completed","model":model}).to_string(),
            None,
        )
    })
    .await;
    let (dir, gateway) = probing_fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
    let mut settings = gateway.view().settings;
    settings.max_retries = 0;
    update(&gateway, &dir, Edit::Settings { settings });
    start(&gateway, &dir).await;

    for (thread, model) in [
        ("fixture-v022-luna-error", "fixture-luna"),
        ("fixture-v022-unknown-error", "fixture-unknown"),
    ] {
        let response = thread_request(
            &gateway,
            "/v1/responses",
            thread,
            serde_json::to_vec(&json!({
                "model": model,
                "input": [{"role":"user","content":"fixture"}]
            }))
            .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }
    let astra = thread_request(
        &gateway,
        "/v1/responses",
        "fixture-v022-astra-after-error",
        br#"{"model":"fixture-astra","input":[{"role":"user","content":"fixture"}]}"#.to_vec(),
    )
    .await;
    assert_eq!(astra.status(), StatusCode::OK);

    let provider = &gateway.view().providers[0];
    assert!(provider.health.available);
    assert_eq!(provider.health.failures, 0);
    for model in ["fixture-luna", "fixture-unknown"] {
        let state = provider
            .model_health
            .iter()
            .find(|entry| entry.model == model)
            .unwrap_or_else(|| panic!("missing model health for {model}"));
        assert!(
            state.health.failures >= 1,
            "model {model} should retain its failure"
        );
    }
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn invalid_api_key_marks_provider_scope() {
    let port = serve(|_| async {
        json_response(
            StatusCode::UNAUTHORIZED,
            r#"{"error":{"code":"invalid_api_key","message":"fixture invalid key"}}"#,
            None,
        )
    })
    .await;
    let (dir, gateway) = probing_fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
    let mut settings = gateway.view().settings;
    settings.max_retries = 0;
    update(&gateway, &dir, Edit::Settings { settings });
    start(&gateway, &dir).await;
    let response = thread_request(
        &gateway,
        "/v1/responses",
        "fixture-v022-invalid-key",
        br#"{"model":"fixture-luna","input":[{"role":"user","content":"fixture"}]}"#.to_vec(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let provider = &gateway.view().providers[0];
    assert_eq!(provider.health.failures, 0);
    assert_eq!(
        provider.health.cooldown_reason.as_deref(),
        Some("hard_rejection")
    );
    assert!(provider.model_health.is_empty());
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn metadata_parse_failure_keeps_existing_owner_pinned() {
    let owner_hits = Arc::new(AtomicUsize::new(0));
    let owner_count = owner_hits.clone();
    let owner_body = seed_response();
    let owner_response_body = owner_body.clone();
    let owner_port = serve(move |_| {
        let owner_body = owner_response_body.clone();
        owner_count.fetch_add(1, Ordering::SeqCst);
        async move { json_response(StatusCode::OK, owner_body, None) }
    })
    .await;
    let receiver_hits = Arc::new(AtomicUsize::new(0));
    let receiver_count = receiver_hits.clone();
    let receiver_port = serve(move |_| {
        receiver_count.fetch_add(1, Ordering::SeqCst);
        async {
            json_response(
                StatusCode::OK,
                br#"{"provider":"receiver"}"#.as_slice(),
                None,
            )
        }
    })
    .await;
    let (dir, gateway) = probing_fixture(vec![
        format!("http://127.0.0.1:{owner_port}/v1"),
        format!("http://127.0.0.1:{receiver_port}/v1"),
    ])
    .await;
    start(&gateway, &dir).await;
    let ids = provider_ids(&gateway);
    let session = "fixture-v022-malformed-owner";
    let seed = thread_request(&gateway, "/v1/responses", session, initial_body()).await;
    assert_eq!(seed.status(), StatusCode::OK);
    assert_eq!(response_bytes(seed).await, owner_body.as_bytes());
    update(
        &gateway,
        &dir,
        Edit::Reorder {
            ids: vec![ids[1].clone(), ids[0].clone()],
        },
    );

    let malformed = thread_request(
        &gateway,
        "/v1/responses",
        session,
        br#"{"model":"fixture-model","input":["#.to_vec(),
    )
    .await;
    assert_eq!(malformed.status(), StatusCode::OK);
    assert_eq!(response_bytes(malformed).await, owner_body.as_bytes());
    assert_eq!(owner_hits.load(Ordering::SeqCst), 2);
    assert_eq!(receiver_hits.load(Ordering::SeqCst), 0);
    gateway.stop().await.unwrap();
}
#[path = "search_owner_tests.rs"]
mod search_owner_tests;
