use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const ACCOUNT_TOKEN: &str = "fixture-account-access-token";
const ACCOUNT: &str =
    r#"{"success":true,"data":{"quota":750000,"used_quota":250000,"request_count":12}}"#;
const STATUS: &str =
    r#"{"success":true,"data":{"quota_per_unit":500000,"quota_display_type":"USD"}}"#;
const KEY: &str = r#"{"code":true,"data":{"object":"token_usage","unlimited_quota":true,"total_available":100000000,"total_used":100000}}"#;

fn save_account(g: &Gateway, t: &tempfile::TempDir, token: Option<&str>, base: Option<&str>) {
    let p = &g.view().providers[0];
    update(
        g,
        t,
        Edit::SaveProvider {
            search_only: None,
            end_on_message_stop: None,
            filter_count_tokens: None,
            buffer_complete_response: None,
            endless_mode: None,
            upstream_retry_seconds: None,
            normal_retry_seconds: None,
            id: Some(p.id.clone()),
            base_url: base.unwrap_or(&p.base_url).into(),
            token: String::new(),
            quota_access_token: token.map(str::to_owned),
            name: None,
        },
    );
}

#[tokio::test]
async fn account_credentials_do_not_enter_model_queries_or_invalidate_their_cache() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    let port = server(move |request| {
        counted.fetch_add(1, Ordering::SeqCst);
        async move {
            assert_eq!(request.uri().path(), "/v1/models");
            assert_eq!(
                request.headers().get("authorization").unwrap(),
                "Bearer upstream-fixture-token"
            );
            Response::new(full(r#"{"data":[{"id":"fixture-model"}]}"#))
        }
    })
    .await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
    save_account(&g, &t, Some(ACCOUNT_TOKEN), None);
    let id = g.view().providers[0].id.clone();
    let models = g.list_models(&id, true).await.unwrap();
    assert_eq!(models.models, ["fixture-model"]);
    save_account(&g, &t, Some("fixture-replacement-account-token"), None);
    let cached = g.list_models(&id, false).await.unwrap();
    assert!(!cached.stale);
    assert_eq!(cached.models, models.models);
    assert_eq!(cached.version, models.version);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn account_balance_uses_separate_credentials_and_keeps_key_quota_secondary() {
    for unlimited in [false, true] {
        let port = server(move |req| async move {
            let auth = req.headers().get("authorization").and_then(|h| h.to_str().ok());
            let body = match req.uri().path() {
                "/site/api/user/self" => {
                    assert_eq!(auth, Some("Bearer fixture-account-access-token"));
                    ACCOUNT.to_owned()
                }
                "/site/api/status" => { assert_eq!(auth, None); STATUS.to_owned() }
                "/site/api/usage/token/" => {
                    assert_eq!(auth, Some("Bearer upstream-fixture-token"));
                    serde_json::json!({"code":true,"data":{"object":"token_usage","unlimited_quota":unlimited,"total_available":100000,"total_used":50000,"total_granted":150000}}).to_string()
                }
                _ => panic!("unexpected query endpoint"),
            };
            Response::new(full(body))
        }).await;
        let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}/site/v1")]).await;
        save_account(&g, &t, Some(ACCOUNT_TOKEN), None);
        let q = g
            .query_quota(&g.view().providers[0].id, true)
            .await
            .unwrap();
        assert_eq!(q.state, "ok");
        assert_eq!(q.plans.len(), 2);
        assert_eq!(q.plans[0].name, "账户余额");
        assert_eq!(q.plans[0].remaining, Some(1.5));
        assert_eq!(q.plans[0].used, Some(0.5));
        assert_eq!(q.plans[0].unit, "USD");
        assert!(!q.plans[0].unlimited);
        assert_eq!(q.plans[1].name, "Key 额度");
        assert_eq!(q.plans[1].unlimited, unlimited);
        assert_eq!(q.total_usage.unwrap().requests, Some(12.));
    }
}

#[tokio::test]
async fn account_balance_remains_successful_when_optional_key_query_fails() {
    for status in [401, 403, 429, 500] {
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        let port = server(move |req| {
            counted.fetch_add(1, Ordering::SeqCst);
            async move {
                match req.uri().path() {
                    "/api/user/self" => Response::new(full(ACCOUNT)),
                    "/api/status" => Response::new(full(STATUS)),
                    "/api/usage/token/" => Response::builder()
                        .status(status)
                        .header("retry-after", "60")
                        .body(full("unavailable"))
                        .unwrap(),
                    _ => panic!("unexpected query endpoint"),
                }
            }
        })
        .await;
        let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
        save_account(&g, &t, Some(ACCOUNT_TOKEN), None);
        let id = g.view().providers[0].id.clone();
        let q = g.query_quota(&id, true).await.unwrap();
        assert_eq!(q.state, "ok");
        assert!(!q.stale);
        assert_eq!(q.plans[0].remaining, Some(1.5));
        assert_eq!(q.plans[0].unit, "USD");
        assert!(q.error.unwrap().contains("Key 配额查询失败"));
        if status == 429 {
            assert!(q.retry_at.is_some());
            g.query_quota(&id, true).await.unwrap();
            assert_eq!(hits.load(Ordering::SeqCst), 3);
        }
    }
}

#[tokio::test]
async fn account_failures_preserve_old_balance_and_honor_backoff() {
    for status in [401, 403, 429, 503] {
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        let port = server(move |req| {
            let counted = counted.clone();
            async move {
                match req.uri().path() {
                    "/api/user/self" if counted.fetch_add(1, Ordering::SeqCst) > 0 => {
                        Response::builder()
                            .status(status)
                            .header("retry-after", "60")
                            .body(full(format!("{{\"message\":\"{ACCOUNT_TOKEN}\"}}")))
                            .unwrap()
                    }
                    "/api/user/self" => Response::new(full(ACCOUNT)),
                    "/api/status" => Response::new(full(STATUS)),
                    "/api/usage/token/" => Response::new(full(KEY)),
                    _ => panic!("unexpected query endpoint"),
                }
            }
        })
        .await;
        let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
        save_account(&g, &t, Some(ACCOUNT_TOKEN), None);
        let id = g.view().providers[0].id.clone();
        let first = g.query_quota(&id, true).await.unwrap();
        let failed = g.query_quota(&id, true).await.unwrap();
        assert_eq!(failed.state, "error");
        assert!(failed.stale);
        assert_eq!(failed.success_at, first.success_at);
        assert_eq!(failed.plans[0].remaining, Some(1.5));
        assert!(!serde_json::to_string(&failed)
            .unwrap()
            .contains(ACCOUNT_TOKEN));
        assert!(failed.error.as_ref().unwrap().contains(match status {
            401 => "认证",
            403 => "权限",
            429 => "限流",
            _ => "不可用",
        }));
        if status == 429 {
            g.query_quota(&id, true).await.unwrap();
            assert_eq!(hits.load(Ordering::SeqCst), 2);
        }
    }
}

#[tokio::test]
async fn account_network_failure_keeps_the_last_balance() {
    let port = server(|req| async move {
        Response::new(full(match req.uri().path() {
            "/api/user/self" => ACCOUNT,
            "/api/status" => STATUS,
            "/api/usage/token/" => KEY,
            _ => panic!("unexpected query endpoint"),
        }))
    })
    .await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
    save_account(&g, &t, Some(ACCOUNT_TOKEN), None);
    let id = g.view().providers[0].id.clone();
    g.query_quota(&id, true).await.unwrap();
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);
    let mut input = g.query_input(&id).unwrap();
    input.base = format!("http://127.0.0.1:{closed_port}/v1");
    let failed = g.0.quota.query(input, true).await.unwrap();
    assert_eq!(failed.state, "error");
    assert!(failed.stale);
    assert_eq!(failed.plans[0].remaining, Some(1.5));
    assert!(failed.error.is_some());
}

