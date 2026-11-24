//! Endless mode's three retry waits, their settings applied to requests
//! already in flight, and the requests it holds.
use super::*;
use crate::events::ErrorBasis;
use crate::gateway::test_support::form;
use crate::gateway::{model::RetryClass, test_support::until};

/// An upstream that answers its `n`th request, from 0, with `answer(n)`.
async fn answering(
    answer: impl Fn(usize) -> Response<WireBody> + Send + Sync + 'static,
) -> (LoopbackServer, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let answer = Arc::new(answer);
    let upstream = server(move |_, _| {
        let n = seen.fetch_add(1, Ordering::SeqCst);
        let answer = answer.clone();
        async move { answer(n) }
    })
    .await;
    (upstream, hits)
}
fn rejected(status: u16, kind: &str, message: &str) -> Response<WireBody> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full(
            json!({"type": "error", "error": {"type": kind, "message": message}}).to_string(),
        ))
        .unwrap()
}
/// A successful response whose stream opens with an error event.
fn refused(error: serde_json::Value) -> Response<WireBody> {
    sse(format!(
        "event: error\ndata: {}\n\n",
        json!({"type": "error", "error": error})
    ))
}
/// Intervals under which only a retry of `class` returns before the
/// fixture's wait for a response runs out.
fn only(class: RetryClass) -> Pacing {
    let seconds = |own| Some(if class == own { 1 } else { 60 });
    Pacing {
        risk: seconds(RetryClass::Risk),
        limit: seconds(RetryClass::Limit),
        normal: seconds(RetryClass::Normal),
        ..Default::default()
    }
}
/// A started gateway whose first provider runs endless mode at `pace`.
async fn endless(client: ClientId, urls: &[String], buffer: bool, pace: Pacing) -> Fixture {
    let f = Fixture::new(client, urls).await;
    options(&f, buffer, true, false);
    pacing(&f, pace);
    f.start().await;
    f
}
/// Waits for the first provider to be between attempts after `hits` of them
/// and returns what its retry is waiting out.
async fn waiting(f: &Fixture, hits: &AtomicUsize, count: usize) -> Option<ErrorBasis> {
    until(|| {
        let view = f.gateway.view();
        hits.load(Ordering::SeqCst) == count
            && view.providers[0].active_requests == 0
            && view.providers[0].health.cause.is_some()
            && !view.transient_retries.is_empty()
    })
    .await;
    let health = f.gateway.view().providers[0].health.clone();
    // The retry is the provider's whole reaction: it is left without a
    // cooldown and its circuit stays closed.
    assert!(health.available);
    assert_eq!(health.retry_in, 0);
    assert_eq!(health.cooldown_reason, None);
    assert_eq!(health.failures, 0);
    health.cause.and_then(|cause| cause.error_basis)
}
async fn completes(response: Response<Incoming>) {
    let body = response_bytes(response).await;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("complete"), "{text}");
}

#[tokio::test]
async fn a_listed_status_is_retried_whatever_its_body_says() {
    for (status, kind, codes) in [
        (429, "billing_error", vec![429, 520]),
        (403, "permission_error", vec![403]),
        (400, "invalid_request_error", vec![400]),
    ] {
        let (upstream, hits) = answering(move |n| match n {
            0 => rejected(status, kind, "Invalid parameter: max_tokens"),
            _ => sse(COMPLETE),
        })
        .await;
        let pace = Pacing {
            codes: Some(codes),
            ..only(RetryClass::Risk)
        };
        let f = endless(ClientId::Claude, &[upstream.url("/v1")], true, pace).await;
        let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
        assert_eq!(
            waiting(&f, &hits, 1).await,
            Some(ErrorBasis::RiskControl),
            "{status}"
        );
        completes(response).await;
        assert_eq!(hits.load(Ordering::SeqCst), 2, "{status}");
        f.stop().await;
    }
}

