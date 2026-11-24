use super::*;
use crate::gateway::routing::Requirement;
use crate::gateway::test_support::settle;
use serde_json::json;
use std::fs;

fn allow(g: &Gateway, t: &tempfile::TempDir, n: usize, models: Option<Vec<&str>>) {
    update(
        g,
        t,
        Edit::ModelsProvider {
            id: g.view().providers[n].id.clone(),
            allowed_models: models.map(|m| m.into_iter().map(str::to_owned).collect()),
        },
    );
}
async fn body(response: Response<Incoming>) -> Bytes {
    response.into_body().collect().await.unwrap().to_bytes()
}

#[test]
fn provider_display_name_is_trimmed_and_host_is_the_blank_fallback() {
    let mut store = Store::default();
    store
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
                base_url: "https://api.example.invalid/deployment".into(),
                token: "fixture-token".into(),
                name: Some("  主力供应商  ".into()),
            },
            false,
        )
        .unwrap();
    assert_eq!(store.providers[0].name, "主力供应商");

    store
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
                base_url: "https://fallback.example.invalid/v1".into(),
                token: "fixture-token-2".into(),
                name: Some("  ".into()),
            },
            false,
        )
        .unwrap();
    assert_eq!(store.providers[1].name, "fallback.example.invalid");

    let invalid = store.edit(
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
            base_url: "https://invalid.example.invalid".into(),
            token: "fixture-token-3".into(),
            name: Some("x".repeat(121)),
        },
        false,
    );
    assert_eq!(invalid.unwrap_err().code, "NAME");
}

#[tokio::test]
async fn exact_rules_filter_before_attempts_and_preserve_original_json_and_resources() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut urls = Vec::new();
    for n in 0..5 {
        let seen = seen.clone();
        let port = server(move |req| {
            let seen = seen.clone();
            async move {
                let payload = req.into_body().collect().await.unwrap().to_bytes();
                seen.lock().push((n, payload.clone()));
                Response::new(full(payload))
            }
        })
        .await;
        urls.push(format!("http://127.0.0.1:{port}"));
    }
    let (t, g) = fixture(urls).await;
    for n in 0..4 {
        allow(&g, &t, n, Some(vec!["other"]));
    }
    allow(&g, &t, 4, Some(vec!["Exact"]));
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    start(&g, &t).await;
    let config = std::fs::read(t.path().join("config.toml")).unwrap();
    let payload = b"{ \"model\" : \"Exact\", \"future\" : [3, 7] }";
    assert_eq!(
        body(
            request(
                &g,
                "/v1/responses",
                payload.to_vec(),
                vec![("content-type", "application/json")]
            )
            .await
        )
        .await
        .as_ref(),
        payload
    );
    settle(&g).await;
    assert_eq!(seen.lock()[0].0, 4);
    assert_eq!(g.view().providers[4].active_requests, 0);
    assert!(g.view().providers[..4]
        .iter()
        .all(|p| p.active_requests == 0));

    let denied = request(
        &g,
        "/v1/responses",
        br#"{"model":"exact"}"#.to_vec(),
        vec![("content-type", "application/json")],
    )
    .await;
    assert_eq!(denied.status(), 400);
    assert!(String::from_utf8(body(denied).await.to_vec())
        .unwrap()
        .contains("MODEL_NOT_ALLOWED"));
    let unknown = request(&g, "/v1/future", vec![0, 255], vec![]).await;
    assert_eq!(unknown.status(), 400);
    assert!(String::from_utf8(body(unknown).await.to_vec())
        .unwrap()
        .contains("MODEL_UNDETERMINED"));
    let resource = request(&g, "/v1/files", vec![0, 255], vec![]).await;
    assert_eq!(body(resource).await.as_ref(), &[0, 255]);
    assert_eq!(std::fs::read(t.path().join("config.toml")).unwrap(), config);
    assert_eq!(
        std::fs::read(t.path().join("auth.json")).unwrap(),
        b"unchanged-auth"
    );
    g.stop().await.unwrap();
}

