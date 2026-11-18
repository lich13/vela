use super::*;
use std::io::Write;

const MESSAGE: &[u8] = br#"{"id":"msg_fixture","type":"message","model":"claude-fixture","stop_reason":"end_turn","content":[{"type":"text","text":"complete"}],"usage":{"input_tokens":5,"output_tokens":3}}"#;
const STREAM_REQUEST: &[u8] = br#"{"model":"claude-fixture","stream":true,"max_tokens":32,"messages":[{"role":"user","content":"fixture"}]}"#;
const JSON_REQUEST: &[u8] = br#"{"model":"claude-fixture","stream":false,"max_tokens":32,"messages":[{"role":"user","content":"fixture"}]}"#;
const COMPLETE: &[u8] = b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_fixture\",\"type\":\"message\",\"model\":\"claude-fixture\",\"usage\":{\"input_tokens\":5}}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"complete\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
const PARTIAL: &[u8] = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"discard-partial\"}}\n\n";

fn options(f: &Fixture, buffer: bool, endless: bool, filter: bool) {
    let provider = &f.gateway.view().providers[0];
    f.edit(Edit::SaveProvider {
        id: Some(provider.id.clone()),
        base_url: provider.base_url.clone(),
        token: String::new(),
        name: None,
        quota_access_token: None,
        search_only: None,
        end_on_message_stop: None,
        filter_count_tokens: Some(filter),
        buffer_complete_response: Some(buffer),
        endless_mode: Some(endless),
        upstream_retry_seconds: Some(1),
        normal_retry_seconds: Some(1),
    });
    let mut settings = f.gateway.view().settings;
    settings.max_retries = 1;
    settings.rate_limit_seconds = 1;
    settings.retry_budget_seconds = 10;
    f.edit(Edit::Settings { settings });
}
fn sse(bytes: impl Into<Bytes>) -> Response<WireBody> {
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(full(bytes))
        .unwrap()
}
fn json(bytes: impl Into<Bytes>) -> Response<WireBody> {
    Response::builder()
        .header("content-type", "application/json")
        .body(full(bytes))
        .unwrap()
}
async fn settled(f: &Fixture) {
    super::super::test_support::settled(&f.gateway).await;
}

#[tokio::test]
async fn count_filter_is_provider_scoped_and_leaves_no_usage_or_health_failure() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let upstream = server(move |_, _| {
        let seen = seen.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            json(br#"{"input_tokens":7}"#.as_slice())
        }
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, false, false, true);
    let id = f.gateway.view().providers[0].id.clone();
    f.edit(Edit::ConcurrencyProvider {
        id: id.clone(),
        max_concurrency: 1,
    });
    f.start().await;
    // Hold the sole concurrency slot to prove pre-admit filtering returns 404 without queuing.
    let held = f
        .gateway
        .0
        .admission
        .acquire(
            &[f.gateway.route(&id).unwrap()],
            false,
            100,
            &mut super::super::admission::Budget::new(1),
        )
        .await
        .unwrap();
    for path in [
        "/v1/messages/count_tokens",
        "/v1/messages/count_tokens?beta=true",
    ] {
        let response = tokio::time::timeout(
            Duration::from_millis(300),
            request(&f.gateway, path, JSON_REQUEST),
        )
        .await
        .expect("filtered count_tokens should bypass queue");
        assert_eq!(response.status(), 404);
        let body = response_bytes(response).await;
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["type"], "not_found_error");
    }
    // Unprefixed /messages without /v1 is rejected at listener routing.
    assert_eq!(
        request(&f.gateway, "/messages/count_tokens", JSON_REQUEST)
            .await
            .status(),
        404
    );
    drop(held);
    settled(&f).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    no_new_records(&f.usage, 0).await;
    assert_eq!(f.gateway.view().providers[0].health.failures, 0);
    options(&f, false, false, false);
    assert_eq!(
        request(&f.gateway, "/v1/messages/count_tokens", JSON_REQUEST)
            .await
            .status(),
        200
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    f.stop().await;
}