#[tokio::test]
async fn a_status_taken_off_the_list_waits_the_ordinary_interval() {
    // Claude Code lists 520 until the list is edited. Off the list it is the
    // fault Cloudflare reports of the site behind it.
    for (codes, class, basis) in [
        (None, RetryClass::Risk, Some(ErrorBasis::RiskControl)),
        (
            Some(vec![]),
            RetryClass::Normal,
            Some(ErrorBasis::ServiceError),
        ),
    ] {
        let (upstream, hits) = answering(|n| match n {
            0 => Response::builder()
                .status(520)
                .body(full("error code: 520"))
                .unwrap(),
            _ => sse(COMPLETE),
        })
        .await;
        let pace = Pacing {
            codes,
            ..only(class)
        };
        let f = endless(ClientId::Claude, &[upstream.url("/v1")], true, pace).await;
        let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
        assert_eq!(waiting(&f, &hits, 1).await, basis, "{class:?}");
        completes(response).await;
        assert_eq!(hits.load(Ordering::SeqCst), 2, "{class:?}");
        f.stop().await;
    }
}

#[tokio::test]
async fn a_rate_limit_waits_its_own_interval_whatever_retry_after_says() {
    for (body, basis) in [
        // No dimension named.
        (
            json!({"type": "error", "error": {"type": "rate_limit_error", "message": "rate limit"}}),
            ErrorBasis::LimitRetry,
        ),
        // Outside endless mode this message is a capacity cooldown.
        (
            json!({"error": {"message": "Service temporarily unavailable"}}),
            ErrorBasis::LimitRetry,
        ),
        // A 429 is a rate limit even where its envelope reads as a fault.
        (
            json!({"error": {"type": "api_error", "message": "upstream failure"}}),
            ErrorBasis::LimitRetry,
        ),
        // A named dimension is kept.
        (
            json!({"error": {"code": "rpm_limit_exceeded", "message": "RPM limit exceeded"}}),
            ErrorBasis::Rpm,
        ),
    ] {
        let failure = body.to_string();
        let (upstream, hits) = answering(move |n| match n {
            0 => Response::builder()
                .status(429)
                .header("content-type", "application/json")
                .header("retry-after", "300")
                .body(full(failure.clone()))
                .unwrap(),
            _ => sse(COMPLETE),
        })
        .await;
        let f = endless(
            ClientId::Claude,
            &[upstream.url("/v1")],
            true,
            only(RetryClass::Limit),
        )
        .await;
        let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
        assert_eq!(waiting(&f, &hits, 1).await, Some(basis), "{body}");
        assert!(f.gateway.view().transient_retries[0].retry_in <= 1);
        completes(response).await;
        assert_eq!(hits.load(Ordering::SeqCst), 2, "{body}");
        f.stop().await;
    }
}

#[tokio::test]
async fn a_timeout_and_a_rate_limit_under_another_status_are_retried() {
    for (status, body, class) in [
        (
            408,
            json!({"type": "error", "error": {"type": "timeout_error", "message": "Request timeout"}}),
            RetryClass::Normal,
        ),
        (
            400,
            json!({"error": {"code": "rpm_limit_exceeded", "message": "RPM limit exceeded"}}),
            RetryClass::Limit,
        ),
        (
            529,
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
            RetryClass::Normal,
        ),
    ] {
        let failure = body.to_string();
        let (upstream, hits) = answering(move |n| match n {
            0 => Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(full(failure.clone()))
                .unwrap(),
            _ => sse(COMPLETE),
        })
        .await;
        let f = endless(ClientId::Claude, &[upstream.url("/v1")], true, only(class)).await;
        completes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await).await;
        assert_eq!(hits.load(Ordering::SeqCst), 2, "{status}");
        f.stop().await;
    }
}

#[tokio::test]
async fn a_rejection_inside_a_successful_response_is_sorted_by_what_it_carries() {
    for (error, codes, class, basis) in [
        (
            json!({"type": "rate_limit_error", "message": "rate limit"}),
            None,
            RetryClass::Limit,
            ErrorBasis::LimitRetry,
        ),
        (
            json!({"type": "api_error", "code": "429", "message": "upstream refused"}),
            None,
            RetryClass::Limit,
            ErrorBasis::LimitRetry,
        ),
        (
            json!({"type": "api_error", "code": 520, "message": "upstream unreachable"}),
            None,
            RetryClass::Risk,
            ErrorBasis::RiskControl,
        ),
        // A listed code is retried even where the rejection reads as final.
        (
            json!({"type": "permission_error", "code": "403", "message": "forbidden"}),
            Some(vec![403]),
            RetryClass::Risk,
            ErrorBasis::RiskControl,
        ),
    ] {
        let failure = error.clone();
        let (upstream, hits) = answering(move |n| match n {
            0 => refused(failure.clone()),
            _ => sse(COMPLETE),
        })
        .await;
        let pace = Pacing {
            codes,
            ..only(class)
        };
        let f = endless(ClientId::Claude, &[upstream.url("/v1")], true, pace).await;
        let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
        assert_eq!(waiting(&f, &hits, 1).await, Some(basis), "{error}");
        completes(response).await;
        assert_eq!(hits.load(Ordering::SeqCst), 2, "{error}");
        f.stop().await;
    }
}