#[tokio::test]
async fn multipart_compressed_hints_and_manual_or_context_restrictions() {
    let p = server(|r| async {
        Response::new(full(r.into_body().collect().await.unwrap().to_bytes()))
    })
    .await;
    let (t, g) = fixture(vec![
        format!("http://127.0.0.1:{p}"),
        format!("http://127.0.0.1:{p}"),
    ])
    .await;
    allow(&g, &t, 0, Some(vec!["a"]));
    allow(&g, &t, 1, Some(vec!["b"]));
    start(&g, &t).await;
    let denied = request(
        &g,
        "/v1/responses",
        br#"{"model":"b"}"#.to_vec(),
        vec![("content-type", "application/json")],
    )
    .await;
    assert_eq!(denied.status(), 400);
    let multipart = b"--test-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"binary\"\r\n\r\n\0\xff\r\n--test-boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\na\r\n--test-boundary--\r\n";
    let response = request(
        &g,
        "/v1/images/edits",
        multipart.to_vec(),
        vec![(
            "content-type",
            "multipart/form-data; boundary=\"test-boundary\"",
        )],
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(body(response).await.as_ref(), multipart);
    let payload = br#"{"model":"a","unknown":"keep"}"#;
    use std::io::Write;
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzip.write_all(payload).unwrap();
    let compressed = gzip.finish().unwrap();
    assert_eq!(
        body(
            request(
                &g,
                "/v1/responses",
                compressed.clone(),
                vec![
                    ("content-type", "application/json"),
                    ("content-encoding", "gzip")
                ]
            )
            .await
        )
        .await
        .as_ref(),
        compressed
    );
    let id = g.view().providers[0].id.clone();
    g.remember_model("context-a", &id, Some("a"));
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    let inherited = request(
        &g,
        "/v1/responses",
        br#"{"previous_response_id":"context-a"}"#.to_vec(),
        vec![("content-type", "application/json")],
    )
    .await;
    assert_eq!(inherited.status(), 200);
    body(inherited).await;
    let denied = request(
        &g,
        "/v1/responses",
        br#"{"previous_response_id":"context-a","model":"b"}"#.to_vec(),
        vec![("content-type", "application/json")],
    )
    .await;
    assert_eq!(denied.status(), 400);
    g.stop().await.unwrap();
}

#[tokio::test]
async fn waiting_requests_recheck_rules_without_resetting_occupied_slots() {
    let (t, g) = fixture(vec!["https://example.invalid".into()]).await;
    let id = g.view().providers[0].id.clone();
    update(
        &g,
        &t,
        Edit::ConcurrencyProvider {
            id: id.clone(),
            max_concurrency: 1,
        },
    );
    allow(&g, &t, 0, Some(vec!["a"]));
    start(&g, &t).await;
    let routes = vec![g.route(&id).unwrap()];
    let held =
        g.0.admission
            .acquire_for(
                &routes,
                false,
                100,
                &mut admission::Budget::new(5),
                &Requirement::model(Some("a")),
            )
            .await
            .unwrap();
    let next = g.clone();
    let queued_routes = routes.clone();
    let wait = tokio::spawn(async move {
        next.0
            .admission
            .acquire_for(
                &queued_routes,
                false,
                100,
                &mut admission::Budget::new(5),
                &Requirement::model(Some("a")),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while g.view().waiting_requests != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    allow(&g, &t, 0, Some(vec!["b"]));
    assert!(matches!(
        wait.await.unwrap(),
        Err(admission::Rejected::Model)
    ));
    assert_eq!(g.view().providers[0].active_requests, 1);

    drop(held);
    assert_eq!(g.view().providers[0].active_requests, 0);
    g.stop().await.unwrap();
}

#[tokio::test]
async fn catalog_is_cached_singleflight_bounded_and_has_no_business_side_effects() {
    let count = Arc::new(AtomicUsize::new(0));
    let hits = count.clone();
    let port = server(move |r| { let hits = hits.clone(); async move {
        assert_eq!(r.uri().path(),"/tenant/v1/models");
        assert_eq!(r.headers()["authorization"],"Bearer upstream-fixture-token");
        hits.fetch_add(1,Ordering::Relaxed); tokio::time::sleep(Duration::from_millis(60)).await;
        Response::new(full(r#"{"data":[{"id":"b"},{"id":"a"},{"id":"a"},{"id":"upstream-fixture-token"}],"ignored":"secret"}"#))
    }}).await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}/tenant/v1")]).await;
    let id = g.view().providers[0].id.clone();
    let auth = std::fs::read(t.path().join("auth.json")).unwrap();
    let config = std::fs::read(t.path().join("config.toml")).unwrap();
    assert!(g.list_models(&id, false).await.unwrap().models.is_empty());
    assert_eq!(
        count.load(Ordering::Relaxed),
        0,
        "opening a panel only reads cache"
    );
    let (a, b) = tokio::join!(g.list_models(&id, true), g.list_models(&id, true));
    assert_eq!(a.unwrap().models, vec!["a", "b"]);
    assert_eq!(b.unwrap().models, vec!["a", "b"]);
    g.list_models(&id, false).await.unwrap();
    assert_eq!(count.load(Ordering::Relaxed), 1);

    assert_eq!(g.view().active_connections, 0);
    assert_eq!(std::fs::read(t.path().join("auth.json")).unwrap(), auth);
    assert_eq!(std::fs::read(t.path().join("config.toml")).unwrap(), config);
}

#[tokio::test]
async fn v02514_new_provider_discovers_once_failed_refresh_keeps_old_and_deleted_result_is_discarded(
) {
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let failed = fail.clone();
    let port = server(move |_| {
        let count = count.clone();
        let failed = failed.clone();
        async move {
            count.fetch_add(1, Ordering::SeqCst);
            if failed.load(Ordering::SeqCst) {
                Response::builder()
                    .status(500)
                    .body(full("fixture error"))
                    .unwrap()
            } else {
                Response::new(full(r#"{"data":[{"id":"fixture-model"}]}"#))
            }
        }
    })
    .await;
    let (t, g) = fixture(vec![]).await;
    g.0.discover_new_providers.store(true, Ordering::Relaxed);
    update(
        &g,
        &t,
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
            base_url: format!("http://127.0.0.1:{port}"),
            token: "fixture-local".into(),
            name: None,
        },
    );
    let id = g.view().providers[0].id.clone();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if g.list_models(&id, false)
                .await
                .unwrap()
                .checked_at
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    for _ in 0..3 {
        assert_eq!(
            g.list_models(&id, false).await.unwrap().models,
            vec!["fixture-model"]
        );
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    fail.store(true, Ordering::SeqCst);
    let old = g.list_models(&id, true).await.unwrap();
    assert!(old.error.is_some());
    assert_eq!(old.models, vec!["fixture-model"]);
    assert!(!old.stale);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate2 = gate.clone();
    let entered2 = entered.clone();
    let blocked = server(move |_| {
        let gate = gate2.clone();
        let entered = entered2.clone();
        async move {
            entered.notify_one();
            let _permit = gate.acquire().await.unwrap();
            Response::new(full(r#"{"data":[{"id":"late"}]}"#))
        }
    })
    .await;
    update(
        &g,
        &t,
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
            id: Some(id.clone()),
            base_url: format!("http://127.0.0.1:{blocked}"),
            token: "fixture-new".into(),
            name: None,
        },
    );
    assert!(g.list_models(&id, false).await.unwrap().stale);
    let next = g.clone();
    let queried = id.clone();
    let task = tokio::spawn(async move { next.list_models(&queried, true).await });
    entered.notified().await;
    update(&g, &t, Edit::DeleteProvider { id: id.clone() });
    gate.add_permits(1);
    assert_eq!(task.await.unwrap().unwrap_err().code, "STALE");
    let cache = super::super::catalog::Service::new(t.path().join("provider-models.json"));
    assert!(cache.cached(&id, "fixture").models.is_empty());
}

#[tokio::test]
async fn catalog_errors_and_changed_credentials_do_not_expose_raw_responses() {
    for status in [301, 401, 403, 429, 500] {
        let port = server(move |_| async move {
            Response::builder()
                .status(status)
                .header("retry-after", "120")
                .body(full("private-upstream-response"))
                .unwrap()
        })
        .await;
        let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}")]).await;
        let id = g.view().providers[0].id.clone();
        let result = g.list_models(&id, true).await.unwrap();
        assert!(result.error.is_some());
        assert!(!serde_json::to_string(&result)
            .unwrap()
            .contains("private-upstream"));
        if status == 429 {
            assert!(result.retry_at.unwrap() >= quota::now() + 119);
        }
        assert_eq!(g.view().providers[0].health.failures, 0);
        drop(t);
    }
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let entered = Arc::new(tokio::sync::Notify::new());
    let r = gate.clone();
    let e = entered.clone();
    let port = server(move |_| {
        let r = r.clone();
        let e = e.clone();
        async move {
            e.notify_one();
            let _ = r.acquire().await.unwrap();
            Response::new(full(r#"{"data":[{"id":"old-model"}]}"#))
        }
    })
    .await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}")]).await;
    let id = g.view().providers[0].id.clone();
    let next = g.clone();
    let task_id = id.clone();
    let task = tokio::spawn(async move { next.list_models(&task_id, true).await });
    entered.notified().await;
    update(
        &g,
        &t,
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
            id: Some(id),
            base_url: format!("http://127.0.0.1:{port}"),
            token: "changed-fixture".into(),
            name: None,
        },
    );
    gate.add_permits(1);
    assert_eq!(task.await.unwrap().unwrap_err().code, "STALE");
}

#[tokio::test]
async fn completed_http_response_keeps_model_affinity_and_health() {
    let port = server(|req| async {
        let _ = req.into_body().collect().await;
        Response::new(full(r#"{"id":"response-normal"}"#))
    })
    .await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}")]).await;
    allow(&g, &t, 0, Some(vec!["allowed"]));
    start(&g, &t).await;
    let first = request(
        &g,
        "/v1/responses",
        br#"{"model":"allowed"}"#.to_vec(),
        vec![("content-type", "application/json")],
    )
    .await;
    body(first).await;

    assert_eq!(g.view().providers[0].health.failures, 0);
    let inherited = request(
        &g,
        "/v1/responses",
        br#"{"previous_response_id":"response-normal"}"#.to_vec(),
        vec![("content-type", "application/json")],
    )
    .await;
    assert_eq!(inherited.status(), 200);
    body(inherited).await;
    let unknown_ws = request(
        &g,
        "/v1/future/socket",
        vec![],
        vec![
            ("upgrade", "websocket"),
            ("connection", "upgrade"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ("sec-websocket-version", "13"),
        ],
    )
    .await;
    assert_eq!(unknown_ws.status(), 400);
    assert!(String::from_utf8(body(unknown_ws).await.to_vec())
        .unwrap()
        .contains("MODEL_UNDETERMINED"));
    g.stop().await.unwrap();
}

#[tokio::test]
async fn catalog_limits_response_size_and_request_duration() {
    let port = server(|_| async { Response::new(full(vec![b'x'; 2 * 1024 * 1024 + 1])) }).await;
    let (_t, g) = fixture(vec![format!("http://127.0.0.1:{port}")]).await;
    let id = g.view().providers[0].id.clone();
    assert!(g
        .list_models(&id, true)
        .await
        .unwrap()
        .error
        .unwrap()
        .contains("2 MB"));
    let port = server(|_| async {
        tokio::time::sleep(Duration::from_secs(15)).await;
        Response::new(full("{}"))
    })
    .await;
    let (_t, g) = fixture(vec![format!("http://127.0.0.1:{port}")]).await;
    let id = g.view().providers[0].id.clone();
    assert!(g
        .list_models(&id, true)
        .await
        .unwrap()
        .error
        .unwrap()
        .contains("超时"));
}

#[test]
fn provider_message_stop_edits_remain_independent() {
    for stored in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.json");
        let mut value = serde_json::to_value(Store::default()).unwrap();
        value["providers"] = json!([
            {"id":"edited","name":"Edited","baseUrl":"https://one.example.invalid","token":"fixture","queued":true,"version":"one","endOnMessageStop":stored},
            {"id":"second","name":"Second","baseUrl":"https://two.example.invalid","token":"fixture","queued":true,"version":"two","endOnMessageStop":stored}
        ]);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let (mut store, revision) = Store::load(&path).unwrap();
        assert_eq!(store.providers[0].end_on_message_stop, stored);
        assert_eq!(store.providers[1].end_on_message_stop, stored);
        store
            .edit(
                form(json!({"baseUrl": "https://new.example.invalid"})),
                false,
            )
            .unwrap();
        assert!(!store.providers[2].end_on_message_stop);
        store
            .edit(
                form(json!({"id": "edited", "token": "", "endOnMessageStop": !stored})),
                false,
            )
            .unwrap();
        assert_eq!(store.providers[0].end_on_message_stop, !stored);
        assert_eq!(store.providers[1].end_on_message_stop, stored);
        store.persist(&path, &revision).unwrap();
        let (reloaded, _) = Store::load(&path).unwrap();
        assert_eq!(reloaded.providers[0].end_on_message_stop, !stored);
        assert_eq!(reloaded.providers[1].end_on_message_stop, stored);
        assert!(!reloaded.providers[2].end_on_message_stop);
    }
}

#[test]
fn auxiliary_endpoint_clear_preserves_connection_and_deleted_providers_leave_no_metadata() {
    let mut store = Store::default();
    store
        .edit(
            Edit::SaveGrokProvider {
                delegate: Default::default(),
                id: None,
                name: Some("fixture".into()),
                base_url: "https://primary.example.invalid".into(),
                token: "fixture".into(),
                quota_access_token: None,
                protocol: grok_config::Protocol::Messages,
            },
            false,
        )
        .unwrap();
    let id = store.providers[0].id.clone();
    let connection = store.grok_connections.get_mut(&id).unwrap();
    connection
        .headers
        .insert("x-api-key".into(), "fixture-auth".into());
    connection
        .query
        .insert("region".into(), "fixture-region".into());
    let connection = serde_json::to_value(connection).unwrap();
    for base_urls in [
        vec!["https://backup.example.invalid".into()],
        vec![],
        vec![store.providers[0].base_url.clone()],
    ] {
        store
            .edit(
                Edit::EndpointsProvider {
                    id: id.clone(),
                    base_urls,
                },
                false,
            )
            .unwrap();
        assert_eq!(
            serde_json::to_value(&store.grok_connections[&id]).unwrap(),
            connection
        );
    }
    store.dsh_models.insert(id.clone(), Default::default());
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("gateway.json");
    let mut orphaned = store.clone();
    orphaned.providers.clear();
    fs::write(&path, serde_json::to_vec(&orphaned).unwrap()).unwrap();
    let (loaded, _) = Store::load(&path).unwrap();
    assert!(loaded.grok_connections.is_empty() && loaded.dsh_models.is_empty());
    store.edit(Edit::DeleteProvider { id }, false).unwrap();
    assert!(store.grok_connections.is_empty() && store.dsh_models.is_empty());
}

#[test]
fn endless_retry_waits_longer_for_risk_control_and_older_stores_keep_their_interval() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gateway.json");
    let mut value = serde_json::to_value(Store::default()).unwrap();
    value["providers"] = json!([
        {"id":"saved","name":"Saved","baseUrl":"https://one.example.invalid","token":"fixture","queued":true,"version":"one","upstreamRetrySeconds":5},
        {"id":"fresh","name":"Fresh","baseUrl":"https://two.example.invalid","token":"fixture","queued":true,"version":"two"}
    ]);
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let (mut store, _) = Store::load(&path).unwrap();
    let seconds = |p: &Provider| (p.upstream_retry_seconds, p.normal_retry_seconds);
    assert_eq!(seconds(&store.providers[0]), (5, 3));
    assert_eq!(seconds(&store.providers[1]), (33, 3));
    let save = |id: Option<&str>, risk, normal| Edit::SaveProvider {
        delegate: Default::default(),
        id: id.map(str::to_owned),
        name: None,
        base_url: "https://one.example.invalid".into(),
        token: "fixture".into(),
        quota_access_token: None,
        end_on_message_stop: None,
        filter_count_tokens: None,
        buffer_complete_response: None,
        endless_mode: None,
        upstream_retry_seconds: risk,
        normal_retry_seconds: normal,
        risk_codes: None,
        retry_intensity: None,
        limit_retry_seconds: None,
        limit_rest_seconds: None,
    };
    store
        .edit(save(Some("saved"), None, Some(9)), false)
        .unwrap();
    assert_eq!(seconds(&store.providers[0]), (5, 9));
    store.edit(save(None, None, None), false).unwrap();
    assert_eq!(seconds(&store.providers[2]), (33, 3));

    let plain = crate::events::Details::default();
    let embedded = |code: Option<&str>, kind: Option<&str>| crate::events::Details {
        upstream_code: code.map(str::to_owned),
        upstream_type: kind.map(str::to_owned),
        ..Default::default()
    };
    use crate::gateway::model::{retry_class, RetryClass::*};
    let codes = crate::gateway::ClientId::Claude.default_risk_codes();
    assert_eq!(codes, [520]);
    let classified = |basis| crate::events::Details {
        error_basis: Some(basis),
        ..Default::default()
    };
    let limited = classified(crate::events::ErrorBasis::Rpm);
    let overloaded = classified(crate::events::ErrorBasis::Capacity);
    for (status, details, expected) in [
        (Some(520), &plain, Risk),
        (Some(429), &plain, Limit),
        // A 429 that only said the service was unavailable is a rate limit.
        (Some(429), &overloaded, Limit),
        (Some(503), &plain, Normal),
        (Some(529), &overloaded, Normal),
        // Connection failures and timeouts carry no status.
        (None, &plain, Normal),
        (Some(200), &plain, Normal),
        (Some(200), &embedded(Some("520"), None), Risk),
        (Some(200), &embedded(Some("429"), None), Limit),
        (Some(200), &embedded(None, Some("rate_limit_error")), Limit),
        (Some(200), &limited, Limit),
        (Some(200), &overloaded, Normal),
        (Some(200), &embedded(Some("500"), Some("api_error")), Normal),
        // A real status decides; the body's wording does not override it,
        // but content classified as a rate limit is one under any status.
        (
            Some(503),
            &embedded(Some("429"), Some("rate_limit_error")),
            Normal,
        ),
        (Some(400), &limited, Limit),
    ] {
        assert_eq!(
            retry_class(codes, status, details),
            expected,
            "{status:?} {:?} {:?} {:?}",
            details.upstream_code,
            details.upstream_type,
            details.error_basis
        );
    }
    // The list decides first: a listed 429 is risk control, and a status
    // taken off the list falls back to its own class.
    assert_eq!(retry_class(&[403, 429], Some(403), &plain), Risk);
    assert_eq!(retry_class(&[403, 429], Some(429), &plain), Risk);
    assert_eq!(retry_class(&[], Some(520), &plain), Normal);
    assert_eq!(retry_class(&[], Some(429), &plain), Limit);
}

#[test]
fn endless_pacing_is_checked_on_saving_and_unset_fields_keep_what_was_saved() {
    use crate::gateway::ClientId;
    assert_eq!(ClientId::Claude.default_risk_codes(), [520]);
    assert_eq!(ClientId::Dsh.default_risk_codes(), [524]);
    assert!(ClientId::Codex.default_risk_codes().is_empty());
    assert!(ClientId::Grok.default_risk_codes().is_empty());

    let save = |id: Option<&str>, codes: Option<Vec<u16>>, intensity, limit| Edit::SaveProvider {
        delegate: Default::default(),
        id: id.map(str::to_owned),
        name: None,
        base_url: "https://one.example.invalid".into(),
        token: "fixture".into(),
        quota_access_token: None,
        end_on_message_stop: None,
        filter_count_tokens: None,
        buffer_complete_response: None,
        endless_mode: None,
        upstream_retry_seconds: None,
        normal_retry_seconds: None,
        risk_codes: codes,
        retry_intensity: intensity,
        limit_retry_seconds: limit,
        limit_rest_seconds: None,
    };
    let pacing = |store: &Store| {
        let p = &store.providers[0];
        (
            p.risk_codes.clone(),
            p.retry_intensity(),
            p.limit_retry_seconds(),
        )
    };
    let mut store = Store::default();
    store.edit(save(None, None, None, None), false).unwrap();
    let (id, version) = {
        let p = &store.providers[0];
        (p.id.clone(), p.version.clone())
    };
    // Nothing chosen: the client's own list, the middle intensity, and a
    // rate limit waits what risk control waits.
    assert_eq!(pacing(&store), (None, 5, 33));

    let all = Some(vec![599, 400, 599]);
    store
        .edit(save(Some(&id), all, Some(10), Some(7)), false)
        .unwrap();
    assert_eq!(pacing(&store), (Some(vec![400, 599]), 10, 7));
    // An empty list is a choice of its own, and the rest stays as saved.
    store
        .edit(save(Some(&id), Some(vec![]), None, None), false)
        .unwrap();
    assert_eq!(pacing(&store), (Some(vec![]), 10, 7));
    let sixteen = Some((400..416).collect());
    store
        .edit(save(Some(&id), sixteen, Some(0), None), false)
        .unwrap();
    assert_eq!(pacing(&store).1, 0);

    let saved = pacing(&store);
    for (codes, intensity) in [
        (Some(vec![399]), None),
        (Some(vec![600]), None),
        (Some((400..417).collect()), None),
        (None, Some(11)),
    ] {
        let edit = save(Some(&id), codes.clone(), intensity, None);
        assert!(store.edit(edit, false).is_err(), "{codes:?} {intensity:?}");
    }
    assert_eq!(pacing(&store), saved);
    // Pacing is not what a conversation's affinity to the provider rests on.
    assert_eq!(store.providers[0].version, version);

    // A store written before these fields reads the same defaults.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gateway.json");
    let mut value = serde_json::to_value(Store::default()).unwrap();
    value["providers"] = json!([
        {"id":"saved","name":"Saved","baseUrl":"https://one.example.invalid","token":"fixture","queued":true,"version":"one","upstreamRetrySeconds":5}
    ]);
    value["settings"]
        .as_object_mut()
        .unwrap()
        .retain(|key, _| key != "stallSeconds" && key != "holdIdleSeconds");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let (store, _) = Store::load(&path).unwrap();
    assert_eq!(pacing(&store), (None, 5, 5));
    assert_eq!(
        (
            store.settings.stall_seconds,
            store.settings.hold_idle_seconds
        ),
        (120, 60)
    );
    for (stall, hold, valid) in [
        (1, 1, true),
        (86400, 86400, true),
        (0, 60, false),
        (120, 0, false),
        (86401, 60, false),
        (120, 86401, false),
    ] {
        let settings = Settings {
            stall_seconds: stall,
            hold_idle_seconds: hold,
            ..Settings::default()
        };
        assert_eq!(settings.validate().is_ok(), valid, "{stall} {hold}");
    }
}

/// A provider form as the interface sends it, with `fields` over a new
/// provider's.
fn form(fields: serde_json::Value) -> Edit {
    let mut edit = json!({
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

#[test]
fn a_provider_form_carries_its_takeover_and_rest_settings_beside_the_others() {
    use crate::gateway::model::Delegated;
    let mut store = Store::default();
    store.edit(form(json!({})), false).unwrap();
    let id = store.providers[0].id.clone();
    let edit = |store: &mut Store, fields: serde_json::Value| {
        let mut fields = fields;
        fields["id"] = json!(id);
        fields["token"] = json!("");
        store.edit(form(fields), false)
    };
    // Nothing chosen: eight seconds of rest after a rate limit, nothing taken.
    assert_eq!(store.providers[0].limit_rest_seconds(), 8);
    assert!(store.delegates.is_empty());

    edit(
        &mut store,
        json!({
            "endlessMode": true,
            "limitRetrySeconds": 9,
            "limitRestSeconds": 0,
            "takeCompaction": true,
            "compactionModel": " compact-fixture ",
            "compactionEffort": " high ",
            "takeAuxiliary": true,
            "auxiliaryModel": "",
            "auxiliaryEffort": "",
            "takeWebSearch": true,
            "webSearchModel": " search-fixture ",
            "webSearchEffort": "low",
        }),
    )
    .unwrap();
    let p = &store.providers[0];
    assert!(p.endless_mode);
    assert_eq!((p.limit_retry_seconds(), p.limit_rest_seconds()), (9, 0));
    let delegate = &store.delegates[&id];
    assert_eq!(
        delegate.takes(Delegated::Compaction),
        Some((Some("compact-fixture"), Some("high")))
    );
    assert_eq!(delegate.takes(Delegated::Auxiliary), Some((None, None)));
    assert_eq!(
        delegate.takes(Delegated::WebSearch),
        Some((Some("search-fixture"), Some("low")))
    );
    let saved = serde_json::to_value(&store).unwrap();
    assert_eq!(
        saved["delegates"][&id],
        json!({
            "compaction": true, "compactionModel": "compact-fixture",
            "compactionEffort": "high", "auxiliary": true,
            "webSearch": true, "webSearchModel": "search-fixture", "webSearchEffort": "low"
        })
    );
    assert_eq!(saved["providers"][0]["limitRestSeconds"], 0);

    // A form without these fields leaves them as saved.
    edit(&mut store, json!({"endlessMode": false})).unwrap();
    assert_eq!(store.providers[0].limit_rest_seconds(), 0);
    assert_eq!(
        store.delegates[&id].takes(Delegated::Compaction),
        Some((Some("compact-fixture"), Some("high")))
    );
    // An option switched off keeps its model and effort for the next time.
    edit(&mut store, json!({"takeCompaction": false})).unwrap();
    assert_eq!(store.delegates[&id].takes(Delegated::Compaction), None);
    assert_eq!(
        (
            store.delegates[&id].compaction_model.as_deref(),
            store.delegates[&id].compaction_effort.as_deref()
        ),
        (Some("compact-fixture"), Some("high"))
    );
    // Emptied, they are cleared, and a provider left taking nothing has no entry.
    edit(
        &mut store,
        json!({
            "compactionModel": " ", "compactionEffort": "", "takeAuxiliary": false,
            "takeWebSearch": false, "webSearchModel": "", "webSearchEffort": " "
        }),
    )
    .unwrap();
    assert!(store.delegates.is_empty());

    let long = "m".repeat(257);
    for model in ["claude-*", "bad\u{7}model", long.as_str()] {
        assert!(
            edit(&mut store, json!({"auxiliaryModel": model})).is_err(),
            "{model}"
        );
    }
    // An effort is a level's name as the API spells it.
    let long = "e".repeat(33);
    for effort in ["High", "x high", "高", long.as_str()] {
        assert!(
            edit(&mut store, json!({"auxiliaryEffort": effort})).is_err(),
            "{effort}"
        );
    }
    assert!(store.delegates.is_empty());

    // A copy takes the settings with it; deleting it drops only its own.
    edit(
        &mut store,
        json!({
            "takeAuxiliary": true, "auxiliaryModel": "small-fixture",
            "takeWebSearch": true, "webSearchModel": "search-fixture"
        }),
    )
    .unwrap();
    store
        .edit(Edit::DuplicateProvider { id: id.clone() }, false)
        .unwrap();
    let copied = store.providers[1].id.clone();
    assert!(store.delegates[&copied] == store.delegates[&id]);
    assert_eq!(
        store.delegates[&copied].takes(Delegated::WebSearch),
        Some((Some("search-fixture"), None))
    );
    assert_eq!(store.providers[1].limit_rest_seconds, Some(0));
    store
        .edit(Edit::DeleteProvider { id: copied.clone() }, false)
        .unwrap();
    assert!(!store.delegates.contains_key(&copied) && store.delegates.contains_key(&id));

    // A store written before these settings reads their defaults, and one
    // that names a provider no longer there forgets it.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gateway.json");
    let mut value = serde_json::to_value(&store).unwrap();
    value["delegates"]["gone"] = json!({"compaction": true});
    // Web searches were the last kind a provider came to take.
    value["delegates"][&id] = json!({"auxiliary": true, "auxiliaryModel": "small-fixture"});
    value["providers"][0]
        .as_object_mut()
        .unwrap()
        .remove("limitRestSeconds");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let (store, _) = Store::load(&path).unwrap();
    assert_eq!(store.providers[0].limit_rest_seconds(), 8);
    assert_eq!(store.delegates.keys().collect::<Vec<_>>(), [&id]);
    assert_eq!(
        store.delegates[&id].takes(Delegated::Auxiliary),
        Some((Some("small-fixture"), None))
    );
    assert_eq!(store.delegates[&id].takes(Delegated::WebSearch), None);
}

#[test]
fn a_copy_takes_everything_but_the_identity_and_sits_below_its_original() {
    let mut store = Store::default();
    let save = |name: &str| Edit::SaveDshProvider {
        id: None,
        name: Some(name.into()),
        base_url: "https://primary.example.invalid".into(),
        token: "fixture".into(),
        quota_access_token: Some("fixture-access".into()),
        protocol: grok_config::Protocol::ChatCompletions,
        endless_mode: Some(true),
        upstream_retry_seconds: Some(7),
        normal_retry_seconds: Some(2),
        risk_codes: Some(vec![524, 403]),
        retry_intensity: Some(0),
        limit_retry_seconds: Some(11),
        limit_rest_seconds: None,
    };
    store.edit(save("Original"), false).unwrap();
    store.edit(save("Last"), false).unwrap();
    let id = store.providers[0].id.clone();
    for edit in [
        Edit::EndpointsProvider {
            id: id.clone(),
            base_urls: vec!["https://backup.example.invalid".into()],
        },
        Edit::ConcurrencyProvider {
            id: id.clone(),
            max_concurrency: 3,
        },
        Edit::RpmProvider {
            id: id.clone(),
            max_rpm: 9,
        },
        Edit::QueueProvider {
            id: id.clone(),
            queued: false,
        },
        Edit::ModelMapProvider {
            id: id.clone(),
            mapping: [("asked".into(), "sent".into())].into(),
        },
    ] {
        store.edit(edit, false).unwrap();
    }
    store.dsh_models.insert(
        id.clone(),
        dsh::ModelMapping([("deepseek-flash".to_owned(), Some("vendor".to_owned()))].into()),
    );
    let selected = store.selected.clone();
    assert_eq!(selected.as_deref(), Some(id.as_str()));

    store
        .edit(Edit::DuplicateProvider { id: id.clone() }, false)
        .unwrap();
    let names: Vec<_> = store.providers.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Original", "Original (copy)", "Last"]);
    assert_eq!(store.selected, selected);
    let (original, copy) = (&store.providers[0], &store.providers[1]);
    assert_ne!(copy.id, original.id);
    assert_ne!(copy.version, original.version);
    let mut same = copy.clone();
    same.id.clone_from(&original.id);
    same.version.clone_from(&original.version);
    same.name.clone_from(&original.name);
    assert_eq!(
        serde_json::to_value(&same).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    assert_eq!(
        (
            copy.risk_codes.as_deref(),
            copy.retry_intensity,
            copy.limit_retry_seconds
        ),
        (Some([403, 524].as_slice()), Some(0), Some(11))
    );
    let copied = copy.id.clone();
    assert_eq!(store.quota_access_tokens[&copied], "fixture-access");
    assert_eq!(
        store.provider_endpoints[&copied],
        store.provider_endpoints[&id]
    );
    assert_eq!(
        serde_json::to_value(&store.grok_connections[&copied]).unwrap(),
        serde_json::to_value(&store.grok_connections[&id]).unwrap()
    );
    assert_eq!(store.dsh_models[&copied], store.dsh_models[&id]);
    assert_eq!(store.model_maps[&copied], store.model_maps[&id]);

    // A long name makes room for the suffix; a copy of a copy says so twice.
    store
        .edit(
            Edit::RenameProvider {
                id: copied.clone(),
                name: "长".repeat(120),
            },
            false,
        )
        .unwrap();
    store
        .edit(Edit::DuplicateProvider { id: copied.clone() }, false)
        .unwrap();
    assert_eq!(
        store.providers[2].name,
        format!("{} (copy)", "长".repeat(113))
    );
    assert!(store
        .edit(
            Edit::DuplicateProvider {
                id: "missing".into()
            },
            false
        )
        .is_err());

    // Deleting the copy takes its own data and leaves the original's.
    store
        .edit(Edit::DeleteProvider { id: copied.clone() }, false)
        .unwrap();
    assert!(!store.model_maps.contains_key(&copied) && !store.dsh_models.contains_key(&copied));
    assert!(store.model_maps.contains_key(&id) && store.quota_access_tokens.contains_key(&id));
}

#[test]
fn a_model_mapping_holds_complete_model_ids_and_is_dropped_with_its_provider() {
    let mut store = Store::default();
    store
        .edit(
            Edit::SaveProvider {
                delegate: Default::default(),
                id: None,
                name: None,
                base_url: "https://one.example.invalid".into(),
                token: "fixture".into(),
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
            false,
        )
        .unwrap();
    let id = store.providers[0].id.clone();
    let version = store.providers[0].version.clone();
    let map = |pairs: &[(&str, &str)]| Edit::ModelMapProvider {
        id: id.clone(),
        mapping: pairs
            .iter()
            .map(|(from, to)| ((*from).to_owned(), (*to).to_owned()))
            .collect(),
    };
    store
        .edit(map(&[(" asked ", " sent "), ("second", "sent")]), false)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&store).unwrap()["modelMaps"][&id],
        json!({"asked": "sent", "second": "sent"})
    );
    assert_eq!(store.providers[0].version, version);

    let long = "m".repeat(257);
    for pairs in [
        vec![("", "sent")],
        vec![("asked", " ")],
        vec![("claude-*", "sent")],
        vec![("asked", "sent*")],
        vec![("asked\u{7}", "sent")],
        vec![("same", "same")],
        vec![("asked", " asked")],
        // Two keys that are one model once trimmed.
        vec![("asked", "sent"), (" asked", "other")],
        vec![(long.as_str(), "sent")],
        vec![("asked", long.as_str())],
    ] {
        assert!(store.edit(map(&pairs), false).is_err(), "{pairs:?}");
    }
    let many: Vec<(String, String)> = (0..513).map(|n| (format!("m{n}"), "sent".into())).collect();
    let refs = |n: usize| -> Vec<(&str, &str)> {
        many[..n]
            .iter()
            .map(|(from, to)| (from.as_str(), to.as_str()))
            .collect()
    };
    assert!(store.edit(map(&refs(513)), false).is_err());
    assert_eq!(
        store.model_maps[&id].len(),
        2,
        "a refused edit changes nothing"
    );
    store.edit(map(&refs(512)), false).unwrap();
    assert_eq!(store.model_maps[&id].len(), 512);
    assert!(store
        .edit(
            Edit::ModelMapProvider {
                id: "missing".into(),
                mapping: Default::default()
            },
            false
        )
        .is_err());

    // An empty mapping is not kept, and a saved one does not outlive its provider.
    store.edit(map(&[]), false).unwrap();
    assert!(store.model_maps.is_empty());
    assert!(serde_json::to_value(&store)
        .unwrap()
        .get("modelMaps")
        .is_none());
    store.edit(map(&[("asked", "sent")]), false).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gateway.json");
    let mut orphaned = store.clone();
    orphaned.providers.clear();
    fs::write(&path, serde_json::to_vec(&orphaned).unwrap()).unwrap();
    assert!(Store::load(&path).unwrap().0.model_maps.is_empty());
    fs::write(&path, serde_json::to_vec(&store).unwrap()).unwrap();
    assert_eq!(
        Store::load(&path).unwrap().0.model_maps[&id]["asked"],
        "sent"
    );
    store.edit(Edit::DeleteProvider { id }, false).unwrap();
    assert!(store.model_maps.is_empty());
}

#[test]
fn only_the_claude_gateway_takes_a_model_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let codex = Gateway::new(dir.path().join("codex")).unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let refused = codex.edit(
        Edit::ModelMapProvider {
            id: "any".into(),
            mapping: Default::default(),
        },
        &codex.view().revision,
        &home,
    );
    assert_eq!(
        refused.err().map(|error| error.message).as_deref(),
        Some("此操作仅适用于 Claude Code")
    );
}

#[test]
fn a_provider_once_kept_for_searches_is_loaded_as_one_that_takes_them_over() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gateway.json");
    let mut value = serde_json::to_value(Store::default()).unwrap();
    value["providers"] = json!([
        {"id":"search","name":"Search","baseUrl":"https://one.example.invalid","token":"fixture","queued":true,"version":"one","searchOnly":true},
        {"id":"ordinary","name":"Ordinary","baseUrl":"https://two.example.invalid","token":"fixture","queued":true,"version":"two","searchOnly":false},
        {"id":"named","name":"Named","baseUrl":"https://three.example.invalid","token":"fixture","queued":true,"version":"three","searchOnly":true}
    ]);
    // What a provider already takes over is kept beside the search.
    value["delegates"] = json!({"named": {"auxiliary": true, "auxiliaryModel": "aux-fixture"}});
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let (store, revision) = Store::load(&path).unwrap();
    let saved = serde_json::to_value(&store).unwrap();
    // The request's own model and effort are used: none is named.
    assert_eq!(
        saved["delegates"],
        json!({
            "search": {"webSearch": true},
            "named": {"auxiliary": true, "auxiliaryModel": "aux-fixture", "webSearch": true}
        })
    );
    // The old field is not written again, and a second load changes nothing.
    assert!(saved["providers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|provider| provider.get("searchOnly").is_none()));
    store.persist(&path, &revision).unwrap();
    let (again, _) = Store::load(&path).unwrap();
    assert_eq!(serde_json::to_value(&again).unwrap(), saved);
}

#[test]
fn a_new_provider_that_names_nothing_takes_every_kind_its_client_hands_over() {
    let form = |op: &str, extra: serde_json::Value| -> Edit {
        let mut form = json!({
            "op": op, "baseUrl": "https://new.example.invalid/v1", "token": "fixture"
        });
        form.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::from_value(form).unwrap()
    };
    let taken = |client: ClientId, edit: Edit| {
        let dir = tempfile::tempdir().unwrap();
        let gateway = Gateway::new_client(dir.path().join("data"), client, None).unwrap();
        match gateway.with_new_provider_defaults(edit) {
            Edit::SaveProvider { delegate, .. } | Edit::SaveGrokProvider { delegate, .. } => (
                delegate.take_compaction,
                delegate.take_auxiliary,
                delegate.take_web_search,
                delegate.compaction_model,
                delegate.web_search_effort,
            ),
            _ => unreachable!(),
        }
    };
    let all = (Some(true), Some(true), Some(true), None, None);
    assert_eq!(taken(ClientId::Codex, form("saveProvider", json!({}))), all);
    assert_eq!(
        taken(ClientId::Claude, form("saveProvider", json!({}))),
        all
    );
    assert_eq!(
        taken(
            ClientId::Grok,
            form("saveGrokProvider", json!({"protocol": "responses"}))
        ),
        (None, None, Some(true), None, None)
    );
    // A form that names any of them is taken at its word.
    assert_eq!(
        taken(
            ClientId::Codex,
            form("saveProvider", json!({"takeAuxiliary": false}))
        ),
        (None, Some(false), None, None, None)
    );
    // So is the form of a provider that already exists.
    assert_eq!(
        taken(
            ClientId::Codex,
            form("saveProvider", json!({"id": "saved"}))
        ),
        (None, None, None, None, None)
    );
}

#[test]
fn a_claude_model_belongs_to_the_family_its_id_names() {
    use crate::gateway::model::model_family;
    for (model, family) in [
        ("claude-fable-5-2", Some("fable")),
        ("claude-fable-5-2[1m]", Some("fable")),
        ("claude-opus-4-1-20250805", Some("opus")),
        ("claude-opus-4-8", Some("opus")),
        ("claude-3-opus-20240229", Some("opus")),
        ("claude-sonnet-4-5-20250929", Some("sonnet")),
        ("claude-sonnet-4-5-20250929[1m]", Some("sonnet")),
        ("claude-3-5-sonnet-20241022", Some("sonnet")),
        ("claude-3-7-sonnet-latest", Some("sonnet")),
        ("claude-haiku-4-5-20251001", Some("haiku")),
        ("claude-3-5-haiku-20241022", Some("haiku")),
        (" Claude-Opus-4-1 ", Some("opus")),
        // Vendor-prefixed and vendor-suffixed IDs name the same model.
        ("anthropic/claude-opus-4-1", Some("opus")),
        ("us.anthropic.claude-3-5-haiku-20241022-v1:0", Some("haiku")),
        ("claude-sonnet-4-5@20250929", Some("sonnet")),
        // The aliases a Claude client sends as they are.
        ("opus", Some("opus")),
        ("opus[1m]", Some("opus")),
        ("opusplan", Some("opus")),
        ("sonnet", Some("sonnet")),
        ("sonnet[1m]", Some("sonnet")),
        ("haiku", Some("haiku")),
        ("fable", Some("fable")),
        // The family is a whole word of a Claude model ID.
        ("claude-opusx-4", None),
        ("claude-instant-1.2", None),
        ("claude-fixture", None),
        ("gpt-5-opus", None),
        ("notclaude-opus-4", None),
        ("deepseek-sonnet", None),
        ("default", None),
        ("", None),
    ] {
        assert_eq!(model_family(model), family, "{model}");
    }
}

#[test]
fn a_mapping_resolves_the_model_itself_before_its_family() {
    use crate::gateway::model::mapped_model;
    let mapping: BTreeMap<String, String> = [
        ("family:opus", "family-upstream"),
        ("claude-opus-4-8", "exact-upstream"),
        ("vendor-model", "vendor-upstream"),
    ]
    .into_iter()
    .map(|(from, to)| (from.to_owned(), to.to_owned()))
    .collect();
    for (model, upstream) in [
        ("claude-opus-4-8", Some("exact-upstream")),
        ("claude-opus-4-8[1m]", Some("family-upstream")),
        ("claude-opus-4-1-20250805", Some("family-upstream")),
        ("opusplan", Some("family-upstream")),
        ("vendor-model", Some("vendor-upstream")),
        ("claude-sonnet-4-5", None),
        ("gpt-opus", None),
    ] {
        assert_eq!(mapped_model(&mapping, model), upstream, "{model}");
    }
    assert_eq!(mapped_model(&BTreeMap::new(), "claude-opus-4-8"), None);
}

#[test]
fn a_model_mapping_takes_the_four_family_keys_and_no_other() {
    use crate::gateway::model::{model_map, MODEL_FAMILIES};
    for family in MODEL_FAMILIES {
        let key = format!("family:{family}");
        assert_eq!(
            model_map([(format!(" {key} "), "upstream".to_owned())].into()).unwrap(),
            [(key, "upstream".to_owned())].into()
        );
    }
    for key in ["family:", "family:claude", "family:Opus", "family:opus-4"] {
        assert!(
            model_map([(key.to_owned(), "upstream".to_owned())].into()).is_err(),
            "{key}"
        );
    }
}