#[tokio::test]
async fn buffered_partial_sse_is_discarded_and_complete_attempt_delivered_once() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let upstream = server(move |_, _| {
        let seen = seen.clone();
        async move {
            sse(if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                PARTIAL
            } else {
                COMPLETE
            })
        }
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, true, false, false);
    f.start().await;
    let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
    assert_eq!(response.status(), 200);
    let body = String::from_utf8(response_bytes(response).await.to_vec()).unwrap();
    assert!(!body.contains("discard-partial"));
    assert_eq!(body.matches("event: message_stop").count(), 1);
    assert!(body.contains("complete"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    settled(&f).await;
    assert!(std::fs::read_dir(f.gateway.0.spool.path())
        .unwrap()
        .next()
        .is_none());
    f.stop().await;
}

#[tokio::test]
async fn first_token_timing_and_timeout_exclude_local_admission_wait() {
    for buffer in [false, true] {
        let upstream = server(move |_, _| async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            sse(COMPLETE)
        })
        .await;
        let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
        options(&f, buffer, false, false);
        let id = f.gateway.view().providers[0].id.clone();
        f.edit(Edit::ConcurrencyProvider {
            id: id.clone(),
            max_concurrency: 1,
        });
        let mut settings = f.gateway.view().settings;
        settings.first_byte_seconds = 1;
        settings.queue_seconds = 5;
        f.edit(Edit::Settings { settings });
        f.start().await;
        let held = f
            .gateway
            .0
            .admission
            .acquire(
                &[f.gateway.route(&id).unwrap()],
                false,
                100,
                &mut super::super::admission::Budget::new(1),
            )
            .await
            .unwrap();
        let gateway = f.gateway.clone();
        let task = tokio::spawn(async move {
            response_bytes(request(&gateway, "/v1/messages", STREAM_REQUEST).await).await
        });
        super::super::test_support::until(|| f.gateway.view().waiting_requests == 1).await;
        tokio::time::sleep(Duration::from_millis(1400)).await;
        drop(held);
        let body = tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&body).contains("event: message_stop"));
        let rows = recorded(&f.usage, 1).await;
        let attempt = &rows[0].attempts[0];
        assert_eq!(attempt.outcome, "success");
        assert!(attempt.first_token_ms.unwrap() < 1000);
        let transfer = attempt.transfer.as_ref().unwrap();
        assert!(transfer.sent_at - transfer.received_at.unwrap() >= 1000);
        f.stop().await;
    }
}

#[tokio::test]
async fn queued_request_keeps_a_full_upstream_first_byte_timeout() {
    for buffer in [false, true] {
        let upstream = server(move |_, _| async move {
            tokio::time::sleep(Duration::from_millis(750)).await;
            sse(COMPLETE)
        })
        .await;
        let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
        options(&f, buffer, false, false);
        let id = f.gateway.view().providers[0].id.clone();
        f.edit(Edit::ConcurrencyProvider {
            id: id.clone(),
            max_concurrency: 1,
        });
        let mut settings = f.gateway.view().settings;
        settings.first_byte_seconds = 2;
        settings.retry_budget_seconds = 1;
        f.edit(Edit::Settings { settings });
        f.start().await;
        let held = f
            .gateway
            .0
            .admission
            .acquire(
                &[f.gateway.route(&id).unwrap()],
                false,
                100,
                &mut super::super::admission::Budget::new(1),
            )
            .await
            .unwrap();
        let gateway = f.gateway.clone();
        let task = tokio::spawn(async move {
            response_bytes(request(&gateway, "/v1/messages", STREAM_REQUEST).await).await
        });
        super::super::test_support::until(|| f.gateway.view().waiting_requests == 1).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        drop(held);
        let body = tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(&body).contains("event: message_stop"));
        assert_eq!(
            recorded(&f.usage, 1).await[0].attempts[0].outcome,
            "success"
        );
        f.stop().await;
    }
}