#[tokio::test]
async fn a_quota_window_is_waited_out_and_what_waiting_cannot_cure_is_answered() {
    // A day's quota comes back on its own, under either status.
    for status in [429, 403] {
        let (upstream, hits) = answering(move |n| match n {
            0 => rejected(status, "billing_error", "daily usage limit exceeded"),
            _ => sse(COMPLETE),
        })
        .await;
        let f = endless(
            ClientId::Claude,
            &[upstream.url("/v1")],
            true,
            only(RetryClass::Limit),
        )
        .await;
        let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
        assert_eq!(
            waiting(&f, &hits, 1).await,
            Some(ErrorBasis::QuotaExhausted),
            "{status}"
        );
        completes(response).await;
        assert_eq!(hits.load(Ordering::SeqCst), 2, "{status}");
        f.stop().await;
    }
    // A balance, a model the plan lacks and a request the plan cannot make
    // do not, though each arrives as a 429.
    for error in [
        json!({"type": "insufficient_quota", "message": "You exceeded your current quota"}),
        json!({
            "details": {"error_code": "credits_required", "model": "claude-sonnet-4-5"},
            "message": "Usage credits are required for this model."
        }),
        json!({
            "type": "rate_limit_error",
            "message": "Extra usage is required for long context requests."
        }),
    ] {
        let failure = json!({"type": "error", "error": error}).to_string();
        let (upstream, hits) = answering(move |_| {
            Response::builder()
                .status(429)
                .header("content-type", "application/json")
                .body(full(failure.clone()))
                .unwrap()
        })
        .await;
        let f = endless(
            ClientId::Claude,
            &[upstream.url("/v1")],
            true,
            only(RetryClass::Limit),
        )
        .await;
        let body = response_bytes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await).await;
        assert!(
            String::from_utf8_lossy(&body).contains("event: error"),
            "{error}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1, "{error}");
        f.stop().await;
    }
}