#[tokio::test]
async fn replacing_account_credentials_rejects_an_old_inflight_balance() {
    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let (arrived, finish) = (entered.clone(), release.clone());
    let port = server(move |req| {
        let (arrived, finish) = (arrived.clone(), finish.clone());
        async move {
            let body = match req.uri().path() {
                "/api/user/self" => {
                    arrived.add_permits(1);
                    finish.acquire().await.unwrap().forget();
                    ACCOUNT
                }
                "/api/status" => STATUS,
                "/api/usage/token/" => KEY,
                _ => panic!("unexpected query endpoint"),
            };
            Response::new(full(body))
        }
    })
    .await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
    save_account(&g, &t, Some(ACCOUNT_TOKEN), None);
    let id = g.view().providers[0].id.clone();
    let querying = g.clone();
    let flight = tokio::spawn(async move { querying.query_quota(&id, true).await });
    tokio::time::timeout(Duration::from_secs(3), entered.acquire())
        .await
        .unwrap()
        .unwrap()
        .forget();
    save_account(&g, &t, Some("fixture-account-replacement"), None);
    release.add_permits(1);
    let failure = flight.await.unwrap().err().unwrap();
    assert_eq!(failure.code, code::STALE);
    assert!(g.view().providers[0].quota.is_none());
}