#[tokio::test]
async fn endless_retries_429_and_5xx_past_global_budget_on_the_original_provider() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let upstream = server(move |_, _| {
        let seen = seen.clone();
        async move {
            match seen.fetch_add(1, Ordering::SeqCst) {
                0 => Response::builder()
                    .status(429)
                    .header("content-type", "application/json")
                    .body(full(
                        br#"{"error":{"type":"rate_limit_error","message":"rate limit"}}"#
                            .as_slice(),
                    ))
                    .unwrap(),
                1 => Response::builder()
                    .status(503)
                    .body(full("temporary service failure"))
                    .unwrap(),
                _ => sse(COMPLETE),
            }
        }
    })
    .await;
    let other_hits = Arc::new(AtomicUsize::new(0));
    let other_seen = other_hits.clone();
    let other = server(move |_, _| {
        let seen = other_seen.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            sse(COMPLETE)
        }
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1"), other.url("/v1")]).await;
    options(&f, true, true, false);
    let mut settings = f.gateway.view().settings;
    settings.max_retries = 0;
    settings.retry_budget_seconds = 1;
    settings.first_byte_seconds = 1;
    f.edit(Edit::Settings { settings });
    f.start().await;
    let body = String::from_utf8(
        response_bytes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await)
            .await
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("event: ping"));
    assert!(body.contains("complete"));
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    assert_eq!(other_hits.load(Ordering::SeqCst), 0);
    settled(&f).await;
    let rows = recorded(&f.usage, 1).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].attempts.len(), 1);
    assert_eq!(rows[0].attempts[0].outcome, "success");
    assert!(rows[0].attempts[0].first_token_ms.unwrap() < 1000);
    let transfer = rows[0].attempts[0].transfer.as_ref().unwrap();
    assert!(transfer.sent_at - transfer.received_at.unwrap() >= 1000);
    f.stop().await;
}

#[tokio::test]
async fn finite_buffer_stops_at_retry_limit_and_never_sends_partial_content() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let upstream = server(move |_, _| {
        let seen = seen.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            sse(PARTIAL)
        }
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, true, false, false);
    f.start().await;
    let body = String::from_utf8(
        response_bytes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await)
            .await
            .to_vec(),
    )
    .unwrap();
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert!(!body.contains("discard-partial"));
    assert!(body.contains("event: error"));
    f.stop().await;
}

#[tokio::test]
async fn buffered_json_and_gzip_require_complete_protocol_and_encoding() {
    for gzip in [false, true] {
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let upstream = server(move |_, _| {
            let seen = seen.clone();
            async move {
                let first = seen.fetch_add(1, Ordering::SeqCst) == 0;
                if gzip {
                    let mut encoder =
                        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                    encoder.write_all(MESSAGE).unwrap();
                    let mut bytes = encoder.finish().unwrap();
                    if first {
                        bytes.truncate(bytes.len() - 5);
                    }
                    Response::builder()
                        .header("content-type", "application/json")
                        .header("content-encoding", "gzip")
                        .body(full(bytes))
                        .unwrap()
                } else {
                    json(if first {
                        br#"{"type":"message","content":[]}"#.as_slice()
                    } else {
                        MESSAGE
                    })
                }
            }
        })
        .await;
        let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
        options(&f, true, false, false);
        f.start().await;
        let response = request(&f.gateway, "/v1/messages", JSON_REQUEST).await;
        assert_eq!(response.status(), 200);
        let bytes = response_bytes(response).await;
        let decoded =
            replay::decode_prefix(&bytes, if gzip { "gzip" } else { "identity" }, 1024 * 1024)
                .unwrap();
        assert_eq!(decoded, MESSAGE);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        f.stop().await;
    }
}

#[tokio::test]
async fn endless_header_timeout_is_omitted() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let upstream = server(move |_, _| {
        let seen = seen.clone();
        async move {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            sse(COMPLETE)
        }
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, true, true, false);
    let mut settings = f.gateway.view().settings;
    settings.first_byte_seconds = 1;
    settings.retry_budget_seconds = 1;
    f.edit(Edit::Settings { settings });
    f.start().await;
    let bytes = response_bytes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await).await;
    assert!(String::from_utf8_lossy(&bytes).contains("complete"));
    settled(&f).await;
    let rows = recorded(&f.usage, 1).await;
    assert_eq!(rows[0].attempts.len(), 1);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    f.stop().await;
}