#[tokio::test]
async fn a_rate_limit_rests_the_provider_for_its_retry_and_for_new_requests() {
    let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
    let times = sent.clone();
    let (upstream, hits) = answering(move |n| {
        times.lock().unwrap().push(std::time::Instant::now());
        match n {
            0 => rejected(429, "rate_limit_error", "rate limit"),
            _ => sse(COMPLETE),
        }
    })
    .await;
    // The retry's own second ends inside the two-second rest.
    let pace = Pacing {
        rest: Some(2),
        intensity: Some(0),
        ..only(RetryClass::Limit)
    };
    let f = endless(ClientId::Claude, &[upstream.url("/v1")], true, pace).await;
    let limited = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
    waiting(&f, &hits, 1).await;
    assert_eq!(f.gateway.view().transient_retries[0].retry_in, 2);
    // A request that arrives during the rest is not sent before it ends.
    let newcomer = tokio::spawn({
        let gateway = f.gateway.clone();
        async move { completes(request(&gateway, "/v1/messages", STREAM_REQUEST).await).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    completes(limited).await;
    newcomer.await.unwrap();
    let sent = sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 3);
    for later in &sent[1..] {
        let rested = later.duration_since(sent[0]);
        assert!(
            rested >= Duration::from_secs(2) && rested < Duration::from_secs(5),
            "sent {rested:?} after the rate limit"
        );
    }
    f.stop().await;
}

#[tokio::test]
async fn an_interval_edited_during_the_wait_moves_the_retry() {
    let (upstream, hits) = answering(|n| match n {
        0 => rejected(429, "rate_limit_error", "rate limit"),
        _ => sse(COMPLETE),
    })
    .await;
    let pace = Pacing {
        limit: Some(60),
        ..Default::default()
    };
    let f = endless(ClientId::Claude, &[upstream.url("/v1")], true, pace).await;
    let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
    waiting(&f, &hits, 1).await;
    // A lone request waits a fifth of the interval at the default intensity.
    assert_eq!(f.gateway.view().transient_retries[0].retry_in, 12);
    pacing(
        &f,
        Pacing {
            intensity: Some(0),
            ..Default::default()
        },
    );
    assert_eq!(f.gateway.view().transient_retries[0].retry_in, 60);
    pacing(
        &f,
        Pacing {
            limit: Some(1),
            ..Default::default()
        },
    );
    completes(response).await;
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    f.stop().await;
}

#[tokio::test]
async fn an_interval_edited_during_an_attempt_paces_the_failure_that_follows() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let held = Arc::new(tokio::sync::Notify::new());
    let gate = held.clone();
    let upstream = server(move |_, _| {
        let first = seen.fetch_add(1, Ordering::SeqCst) == 0;
        let gate = gate.clone();
        async move {
            if first {
                gate.notified().await;
                rejected(429, "rate_limit_error", "rate limit")
            } else {
                sse(COMPLETE)
            }
        }
    })
    .await;
    let pace = Pacing {
        limit: Some(60),
        ..Default::default()
    };
    let f = endless(ClientId::Claude, &[upstream.url("/v1")], true, pace).await;
    let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
    until(|| hits.load(Ordering::SeqCst) == 1).await;
    pacing(
        &f,
        Pacing {
            limit: Some(1),
            ..Default::default()
        },
    );
    held.notify_one();
    completes(response).await;
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    f.stop().await;
}

#[tokio::test]
async fn endless_mode_switched_on_during_a_request_holds_it_from_the_next_attempt() {
    // A broken stream is retried once by the buffer alone; the rate limit
    // after it would be answered if endless mode had not taken the request.
    let (upstream, hits) = answering(|n| match n {
        0 => sse(PARTIAL),
        1 => rejected(429, "rate_limit_error", "rate limit"),
        _ => sse(COMPLETE),
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, true, false, false);
    f.start().await;
    let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
    until(|| {
        hits.load(Ordering::SeqCst) == 1 && f.gateway.view().providers[0].active_requests == 0
    })
    .await;
    options(&f, true, true, false);
    completes(response).await;
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    f.stop().await;
}

#[tokio::test]
async fn endless_mode_switched_off_during_the_wait_returns_the_request_to_the_retry_limit() {
    let (upstream, hits) = answering(|_| rejected(429, "rate_limit_error", "rate limit")).await;
    let f = endless(
        ClientId::Claude,
        &[upstream.url("/v1")],
        true,
        Pacing::default(),
    )
    .await;
    let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
    waiting(&f, &hits, 2).await;
    options(&f, true, false, false);
    // Two attempts are already past the fixture's single retry.
    let body = response_bytes(response).await;
    assert!(String::from_utf8_lossy(&body).contains("event: error"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    f.stop().await;
}

#[tokio::test]
async fn endless_requests_wait_side_by_side_past_the_waiting_limit() {
    let (upstream, hits) = answering(|n| match n {
        0 | 1 => rejected(429, "rate_limit_error", "rate limit"),
        _ => sse(COMPLETE),
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, true, true, false);
    let mut settings = f.gateway.view().settings;
    settings.max_waiting = 1;
    f.edit(Edit::Settings { settings });
    f.start().await;
    let first = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
    let second = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
    until(|| f.gateway.view().transient_retries.len() == 2).await;
    completes(first).await;
    completes(second).await;
    assert_eq!(hits.load(Ordering::SeqCst), 4);
    f.stop().await;
}

#[tokio::test]
async fn a_token_replaced_during_the_wait_is_the_one_the_retry_sends() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let tokens = Arc::new(Mutex::new(Vec::new()));
    let sent = tokens.clone();
    let upstream = server(move |request: Request<Incoming>, _| {
        let first = seen.fetch_add(1, Ordering::SeqCst) == 0;
        sent.lock().push(
            ["authorization", "x-api-key"]
                .into_iter()
                .filter_map(|name| request.headers().get(name))
                .filter_map(|value| value.to_str().ok())
                .collect::<Vec<_>>()
                .join(" "),
        );
        async move {
            if first {
                rejected(429, "rate_limit_error", "rate limit")
            } else {
                sse(COMPLETE)
            }
        }
    })
    .await;
    let pace = Pacing {
        limit: Some(60),
        ..Default::default()
    };
    let f = endless(ClientId::Claude, &[upstream.url("/v1")], true, pace).await;
    let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
    waiting(&f, &hits, 1).await;
    let provider = f.gateway.view().providers[0].clone();
    f.edit(form(serde_json::json!({
        "id": provider.id,
        "baseUrl": provider.base_url,
        "token": "fixture-rotated-key",
    })));
    // The edited provider is a fresh start: the retry does not sit out the
    // minute its old credentials earned.
    completes(response).await;
    let tokens = tokens.lock().clone();
    assert_eq!(tokens.len(), 2);
    assert!(tokens[0].contains(FIXTURE_KEY) && !tokens[0].contains("fixture-rotated-key"));
    assert!(tokens[1].contains("fixture-rotated-key") && !tokens[1].contains(FIXTURE_KEY));
    f.stop().await;
}

#[tokio::test]
async fn deleting_the_provider_answers_the_request_waiting_on_it() {
    let (upstream, hits) = answering(|_| rejected(429, "rate_limit_error", "rate limit")).await;
    let (other, other_hits) = answering(|_| sse(COMPLETE)).await;
    let pace = Pacing {
        limit: Some(60),
        ..Default::default()
    };
    let f = endless(
        ClientId::Claude,
        &[upstream.url("/v1"), other.url("/v1")],
        true,
        pace,
    )
    .await;
    let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
    waiting(&f, &hits, 1).await;
    let id = f.gateway.view().providers[0].id.clone();
    f.edit(Edit::DeleteProvider { id });
    let body = response_bytes(response).await;
    assert!(String::from_utf8_lossy(&body).contains("event: error"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // It was this provider's request; it does not move to another one.
    assert_eq!(other_hits.load(Ordering::SeqCst), 0);
    f.stop().await;
}

#[tokio::test]
async fn a_request_outwaits_the_queue_limit_for_whichever_endless_provider_frees_up() {
    let mut upstreams = Vec::new();
    let mut gates = Vec::new();
    let mut counts = Vec::new();
    for _ in 0..2 {
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let held = Arc::new(tokio::sync::Notify::new());
        let gate = held.clone();
        upstreams.push(
            server(move |_, _| {
                let first = seen.fetch_add(1, Ordering::SeqCst) == 0;
                let gate = gate.clone();
                async move {
                    if first {
                        gate.notified().await;
                    }
                    sse(COMPLETE)
                }
            })
            .await,
        );
        gates.push(held);
        counts.push(hits);
    }
    let urls: Vec<_> = upstreams
        .iter()
        .map(|upstream| upstream.url("/v1"))
        .collect();
    let f = Fixture::new(ClientId::Claude, &urls).await;
    for provider in f.gateway.view().providers {
        f.edit(form(serde_json::json!({
            "id": provider.id,
            "baseUrl": provider.base_url,
            "token": "",
            "endlessMode": true,
            "limitRestSeconds": 0,
        })));
        f.edit(Edit::ConcurrencyProvider {
            id: provider.id,
            max_concurrency: 1,
        });
    }
    f.edit(Edit::Mode {
        mode: "auto".into(),
    });
    f.start().await;
    let send = || {
        let gateway = f.gateway.clone();
        tokio::spawn(async move {
            response_bytes(request(&gateway, "/v1/messages", STREAM_REQUEST).await).await
        })
    };
    let first = send();
    until(|| counts[0].load(Ordering::SeqCst) == 1).await;
    let second = send();
    until(|| counts[1].load(Ordering::SeqCst) == 1).await;
    let third = send();
    // Past the fixture's two-second queue limit it is still waiting.
    tokio::time::sleep(Duration::from_millis(2600)).await;
    assert!(!third.is_finished());
    assert_eq!(f.gateway.view().pending_requests.len(), 3);
    gates[1].notify_one();
    for response in [second, third] {
        let body = response.await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("complete"));
    }
    assert_eq!(counts[1].load(Ordering::SeqCst), 2);
    gates[0].notify_one();
    assert!(String::from_utf8_lossy(&first.await.unwrap()).contains("complete"));
    assert_eq!(counts[0].load(Ordering::SeqCst), 1);
    f.stop().await;
}

#[tokio::test]
async fn codex_endless_mode_sorts_and_retries_its_failures_the_same_way() {
    // Codex lists no risk-control status until one is added.
    for (status, codes, class) in [
        (429, None, RetryClass::Limit),
        (502, None, RetryClass::Normal),
        (502, Some(vec![502]), RetryClass::Risk),
    ] {
        let (upstream, hits) = answering(move |n| match n {
            0 | 1 => Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(full(
                    json!({"error": {"type": "server_error", "message": "upstream failure"}})
                        .to_string(),
                ))
                .unwrap(),
            _ => json(HTTP_RESPONSE),
        })
        .await;
        let pace = Pacing {
            codes,
            ..only(class)
        };
        let f = endless(ClientId::Codex, &[upstream.url("/v1")], false, pace).await;
        // The second failure is already past the fixture's single retry.
        let response = request(&f.gateway, "/v1/responses", HTTP_REQUEST).await;
        assert_eq!(response.status(), 200, "{status} {class:?}");
        assert_eq!(response_bytes(response).await, HTTP_RESPONSE);
        assert_eq!(hits.load(Ordering::SeqCst), 3, "{status} {class:?}");
        f.stop().await;
    }
}

/// A started Claude gateway in automatic mode whose first provider runs
/// endless mode at `pace`.
async fn queued(urls: &[String], pace: Pacing) -> Fixture {
    let f = Fixture::new(ClientId::Claude, urls).await;
    options(&f, true, true, false);
    pacing(&f, pace);
    f.edit(Edit::Mode {
        mode: "auto".into(),
    });
    f.start().await;
    f
}

#[tokio::test]
async fn a_model_the_endless_provider_lacks_moves_the_request_on_at_once() {
    // Under an ordinary status, and under one listed as risk control.
    for (status, kind, message) in [
        (
            404,
            "model_not_found",
            "The model claude-fixture does not exist",
        ),
        (
            429,
            "new_api_error",
            "no available channel for model claude-fixture",
        ),
    ] {
        let (first, first_hits) = answering(move |_| rejected(status, kind, message)).await;
        let (second, second_hits) = answering(|_| sse(COMPLETE)).await;
        // Every retry interval outlasts the fixture's wait for a response.
        let pace = Pacing {
            codes: Some(vec![429]),
            risk: Some(60),
            limit: Some(60),
            normal: Some(60),
            ..Default::default()
        };
        let f = queued(&[first.url("/v1"), second.url("/v1")], pace).await;
        completes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await).await;
        assert_eq!(first_hits.load(Ordering::SeqCst), 1, "{status}");
        assert_eq!(second_hits.load(Ordering::SeqCst), 1, "{status}");
        f.stop().await;
    }
}

#[tokio::test]
async fn a_model_the_only_provider_lacks_is_answered_without_a_retry() {
    let (upstream, hits) = answering(|_| {
        rejected(
            404,
            "model_not_found",
            "The model claude-fixture does not exist",
        )
    })
    .await;
    let f = endless(
        ClientId::Claude,
        &[upstream.url("/v1")],
        true,
        only(RetryClass::Normal),
    )
    .await;
    let body = response_bytes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await).await;
    assert!(String::from_utf8_lossy(&body).contains("model_not_found"));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    f.stop().await;
}

#[tokio::test]
async fn a_rate_limit_is_still_retried_where_it_was_with_another_provider_free() {
    let (first, first_hits) = answering(|n| match n {
        0 => rejected(429, "rate_limit_error", "rate limit"),
        _ => sse(COMPLETE),
    })
    .await;
    let (second, second_hits) = answering(|_| sse(COMPLETE)).await;
    let f = queued(
        &[first.url("/v1"), second.url("/v1")],
        only(RetryClass::Limit),
    )
    .await;
    completes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await).await;
    assert_eq!(first_hits.load(Ordering::SeqCst), 2);
    assert_eq!(second_hits.load(Ordering::SeqCst), 0);
    f.stop().await;
}