#[tokio::test]
async fn account_credentials_persist_clear_and_invalidate_cache_without_leaking_to_views() {
    let port = server(|req| async move {
        Response::new(full(match req.uri().path() {
            "/api/user/self" => ACCOUNT,
            "/api/status" => STATUS,
            "/api/usage/token/" => KEY,
            _ => panic!("unexpected query endpoint"),
        }))
    })
    .await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
    let id = g.view().providers[0].id.clone();
    assert!(!g.view().providers[0].has_quota_access_token);
    let initial_version = g.view().providers[0].quota_version.clone();
    save_account(&g, &t, Some(ACCOUNT_TOKEN), None);
    let version = g.view().providers[0].quota_version.clone();
    assert_ne!(version, initial_version);
    g.query_quota(&id, true).await.unwrap();
    save_account(&g, &t, None, None);
    assert_eq!(g.view().providers[0].quota_version, version);
    assert!(g.view().providers[0].quota.is_some());
    let (store, _) = Store::load(&g.0.data.join("gateway.json")).unwrap();
    assert_eq!(store.quota_access_tokens[&id], ACCOUNT_TOKEN);
    assert!(!serde_json::to_string(&g.view())
        .unwrap()
        .contains(ACCOUNT_TOKEN));
    save_account(&g, &t, Some("fixture-account-replacement"), None);
    assert_ne!(g.view().providers[0].quota_version, version);
    assert!(g.view().providers[0].quota.is_none());
    save_account(&g, &t, Some(""), None);
    assert!(!g.view().providers[0].has_quota_access_token);
    let (store, _) = Store::load(&g.0.data.join("gateway.json")).unwrap();
    assert!(store.quota_access_tokens.is_empty());
    save_account(&g, &t, Some(ACCOUNT_TOKEN), None);
    save_account(&g, &t, None, Some("https://other.example.invalid/v1"));
    assert!(!g.view().providers[0].has_quota_access_token);
    save_account(&g, &t, Some(ACCOUNT_TOKEN), None);
    update(
        &g,
        &t,
        Edit::EndpointsProvider {
            id: id.clone(),
            base_urls: vec!["https://secondary.example.invalid/v1".into()],
        },
    );
    update(
        &g,
        &t,
        Edit::SwapEndpoint {
            id: id.clone(),
            base_url: "https://secondary.example.invalid/v1".into(),
        },
    );
    assert!(!g.view().providers[0].has_quota_access_token);
    save_account(&g, &t, Some(ACCOUNT_TOKEN), None);
    update(&g, &t, Edit::DeleteProvider { id });
    let (store, _) = Store::load(&g.0.data.join("gateway.json")).unwrap();
    assert!(store.quota_access_tokens.is_empty());
}