#[tokio::test]
async fn endless_permanent_rejections_stop_and_consumed_usage_is_kept() {
    for (status, error, billed) in [
        (401, "authentication_error", false),
        (429, "billing_error", false),
        (500, "invalid_request_error", false),
        (429, "rate_limit_error", true),
    ] {
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let upstream=server(move |_, _| {let seen=seen.clone();async move {
            if seen.fetch_add(1,Ordering::SeqCst)>0 {return sse(COMPLETE);}
            let mut value=json!({"type":"error","error":{"type":error,"message": if error=="invalid_request_error" {"Invalid parameter: max_tokens"} else {error}}});
            if billed {value["usage"]=json!({"input_tokens":5,"output_tokens":1});}
            Response::builder().status(status).header("content-type","application/json").body(full(value.to_string())).unwrap()
        }}).await;
        let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
        options(&f, true, true, false);
        f.start().await;
        let bytes = response_bytes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await).await;
        settled(&f).await;
        if billed {
            assert_eq!(hits.load(Ordering::SeqCst), 2);
            let rows = recorded(&f.usage, 1).await;
            assert_eq!(rows[0].attempts.len(), 2);
            assert!(String::from_utf8_lossy(&bytes).contains("complete"));
        } else {
            assert_eq!(hits.load(Ordering::SeqCst), 1);
            no_new_records(&f.usage, 0).await;
            assert!(String::from_utf8_lossy(&bytes).contains("event: error"));
        }
        f.stop().await;
    }
}