#[tokio::test]
async fn codex_catalog_is_generated_from_the_union_and_cached_per_provider() {
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    let port = server(move |request| {
        let counted = counted.clone();
        async move {
            assert_eq!(request.uri().path(), "/v1/models");
            assert_eq!(
                request.headers().get("authorization").unwrap(),
                "Bearer upstream-fixture-token"
            );
            match request.uri().query() {
                None => Response::new(full(
                    r#"{"data":[{"id":"fixture-model"},{"id":"fixture-extra"}]}"#,
                )),
                Some("client_version=1") => {
                    counted.fetch_add(1, Ordering::SeqCst);
                    Response::new(full(
                        r#"{"models":[{"slug":"fixture-model","display_name":"Fixture","base_instructions":"fixture instructions","supported_reasoning_levels":[{"effort":"medium","description":"fixture"}]},{"slug":"fixture-upstream-only","display_name":"Other","supported_reasoning_levels":[]}]}"#,
                    ))
                }
                Some("client_version=2") => Response::new(full(
                    r#"{"object":"list","data":[{"id":"fixture-model"}]}"#,
                )),
                _ => Response::builder().status(502).body(full("{}")).unwrap(),
            }
        }
    })
    .await;
    let (_t, g) = fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
    let id = g.view().providers[0].id.clone();
    g.list_models(&id, true).await.unwrap();
    assert_eq!(
        g.model_union(None).unwrap(),
        ["fixture-extra", "fixture-model"]
    );
    let ask = |query: &'static str| {
        let g = g.clone();
        let id = id.clone();
        async move {
            catalog::codex_catalog(
                &g,
                &[id],
                &format!("/v1/models{query}").parse().unwrap(),
                &hyper::HeaderMap::new(),
            )
            .await
        }
    };
    for _ in 0..2 {
        let response = ask("?client_version=1").await.unwrap();
        assert_eq!(response.headers()["x-models-etag"].len(), 64);
        assert_eq!(response.headers()["etag"].len(), 66);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let models = body["models"].as_array().unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0]["slug"], "fixture-extra");
        assert_eq!(models[0]["base_instructions"], "fixture instructions");
        assert_eq!(models[1]["display_name"], "Fixture");
    }
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // A provider without a Codex catalog still yields one, from Codex's defaults.
    let response = ask("?client_version=2").await.unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let models = body["models"].as_array().unwrap();
    assert_eq!(
        (&models[0]["slug"], &models[1]["slug"]),
        (&"fixture-extra".into(), &"fixture-model".into())
    );
    assert_eq!(models[1]["display_name"], "fixture-model");
    assert!(models[1]["base_instructions"].as_str().unwrap().len() > 1000);
    // An upstream failure and a plain list are forwarded.
    for query in ["?client_version=3", ""] {
        assert!(ask(query).await.is_none(), "{query}");
    }
    // The file Codex loads at startup is the same catalog; with the provider
    // out of reach, the entries written last stand in for its own.
    let previous = br#"{"models":[{"slug":"fixture-model","display_name":"Kept","base_instructions":"kept","supported_reasoning_levels":[{"effort":"medium","description":"kept"}]}]}"#;
    let written = catalog::codex_file(&g, Some(previous)).await.unwrap();
    let written: serde_json::Value = serde_json::from_slice(&written).unwrap();
    let models = written["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0]["slug"], "fixture-extra");
    assert_eq!(models[0]["base_instructions"], "kept");
    assert_eq!(models[1]["display_name"], "Kept");
}