#[tokio::test]
async fn endless_wait_releases_slots_and_stops_on_cancel_disconnect_or_shutdown() {
    for end in ["cancel", "disconnect", "shutdown"] {
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let upstream = server(move |_, _| {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                Response::builder()
                    .status(429)
                    .header("content-type", "application/json")
                    .header("retry-after", "10")
                    .body(full(
                        r#"{"error":{"type":"rate_limit_error","message":"rate limit"}}"#,
                    ))
                    .unwrap()
            }
        })
        .await;
        let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
        options(&f, false, true, false);
        f.start().await;
        let mut response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
        let ping = response.body_mut().frame().await.unwrap().unwrap();
        assert!(String::from_utf8_lossy(ping.data_ref().unwrap()).contains("event: ping"));
        super::super::test_support::until(|| {
            hits.load(Ordering::SeqCst) == 1 && f.gateway.view().providers[0].active_requests == 0
        })
        .await;
        match end {
            "cancel" => {
                let pending = f.gateway.view().pending_requests;
                assert_eq!(pending.len(), 1);
                assert_eq!(pending[0].remaining_seconds, None);
                assert!(f.gateway.cancel_wait(&pending[0].request_id));
                assert!(String::from_utf8_lossy(&response_bytes(response).await)
                    .contains("event: error"));
            }
            "disconnect" => drop(response),
            _ => {
                f.stop().await;
                assert!(String::from_utf8_lossy(&response_bytes(response).await)
                    .contains("event: error"));
            }
        }
        // A clean TCP FIN is also a legal half-close; the next heartbeat observes a closed consumer.
        tokio::time::timeout(Duration::from_secs(6), async {
            while !f.gateway.view().pending_requests.is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{end} left a pending request"));
        settled(&f).await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        no_new_records(&f.usage, 0).await;
        if end != "shutdown" {
            f.stop().await;
        }
    }
}

#[tokio::test]
async fn buffer_sends_heartbeats_without_handing_partial_business_content() {
    let released = Arc::new(tokio::sync::Notify::new());
    let wait = released.clone();
    let upstream=server(move |_, _| {let wait=wait.clone();async move {
        let stream=async_stream::stream! {
            yield Ok::<_,BoxError>(Frame::data(Bytes::from_static(PARTIAL)));
            wait.notified().await;
            yield Ok(Frame::data(Bytes::from_static(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")));
        };
        Response::builder().header("content-type","text/event-stream").body(StreamBody::new(stream).boxed_unsync()).unwrap()
    }}).await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, true, false, false);
    let mut settings = f.gateway.view().settings;
    settings.idle_seconds = 10;
    settings.first_byte_seconds = 10;
    settings.retry_budget_seconds = 20;
    f.edit(Edit::Settings { settings });
    f.start().await;
    let mut response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
    for _ in 0..2 {
        let frame = tokio::time::timeout(Duration::from_secs(6), response.body_mut().frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let text = String::from_utf8_lossy(frame.data_ref().unwrap());
        assert!(text.contains("event: ping"));
        assert!(!text.contains("discard-partial"));
    }
    assert_eq!(f.gateway.view().pending_requests.len(), 1);
    released.notify_one();
    let bytes = response_bytes(response).await;
    assert!(String::from_utf8_lossy(&bytes).contains("discard-partial"));
    f.stop().await;
}

#[tokio::test]
async fn streamed_gzip_is_decoded_for_heartbeats_and_large_buffer_spools_are_removed() {
    for gzip in [false, true] {
        let mut body = Vec::new();
        for _ in 0..768 {
            body.extend_from_slice(format!("event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"delta\":{{\"type\":\"text_delta\",\"text\":\"{}\"}}}}\n\n", "x".repeat(4096)).as_bytes());
        }
        body.extend_from_slice(COMPLETE);
        let expected = body.clone();
        if gzip {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(&body).unwrap();
            body = encoder.finish().unwrap();
        }
        let upstream = server(move |_, _| {
            let bytes = body.clone();
            async move {
                let mut response = sse(bytes);
                if gzip {
                    response.headers_mut().insert(
                        "content-encoding",
                        hyper::header::HeaderValue::from_static("gzip"),
                    );
                }
                response
            }
        })
        .await;
        let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
        options(&f, true, false, false);
        f.start().await;
        let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
        assert!(response.headers().get("content-encoding").is_none());
        assert!(response.headers().get("content-length").is_none());
        let bytes = response_bytes(response).await;
        assert!(bytes.ends_with(&expected));
        settled(&f).await;
        assert!(std::fs::read_dir(f.gateway.0.spool.path())
            .unwrap()
            .next()
            .is_none());
        f.stop().await;
    }
}

#[tokio::test]
async fn explicit_compaction_headers_classify_usage_and_suspend_execution_timeouts() {
    let sequence = Arc::new(AtomicUsize::new(0));
    let upstream = server(move |_, _| {
        let sequence = sequence.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            json(String::from_utf8(MESSAGE.to_vec()).unwrap().replace(
                "msg_fixture",
                &format!("msg_fixture_{}", sequence.fetch_add(1, Ordering::SeqCst)),
            ))
        }
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    let mut settings = f.gateway.view().settings;
    settings.first_byte_seconds = 1;
    settings.retry_budget_seconds = 1;
    f.edit(Edit::Settings { settings });
    f.start().await;
    for header in ["x-claude-code-compaction"] {
        let client: HttpClient =
            Client::builder(TokioExecutor::new()).build(Connector::new(Duration::from_secs(2), 0));
        let token = f.gateway.0.inner.lock().store.local_token.clone();
        let request = Request::builder()
            .method("POST")
            .uri(format!(
                "http://127.0.0.1:{}/v1/messages",
                f.gateway.view().settings.port
            ))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .header(header, "manual")
            .body(full(JSON_REQUEST.to_vec()))
            .unwrap();
        let response = client.request(request);
        tokio::pin!(response);
        tokio::select! { _=&mut response => panic!("compaction did not wait"), _=tokio::time::sleep(Duration::from_millis(200))=>{} }
        assert_eq!(f.gateway.view().pending_requests[0].phase, "compaction");
        let response = tokio::time::timeout(WAIT, response).await.unwrap().unwrap();
        assert_eq!(response.status(), 200);
        response_bytes(response).await;
    }
    settled(&f).await;
    let rows = recorded(&f.usage, 1).await;
    assert!(rows
        .iter()
        .all(|row| row.attempts[0].operation == Operation::Compaction));
    f.stop().await;
}

#[tokio::test]
async fn context_compacted_header_and_prompt_words_are_ordinary_messages() {
    let next_id = Arc::new(AtomicUsize::new(0));
    let upstream = server(move |_, _| {
        let id = next_id.fetch_add(1, Ordering::SeqCst);
        async move {
            let mut message: serde_json::Value = serde_json::from_slice(MESSAGE).unwrap();
            message["id"] = json!(format!("msg_fixture_{id}"));
            json(message.to_string())
        }
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    f.start().await;
    for header in [
        "x-claude-code-context-compacted",
        "x-cc-context-compacted",
        "x-cc-compaction-request",
    ] {
        let client: HttpClient =
            Client::builder(TokioExecutor::new()).build(Connector::new(Duration::from_secs(2), 0));
        let token = f.gateway.0.inner.lock().store.local_token.clone();
        let request=Request::builder().method("POST").uri(format!("http://127.0.0.1:{}/v1/messages",f.gateway.view().settings.port)).header("authorization",format!("Bearer {token}")).header("content-type","application/json").header(header,"true").body(full(br#"{"model":"claude-fixture","messages":[{"role":"user","content":"compact summarize compression request"}]}"#.as_slice())).unwrap();
        let response = client.request(request).await.unwrap();
        assert_eq!(response.status(), 200);
        response_bytes(response).await;
    }
    settled(&f).await;
    assert!(recorded(&f.usage, 3)
        .await
        .iter()
        .all(|row| row.attempts[0].operation == Operation::Model));
    f.stop().await;
}

#[tokio::test]
async fn buffer_advances_phase_to_streaming_and_buffering_only_after_business_tokens() {
    let ping_sent = Arc::new(tokio::sync::Notify::new());
    let send_delta = Arc::new(tokio::sync::Notify::new());
    let delta_sent = Arc::new(tokio::sync::Notify::new());
    let finish_stream = Arc::new(tokio::sync::Notify::new());
    let (ps, sd, ds, fs) = (
        ping_sent.clone(),
        send_delta.clone(),
        delta_sent.clone(),
        finish_stream.clone(),
    );
    let upstream = server(move |_, _| {
        let (ps, sd, ds, fs) = (ps.clone(), sd.clone(), ds.clone(), fs.clone());
        async move {
            let stream = async_stream::stream! {
                yield Ok::<_, BoxError>(Frame::data(Bytes::from_static(
                    b"event: ping\ndata: {\"type\":\"ping\"}\n\n",
                )));
                ps.notify_one();
                sd.notified().await;
                yield Ok(Frame::data(Bytes::from_static(PARTIAL)));
                ds.notify_one();
                fs.notified().await;
                yield Ok(Frame::data(Bytes::from_static(
                    b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                )));
            };
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(StreamBody::new(stream).boxed_unsync())
                .unwrap()
        }
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, true, false, false);
    f.start().await;
    let gw = f.gateway.clone();
    let task = tokio::spawn(async move {
        response_bytes(request(&gw, "/v1/messages", STREAM_REQUEST).await).await
    });
    ping_sent.notified().await;
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(f.gateway.view().pending_requests[0].phase, "headers");
    assert_eq!(f.gateway.freeze_gate().view().streaming, 0);

    send_delta.notify_one();
    delta_sent.notified().await;
    super::super::test_support::until(|| {
        f.gateway
            .view()
            .pending_requests
            .first()
            .is_some_and(|r| r.phase == "buffering")
            && f.gateway.freeze_gate().view().streaming == 1
    })
    .await;

    finish_stream.notify_one();
    let bytes = tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&bytes).contains("discard-partial"));
    f.stop().await;
}

#[tokio::test]
async fn stream_and_initial_rejection_preserve_error_details_and_endless_keeps_circuit_closed() {
    // 1) Gzipped 400 prompt-too-long on streamed & non-streamed requests preserves type/message/status.
    let upstream = server(move |_, _| async move {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder
            .write_all(
                br#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#,
            )
            .unwrap();
        Response::builder()
            .status(400)
            .header("content-type", "application/json")
            .header("content-encoding", "gzip")
            .body(full(encoder.finish().unwrap()))
            .unwrap()
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, true, true, false);
    f.start().await;
    let sse_err = String::from_utf8(
        response_bytes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await)
            .await
            .to_vec(),
    )
    .unwrap();
    assert!(sse_err.contains("invalid_request_error"), "{sse_err}");
    assert!(sse_err.contains("prompt is too long"), "{sse_err}");

    // HTTP 200 SSE carrying initial invalid_request_error maps to HTTP 400 on non-streamed fallback.
    let sse_reject = server(move |_, _| async move {
        sse(b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"prompt is too long\"}}\n\n".as_slice())
    })
    .await;
    let f2 = Fixture::new(ClientId::Claude, &[sse_reject.url("/v1")]).await;
    options(&f2, true, true, false);
    f2.start().await;
    let non_stream = request(&f2.gateway, "/v1/messages", JSON_REQUEST).await;
    assert_eq!(non_stream.status(), 400);
    let body2 = String::from_utf8(response_bytes(non_stream).await.to_vec()).unwrap();
    assert!(body2.contains("prompt is too long"), "{body2}");
    f2.stop().await;
    f.stop().await;

    // 2) Endless retries do not open circuit or count failures, but expose health.cause during retry.
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let flaky = server(move |_, _| {
        let seen = seen.clone();
        async move {
            let n = seen.fetch_add(1, Ordering::SeqCst);
            if n < 3 {
                Response::builder()
                    .status(if n == 0 { 429 } else { 502 })
                    .body(full("bad gateway"))
                    .unwrap()
            } else {
                sse(COMPLETE)
            }
        }
    })
    .await;
    let f3 = Fixture::new(ClientId::Claude, &[flaky.url("/v1")]).await;
    options(&f3, true, true, false);
    let mut settings = f3.gateway.view().settings;
    settings.failure_threshold = 2;
    settings.transient_failure_threshold = 2;
    f3.edit(Edit::Settings { settings });
    f3.start().await;
    let gw3 = f3.gateway.clone();
    let task3 = tokio::spawn(async move {
        response_bytes(request(&gw3, "/v1/messages", STREAM_REQUEST).await).await
    });
    super::super::test_support::until(|| {
        hits.load(Ordering::SeqCst) >= 2 && f3.gateway.view().providers[0].health.cause.is_some()
    })
    .await;
    let mid = &f3.gateway.view().providers[0].health;
    assert_eq!(mid.failures, 0);
    assert_eq!(mid.state, super::super::circuit::CircuitState::Closed);
    let done = tokio::time::timeout(Duration::from_secs(15), task3)
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&done).contains("complete"));
    let after = &f3.gateway.view().providers[0].health;
    assert_eq!(after.failures, 0);
    assert!(after.cause.is_none());
    f3.stop().await;
}

#[tokio::test]
async fn endless_freeze_pauses_dispatch_then_resumes_same_provider() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let upstream = server(move |_, _| {
        let seen = seen.clone();
        async move {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                Response::builder()
                    .status(503)
                    .body(full("temporary service failure"))
                    .unwrap()
            } else {
                sse(COMPLETE)
            }
        }
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, true, true, false);
    f.start().await;
    let response = request(&f.gateway, "/v1/messages", STREAM_REQUEST).await;
    super::super::test_support::until(|| {
        hits.load(Ordering::SeqCst) == 1 && f.gateway.view().providers[0].active_requests == 0
    })
    .await;
    f.gateway
        .freeze_gate()
        .set_selected(Some(ClientId::Claude), true)
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    f.gateway
        .freeze_gate()
        .set_selected(Some(ClientId::Claude), false)
        .unwrap();
    assert!(String::from_utf8_lossy(&response_bytes(response).await).contains("complete"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    f.stop().await;
}

#[tokio::test]
async fn buffered_declared_length_must_finish_even_after_message_stop() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let upstream = server(move |_, _| {
        let seen = seen.clone();
        async move {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                let frames = async_stream::stream! {
                    yield Ok::<_, BoxError>(Frame::data(Bytes::from_static(COMPLETE)));
                    tokio::time::sleep(Duration::from_millis(50)).await;
                };
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .header("content-length", COMPLETE.len() + 100)
                    .body(StreamBody::new(frames).boxed_unsync())
                    .unwrap()
            } else {
                sse(COMPLETE)
            }
        }
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, true, false, false);
    let provider = f.gateway.view().providers[0].clone();
    f.edit(Edit::SaveProvider {
        id: Some(provider.id),
        base_url: provider.base_url,
        token: String::new(),
        name: None,
        quota_access_token: None,
        search_only: None,
        end_on_message_stop: Some(true),
        filter_count_tokens: None,
        buffer_complete_response: None,
        endless_mode: None,
        upstream_retry_seconds: None,
        normal_retry_seconds: None,
    });
    f.start().await;
    let body = response_bytes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await).await;
    assert_eq!(
        String::from_utf8_lossy(&body)
            .matches("event: message_stop")
            .count(),
        1,
        "hits={} body={}",
        hits.load(Ordering::SeqCst),
        String::from_utf8_lossy(&body)
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    f.stop().await;
}

#[tokio::test]
async fn endless_without_buffer_never_replays_business_content_already_delivered() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let upstream=server(move |_, _| {let seen=seen.clone();async move {
        seen.fetch_add(1,Ordering::SeqCst);
        let frames=async_stream::stream! {yield Ok::<_,BoxError>(Frame::data(Bytes::from_static(PARTIAL)));tokio::time::sleep(Duration::from_millis(100)).await;};
        Response::builder().header("content-type","text/event-stream").body(StreamBody::new(frames).boxed_unsync()).unwrap()
    }}).await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, false, true, false);
    f.start().await;
    let body = response_bytes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await).await;
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("discard-partial"));
    assert!(text.contains("event: error"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    f.stop().await;
}

#[tokio::test]
async fn buffered_truncated_zstd_is_retried_before_delivery() {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let upstream = server(move |_, _| {
        let seen = seen.clone();
        async move {
            let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 1).unwrap();
            encoder.write_all(MESSAGE).unwrap();
            encoder.flush().unwrap();
            let incomplete = encoder.get_ref().clone();
            let complete = encoder.finish().unwrap();
            Response::builder()
                .header("content-type", "application/json")
                .header("content-encoding", "zstd")
                .body(full(if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                    incomplete
                } else {
                    complete
                }))
                .unwrap()
        }
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, true, false, false);
    f.start().await;
    let response = request(&f.gateway, "/v1/messages", JSON_REQUEST).await;
    assert_eq!(response.status(), 200);
    let bytes = response_bytes(response).await;
    assert_eq!(
        replay::decode_prefix(&bytes, "zstd", 1024 * 1024).unwrap(),
        MESSAGE
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    f.stop().await;
}

#[tokio::test]
async fn endless_retries_empty_200_and_first_event_timeout_before_business_delivery() {
    for timeout in [false, true] {
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let upstream=server(move |_, _| {let seen=seen.clone();async move {
            if seen.fetch_add(1,Ordering::SeqCst)==0 {
                let frames=async_stream::stream! {
                    yield Ok::<_,BoxError>(Frame::data(Bytes::from_static(b"event: ping\ndata: {\"type\":\"ping\"}\n\n")));
                    if timeout {tokio::time::sleep(Duration::from_secs(3)).await;}
                };
                Response::builder().header("content-type","text/event-stream").body(StreamBody::new(frames).boxed_unsync()).unwrap()
            } else {sse(COMPLETE)}
        }}).await;
        let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
        options(&f, false, true, false);
        let mut settings = f.gateway.view().settings;
        settings.first_byte_seconds = 1;
        settings.idle_seconds = 1;
        f.edit(Edit::Settings { settings });
        f.start().await;
        let body = response_bytes(request(&f.gateway, "/v1/messages", STREAM_REQUEST).await).await;
        assert!(String::from_utf8_lossy(&body).contains("complete"));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        f.stop().await;
    }
}

#[tokio::test]
async fn terminal_gateway_and_claude_rejections_disable_client_sdk_blind_retries() {
    let upstream = server(|_, _| async {
        Response::builder()
            .status(429)
            .header("content-type", "application/json")
            .header("retry-after", "10")
            .body(full(
                r#"{"error":{"type":"rate_limit_error","message":"rate limit"}}"#,
            ))
            .unwrap()
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[upstream.url("/v1")]).await;
    options(&f, true, false, false);
    f.edit(Edit::Mode {
        mode: "auto".into(),
    });
    let mut settings = f.gateway.view().settings;
    settings.retry_budget_seconds = 1;
    settings.rate_limit_seconds = 10;
    f.edit(Edit::Settings { settings });
    f.start().await;
    let response = request(&f.gateway, "/v1/messages", JSON_REQUEST).await;
    assert_eq!(response.status(), 504);
    assert_eq!(
        response
            .headers()
            .get("x-should-retry")
            .and_then(|v| v.to_str().ok()),
        Some("false")
    );
    f.stop().await;
}