#[tokio::test]
async fn codex_model_catalog_switch_follows_the_gateway_into_and_out_of_config() {
    let port =
        server(|_| async { Response::new(full(r#"{"data":[{"id":"fixture-model"}]}"#)) }).await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
    let id = g.view().providers[0].id.clone();
    g.list_models(&id, true).await.unwrap();
    let config = t.path().join("config.toml");
    let file = t.path().join("vela-models.json");
    let named = || {
        let text = std::fs::read_to_string(&config).unwrap();
        toml::from_str::<toml::Value>(&text)
            .unwrap()
            .get("model_catalog_json")
            .and_then(|v| v.as_str().map(str::to_owned))
    };
    let before = std::fs::read_to_string(&config).unwrap();
    assert!(!g.view().model_catalog);
    update(&g, &t, Edit::ModelCatalog { enabled: true });
    assert!(g.view().model_catalog);
    // Nothing is published for a stopped gateway.
    g.sync_catalog().await.unwrap();
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
    start(&g, &t).await;
    g.sync_catalog().await.unwrap();
    assert_eq!(named().as_deref(), file.to_str());
    let catalog: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(catalog["models"][0]["slug"], "fixture-model");
    assert_eq!(catalog["models"].as_array().unwrap().len(), 1);
    // The key leaves with the gateway; the file waits for the next start.
    g.stop().await.unwrap();
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
    assert!(file.exists());
    start(&g, &t).await;
    g.sync_catalog().await.unwrap();
    assert_eq!(named().as_deref(), file.to_str());
    // Switching off while running withdraws both.
    update(&g, &t, Edit::ModelCatalog { enabled: false });
    g.sync_catalog().await.unwrap();
    assert_eq!(named(), None);
    assert!(!file.exists());
    g.stop().await.unwrap();
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
}

#[tokio::test]
async fn codex_picker_lists_the_official_catalog_verbatim_or_falls_back_to_the_queue() {
    let port =
        server(|_| async { Response::new(full(r#"{"data":[{"id":"fixture-model"}]}"#)) }).await;
    let (t, g) = fixture(vec![format!("http://127.0.0.1:{port}/v1")]).await;
    let id = g.view().providers[0].id.clone();
    g.list_models(&id, true).await.unwrap();
    let file = t.path().join("vela-models.json");
    // No download is reachable yet.
    *g.0.official_source.lock() = Some("http://127.0.0.1:1/models.json".into());
    update(&g, &t, Edit::ModelCatalog { enabled: true });
    update(&g, &t, Edit::ModelCatalogSource { official: true });
    assert!(g.view().model_catalog_official);
    start(&g, &t).await;
    // The queue's models stand in, and the reason is reported.
    let error = g.sync_catalog().await.unwrap_err();
    assert!(
        error.message.ends_with("已改用队列模型"),
        "{}",
        error.message
    );
    let catalog: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(catalog["models"][0]["slug"], "fixture-model");
    // A download asked for by hand fails aloud; either way the reason is shown.
    assert!(!g
        .refresh_official_catalog(false)
        .await
        .is_ok_and(|changed| changed));
    assert!(g.refresh_official_catalog(true).await.is_err());
    assert!(codex_official::cached(&g.0.data).is_none());
    let view = g.view();
    assert!(view.model_catalog_official_status.is_none());
    assert!(view.model_catalog_official_error.is_some());
    // Once downloaded, the file is OpenAI's catalog byte for byte.
    let official = br#"{"models":[{"slug":"official-a","supported_reasoning_levels":[{"effort":"low"}]},{"slug":"official-b","visibility":"hide"}]}"#;
    let source = server(move |_| async move { Response::new(full(&official[..])) }).await;
    *g.0.official_source.lock() = Some(format!("http://127.0.0.1:{source}/models.json"));
    assert!(g.refresh_official_catalog(false).await.unwrap());
    assert_eq!(codex_official::cached(&g.0.data).unwrap(), official);
    let view = g.view();
    let held = view.model_catalog_official_status.unwrap();
    assert_eq!((held.models, held.version.len()), (2, 8));
    assert!(view.model_catalog_official_error.is_none());
    // The same content by hand changes nothing; a dead source keeps what is held.
    assert!(!g.refresh_official_catalog(true).await.unwrap());
    *g.0.official_source.lock() = Some("http://127.0.0.1:1/models.json".into());
    assert!(g.refresh_official_catalog(true).await.is_err());
    let view = g.view();
    assert_eq!(
        view.model_catalog_official_status.unwrap().version,
        held.version
    );
    assert!(view.model_catalog_official_error.is_some());
    assert_eq!(codex_official::cached(&g.0.data).unwrap(), official);
    g.sync_catalog().await.unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), official);
    // Back on the queue's models, and never for another client.
    update(&g, &t, Edit::ModelCatalogSource { official: false });
    assert!(g.refresh_official_catalog(true).await.is_err());
    g.sync_catalog().await.unwrap();
    let catalog: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(catalog["models"][0]["slug"], "fixture-model");
    g.stop().await.unwrap();
    let claude = g.companion(t.path().join("claude-data")).unwrap();
    let revision = claude.view().revision;
    assert!(claude
        .edit(
            Edit::ModelCatalogSource { official: true },
            &revision,
            t.path()
        )
        .is_err());
}
