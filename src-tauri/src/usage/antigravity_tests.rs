use super::super::store::Store;
use super::*;
use http_body_util::{BodyExt, Full};
use hyper::{body::Bytes, service::service_fn, Response};
use parking_lot::Mutex;
use std::sync::Arc;

fn step() -> Value {
    json!({"metadata":{"executionId":"exec","startedAt":"2026-01-01T00:00:00Z","finishedGeneratingAt":"2026-01-01T00:00:02Z","completedAt":"2026-01-01T00:00:30Z"}})
}
fn entry(id: &str, index: usize) -> Value {
    let usage = json!({"responseId":format!("response-{id}"),"model":"MODEL_PLACEHOLDER_FIXTURE","apiProvider":"API_PROVIDER_FIXTURE","inputTokens":"100","cacheReadTokens":"40","outputTokens":"20","thinkingOutputTokens":"12","responseOutputTokens":"8"});
    json!({"executionId":"exec","stepIndices":[index],"chatModel":{"model":"MODEL_PLACEHOLDER_FIXTURE","responseModel":"fixture-response","modelDisplayName":"Fixture Model (High)","customMetadata":{"request_id":id},"usage":usage,"retryInfos":[{"usage":usage}],"timeToFirstToken":"0.25s","streamingDuration":"1.75s"}})
}
fn parsed(v: &Value) -> Record {
    record(
        "fixture",
        v,
        &Steps::from([step()].as_slice()),
        0,
        &Learned::default(),
    )
    .unwrap()
    .unwrap()
}

#[test]
fn request_timings_override_step_clocks_and_small_generation_is_not_a_speed() {
    for native in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        if native {
            while store.compact_storage_batch().unwrap() {}
        }
        let mut memory = Totals::default();
        for (index, generation) in [Some(0), Some(50), Some(51), None].into_iter().enumerate() {
            let mut v = entry(&format!("timing-{index}"), 0);
            v["chatModel"]["timeToFirstToken"] = json!("1.999s");
            if let Some(ms) = generation {
                v["chatModel"]["streamingDuration"] = json!(ms as f64 / 1000.0);
            } else {
                v["chatModel"]
                    .as_object_mut()
                    .unwrap()
                    .remove("streamingDuration");
            }
            let r = parsed(&v);
            let a = &r.attempts[0];
            assert_eq!(a.first_token_ms, Some(1999));
            assert_eq!(
                a.measured_duration_ms(),
                Some(generation.map_or(2000, |ms| 1999 + ms))
            );
            assert_eq!(a.measured_generation_ms(), generation.filter(|ms| *ms > 50));
            memory.add_record(&r);
            store.write_batch(&[r], None).unwrap();
        }
        if native {
            while store.compact_storage_batch().unwrap() {}
        }
        let total = store.dashboard(&Filter::default()).unwrap().totals;
        assert_eq!(total.generation_ms, 51);
        assert_eq!(total.generation_ms, memory.generation_ms);
        assert_eq!(total.duration_ms, memory.duration_ms);
        assert_eq!(total.first_token_sum_ms, 1999 * 4);
    }
}

#[test]
fn placeholder_mirror_uses_real_model_and_final_timings_once() {
    let r = parsed(&entry("one", 0));
    let a = &r.attempts[0];
    assert_eq!(r.attempts.len(), 1);
    assert_eq!(a.requested_model, None);
    assert_eq!(a.response_model.as_deref(), Some("fixture-response"));
    assert_eq!(a.model_display_name.as_deref(), Some("Fixture Model"));
    assert_eq!(a.api_provider.as_deref(), Some("API_PROVIDER_FIXTURE"));
    assert_eq!(a.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(
        (a.first_token_ms, a.generation_ms, a.measured_duration_ms()),
        (Some(250), Some(1750), Some(2000))
    );
    assert_eq!(r.tokens().total(), Some(160));
    assert_eq!(r.known_success(), Some(true));
    assert_eq!(r.finished_at, Some(r.started_at + 2000));
    assert_eq!(a.tokens.cache_write, None);
}

#[test]
fn zero_step_clock_uses_request_timings_and_unknown_suffixes_are_preserved() {
    let mut s = step();
    s["metadata"]["finishedGeneratingAt"] = s["metadata"]["startedAt"].clone();
    let steps = Steps::from([s].as_slice());
    let mut v = entry("one", 0);
    v["chatModel"]["responseModel"] = json!("gemini-4-argon-f");
    let r = record("fixture", &v, &steps, 0, &Learned::default())
        .unwrap()
        .unwrap();
    assert_eq!(r.attempts[0].measured_duration_ms(), Some(2000));
    assert_eq!(r.finished_at, Some(r.started_at + 2000));
    assert_eq!(
        r.attempts[0].response_model.as_deref(),
        Some("gemini-4-argon-f")
    );
    v["chatModel"]["modelDisplayName"] = json!("Fixture (Preview) (High)");
    assert_eq!(
        parsed(&v).attempts[0].model_display_name.as_deref(),
        Some("Fixture (Preview)")
    );
    v["chatModel"]["modelDisplayName"] = json!("Fixture (Preview)");
    let r = parsed(&v);
    assert_eq!(
        r.attempts[0].model_display_name.as_deref(),
        Some("Fixture (Preview)")
    );
    assert_eq!(r.attempts[0].reasoning_effort, None);
    for key in ["timeToFirstToken", "streamingDuration"] {
        v["chatModel"].as_object_mut().unwrap().remove(key);
    }
    let r = record("fixture", &v, &steps, 0, &Learned::default())
        .unwrap()
        .unwrap();
    assert_eq!(r.attempts[0].measured_duration_ms(), None);
    assert!(r.completed);
}

#[test]
fn historical_repair_preserves_prices_and_identity_in_both_storage_formats() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let mut old = parsed(&entry("one", 0));
    old.attempts[0].model_display_name = Some("Fixture Model (High)".into());
    old.attempts[0].reasoning_effort = None;
    old.attempts[0].duration_ms = 0;
    old.finished_at = Some(old.started_at);
    let price = super::super::pricing::Quote {
        model: "fixture-response".into(),
        data: Some(json!({"input_cost_per_token":1,"output_cost_per_token":2,"cache_read_input_token_cost":0})),
        version: "historical".into(),
        source: "fixed".into(),
        multiplier: "1".into(),
    }
    .calculate(&old.attempts[0].tokens, None)
    .unwrap();
    old.attempts[0].price = Some(price.clone());
    assert!(normalize_record(&mut old));
    store.write_batch(&[old.clone()], None).unwrap();
    let repaired = store.detail(&old.id).unwrap();
    assert_eq!(repaired.attempts[0].id, old.attempts[0].id);
    assert_eq!(
        repaired.attempts[0].reasoning_effort.as_deref(),
        Some("high")
    );
    assert_eq!(repaired.attempts[0].measured_duration_ms(), Some(2000));
    assert_eq!(repaired.finished_at, Some(old.started_at + 2000));
    assert_eq!(
        serde_json::to_value(&repaired.attempts[0].price).unwrap(),
        serde_json::to_value(Some(&price)).unwrap()
    );
    assert_eq!(
        store.logs(&super::super::Filter::default()).unwrap().total,
        1
    );
}

#[test]
fn retries_keep_individual_models_clocks_and_top_level_error() {
    let mut v = entry("one", 0);
    v["chatModel"]["retryInfos"].as_array_mut().unwrap().insert(0,json!({"traceId":"retry-one","error":"fixture unavailable","startedAt":"2026-01-01T00:00:00Z","completedAt":"2026-01-01T00:00:01Z","usage":{"model":"fixture-earlier","responseId":"early","inputTokens":"2","outputTokens":"0"}}));
    let r = parsed(&v);
    assert_eq!(r.attempts.len(), 2);
    assert_eq!(
        r.attempts[0].response_model.as_deref(),
        Some("fixture-earlier")
    );
    assert_eq!(r.attempts[0].model_display_name, None);
    assert_eq!(r.attempts[0].measured_duration_ms(), Some(1000));
    assert_eq!(r.attempts[0].first_token_ms, None);
    assert_eq!(
        r.attempts[1].availability["attempt_start"],
        "upstream_unreported"
    );
    v["chatModel"]["error"] = json!("fixture rejected");
    let r = parsed(&v);
    assert_eq!(r.attempts.len(), 2);
    assert_eq!(r.known_success(), Some(false));
    assert_eq!(r.final_attempt().unwrap().outcome, "failed");
    v["chatModel"].as_object_mut().unwrap().remove("usage");
    assert_eq!(parsed(&v).known_success(), Some(false));
    v["chatModel"]["error"] = json!({"code":429,"message":"fixture"});
    assert_eq!(
        parsed(&v).final_attempt().unwrap().error_code.as_deref(),
        Some("429")
    );
}

#[test]
fn final_mirror_preserves_retry_fields_and_running_requests_have_an_identity() {
    let mut v = entry("one", 0);
    v["chatModel"]["retryInfos"][0]["usage"]["model"] = json!("fixture-earlier");
    v["chatModel"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("inputTokens");
    let r = parsed(&v);
    assert_eq!(r.attempts.len(), 1);
    assert_eq!(r.attempts[0].tokens.input, Some(100));
    assert_eq!(
        r.attempts[0].response_model.as_deref(),
        Some("fixture-earlier")
    );
    v["chatModel"] =
        json!({"customMetadata":{"request_id":"one"},"responseModel":"fixture-response"});
    let steps = Steps::from([json!({"metadata":{"startedAt":"2026-01-01T00:00:00Z"}})].as_slice());
    let pending = record("fixture", &v, &steps, 0, &Learned::default())
        .unwrap()
        .unwrap();
    assert_eq!(pending.id, r.id);
    assert_eq!(pending.final_attempt().unwrap().outcome, "in_progress");
    assert_eq!(pending.final_attempt().unwrap().tokens.output, None);
}

#[test]
fn reliable_clocks_and_explicit_generation_remain_distinct_in_summary() {
    let mut v = entry("one", 0);
    v["chatModel"]
        .as_object_mut()
        .unwrap()
        .remove("timeToFirstToken");
    let mut r = parsed(&v);
    assert_eq!(r.attempts[0].measured_duration_ms(), Some(2000));
    assert_eq!(r.attempts[0].measured_generation_ms(), Some(1750));
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    price(
        &mut r,
        &Pricing::new(dir.path()).unwrap(),
        &Settings::default(),
    );
    store.write_batch(&[r.clone()], None).unwrap();
    let total = store.dashboard(&Filter::default()).unwrap().totals;
    let mut memory = Totals::default();
    memory.add_record(&r);
    assert_eq!(
        (
            total.duration_ms,
            total.generation_ms,
            total.measured_outputs,
            total.first_token_samples,
            total.sessions
        ),
        (2000, 1750, 20, 0, 1)
    );
    assert_eq!(total.generation_ms, memory.generation_ms);
}

#[test]
fn tool_completion_and_partial_timing_do_not_invent_success_or_duration() {
    let mut v = entry("one", 0);
    v["chatModel"]
        .as_object_mut()
        .unwrap()
        .remove("streamingDuration");
    let mut tool = step();
    tool["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("finishedGeneratingAt");
    let r = record(
        "fixture",
        &v,
        &Steps::from([tool].as_slice()),
        0,
        &Learned::default(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(r.finished_at, None);
    assert_eq!(r.known_success(), None);
    assert_eq!(r.attempts[0].outcome, "in_progress");
    assert_eq!(r.attempts[0].measured_duration_ms(), None);
    assert_eq!(r.attempts[0].first_token_ms, Some(250));
    assert_eq!(duration(&json!("0s")), Some(0));
    assert_eq!(duration(&json!(0.25)), Some(250));
    for invalid in [json!("NaNs"), json!("-1s"), json!("infs"), json!("1e30s")] {
        assert_eq!(duration(&invalid), None);
    }
    assert_eq!(label(Some("person@example.invalid")), None);
}

#[test]
fn fields_roundtrip_metrics_and_price_snapshot_survive_history_repair() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();
    let view = pricing.view();
    let mut config = view.config;
    config.auto_update = false;
    config.fixed.insert("fixture-response".into(),json!({"input_cost_per_token":"0.1","output_cost_per_token":"0.2","cache_read_input_token_cost":"0.01"}));
    pricing.configure(config, &view.revision).unwrap();
    let mut r = parsed(&entry("one", 0));
    price(&mut r, &pricing, &Settings::default());
    store.write_batch(&[r.clone()], None).unwrap();
    let saved = store.detail(&r.id).unwrap();
    assert_eq!(saved.attempts[0].generation_ms, Some(1750));
    assert_eq!(
        saved.attempts[0].model_display_name,
        r.attempts[0].model_display_name
    );
    assert_eq!(saved.attempts[0].api_provider, r.attempts[0].api_provider);
    let before = store.dashboard(&Filter::default()).unwrap().totals;
    assert_eq!(
        (
            before.success,
            before.status_known,
            before.first_token_samples,
            before.first_token_sum_ms
        ),
        (1, 1, 1, 250)
    );
    assert_eq!(before.cost, "14.4");
    r.attempts[0].price.as_mut().unwrap().cost = "999".into();
    store.write_batch(&[r.clone()], None).unwrap();
    assert_eq!(
        store.detail(&r.id).unwrap().attempts[0]
            .price
            .as_ref()
            .unwrap()
            .cost,
        "14.4"
    );
    assert_eq!(
        store.dashboard(&Filter::default()).unwrap().totals.requests,
        1
    );
    store.archive_batch(&[r.id.clone()]).unwrap();
    store.write_batch(&[r], None).unwrap();
    assert_eq!(
        store.dashboard(&Filter::default()).unwrap().totals.requests,
        1
    );
    assert_eq!(store.logs(&Filter::default()).unwrap().total, 0);
}

struct Fixture {
    api: Api,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    changed: Arc<Mutex<String>>,
    entries: Arc<Mutex<Vec<Value>>>,
    /// The reply to `GetAvailableModels`; `{}` carries no list.
    models: Arc<Mutex<Value>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn fixture(broken: bool) -> Fixture {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let changed = Arc::new(Mutex::new("first".to_string()));
    let entries = Arc::new(Mutex::new(vec![entry("first", 0), entry("second", 1)]));
    let models = Arc::new(Mutex::new(json!({})));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (history, version, metadata) = (calls.clone(), changed.clone(), entries.clone());
    let listed = models.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let (history, version, metadata) = (history.clone(), version.clone(), metadata.clone());
            let listed = listed.clone();
            tokio::spawn(async move {
                let service = service_fn(move |r: hyper::Request<hyper::body::Incoming>| {
                    let (history, version, metadata) =
                        (history.clone(), version.clone(), metadata.clone());
                    let listed = listed.clone();
                    async move {
                        let method = r.uri().path().rsplit('/').next().unwrap().to_owned();
                        let body: Value = serde_json::from_slice(
                            &r.into_body().collect().await.unwrap().to_bytes(),
                        )
                        .unwrap();
                        history.lock().push((method.clone(), body.clone()));
                        // `a-foreign` belongs to another instance and
                        // `a-empty` is known here without any usage.
                        let foreign = body["cascadeId"] == "a-foreign";
                        let value = if body["cascadeId"] == "a-broken" {
                            json!({"unexpected":"fixture"})
                        } else if foreign {
                            json!({"code":"not_found"})
                        } else if body["cascadeId"] == "a-empty" {
                            json!({})
                        } else {
                            match method.as_str() {
                                "GetAvailableModels" => listed.lock().clone(),
                                "GetUserStatus" => {
                                    json!({"userStatus":{"email":"fixture@example.invalid"}})
                                }
                                "RetrieveUserQuotaSummary" => {
                                    json!({"response":{"groups":[{"buckets":[{"bucketId":"gemini-5h","window":"5h","resetTime":"2026-11-05T18:00:00Z"}]}]}})
                                }
                                "GetAllCascadeTrajectories" => {
                                    let mut summaries = json!({"z-good":{"lastModifiedTime":*version.lock(),"stepCount":2}});
                                    if broken {
                                        summaries["a-broken"] =
                                            json!({"lastModifiedTime":"first","stepCount":1});
                                    }
                                    json!({"trajectorySummaries":summaries})
                                }
                                "GetCascadeTrajectoryGeneratorMetadata" => {
                                    let offset =
                                        body["generatorMetadataOffset"].as_u64().unwrap_or(0)
                                            as usize;
                                    let mut entries = metadata.lock().clone();
                                    if body["cascadeId"] != "z-good" {
                                        for (i, entry) in entries.iter_mut().enumerate() {
                                            let response = json!(format!(
                                                "{}-response-{i}",
                                                body["cascadeId"].as_str().unwrap()
                                            ));
                                            entry["chatModel"]["usage"]["responseId"] =
                                                response.clone();
                                            entry["chatModel"]["retryInfos"][0]["usage"]
                                                ["responseId"] = response;
                                            entry["chatModel"]["customMetadata"]["request_id"] =
                                                json!(format!(
                                                    "{}-{i}",
                                                    body["cascadeId"].as_str().unwrap()
                                                ));
                                        }
                                    }
                                    json!({"generatorMetadata":&entries[offset.min(entries.len())..(offset + 1).min(entries.len())]})
                                }
                                "GetCascadeTrajectorySteps" => {
                                    let offset = body["stepOffset"].as_u64().unwrap_or(0) as usize;
                                    if body["trajectoryVerbosity"] == 2 {
                                        // One page per step also exercises the unknown-total fallback.
                                        json!({"steps":&[step(),step()][offset.min(2)..(offset + 1).min(2)]})
                                    } else {
                                        json!({"steps":[step()],"detail":"x".repeat(LIMIT + 1)})
                                    }
                                }
                                _ => panic!("unexpected read method"),
                            }
                        };
                        let mut response = Response::new(Full::new(Bytes::from(
                            serde_json::to_vec(&value).unwrap(),
                        )));
                        if foreign {
                            *response.status_mut() = hyper::StatusCode::NOT_FOUND;
                        }
                        Ok::<_, std::convert::Infallible>(response)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                    .await;
            });
        }
    });
    Fixture {
        api: Api::from_single(
            reqwest::Client::builder().no_proxy().build().unwrap(),
            origin,
            "fixture".into(),
        ),
        calls,
        changed,
        entries,
        models,
        server,
    }
}

async fn collect_home(
    api: &Api,
    store: &mut impl Repository,
    home: &Path,
    pricing: &Pricing,
    settings: &Settings,
    report: &mut Report,
    repair: bool,
) -> Result<()> {
    let brain = home.join("antigravity/brain");
    let files = collect_transcript_files(&brain)?;
    api.collect_with_files(store, &brain, &files, pricing, settings, report, repair)
        .await
}

fn scan_home(
    store: &mut impl Repository,
    home: &Path,
    pricing: &Pricing,
    settings: &Settings,
    report: &mut Report,
    repair: bool,
) -> Result<()> {
    let brain = home.join("antigravity/brain");
    let files = collect_transcript_files(&brain)?;
    scan_file_list(store, &brain, &files, pricing, settings, report, repair)
}

#[tokio::test]
async fn manual_repair_bypasses_cursor_and_incremental_sync_reads_only_needed_steps() {
    let f = fixture(false).await;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();
    let settings = Settings::default();
    collect_home(
        &f.api,
        &mut store,
        dir.path(),
        &pricing,
        &settings,
        &mut Report::default(),
        false,
    )
    .await
    .unwrap();
    let id = parsed(&entry("first", 0)).id;
    let mut old = store.detail(&id).unwrap();
    old.attempts[0].model_display_name = None;
    store.write_batch(&[old], None).unwrap();
    collect_home(
        &f.api,
        &mut store,
        dir.path(),
        &pricing,
        &settings,
        &mut Report::default(),
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        store.detail(&id).unwrap().attempts[0].model_display_name,
        None
    );
    collect_home(
        &f.api,
        &mut store,
        dir.path(),
        &pricing,
        &settings,
        &mut Report::default(),
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        store.detail(&id).unwrap().attempts[0]
            .model_display_name
            .as_deref(),
        Some("Fixture Model")
    );
    f.calls.lock().clear();
    *f.changed.lock() = "next".into();
    let sequence: i64 = store
        .connection
        .query_row(
            "SELECT value FROM metadata WHERE key='record_sequence'",
            [],
            |r| r.get::<_, String>(0).map(|s| s.parse().unwrap()),
        )
        .unwrap();
    collect_home(
        &f.api,
        &mut store,
        dir.path(),
        &pricing,
        &settings,
        &mut Report::default(),
        false,
    )
    .await
    .unwrap();
    let next: i64 = store
        .connection
        .query_row(
            "SELECT value FROM metadata WHERE key='record_sequence'",
            [],
            |r| r.get::<_, String>(0).map(|s| s.parse().unwrap()),
        )
        .unwrap();
    assert_eq!(sequence, next);
    let calls = f.calls.lock();
    assert!(!calls.iter().any(|(m, _)| m == "GetCascadeTrajectory"));
    assert!(calls
        .iter()
        .any(|(m, b)| m == "GetCascadeTrajectorySteps" && b["stepOffset"] == 1));
    assert_eq!(
        store.dashboard(&Filter::default()).unwrap().totals.requests,
        2
    );
}

#[tokio::test]
async fn failed_session_does_not_block_other_sessions_or_advance_its_cursor() {
    let f = fixture(true).await;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();
    let mut report = Report::default();
    collect_home(
        &f.api,
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut report,
        false,
    )
    .await
    .unwrap();
    assert_eq!((report.errors, report.files, report.imported), (1, 1, 2));
    // The reason of the skipped session is kept for the sources page.
    assert!(report
        .last_error
        .as_deref()
        .is_some_and(|reason| reason.starts_with("Antigravity")));
    assert_eq!(
        store.cursor(&safe_id("antigravity:rpc:a-broken")).unwrap(),
        None
    );
    assert_eq!(
        store.dashboard(&Filter::default()).unwrap().totals.requests,
        2
    );
    assert_eq!(
        store.dashboard(&Filter::default()).unwrap().totals.requests,
        2
    );
}

#[tokio::test]
async fn local_history_discovers_unlisted_sessions_and_fallback_steps_cover_transcript() {
    let f = fixture(false).await;
    let dir = tempfile::tempdir().unwrap();
    f.entries.lock()[1]
        .as_object_mut()
        .unwrap()
        .remove("stepIndices");
    for id in ["z-good", "local-only"] {
        let path = dir.path().join(format!(
            "antigravity/brain/{id}/.system_generated/logs/transcript.jsonl"
        ));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = if id == "z-good" {
            "{\"step_index\":0,\"input_tokens\":100}\n{\"step_index\":1,\"input_tokens\":100}\n"
        } else {
            "{\"type\":\"PLANNER_RESPONSE\",\"step_index\":0,\"created_at\":\"2026-01-01T00:00:00Z\",\"input_tokens\":100}\n{\"type\":\"PLANNER_RESPONSE\",\"step_index\":1,\"created_at\":\"2026-01-01T00:00:01Z\",\"input_tokens\":100}\n"
        };
        std::fs::write(path, body).unwrap();
    }
    let mut store = Store::open(&dir.path().join("db")).unwrap();
    let pricing = Pricing::new(&dir.path().join("db")).unwrap();
    for _ in 0..2 {
        let mut report = Report::default();
        collect_home(
            &f.api,
            &mut store,
            dir.path(),
            &pricing,
            &Settings::default(),
            &mut report,
            false,
        )
        .await
        .unwrap();
        scan_home(
            &mut store,
            dir.path(),
            &pricing,
            &Settings::default(),
            &mut report,
            false,
        )
        .unwrap();
        assert_eq!((report.total_files, report.files, report.errors), (2, 2, 0));
        assert_eq!(
            store.dashboard(&Filter::default()).unwrap().totals.requests,
            4
        );
        let cursor: Cursor = serde_json::from_str(
            &store
                .cursor(&safe_id("antigravity:rpc:z-good"))
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(cursor.covered, BTreeSet::from([0, 1]));
        assert!(store
            .cursor(&safe_id("antigravity:rpc:local-only"))
            .unwrap()
            .is_none());
    }
    assert!(f
        .calls
        .lock()
        .iter()
        .filter(|(m, _)| m == "GetCascadeTrajectorySteps")
        .all(|(_, b)| b["trajectoryVerbosity"] == 2));
    assert_eq!(
        f.calls
            .lock()
            .iter()
            .filter(|(m, b)| {
                m == "GetCascadeTrajectoryGeneratorMetadata" && b["cascadeId"] == "local-only"
            })
            .count(),
        0
    );
    // Simulate a previously synced RPC cursor (count >= 2 => offset > 0) for a deleted thread
    // whose transcript.jsonl still exists on disk and changes (H3.4).
    store
        .write_batch(
            &[],
            Some((
                &safe_id("antigravity:rpc:local-only"),
                r#"{"modified":"old","count":3,"covered":[0,1]}"#,
            )),
        )
        .unwrap();
    let local_transcript = dir
        .path()
        .join("antigravity/brain/local-only/.system_generated/logs/transcript.jsonl");
    let mut existing = std::fs::read_to_string(&local_transcript).unwrap();
    existing.push_str("{\"step_index\":2,\"input_tokens\":50}\n");
    std::fs::write(&local_transcript, existing).unwrap();
    let mut report = Report::default();
    collect_home(
        &f.api,
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut report,
        false,
    )
    .await
    .unwrap();
    assert_eq!(report.errors, 0);
    assert_eq!(
        f.calls
            .lock()
            .iter()
            .filter(|(m, b)| {
                m == "GetCascadeTrajectoryGeneratorMetadata" && b["cascadeId"] == "local-only"
            })
            .count(),
        0
    );
}

#[tokio::test]
async fn missing_identity_keeps_gap_but_saves_later_requests_and_repairs_after_restart() {
    let f = fixture(false).await;
    let original = f.entries.lock()[0].clone();
    {
        let mut entries = f.entries.lock();
        entries[0].as_object_mut().unwrap().remove("executionId");
        entries[0]["chatModel"]
            .as_object_mut()
            .unwrap()
            .remove("customMetadata");
    }
    let dir = tempfile::tempdir().unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let mut report = Report::default();
    collect_home(
        &f.api,
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut report,
        false,
    )
    .await
    .unwrap();
    assert_eq!(report.errors, 1);
    assert_eq!(
        store.dashboard(&Filter::default()).unwrap().totals.requests,
        1
    );
    let cursor: Cursor = serde_json::from_str(
        &store
            .cursor(&safe_id("antigravity:rpc:z-good"))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cursor.count, 0);
    assert!(cursor.modified.is_empty());
    assert_eq!(cursor.covered, BTreeSet::from([1]));
    drop(store);
    f.entries.lock()[0] = original;
    let mut store = Store::open(dir.path()).unwrap();
    for _ in 0..2 {
        let mut report = Report::default();
        collect_home(
            &f.api,
            &mut store,
            dir.path(),
            &pricing,
            &Settings::default(),
            &mut report,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.errors, 0);
        assert_eq!(
            store.dashboard(&Filter::default()).unwrap().totals.requests,
            2
        );
    }
}

#[test]
fn file_gap_does_not_hide_later_usage_or_advance_past_unresolved_history() {
    let dir = tempfile::tempdir().unwrap();
    let brain = dir.path().join("antigravity/brain");
    let path = brain
        .join("fixture")
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let gap = "{\"step_index\":0,\"input_tokens\":7}\n";
    let later = "{\"request_id\":\"later\",\"model\":\"fixture-model\",\"created_at\":\"2026-01-01T00:00:00Z\",\"input_tokens\":3}\n";
    std::fs::write(&path, format!("{gap}{later}")).unwrap();
    let mut store = Store::open(&dir.path().join("db")).unwrap();
    let pricing = Pricing::new(&dir.path().join("db")).unwrap();
    let key = safe_id(&format!("antigravity:file:{}", path.to_string_lossy()));
    for _ in 0..2 {
        let mut report = Report::default();
        scan_home(
            &mut store,
            dir.path(),
            &pricing,
            &Settings::default(),
            &mut report,
            false,
        )
        .unwrap();
        assert_eq!(report.errors, 1);
        assert_eq!(
            store.dashboard(&Filter::default()).unwrap().totals.requests,
            1
        );
        let cursor: FileCursor =
            serde_json::from_str(&store.cursor(&key).unwrap().unwrap()).unwrap();
        assert_eq!(cursor.offset, 0);
    }
    // A verified API association resolves the gap without rewriting the file.
    let rpc = serde_json::to_string(&Cursor {
        covered: BTreeSet::from([0]),
        ..Cursor::default()
    })
    .unwrap();
    store
        .write_batch(&[], Some((&safe_id("antigravity:rpc:fixture"), &rpc)))
        .unwrap();
    let mut report = Report::default();
    scan_home(
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut report,
        false,
    )
    .unwrap();
    assert_eq!(report.errors, 0);
    let cursor: FileCursor = serde_json::from_str(&store.cursor(&key).unwrap().unwrap()).unwrap();
    assert_eq!(cursor.offset, (gap.len() + later.len()) as u64);
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        format!("{gap}{later}")
    );
}

#[test]
fn known_placeholders_and_enums_resolve_pricing_and_effort_while_unknown_stays_unpriced() {
    let dir = tempfile::tempdir().unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();
    let settings = Settings::default();
    let steps = Steps::from([step()].as_slice());

    for (raw, expected_model, expected_display, expected_effort, expects_priced) in [
        (
            "MODEL_PLACEHOLDER_M37",
            Some("gemini-3.1-pro"),
            Some("Gemini 3.1 Pro"),
            Some("high"),
            true,
        ),
        (
            "MODEL_PLACEHOLDER_M36",
            Some("gemini-3.1-pro"),
            Some("Gemini 3.1 Pro"),
            Some("low"),
            true,
        ),
        (
            "MODEL_PLACEHOLDER_M18",
            Some("gemini-3-flash"),
            Some("Gemini 3 Flash"),
            None,
            true,
        ),
        (
            "MODEL_PLACEHOLDER_M26",
            Some("claude-opus-4-6"),
            Some("Claude Opus 4.6"),
            None,
            true,
        ),
        (
            "MODEL_PLACEHOLDER_M35",
            Some("claude-sonnet-4-6"),
            Some("Claude Sonnet 4.6"),
            None,
            true,
        ),
        (
            "MODEL_CLAUDE_4_5_SONNET_THINKING",
            Some("claude-sonnet-4-5"),
            Some("Claude Sonnet 4.5"),
            None,
            true,
        ),
        (
            "MODEL_CHAT_20706",
            Some("gemini-2.5-flash"),
            Some("Gemini 2.5 Flash"),
            None,
            true,
        ),
        (
            "MODEL_OPENAI_GPT_OSS_120B_MEDIUM",
            Some("gpt-oss-120b"),
            Some("GPT-OSS 120B"),
            Some("medium"),
            false,
        ),
        ("MODEL_PLACEHOLDER_M999", None, None, None, false),
    ] {
        let item = json!({
            "executionId": "exec",
            "stepIndices": [0],
            "chatModel": {
                "model": raw,
                "customMetadata": {"request_id": format!("req-{raw}")},
                "usage": {
                    "responseId": format!("resp-{raw}"),
                    "model": raw,
                    "inputTokens": "1000",
                    "cacheReadTokens": "500",
                    "outputTokens": "200",
                    "thinkingOutputTokens": "80"
                },
                "timeToFirstToken": "0.2s",
                "streamingDuration": "1.0s"
            }
        });
        let mut rec = record("cascade-one", &item, &steps, 0, &Learned::default())
            .unwrap()
            .unwrap();
        price(&mut rec, &pricing, &settings);
        let a = &rec.attempts[0];
        assert_eq!(a.requested_model.as_deref(), expected_model, "{raw}");
        assert_eq!(a.response_model.as_deref(), expected_model, "{raw}");
        assert_eq!(a.model_display_name.as_deref(), expected_display, "{raw}");
        assert_eq!(a.reasoning_effort.as_deref(), expected_effort, "{raw}");
        assert_eq!(a.price.is_some(), expects_priced, "{raw}");
    }

    // Historical attempt with only display name recovers canonical model without fabricating effort for "(Thinking)".
    let mut legacy = Attempt {
        model_display_name: Some("Claude Opus 4.6 (Thinking)".into()),
        duration_ms: 1000,
        ..Attempt::default()
    };
    assert!(normalize_attempt(&mut legacy));
    assert_eq!(
        legacy.model_display_name.as_deref(),
        Some("Claude Opus 4.6")
    );
    assert_eq!(legacy.reasoning_effort.as_deref(), None);
    assert_eq!(legacy.response_model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(legacy.pricing_model.as_deref(), Some("claude-opus-4-6"));
}

#[tokio::test]
async fn multi_window_endpoints_merge_fresher_trajectories_and_fallback_for_brain_only_sessions() {
    let pages = json!([
        {"type":"page","url":"https://127.0.0.1:41001/index.html"},
        {"type":"page","url":"https://127.0.0.1:41001/other"},
        {"type":"other","url":"https://127.0.0.1:41002/worker"},
        {"type":"page","url":"https://127.0.0.1:41002/index.html"}
    ]);
    assert_eq!(
        parse_page_origins(&pages),
        vec![
            "https://127.0.0.1:41001".to_owned(),
            "https://127.0.0.1:41002".to_owned(),
        ]
    );

    async fn spawn_window(
        summaries: Value,
        owned_sessions: BTreeMap<String, Vec<Value>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    break;
                };
                let summaries = summaries.clone();
                let owned_sessions = owned_sessions.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |r: hyper::Request<hyper::body::Incoming>| {
                        let summaries = summaries.clone();
                        let owned_sessions = owned_sessions.clone();
                        async move {
                            let method = r.uri().path().rsplit('/').next().unwrap().to_owned();
                            let body: Value = serde_json::from_slice(
                                &r.into_body().collect().await.unwrap().to_bytes(),
                            )
                            .unwrap();
                            let cid = body["cascadeId"].as_str().unwrap_or("");
                            let value = match method.as_str() {
                                "GetAllCascadeTrajectories" => {
                                    json!({"trajectorySummaries": summaries})
                                }
                                "GetCascadeTrajectoryGeneratorMetadata" => {
                                    let offset =
                                        body["generatorMetadataOffset"].as_u64().unwrap_or(0)
                                            as usize;
                                    if let Some(items) = owned_sessions.get(cid) {
                                        let end = items.len();
                                        json!({"generatorMetadata": &items[offset.min(end)..end]})
                                    } else {
                                        json!({})
                                    }
                                }
                                "GetCascadeTrajectorySteps" => {
                                    json!({"steps": [step(), step()]})
                                }
                                _ => json!({}),
                            };
                            Ok::<_, std::convert::Infallible>(Response::new(Full::new(
                                Bytes::from(serde_json::to_vec(&value).unwrap()),
                            )))
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                        .await;
                });
            }
        });
        (origin, handle)
    }

    let (origin_a, server_a) = spawn_window(
        json!({
            "shared": {"lastModifiedTime":"2026-01-01T00:00:10Z","stepCount":1},
            "only-a": {"lastModifiedTime":"2026-01-01T00:00:20Z","stepCount":1}
        }),
        BTreeMap::from([
            ("shared".into(), vec![entry("shared-stale", 0)]),
            ("only-a".into(), vec![entry("only-a-req", 0)]),
        ]),
    )
    .await;
    let (origin_b, server_b) = spawn_window(
        json!({
            "shared": {"lastModifiedTime":"2026-01-01T00:00:50Z","stepCount":2}
        }),
        BTreeMap::from([
            (
                "shared".into(),
                vec![entry("shared-fresh-1", 0), entry("shared-fresh-2", 1)],
            ),
            ("brain-b".into(), vec![entry("brain-b-req", 0)]),
        ]),
    )
    .await;

    let dir = tempfile::tempdir().unwrap();
    let brain_file = dir
        .path()
        .join("antigravity/brain/brain-b/transcript.jsonl");
    std::fs::create_dir_all(brain_file.parent().unwrap()).unwrap();
    std::fs::write(
        &brain_file,
        "{\"type\":\"PLANNER_RESPONSE\",\"step_index\":0,\"created_at\":\"2026-01-01T00:00:00Z\",\"input_tokens\":50}\n",
    )
    .unwrap();

    let api = Api::from_endpoints(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        vec![(origin_a, "tok-a".into()), (origin_b, "tok-b".into())],
    );
    let mut store = Store::open(dir.path()).unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();
    let mut report = Report::default();
    collect_home(
        &api,
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut report,
        false,
    )
    .await
    .unwrap();
    scan_home(
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut report,
        false,
    )
    .unwrap();
    server_a.abort();
    server_b.abort();

    assert_eq!(report.errors, 0);
    // 2 from fresher window_b "shared" + 1 from window_a "only-a" + 1 from local transcript "brain-b"
    assert_eq!(
        store.dashboard(&Filter::default()).unwrap().totals.requests,
        4
    );
    assert!(store
        .detail(&safe_id("antigravity:request:shared-fresh-2"))
        .is_ok());
    assert!(store
        .detail(&safe_id("antigravity:request:shared-stale"))
        .is_err());
    assert!(store
        .detail(&safe_id("antigravity:request:brain-b:step:0"))
        .is_ok());
}

#[test]
fn antigravity_stats_aggregate_token_mix_savings_dual_tracks_and_sessions() {
    let now_ms = 1_760_000_000_000i64;
    for native in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        if native {
            while store.compact_storage_batch().unwrap() {}
        }
        std::fs::write(dir.path().join("antigravity-accounts.json"), serde_json::to_vec(&json!({"schema":1,"entries":[{"id":"fixture-oauth","name":"Fixture OAuth","kind":"oauth","sourceType":"oauth","payload":"{}","updatedAt":0}]})).unwrap()).unwrap();
        let periods = ["gemini-5h", "gemini-weekly", "3p-5h", "3p-weekly"]
            .into_iter()
            .map(|id| {
                let duration = if id.ends_with("5h") {
                    5 * 3_600_000
                } else {
                    7 * 86_400_000
                };
                (
                    id.to_owned(),
                    quota::Period {
                        start: now_ms + 1 - duration,
                        end: now_ms + 1,
                        remaining_percent: None,
                    },
                )
            })
            .collect();
        store
            .save_antigravity_quota("fixture-oauth", &periods)
            .unwrap();
        let pricing = Pricing::new(dir.path()).unwrap();
        let settings = Settings::default();

        let make_ag = |req_id: &str,
                       cascade: &str,
                       model: &str,
                       at: i64,
                       inp: u64,
                       cr: u64,
                       out: u64,
                       think: u64| {
            let mut r = Record {
                id: safe_id(&format!("antigravity:request:{req_id}")),
                client: "antigravity".into(),
                source: "antigravity".into(),
                started_at: at - 2_000,
                finished_at: Some(at),
                session_id: Some(safe_id(cascade)),
                completed: true,
                attempts: vec![Attempt {
                    id: safe_id(&format!("antigravity:request:{req_id}:final")),
                    provider: Some("fixture-oauth".into()),
                    requested_model: actual_model(Some(model)),
                    response_model: actual_model(Some(model)),
                    started_at: at - 2_000,
                    duration_ms: 2_000,
                    first_token_ms: Some(300),
                    generation_ms: Some(1_700),
                    stream: true,
                    transport: "local_api".into(),
                    outcome: "success".into(),
                    usage_status: "reported".into(),
                    tokens: Tokens {
                        input: Some(inp),
                        cache_read: Some(cr),
                        cache_write: Some(0),
                        output: Some(out),
                        reasoning: Some(think),
                        ..Tokens::default()
                    },
                    ..Attempt::default()
                }],
                ..Record::default()
            };
            normalize_record(&mut r);
            price(&mut r, &pricing, &settings);
            r
        };

        // 1) Gemini 3.1 Pro within 5h window
        let r1 = make_ag(
            "g-recent",
            "session-alpha",
            "MODEL_PLACEHOLDER_M37",
            now_ms - 3_600_000,
            10_000,
            40_000,
            2_000,
            1_200,
        );
        // 2) Gemini 3 Flash outside 5h but inside 7d
        let r2 = make_ag(
            "g-older",
            "session-alpha",
            "MODEL_PLACEHOLDER_M18",
            now_ms - 24 * 3_600_000,
            5_000,
            5_000,
            1_000,
            200,
        );
        // 3) Claude Opus 4.6 within 5h window
        let r3 = make_ag(
            "c-recent",
            "session-beta",
            "MODEL_PLACEHOLDER_M26",
            now_ms - 1_800_000,
            8_000,
            32_000,
            3_000,
            1_800,
        );
        // 4) Non-Antigravity Codex record (must be excluded from Antigravity stats)
        let mut other = r1.clone();
        other.id = safe_id("codex:other");
        other.client = "codex".into();
        other.source = "proxy".into();

        store
            .write_batch(&[r1.clone(), r2, r3.clone(), other], None)
            .unwrap();
        if native {
            while store.compact_storage_batch().unwrap() {}
        }

        let stats = store
            .antigravity_stats(
                &Filter {
                    end: Some(now_ms),
                    ..Filter::default()
                },
                now_ms,
            )
            .unwrap();

        assert_eq!(stats.requests, 3);
        assert_eq!(stats.sessions, 2);
        assert_eq!(stats.token_mix.input, 23_000);
        assert_eq!(stats.token_mix.cache_read, 77_000);
        assert_eq!(stats.token_mix.output, 6_000);
        assert_eq!(stats.token_mix.reasoning, 3_200);
        assert_eq!(stats.token_mix.visible_output, 2_800);
        assert_eq!(stats.token_mix.thinking_share_percent, Some(53.3));
        assert_eq!(stats.token_mix.cache_hit_percent, Some(77.0));
        assert!(decimal(&stats.token_mix.cache_savings_usd).unwrap() > rust_decimal::Decimal::ZERO);

        assert_eq!(stats.accounts[0].tracks[0].id, "gemini");
        assert_eq!(stats.accounts[0].tracks[0].burst_5h.requests, 1);
        assert_eq!(
            stats.accounts[0].tracks[0].burst_5h.cost_usd,
            fmt_usd(r1.cost().unwrap())
        );
        assert_eq!(stats.accounts[0].tracks[0].weekly_7d.requests, 2);
        assert_eq!(stats.accounts[0].tracks[1].id, "claude_gpt");
        assert_eq!(stats.accounts[0].tracks[1].burst_5h.requests, 1);
        assert_eq!(
            stats.accounts[0].tracks[1].burst_5h.cost_usd,
            fmt_usd(r3.cost().unwrap())
        );
        assert_eq!(stats.accounts[0].tracks[1].weekly_7d.requests, 1);

        assert_eq!(stats.families.len(), 2);
        assert_eq!(stats.sessions, 2);

        // Verify session_id filter isolates logs cleanly in both v11 and v12.
        let alpha_logs = store
            .logs(&Filter {
                client: Some("antigravity".into()),
                session_id: Some(safe_id("session-alpha")),
                ..Filter::default()
            })
            .unwrap();
        assert_eq!(alpha_logs.total, 2);
    }

    // F7: Verify cache_savings_usd uses each row's own started_at tariff rather than reusing the first row's timestamp.
    let dir = tempfile::tempdir().unwrap();
    let pricing = Pricing::new(dir.path()).unwrap().frozen();
    let settings = Settings::default();
    let off_peak_ms = chrono::DateTime::parse_from_rfc3339("2026-10-25T02:00:00Z")
        .unwrap()
        .timestamp_millis();
    let peak_ms = chrono::DateTime::parse_from_rfc3339("2026-10-26T02:00:00Z")
        .unwrap()
        .timestamp_millis();
    let build_row = |at: i64| {
        let mut r = Record {
            id: safe_id(&format!("antigravity:request:ds-{at}")),
            client: "antigravity".into(),
            source: "antigravity".into(),
            started_at: at,
            finished_at: Some(at),
            completed: true,
            attempts: vec![Attempt {
                id: safe_id(&format!("antigravity:request:ds-{at}:final")),
                requested_model: Some("deepseek-chat".into()),
                response_model: Some("deepseek-chat".into()),
                started_at: at,
                duration_ms: 1000,
                outcome: "success".into(),
                usage_status: "reported".into(),
                tokens: Tokens {
                    input: Some(1_000),
                    cache_read: Some(10_000),
                    output: Some(100),
                    ..Tokens::default()
                },
                ..Attempt::default()
            }],
            ..Record::default()
        };
        price(&mut r, &pricing, &settings);
        let a = &r.attempts[0];
        crate::usage::storage_v12::AntigravityAttemptRow {
            final_provider: None,
            session_id: None,
            provider: None,
            time: at,
            is_final: true,
            model: a.response_model.clone(),
            display_name: None,
            tokens: a.tokens.clone(),
            cache_savings_rates: a
                .price
                .as_ref()
                .and_then(|p| crate::usage::pricing::cache_savings_rates(&p.rates, &p.multiplier)),
            cost: a.price.as_ref().map(|p| p.cost.clone()),
            request_unpriced: false,
            provider_unpriced: false,
        }
    };
    let mut single_off_peak = UsageStatsAggregator::new(&Filter::default(), peak_ms + 1_000);
    single_off_peak.ingest(build_row(off_peak_ms));
    let off_peak_saving = decimal(&single_off_peak.finish().token_mix.cache_savings_usd).unwrap();
    assert!(off_peak_saving > rust_decimal::Decimal::ZERO);

    let mut combined = UsageStatsAggregator::new(&Filter::default(), peak_ms + 1_000);
    combined.ingest(build_row(off_peak_ms));
    combined.ingest(build_row(peak_ms));
    let combined_saving = decimal(&combined.finish().token_mix.cache_savings_usd).unwrap();
    // Peak tariff is 2x off-peak for deepseek-chat, so combined savings must be 3x off_peak_saving (not 2x).
    assert_eq!(
        combined_saving,
        off_peak_saving * rust_decimal::Decimal::from(3)
    );
}

#[tokio::test]
async fn file_imported_planner_step_is_deduplicated_when_later_covered_by_rpc_sync() {
    let f = fixture(false).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("antigravity/brain/z-good/.system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let rows = concat!(
        "{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"created_at\":\"2026-01-01T00:00:00Z\",\"content\":\"<USER_REQUEST>\\nhello\\n</USER_REQUEST>\\n<USER_SETTINGS_CHANGE>\\nThe user changed setting `Model Selection` from None to Claude Opus 4.6 (Thinking). No need to comment on this change.\\n</USER_SETTINGS_CHANGE>\"}\n",
        "{\"step_index\":1,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-01-01T00:00:01Z\",\"input_tokens\":100,\"cache_read_tokens\":40,\"output_tokens\":20}\n",
    );
    std::fs::write(&path, rows).unwrap();

    for native in [false, true] {
        let db_dir = dir.path().join(if native { "db-v12" } else { "db-v11" });
        let mut store = Store::open(&db_dir).unwrap();
        if native {
            while store.compact_storage_batch().unwrap() {}
        }
        let pricing = Pricing::new(&db_dir).unwrap();

        // 1. Offline file scan imports step 1 with synthetic step ID.
        let mut file_report = Report::default();
        scan_home(
            &mut store,
            dir.path(),
            &pricing,
            &Settings::default(),
            &mut file_report,
            false,
        )
        .unwrap();
        assert_eq!(file_report.imported, 1);
        let synthetic_id = safe_id("antigravity:request:z-good:step:1");
        assert!(store.detail(&synthetic_id).is_ok());
        assert_eq!(
            store.dashboard(&Filter::default()).unwrap().totals.requests,
            1
        );

        // 2. Later RPC sync covers steps 0 and 1 of z-good and supersedes the synthetic step 1 record.
        let mut rpc_report = Report::default();
        collect_home(
            &f.api,
            &mut store,
            dir.path(),
            &pricing,
            &Settings::default(),
            &mut rpc_report,
            false,
        )
        .await
        .unwrap();
        assert!(
            store.detail(&synthetic_id).is_err(),
            "synthetic history_file step record should be removed once RPC covers the step"
        );
        // z-good has 2 RPC entries ("first" at step 0 and "second" at step 1), not 3!
        assert_eq!(
            store.dashboard(&Filter::default()).unwrap().totals.requests,
            2
        );
    }
}

#[test]
fn migration_cleans_up_legacy_thinking_high_effort_and_streaming_stats_match_batch() {
    for native in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        if native {
            while store.compact_storage_batch().unwrap() {}
        }
        // Simulate a record that already ran v02611/v02613 where model_display_name was stripped
        // to "Claude Opus 4.6" and reasoning_effort was mistakenly persisted as "high".
        let mut polluted = parsed(&entry("polluted-opus", 0));
        polluted.attempts[0].requested_model = Some("claude-opus-4-6".into());
        polluted.attempts[0].response_model = Some("claude-opus-4-6".into());
        polluted.attempts[0].pricing_model = Some("claude-opus-4-6".into());
        polluted.attempts[0].model_display_name = Some("Claude Opus 4.6".into());
        polluted.attempts[0].reasoning_effort = Some("high".into());

        // Legitimate Gemini 3.1 Pro (High) must keep reasoning_effort = "high".
        let mut gemini_high = parsed(&entry("gemini-high", 0));
        gemini_high.attempts[0].requested_model = Some("gemini-3.1-pro".into());
        gemini_high.attempts[0].response_model = Some("gemini-3.1-pro".into());
        gemini_high.attempts[0].pricing_model = Some("gemini-3.1-pro".into());
        gemini_high.attempts[0].model_display_name = Some("Gemini 3.1 Pro".into());
        gemini_high.attempts[0].reasoning_effort = Some("high".into());

        assert!(normalize_record(&mut polluted));
        assert!(!normalize_record(&mut gemini_high));
        store
            .write_batch(&[polluted.clone(), gemini_high.clone()], None)
            .unwrap();
        assert_eq!(
            store.detail(&polluted.id).unwrap().attempts[0].reasoning_effort,
            None
        );
        assert_eq!(
            store.detail(&gemini_high.id).unwrap().attempts[0]
                .reasoning_effort
                .as_deref(),
            Some("high")
        );

        // Verify streaming stats match batch stats over identical rows.
        let now_ms = polluted.started_at + 10_000;
        let filter = Filter {
            end: Some(now_ms),
            ..Filter::default()
        };
        let streamed = store.antigravity_stats(&filter, now_ms).unwrap();
        if native {
            let mut batch_rows = Vec::new();
            crate::usage::storage_v12::scan_client_attempts(
                &store.connection,
                "antigravity",
                filter.start.unwrap_or(i64::MIN),
                filter.end.unwrap_or(i64::MAX),
                None,
                |row| {
                    batch_rows.push(row);
                    Ok(())
                },
            )
            .unwrap();
            let mut batch_agg = UsageStatsAggregator::new(&filter, now_ms);
            for row in batch_rows {
                batch_agg.ingest(row);
            }
            let batched = batch_agg.finish();
            assert_eq!(
                serde_json::to_value(&streamed).unwrap(),
                serde_json::to_value(&batched).unwrap()
            );
        }
    }
}

#[test]
fn remove_superseded_rejects_invalid_ids_wrong_sources_missing_rpc_and_rolls_back_on_partial_failure(
) {
    for native in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        if native {
            while store.compact_storage_batch().unwrap() {}
        }

        let session_with_rpc = safe_id("session-with-rpc");
        let session_without_rpc = safe_id("session-without-rpc");

        let make_record =
            |raw_id: &str, source: &str, transport: &str, session_id: &str| -> Record {
                Record {
                    id: safe_id(raw_id),
                    client: "antigravity".into(),
                    source: source.into(),
                    session_id: Some(session_id.into()),
                    started_at: 1_767_225_600_000,
                    finished_at: Some(1_767_225_601_000),
                    completed: true,
                    attempts: vec![Attempt {
                        started_at: 1_767_225_600_000,
                        outcome: "succeeded".into(),
                        transport: transport.into(),
                        usage_status: "reported".into(),
                        tokens: Tokens {
                            input: Some(10),
                            output: Some(5),
                            ..Tokens::default()
                        },
                        ..Attempt::default()
                    }],
                    ..Record::default()
                }
            };

        let valid_history = make_record(
            "antigravity:request:session-with-rpc:step:1",
            "antigravity",
            "history_file",
            &session_with_rpc,
        );
        let replacing_rpc = make_record(
            "antigravity:request:session-with-rpc:rpc:1",
            "antigravity",
            "local_api",
            &session_with_rpc,
        );
        let orphan_history = make_record(
            "antigravity:request:session-without-rpc:step:1",
            "antigravity",
            "history_file",
            &session_without_rpc,
        );
        let codex_row = make_record(
            "codex:request:1",
            "codex",
            "history_file",
            &session_with_rpc,
        );

        store
            .write_batch(
                &[
                    valid_history.clone(),
                    replacing_rpc.clone(),
                    orphan_history.clone(),
                    codex_row.clone(),
                ],
                None,
            )
            .unwrap();

        // 1. Out-of-bounds / invalid IDs are rejected.
        assert!(store
            .write_batch_with_superseded(&[], &["../bad-id".to_string()], None)
            .is_err());
        assert!(store
            .write_batch_with_superseded(&[], &["not-64-hex".to_string()], None)
            .is_err());

        // 2. Non-history_file records (local_api / codex) are rejected and remain intact.
        assert!(store
            .write_batch_with_superseded(&[], std::slice::from_ref(&replacing_rpc.id), None)
            .is_err());
        assert!(store.detail(&replacing_rpc.id).is_ok());
        assert!(store
            .write_batch_with_superseded(&[], std::slice::from_ref(&codex_row.id), None)
            .is_err());
        assert!(store.detail(&codex_row.id).is_ok());

        // 3. history_file record without replacing local_api in the same session is rejected.
        assert!(store
            .write_batch_with_superseded(&[], std::slice::from_ref(&orphan_history.id), None)
            .is_err());
        assert!(store.detail(&orphan_history.id).is_ok());

        // 4. Partial failure rolls back the entire transaction: valid_history must NOT be deleted
        // when paired with an invalid/non-deletable target in the same batch.
        assert!(store
            .write_batch_with_superseded(
                &[],
                &[valid_history.id.clone(), replacing_rpc.id.clone()],
                None
            )
            .is_err());
        assert!(
            store.detail(&valid_history.id).is_ok(),
            "valid_history must survive when a later target in the same batch fails validation"
        );

        // 5. Valid superseded history_file record with replacing local_api succeeds.
        store
            .write_batch_with_superseded(&[], std::slice::from_ref(&valid_history.id), None)
            .unwrap();
        assert!(store.detail(&valid_history.id).is_err());
        assert!(store.detail(&replacing_rpc.id).is_ok());
    }
}

#[tokio::test]
async fn rpc_sync_does_not_delete_history_step_when_later_entry_in_same_session_fails_that_step() {
    let f = fixture(false).await;
    {
        let mut entries = f.entries.lock();
        let mut bad = entry("bad-second", 1);
        bad["chatModel"]["usage"]
            .as_object_mut()
            .unwrap()
            .remove("responseId");
        *entries = vec![entry("good-first", 1), bad];
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("antigravity/brain/z-good/.system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let rows = concat!(
        "{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"created_at\":\"2026-01-01T00:00:00Z\",\"content\":\"<USER_REQUEST>\\nhello\\n</USER_REQUEST>\\n<USER_SETTINGS_CHANGE>\\nThe user changed setting `Model Selection` from None to Claude Opus 4.6 (Thinking). No need to comment on this change.\\n</USER_SETTINGS_CHANGE>\"}\n",
        "{\"step_index\":1,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-01-01T00:00:01Z\",\"input_tokens\":100,\"output_tokens\":20}\n",
    );
    std::fs::write(&path, rows).unwrap();

    let mut store = Store::open(dir.path()).unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();
    scan_home(
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut Report::default(),
        false,
    )
    .unwrap();
    let synthetic_id = safe_id("antigravity:request:z-good:step:1");
    assert!(store.detail(&synthetic_id).is_ok());

    let mut rpc_report = Report::default();
    collect_home(
        &f.api,
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut rpc_report,
        false,
    )
    .await
    .unwrap();
    assert_eq!(rpc_report.errors, 1);
    assert!(
        store.detail(&synthetic_id).is_ok(),
        "step 1 must not be deleted when a later entry in the same session marks step 1 as failed"
    );
}

#[test]
fn extract_settings_model_accepts_unknown_new_models_and_rejects_malformed_blocks() {
    let valid_new = "<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from Claude Opus 4.6 (Thinking) to Opus 5.5. No need to comment on this change.\n</USER_SETTINGS_CHANGE>";
    assert_eq!(
        extract_settings_model(valid_new).as_deref(),
        Some("Opus 5.5")
    );
    let valid_effort = "<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to Gemini 4 Argon (High). No need to comment on this change if the user doesn't ask about it, just be aware of it.\n</USER_SETTINGS_CHANGE>";
    assert_eq!(
        extract_settings_model(valid_effort).as_deref(),
        Some("Gemini 4 Argon (High)")
    );
    let m417 = resolve_model_alias(Some("MODEL_PLACEHOLDER_M417")).unwrap();
    assert_eq!(
        (m417.canonical, m417.display_name, m417.effort),
        ("gemini-4-argon", "Gemini 4 Argon", Some("high"))
    );
    let m402 = resolve_model_alias(Some("MODEL_PLACEHOLDER_M402")).unwrap();
    assert_eq!(
        (m402.canonical, m402.display_name, m402.effort),
        ("claude-opus-5-5", "Claude Opus 5.5", Some("high"))
    );
    assert!(resolve_model_alias(Some("MODEL_GOOGLE_GEMINI_4_ARGON_HIGH")).is_none());
    assert!(resolve_model_alias(Some("MODEL_CLAUDE_4_OPUS_55_HIGH")).is_none());
    let malformed = "<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from A to B\n</USER_SETTINGS_CHANGE>";
    assert!(extract_settings_model(malformed).is_none());
}

#[test]
fn antigravity_provider_attributes_by_request_time_and_clears_historical_backfill() {
    let dir = tempfile::tempdir().unwrap();
    let ag_home = dir.path().join(".gemini");
    let path_pre =
        ag_home.join("antigravity/brain/conv-pre/.system_generated/logs/transcript.jsonl");
    let path1 =
        ag_home.join("antigravity/brain/conv-oauth/.system_generated/logs/transcript.jsonl");
    let path2 =
        ag_home.join("antigravity/brain/conv-second/.system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(path_pre.parent().unwrap()).unwrap();
    std::fs::create_dir_all(path1.parent().unwrap()).unwrap();
    std::fs::create_dir_all(path2.parent().unwrap()).unwrap();

    // Pre-tracking historical request (2026-10-19), pre-switch request (00:30), post-switch request (01:30)
    let row_pre = "{\"step_index\":1,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-10-19T00:00:01Z\",\"model\":\"claude-opus-4-6-thinking\",\"input_tokens\":50,\"output_tokens\":10}\n";
    let row_oauth = "{\"step_index\":1,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-10-20T00:30:00Z\",\"model\":\"claude-opus-4-6-thinking\",\"input_tokens\":100,\"output_tokens\":25}\n";
    let row_second = "{\"step_index\":1,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-10-20T01:30:00Z\",\"model\":\"claude-opus-4-6-thinking\",\"input_tokens\":200,\"output_tokens\":50}\n";
    // Append a half-written trailing line without newline to verify it does not fail sync
    let row_second_with_partial = format!("{row_second}{{\"step_index\":2,\"source\":\"MODEL\"");
    std::fs::write(&path_pre, row_pre).unwrap();
    std::fs::write(&path1, row_oauth).unwrap();
    std::fs::write(&path2, row_second_with_partial).unwrap();

    // The first account was activated at 2026-10-20T00:00:00Z (1792454400000), the second at 2026-10-20T01:00:00Z (1792458000000)
    let vault_json = serde_json::json!({
        "schema": 1,
        "entries": [
            {
                "id": "ag-oauth-1",
                "name": "oauth@example.invalid",
                "kind": "oauth",
                "sourceType": "oauth",
                "email": "oauth@example.invalid",
                "payload": "{\"access_token\":\"tok-1\"}",
                "updatedAt": 1792454400
            },
            {
                "id": "ag-oauth-2",
                "name": "second@example.invalid",
                "kind": "oauth",
                "sourceType": "oauth",
                "email": "second@example.invalid",
                "payload": "{\"access_token\":\"tok-2\"}",
                "updatedAt": 1792458000
            }
        ],
        "activations": [
            { "accountId": "ag-oauth-1", "sinceMs": 1_792_454_400_000i64 },
            { "accountId": "ag-oauth-2", "sinceMs": 1_792_458_000_000i64 }
        ]
    });
    std::fs::write(
        dir.path().join("antigravity-accounts.json"),
        serde_json::to_vec(&vault_json).unwrap(),
    )
    .unwrap();
    // The app is signed in to the second account.
    std::fs::write(
        ag_home.join("jetski-standalone-oauth-token"),
        "{\"access_token\":\"tok-2\"}",
    )
    .unwrap();

    // Sync happens AFTER switching to ag-oauth-2, yet conv-oauth (00:30) is attributed to ag-oauth-1
    // and conv-pre (2026-10-19) stays unattributed (None).
    // Open Store inside the `usage` subdirectory while `antigravity-accounts.json` is in `dir.path()`
    // to verify the production directory layout (`~/.vela` vs `~/.vela/usage`).
    let usage_dir = dir.path().join("usage");
    let mut store = Store::open(&usage_dir).unwrap();
    let pricing = Pricing::new(&usage_dir).unwrap();
    scan_home(
        &mut store,
        &ag_home,
        &pricing,
        &Settings::default(),
        &mut Report::default(),
        false,
    )
    .unwrap();

    let all_ag = Filter {
        client: Some("antigravity".into()),
        ..Filter::default()
    };
    let dash = store.dashboard(&all_ag).unwrap();
    assert_eq!(dash.totals.requests, 3);
    let mut provider_ids: Vec<String> = dash.providers.iter().map(|g| g.id.clone()).collect();
    provider_ids.sort();
    assert_eq!(provider_ids, vec!["ag-oauth-1", "ag-oauth-2", "session"]);

    let stats_all = store.antigravity_stats(&all_ag, 1_800_000_000_000).unwrap();
    let mut stat_provider_ids: Vec<String> =
        stats_all.providers.iter().map(|p| p.id.clone()).collect();
    stat_provider_ids.sort();
    assert_eq!(
        stat_provider_ids,
        vec!["ag-oauth-1", "ag-oauth-2", "session"]
    );

    let oauth_only = Filter {
        client: Some("antigravity".into()),
        provider: Some("ag-oauth-1".into()),
        ..Filter::default()
    };
    assert_eq!(store.dashboard(&oauth_only).unwrap().totals.requests, 1);
    let stats_oauth = store
        .antigravity_stats(&oauth_only, 1_800_000_000_000)
        .unwrap();
    assert_eq!(stats_oauth.requests, 1);
    assert_eq!(stats_oauth.token_mix.total, 125);
    let mut filtered_provider_ids: Vec<String> =
        stats_oauth.providers.iter().map(|p| p.id.clone()).collect();
    filtered_provider_ids.sort();
    assert_eq!(
        filtered_provider_ids,
        vec!["ag-oauth-1", "ag-oauth-2", "session"]
    );
    let oauth_group = stats_oauth
        .providers
        .iter()
        .find(|p| p.id == "ag-oauth-1")
        .unwrap();
    assert!(oauth_group.share_percent < 100.0 && oauth_group.share_percent > 0.0);

    // Verify a single activation span with sinceMs > 0 is NOT zeroed out by provider_timeline.
    let single_span_vault = serde_json::json!({
        "schema": 1,
        "entries": [
            {
                "id": "ag-oauth-1",
                "name": "oauth@example.invalid",
                "kind": "oauth",
                "sourceType": "oauth",
                "email": "oauth@example.invalid",
                "payload": "{\"access_token\":\"tok-1\"}",
                "updatedAt": 1792454400
            }
        ],
        "activations": [
            { "accountId": "ag-oauth-1", "sinceMs": 1_792_454_400_000i64 }
        ]
    });
    std::fs::write(
        dir.path().join("antigravity-accounts.json"),
        serde_json::to_vec(&single_span_vault).unwrap(),
    )
    .unwrap();
    let single_tl = crate::antigravity::auth::provider_timeline(dir.path(), Some(&ag_home));
    assert_eq!(
        single_tl.provider_at(1_792_368_001_000),
        None,
        "requests before a single activation span's sinceMs must remain unattributed"
    );
    assert_eq!(single_tl.provider_at(1_792_456_200_000), Some("ag-oauth-1"));

    let second_only = Filter {
        client: Some("antigravity".into()),
        provider: Some("ag-oauth-2".into()),
        ..Filter::default()
    };
    assert_eq!(store.dashboard(&second_only).unwrap().totals.requests, 1);
    let stats_second = store
        .antigravity_stats(&second_only, 1_800_000_000_000)
        .unwrap();
    assert_eq!(stats_second.requests, 1);
    assert_eq!(stats_second.token_mix.total, 250);

    let session_only = Filter {
        client: Some("antigravity".into()),
        provider: Some("session".into()),
        ..Filter::default()
    };
    assert_eq!(store.dashboard(&session_only).unwrap().totals.requests, 1);
    let stats_session = store
        .antigravity_stats(&session_only, 1_800_000_000_000)
        .unwrap();
    assert_eq!(stats_session.requests, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credential_switch_reuses_incremental_cursors_before_and_after() {
    use crate::antigravity::auth::{self, AppLifecycle};
    struct Lifecycle;
    impl AppLifecycle for Lifecycle {
        fn is_running(&self) -> Result<bool> {
            Ok(false)
        }
        fn quit(&self) -> Result<()> {
            Ok(())
        }
        fn launch(&self) -> Result<()> {
            Ok(())
        }
        fn quit_cli(&self) -> Result<usize> {
            Ok(0)
        }
    }
    let fixture = fixture(false).await;
    let dir = tempfile::tempdir().unwrap();
    let paths = auth::Paths::from_roots(&dir.path().join("home"), &dir.path().join("auth"));
    std::fs::create_dir_all(paths.oauth_file().parent().unwrap()).unwrap();
    // Both logins are recognised; the app ends up signed in to the fixture account.
    std::fs::write(
        paths.oauth_file(),
        br#"{"email":"other@example.invalid","access_token":"fixture-other"}"#,
    )
    .unwrap();
    let other = auth::state_at(&paths).unwrap().accounts[0].id.clone();
    std::fs::write(
        paths.oauth_file(),
        br#"{"email":"fixture@example.invalid","access_token":"fixture-oauth"}"#,
    )
    .unwrap();
    let added = auth::state_at(&paths).unwrap();
    let oauth = added
        .accounts
        .iter()
        .find(|a| a.current)
        .unwrap()
        .id
        .clone();
    assert_ne!(oauth, other);
    let service = crate::usage::Service::new(&paths.data_dir);
    let pricing = Pricing::new(dir.path()).unwrap();
    let settings = Settings::default();
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        TEST_ENDPOINT.with(|endpoint| {
            *endpoint.borrow_mut() = Some((
                fixture.api.endpoints[0].origin.clone(),
                fixture.api.endpoints[0].token.clone(),
            ))
        });
        service
            .query(|store| {
                handle.block_on(collect_home(
                    &fixture.api,
                    store,
                    dir.path(),
                    &pricing,
                    &settings,
                    &mut Report::default(),
                    false,
                ))
            })
            .unwrap();
        let key = safe_id("antigravity:rpc:z-good");
        let before: Cursor = service
            .read(|store| Ok(serde_json::from_str(&store.cursor(&key)?.unwrap()).unwrap()))
            .unwrap();
        let ids = service
            .read(|store| {
                Ok(store
                    .logs(&Filter::default())?
                    .rows
                    .into_iter()
                    .map(|r| r.id)
                    .collect::<Vec<_>>())
            })
            .unwrap();
        fixture.calls.lock().clear();
        let state = auth::with_usage_sync(
            || {
                service
                    .sync_roots(
                        vec![("antigravity".into(), Ok(paths.gemini_home.clone()))],
                        None,
                    )
                    .unwrap();
                service
                    .read(|store| {
                        let cursor: Cursor =
                            serde_json::from_str(&store.cursor(&key)?.unwrap()).unwrap();
                        assert!(cursor.count >= before.count);
                        assert!(cursor.covered.is_superset(&before.covered));
                        for id in &ids {
                            assert_eq!(&store.detail(id)?.id, id);
                        }
                        Ok(())
                    })
                    .unwrap();
            },
            || {
                let switched =
                    auth::switch_account_at(&paths, &Lifecycle, &other, &added.revision)?;
                *fixture.changed.lock() = "after-switch".into();
                Ok(switched)
            },
        )
        .unwrap();
        service
            .read(|store| {
                let saved: String = store
                    .connection
                    .query_row(
                        "SELECT value FROM metadata WHERE key=?1",
                        [format!("antigravity_quota:{oauth}")],
                        |r| r.get(0),
                    )
                    .unwrap();
                let periods: quota::Periods = serde_json::from_str(&saved).unwrap();
                assert!(periods.contains_key("gemini-5h"));
                Ok(())
            })
            .unwrap();
        TEST_ENDPOINT.with(|endpoint| *endpoint.borrow_mut() = None);
        assert_eq!(auth::active_account_id(&state), Some(other));
        assert_eq!(
            service
                .read(|store| Ok(store.dashboard(&Filter::default())?.totals.requests))
                .unwrap(),
            2
        );
        let calls = fixture.calls.lock();
        assert!(!calls.iter().any(|(m, _)| m == "GetCascadeTrajectory"));
        assert!(calls
            .iter()
            .any(|(m, b)| m == "GetCascadeTrajectorySteps" && b["stepOffset"] == 1));
        assert!(!calls
            .iter()
            .any(|(m, b)| m == "GetCascadeTrajectorySteps" && b["stepOffset"] == 0));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn session_title_changes_only_cursor_metadata_and_survives_app_closure() {
    let fixture = fixture(false).await;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();
    let settings = Settings::default();
    let timeline = crate::antigravity::auth::ProviderTimeline::default();
    let mut summary =
        json!({"summary":"初始会话","lastModifiedTime":"fixture-stable","stepCount":2});
    fixture
        .api
        .collect_session(
            "z-good",
            Some(0),
            &summary,
            &mut store,
            &pricing,
            &settings,
            &mut Report::default(),
            false,
            &timeline,
        )
        .await
        .unwrap();
    let cursor_id = safe_id("antigravity:rpc:z-good");
    let mut legacy: Value =
        serde_json::from_str(&store.cursor(&cursor_id).unwrap().unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("session_id");
    legacy.as_object_mut().unwrap().remove("title");
    store
        .write_batch(&[], Some((&cursor_id, &legacy.to_string())))
        .unwrap();
    let changes = store.connection.total_changes();
    let no_title = json!({"lastModifiedTime":"fixture-stable","stepCount":2});
    fixture
        .api
        .collect_session(
            "z-good",
            Some(0),
            &no_title,
            &mut store,
            &pricing,
            &settings,
            &mut Report::default(),
            false,
            &timeline,
        )
        .await
        .unwrap();
    assert_eq!(
        store.connection.total_changes(),
        changes,
        "an unchanged untitled legacy cursor needs no write"
    );
    let before = store.logs(&Filter::default()).unwrap();
    fixture.calls.lock().clear();
    summary["summary"] = json!("修改后的会话");
    fixture
        .api
        .collect_session(
            "z-good",
            Some(0),
            &summary,
            &mut store,
            &pricing,
            &settings,
            &mut Report::default(),
            false,
            &timeline,
        )
        .await
        .unwrap();
    assert!(fixture.calls.lock().is_empty());
    let after = store.logs(&Filter::default()).unwrap();
    assert_eq!(
        serde_json::to_value(before.rows).unwrap(),
        serde_json::to_value(after.rows).unwrap()
    );
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    let saved_cursor: Value =
        serde_json::from_str(&store.cursor(&cursor_id).unwrap().unwrap()).unwrap();
    assert_eq!(saved_cursor["title"].as_str(), Some("修改后的会话"));
    assert_eq!(saved_cursor["session_id"], safe_id("z-good"));
}

#[test]
fn retry_attempt_without_model_inherits_final_attempt_model_and_repairs_existing_records() {
    let mut v = entry("retry-inherit", 0);
    v["chatModel"]["model"] = json!("MODEL_PLACEHOLDER_UNKNOWN_FUTURE");
    v["chatModel"]["responseModel"] = json!("gemini-3.1-pro-high");
    v["chatModel"]["modelDisplayName"] = json!("Gemini 4 Argon (High)");
    v["chatModel"]["retryInfos"] = json!([
        {
            "traceId": "retry-0",
            "error": "unavailable",
            "startedAt": "2026-01-01T00:00:00Z",
            "completedAt": "2026-01-01T00:00:01Z",
            "usage": {
                "inputTokens": "10",
                "outputTokens": "0"
            }
        }
    ]);
    let r = parsed(&v);
    assert_eq!(r.attempts.len(), 2);
    // An enum no list places keeps the backend name, less its effort suffix.
    assert_eq!(
        r.attempts[0].response_model.as_deref(),
        Some("gemini-3.1-pro")
    );
    assert_eq!(r.attempts[0].model_display_name, None);

    // Simulate a legacy DB record whose first retry attempt had no model fields.
    let mut legacy = r.clone();
    legacy.attempts[0].requested_model = None;
    legacy.attempts[0].response_model = None;
    legacy.attempts[0].pricing_model = None;
    assert!(normalize_record(&mut legacy));
    assert!(!normalize_record(&mut legacy));
    assert_eq!(
        legacy.attempts[0].response_model.as_deref(),
        Some("gemini-3.1-pro")
    );
    assert_eq!(legacy.attempts[0].model_display_name, None);
}

#[test]
fn settled_transcript_purges_newly_covered_steps_and_inherits_rpc_last_model() {
    let dir = tempfile::tempdir().unwrap();
    let brain = dir.path().join("antigravity/brain");
    let path = brain
        .join("subagent-cascade")
        .join(".system_generated")
        .join("logs")
        .join("transcript.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let step6 = "{\"type\":\"PLANNER_RESPONSE\",\"step_index\":6,\"status\":\"DONE\",\"created_at\":\"2026-01-01T00:00:00Z\",\"input_tokens\":12,\"output_tokens\":4}\n";
    let step7 = "{\"type\":\"PLANNER_RESPONSE\",\"step_index\":7,\"status\":\"DONE\",\"created_at\":\"2026-01-01T00:00:05Z\",\"input_tokens\":15,\"output_tokens\":5}\n";
    std::fs::write(&path, format!("{step6}{step7}")).unwrap();

    let db_dir = dir.path().join("db");
    let mut store = Store::open(&db_dir).unwrap();
    let pricing = Pricing::new(&db_dir).unwrap();

    let mut report = Report::default();
    scan_home(
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut report,
        false,
    )
    .unwrap();
    let step6_id = safe_id("antigravity:request:subagent-cascade:step:6");
    let step7_id = safe_id("antigravity:request:subagent-cascade:step:7");
    assert!(store.detail(&step6_id).is_ok());
    assert!(store.detail(&step7_id).is_ok());

    // Mark file cursor as complete with purged_covered = 0, then record RPC coverage for step 6
    // and last_model on the RPC cursor.
    let file_key = safe_id(&format!("antigravity:file:{}", path.to_string_lossy()));
    let mut fc: FileCursor =
        serde_json::from_str(&store.cursor(&file_key).unwrap().unwrap()).unwrap();
    fc.complete = true;
    fc.purged_covered = 0;
    let meta = std::fs::metadata(&path).unwrap();
    fc.modified = file_modified(&meta).unwrap_or_default();
    store
        .write_batch(&[], Some((&file_key, &serde_json::to_string(&fc).unwrap())))
        .unwrap();

    let mut rpc_record = parsed(&entry("rpc-step-6", 0));
    rpc_record.session_id = Some(safe_id("subagent-cascade"));
    let rpc = serde_json::to_string(&Cursor {
        covered: BTreeSet::from([6]),
        last_model: Some("gemini-3.1-pro".into()),
        ..Cursor::default()
    })
    .unwrap();
    store
        .write_batch(
            &[rpc_record],
            Some((&safe_id("antigravity:rpc:subagent-cascade"), &rpc)),
        )
        .unwrap();

    // Re-scan with repair=false; step 6 must be purged and step 7 must inherit rpc.last_model.
    let mut report = Report::default();
    scan_home(
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut report,
        false,
    )
    .unwrap();
    assert!(store.detail(&step6_id).is_err());
    let step7_record = store.detail(&step7_id).unwrap();
    assert_eq!(
        step7_record.attempts[0].response_model.as_deref(),
        Some("gemini-3.1-pro")
    );

    // Mark file cursor complete again (now with fc.model = Some("gemini-3.1-pro")) and expand
    // rpc.covered to include step 7; repair=false early-return path must purge step 7.
    let mut fc: FileCursor =
        serde_json::from_str(&store.cursor(&file_key).unwrap().unwrap()).unwrap();
    assert_eq!(fc.model.as_deref(), Some("gemini-3.1-pro"));
    fc.complete = true;
    fc.modified = file_modified(&meta).unwrap_or_default();
    store
        .write_batch(&[], Some((&file_key, &serde_json::to_string(&fc).unwrap())))
        .unwrap();
    let rpc = serde_json::to_string(&Cursor {
        covered: BTreeSet::from([6, 7]),
        last_model: Some("gemini-3.1-pro".into()),
        ..Cursor::default()
    })
    .unwrap();
    store
        .write_batch(
            &[],
            Some((&safe_id("antigravity:rpc:subagent-cascade"), &rpc)),
        )
        .unwrap();
    let mut report = Report::default();
    scan_home(
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut report,
        false,
    )
    .unwrap();
    assert!(store.detail(&step7_id).is_err());
}

#[test]
fn antigravity_cache_hit_rate_is_computed_without_cache_write_for_new_and_legacy_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    while store.compact_storage_batch().unwrap() {}

    // 1. Newly parsed RPC record with cacheReadTokens present and cacheWriteTokens omitted,
    // plus a second RPC record with cacheReadTokens omitted (0 cache hits in Protobuf3 JSON).
    let hit = parsed(&entry("hit-turn", 0));
    assert_eq!(hit.attempts[0].tokens.input, Some(100));
    assert_eq!(hit.attempts[0].tokens.cache_read, Some(40));
    assert_eq!(hit.attempts[0].tokens.cache_write, None);
    assert_eq!(hit.attempts[0].tokens.inclusive_input, Some(140));
    assert_eq!(hit.attempts[0].tokens.cache_sample(), Some((40, 140)));

    let mut miss_json = entry("miss-turn", 0);
    miss_json["chatModel"]["retryInfos"] = json!([]);
    miss_json["chatModel"]["usage"]
        .as_object_mut()
        .unwrap()
        .remove("cacheReadTokens");
    let miss = parsed(&miss_json);
    assert_eq!(miss.attempts[0].tokens.input, Some(100));
    assert_eq!(miss.attempts[0].tokens.cache_read, Some(0));
    assert_eq!(miss.attempts[0].tokens.cache_write, None);
    assert_eq!(miss.attempts[0].tokens.inclusive_input, Some(100));
    assert_eq!(miss.attempts[0].tokens.cache_sample(), Some((0, 100)));

    // 2. Simulate a pre-existing un-normalized row in v12 where inclusive_input and cache_write are None.
    let mut legacy = hit.clone();
    legacy.id = safe_id("antigravity:request:legacy-turn");
    legacy.attempts[0].id = safe_id("antigravity:request:legacy-turn:final");
    legacy.attempts[0].response_id = Some("response-legacy-turn".into());
    legacy.attempts[0].tokens.inclusive_input = None;
    legacy.attempts[0].inclusive_input_tokens = None;

    store.write_batch(&[hit, miss, legacy], None).unwrap();

    // Total cache_read = 40 + 0 + 40 = 80; total eligible input = 140 + 100 + 140 = 380 -> 21.1%
    let stats = store.antigravity_stats(&Filter::default(), now()).unwrap();
    assert_eq!(stats.token_mix.cache_read_eligible, 80);
    assert_eq!(stats.token_mix.cache_input_eligible, 380);
    assert_eq!(stats.token_mix.cache_hit_percent, Some(21.1));

    let dash = store
        .dashboard(&Filter {
            client: Some("antigravity".into()),
            ..Filter::default()
        })
        .unwrap();
    assert_eq!(dash.totals.cache_read_eligible, 80);
    assert_eq!(dash.totals.cache_input_eligible, 380);

    let logs = store
        .logs(&Filter {
            client: Some("antigravity".into()),
            ..Filter::default()
        })
        .unwrap();
    assert_eq!(logs.rows.len(), 3);
    assert!(logs
        .rows
        .iter()
        .all(|item| item.tokens().cache_sample().is_some()));
}

/// One loopback window answering every read with `reply`.
async fn window(reply: Value) -> (Api, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let reply = reply.clone();
            tokio::spawn(async move {
                let service = service_fn(move |_: hyper::Request<hyper::body::Incoming>| {
                    let bytes = Bytes::from(serde_json::to_vec(&reply).unwrap());
                    async move { Ok::<_, std::convert::Infallible>(Response::new(Full::new(bytes))) }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                    .await;
            });
        }
    });
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    (Api::from_single(http, origin, "fixture".into()), server)
}

#[tokio::test]
async fn an_app_without_loaded_conversations_is_an_empty_catalog_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir
        .path()
        .join("antigravity/brain/local-only/transcript.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &transcript,
        "{\"type\":\"PLANNER_RESPONSE\",\"step_index\":0,\"created_at\":\"2026-01-01T00:00:00Z\",\"input_tokens\":50}\n",
    )
    .unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();

    // The app answers `{}` when it has no conversation loaded.
    let (api, server) = window(json!({})).await;
    let mut report = Report::default();
    collect_home(
        &api,
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut report,
        false,
    )
    .await
    .unwrap();
    server.abort();
    assert_eq!((report.errors, report.files), (0, 1));
    assert_eq!(report.last_error, None);

    // Any other shape still is an incomplete catalog.
    let (api, server) = window(json!({"unexpected":"fixture"})).await;
    let error = collect_home(
        &api,
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut Report::default(),
        false,
    )
    .await
    .unwrap_err();
    server.abort();
    assert_eq!(error.message, "Antigravity 会话目录不完整");
}

#[tokio::test]
async fn pending_rpc_usage_completes_without_errors_or_duplicate_accounting() {
    let f = fixture(false).await;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();
    let mut pending = entry("pending", 0);
    pending["executionId"] = json!("pending-execution");
    pending["stepIndices"] = json!([]);
    pending["chatModel"]["startedAt"] = json!("2026-01-01T00:00:00Z");
    pending["chatModel"]
        .as_object_mut()
        .unwrap()
        .remove("streamingDuration");
    *f.entries.lock() = vec![pending];
    let cursor_id = safe_id("antigravity:rpc:z-good");
    let row_id = safe_id("antigravity:request:pending");
    for completed in [false, true, true] {
        if completed {
            f.entries.lock()[0]["chatModel"]["completedAt"] = json!("2026-01-01T00:00:02Z");
        }
        let mut report = Report::default();
        collect_home(
            &f.api,
            &mut store,
            dir.path(),
            &pricing,
            &Settings::default(),
            &mut report,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.errors, 0);
        assert_eq!(report.last_error, None);
        let row = store.detail(&row_id).unwrap();
        assert_eq!(row.known_success(), completed.then_some(true));
        assert_eq!(row.tokens().total(), Some(160));
        assert_eq!(store.logs(&Filter::default()).unwrap().total, 1);
        let cursor: Cursor =
            serde_json::from_str(&store.cursor(&cursor_id).unwrap().unwrap()).unwrap();
        assert_eq!(cursor.count, usize::from(completed));
    }
}

#[tokio::test]
async fn malformed_rpc_request_reports_the_first_concrete_reason() {
    let f = fixture(false).await;
    {
        let mut entries = f.entries.lock();
        entries[0].as_object_mut().unwrap().remove("executionId");
        entries[0]["chatModel"]
            .as_object_mut()
            .unwrap()
            .remove("customMetadata");
        entries[1]["executionId"] = json!("missing-time");
        entries[1]["stepIndices"] = json!([]);
    }
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();
    let mut report = Report::default();
    collect_home(
        &f.api,
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut report,
        false,
    )
    .await
    .unwrap();
    assert_eq!(report.errors, 2);
    assert_eq!(
        report.last_error.as_deref(),
        Some("Antigravity 用量缺少可靠请求标识，未计入账单")
    );
}

#[test]
fn history_read_and_line_failures_keep_the_first_reason() {
    let dir = tempfile::tempdir().unwrap();
    let brain = dir.path().join("antigravity/brain");
    let path = brain.join("fixture/transcript.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{invalid}\n{\"input_tokens\":10}\n").unwrap();
    let mut store = Store::open(&dir.path().join("db")).unwrap();
    let pricing = Pricing::new(&dir.path().join("db")).unwrap();
    let settings = Settings::default();
    let missing = brain.join("missing/transcript.jsonl");
    let mut report = Report::default();
    scan_file_list(
        &mut store,
        &brain,
        &[path, missing.clone()],
        &pricing,
        &settings,
        &mut report,
        false,
    )
    .unwrap();
    assert_eq!(report.errors, 3);
    assert_eq!(
        report.last_error.as_deref(),
        Some("Antigravity 历史文件存在无效完整行")
    );
    let mut report = Report::default();
    scan_file_list(
        &mut store,
        &brain,
        &[missing],
        &pricing,
        &settings,
        &mut report,
        false,
    )
    .unwrap();
    assert_eq!(report.errors, 1);
    assert!(report.last_error.is_some());
}

/// A language server that answers only the token it was started with.
async fn hub(token: &'static str) -> (u16, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let paths = seen.clone();
    let server = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let paths = paths.clone();
            tokio::spawn(async move {
                let service = service_fn(move |r: hyper::Request<hyper::body::Incoming>| {
                    paths.lock().push(r.uri().path().to_owned());
                    let allowed = r
                        .headers()
                        .get("x-codeium-csrf-token")
                        .is_some_and(|sent| sent == token);
                    async move {
                        let mut response = Response::new(Full::new(Bytes::from_static(b"{}")));
                        if !allowed {
                            *response.status_mut() = hyper::StatusCode::UNAUTHORIZED;
                        }
                        Ok::<_, std::convert::Infallible>(response)
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                    .await;
            });
        }
    });
    (port, seen, server)
}

/// The port of an earlier launch: every connection is closed unanswered.
async fn closed_port() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            drop(socket);
        }
    });
    (port, server)
}

#[tokio::test]
async fn daemon_files_connect_only_a_live_instance_before_the_window_port_is_tried() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = dir.path().join("antigravity/daemon");
    std::fs::create_dir_all(&daemon).unwrap();
    let (live, live_seen, live_server) = hub("fixture-live").await;
    let (stale, stale_server) = closed_port().await;
    let (window, window_seen, window_server) = hub("fixture-window").await;
    let publish = |name: &str, https: u16, http: u16, token: &str| {
        let file = json!({"pid":1,"httpsPort":https,"httpPort":http,"lsVersion":"fixture","csrfToken":token});
        std::fs::write(daemon.join(name), file.to_string()).unwrap();
    };
    // A file an earlier launch left behind, and a live port whose token was replaced (401).
    publish("hub_1.json", stale, stale, "fixture-live");
    publish("hub_2.json", live, live, "fixture-replaced");
    let port_file = dir.path().join("DevToolsActivePort");

    // Nothing answers and no window port is published: the app is not running.
    assert!(Api::connect_at(dir.path(), Some(&port_file))
        .await
        .unwrap()
        .is_none());
    assert!(!live_seen.lock().is_empty());
    assert!(live_seen
        .lock()
        .iter()
        .all(|path| path.ends_with("/GetStatus")));

    // Only then is the window port of earlier releases asked.
    std::fs::write(&port_file, format!("{window}\n/devtools/browser/fixture\n")).unwrap();
    assert!(Api::connect_at(dir.path(), Some(&port_file)).await.is_err());
    assert_eq!(*window_seen.lock(), ["/json/list"]);

    // https comes first and gives way to http; a second file of the same
    // instance, a foreign file name and an oversized file add nothing.
    publish("hub_3.json", stale, live, "fixture-live");
    publish("ls_3.json", stale, live, "fixture-live");
    publish("other_3.json", live, live, "fixture-live");
    std::fs::write(daemon.join("hub_4.json"), " ".repeat(8193)).unwrap();
    window_seen.lock().clear();
    let api = Api::connect_at(dir.path(), Some(&port_file))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(api.endpoints.len(), 1);
    assert_eq!(api.endpoints[0].origin, format!("http://127.0.0.1:{live}"));
    assert_eq!(api.endpoints[0].token, "fixture-live");
    assert!(window_seen.lock().is_empty());
    for server in [live_server, stale_server, window_server] {
        server.abort();
    }
}

#[test]
fn models_resolve_by_listed_pair_then_seed_then_enum_then_backend_name() {
    let steps = Steps::from([step()].as_slice());
    let mut list = Learned::default();
    assert!(!list.merge(&json!({}), 1));
    assert!(list.merge(
        &json!({"response":{"models":{
            "fixture-neon-tiered-high":{"model":"MODEL_PLACEHOLDER_M900","displayName":"Fixture Neon (High)"},
            // The app reused an enum the seed still knows as Gemini 3 Pro.
            "fixture-flash-lite":{"model":"MODEL_PLACEHOLDER_M19","displayName":"Fixture Flash Lite"},
            "fixture-carbon-low":{"model":"MODEL_PLACEHOLDER_M901","displayName":"Fixture Carbon (Low)"},
            "fixture person@example.invalid":{"model":"MODEL_PLACEHOLDER_M903"}
        }}}),
        1
    ));
    assert_eq!(list.models.len(), 3);
    let resolved = |model: &str, shown: &str, backend: &str| {
        let mut v = entry("one", 0);
        v["chatModel"]["model"] = json!(model);
        v["chatModel"]["usage"]["model"] = json!(model);
        v["chatModel"]["retryInfos"][0]["usage"]["model"] = json!(model);
        v["chatModel"]["modelDisplayName"] = json!(shown);
        v["chatModel"]["responseModel"] = json!(backend);
        let r = record("fixture", &v, &steps, 0, &list).unwrap().unwrap();
        let a = r.attempts[0].clone();
        (
            a.response_model,
            a.requested_model,
            a.model_display_name,
            a.reasoning_effort,
        )
    };
    let text = |s: &str| Some(s.to_owned());
    // 1. The listed (enum, label) pair, less the effort its label shows and `-tiered`.
    assert_eq!(
        resolved(
            "MODEL_PLACEHOLDER_M900",
            "Fixture Neon (High)",
            "fixture-backend-f"
        ),
        (
            text("fixture-neon"),
            text("fixture-neon"),
            text("Fixture Neon"),
            text("high")
        )
    );
    assert_eq!(
        resolved(
            "MODEL_PLACEHOLDER_M19",
            "Fixture Flash Lite",
            "fixture-backend"
        ),
        (
            text("fixture-flash-lite"),
            text("fixture-flash-lite"),
            text("Fixture Flash Lite"),
            None
        )
    );
    // 2. Without that pair the seed decides, yet label and effort stay the entry's own.
    assert_eq!(
        resolved(
            "MODEL_PLACEHOLDER_M19",
            "Gemini 3 Pro (Low)",
            "fixture-backend"
        ),
        (
            text("gemini-3-pro-preview"),
            text("gemini-3-pro-preview"),
            text("Gemini 3 Pro"),
            text("low")
        )
    );
    // 3. An enum only the list knows, under a label it does not show.
    assert_eq!(
        resolved(
            "MODEL_PLACEHOLDER_M901",
            "Fixture Carbon Renamed (High)",
            "fixture-backend-high"
        ),
        (
            text("fixture-carbon"),
            text("fixture-carbon"),
            text("Fixture Carbon Renamed"),
            text("high")
        )
    );
    // 4. An enum nothing places: the backend name without its effort suffix.
    assert_eq!(
        resolved(
            "MODEL_PLACEHOLDER_M902",
            "Fixture Argon (XHigh)",
            "fixture-argon-xhigh"
        ),
        (
            text("fixture-argon"),
            None,
            text("Fixture Argon"),
            text("xhigh")
        )
    );
    assert_eq!(
        resolved(
            "MODEL_PLACEHOLDER_M902",
            "Fixture Argon",
            "fixture-argon-max"
        ),
        (
            text("fixture-argon"),
            None,
            text("Fixture Argon"),
            text("max")
        )
    );

    // A later list keeps what it no longer names, and a reused enum means
    // its newest model unless the label says otherwise.
    assert!(list.merge(
        &json!({"response":{"models":{
            "fixture-xenon":{"model":"MODEL_PLACEHOLDER_M901"},
            "fixture-hydrogen-tiered-high":{"model":"MODEL_PLACEHOLDER_M904"}
        }}}),
        2
    ));
    assert_eq!((list.models.len(), list.fetched_at), (5, 2));
    assert_eq!(
        list.resolve("MODEL_PLACEHOLDER_M901", None).as_deref(),
        Some("fixture-xenon")
    );
    assert_eq!(
        list.resolve("MODEL_PLACEHOLDER_M901", Some("Fixture Carbon (Low)"))
            .as_deref(),
        Some("fixture-carbon")
    );
    // An entry listed without a label drops the effort the request shows.
    for (shown, model) in [
        ("Fixture Hydrogen (High)", "fixture-hydrogen"),
        ("Fixture Hydrogen (Low)", "fixture-hydrogen-tiered-high"),
    ] {
        assert_eq!(
            list.resolve("MODEL_PLACEHOLDER_M904", Some(shown))
                .as_deref(),
            Some(model)
        );
    }
}

#[tokio::test]
async fn the_model_list_is_learned_once_a_day_and_again_for_an_unplaced_model() {
    let f = fixture(false).await;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();
    let settings = Settings::default();
    let asked = |f: &Fixture| {
        f.calls
            .lock()
            .iter()
            .filter(|(m, _)| m == "GetAvailableModels")
            .count()
    };
    *f.models.lock() = json!({"response":{"models":{"fixture-neon-high":{"model":"MODEL_PLACEHOLDER_FIXTURE","displayName":"Fixture Model (High)"}}}});
    let mut sync = Vec::new();
    for (step, repair) in [(0, false), (1, false), (2, true), (3, false)] {
        match step {
            // Within a day, with every model placed, the app is not asked again.
            1 => *f.changed.lock() = "next".into(),
            // A label the list does not pair asks once in that run.
            2 => {
                for entry in f.entries.lock().iter_mut() {
                    entry["chatModel"]["modelDisplayName"] = json!("Fixture Renamed (High)");
                }
            }
            // A list older than a day is fetched again.
            3 => {
                let mut saved: Learned =
                    serde_json::from_str(&store.antigravity_models().unwrap().unwrap()).unwrap();
                assert!(saved.models.contains_key("fixture-neon-high"));
                saved.fetched_at -= 86_400_000;
                store
                    .save_antigravity_models(&serde_json::to_string(&saved).unwrap())
                    .unwrap();
            }
            _ => {}
        }
        collect_home(
            &f.api,
            &mut store,
            dir.path(),
            &pricing,
            &settings,
            &mut Report::default(),
            repair,
        )
        .await
        .unwrap();
        sync.push(asked(&f));
    }
    assert_eq!(sync, [1, 1, 2, 3]);
    // The list was there before the first session was read.
    let first = store.detail(&parsed(&entry("first", 0)).id).unwrap();
    assert_eq!(
        (
            first.attempts[0].requested_model.as_deref(),
            first.attempts[0].response_model.as_deref()
        ),
        (Some("fixture-neon"), Some("fixture-neon"))
    );
}

#[test]
fn transcript_models_resolve_through_the_list_and_keep_the_selected_label() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("antigravity/brain/cascade/.system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let rows = concat!(
        "{\"step_index\":0,\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"created_at\":\"2026-01-01T00:00:00Z\",\"content\":\"<USER_SETTINGS_CHANGE>\\nThe user changed setting `Model Selection` from None to Fixture Argon (XHigh). No need to comment on this change.\\n</USER_SETTINGS_CHANGE>\"}\n",
        "{\"step_index\":1,\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-01-01T00:00:01Z\",\"model\":\"fixture-argon-xhigh\",\"input_tokens\":10,\"output_tokens\":2}\n",
        "{\"step_index\":2,\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-01-01T00:00:02Z\",\"model\":\"fixture-neon-tiered-high\",\"input_tokens\":10,\"output_tokens\":2}\n",
        "{\"step_index\":3,\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-01-01T00:00:03Z\",\"model\":\"fixture-argon-f\",\"input_tokens\":10,\"output_tokens\":2}\n",
    );
    std::fs::write(&path, rows).unwrap();
    let mut store = Store::open(&dir.path().join("db")).unwrap();
    let pricing = Pricing::new(&dir.path().join("db")).unwrap();
    let mut list = Learned::default();
    list.merge(
        &json!({"response":{"models":{"fixture-neon-tiered-high":{"model":"MODEL_PLACEHOLDER_M900","displayName":"Fixture Neon (High)"}}}}),
        1,
    );
    store
        .save_antigravity_models(&serde_json::to_string(&list).unwrap())
        .unwrap();
    let mut report = Report::default();
    scan_home(
        &mut store,
        dir.path(),
        &pricing,
        &Settings::default(),
        &mut report,
        false,
    )
    .unwrap();
    assert_eq!((report.errors, report.imported), (0, 3));
    let row = |step: u64| {
        let r = store
            .detail(&safe_id(&format!(
                "antigravity:request:cascade:step:{step}"
            )))
            .unwrap();
        let a = r.attempts[0].clone();
        (a.response_model, a.model_display_name, a.reasoning_effort)
    };
    let text = |s: &str| Some(s.to_owned());
    // A backend name the list does not know only loses its effort suffix,
    // and keeps the label the user selected.
    assert_eq!(
        row(1),
        (text("fixture-argon"), text("Fixture Argon"), text("xhigh"))
    );
    // A listed id resolves through its own entry, label included.
    assert_eq!(
        row(2),
        (text("fixture-neon"), text("Fixture Neon"), text("high"))
    );
    // Any other suffix is part of the name.
    assert_eq!(
        row(3),
        (
            text("fixture-argon-f"),
            text("Fixture Argon"),
            text("xhigh")
        )
    );

    // A model id inherited from the app's own reading is not a label.
    let other = dir
        .path()
        .join("antigravity/brain/other/.system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(other.parent().unwrap()).unwrap();
    let rpc = serde_json::to_string(&Cursor {
        last_model: Some("fixture-carbon".into()),
        ..Cursor::default()
    })
    .unwrap();
    store
        .write_batch(&[], Some((&safe_id("antigravity:rpc:other"), &rpc)))
        .unwrap();
    let mut written = String::new();
    for step in 1..=2 {
        written += &format!("{{\"step_index\":{step},\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-01-01T00:00:0{step}Z\",\"model\":\"fixture-argon-f\",\"input_tokens\":10,\"output_tokens\":2}}\n");
        std::fs::write(&other, &written).unwrap();
        scan_home(
            &mut store,
            dir.path(),
            &pricing,
            &Settings::default(),
            &mut report,
            false,
        )
        .unwrap();
    }
    let later = store
        .detail(&safe_id("antigravity:request:other:step:2"))
        .unwrap();
    assert_eq!(
        (
            later.attempts[0].response_model.as_deref(),
            later.attempts[0].model_display_name.as_deref()
        ),
        (Some("fixture-argon-f"), None)
    );
}

#[tokio::test]
async fn unlisted_conversations_are_read_by_id_and_unclaimed_ones_wait_for_a_change() {
    let f = fixture(false).await;
    let dir = tempfile::tempdir().unwrap();
    let conversations = dir.path().join("antigravity/conversations");
    std::fs::create_dir_all(&conversations).unwrap();
    for id in ["a-unlisted", "a-foreign", "a-empty", "z-good"] {
        std::fs::write(conversations.join(format!("{id}.db")), b"fixture").unwrap();
    }
    std::fs::write(conversations.join("a-unlisted.db-wal"), b"fixture").unwrap();
    let mut store = Store::open(&dir.path().join("db")).unwrap();
    let pricing = Pricing::new(&dir.path().join("db")).unwrap();
    let settings = Settings::default();
    let asked = |id: &str| {
        f.calls
            .lock()
            .iter()
            .filter(|(m, b)| m == "GetCascadeTrajectoryGeneratorMetadata" && b["cascadeId"] == id)
            .count()
    };
    let stamp = |id: &str| {
        let db = conversations.join(format!("{id}.db"));
        format!(
            "{}+{}",
            local_file_stamp(&db),
            local_file_stamp(&db.with_extension("db-wal"))
        )
    };
    let cursor = |store: &Store, id: &str| -> Cursor {
        let saved = store.cursor(&safe_id(&format!("antigravity:rpc:{id}")));
        serde_json::from_str(&saved.unwrap().unwrap()).unwrap()
    };
    let mut report = Report::default();
    collect_home(
        &f.api,
        &mut store,
        dir.path(),
        &pricing,
        &settings,
        &mut report,
        false,
    )
    .await
    .unwrap();
    // The app lists one session; the unlisted one answers by id just as fully.
    assert_eq!(
        (
            report.total_files,
            report.files,
            report.errors,
            report.imported
        ),
        (4, 4, 0, 4)
    );
    assert_eq!(
        store.dashboard(&Filter::default()).unwrap().totals.requests,
        4
    );
    // The page that claimed the conversation is not fetched a second time.
    assert_eq!(asked("a-unlisted"), 3);
    let unlisted = cursor(&store, "a-unlisted");
    assert_eq!(
        (unlisted.count, unlisted.local_stamp),
        (2, stamp("a-unlisted"))
    );
    assert_eq!(cursor(&store, "z-good").local_stamp, "");
    // 404 is another instance's conversation and an empty reply claims
    // nothing: both are remembered without an error.
    for id in ["a-foreign", "a-empty"] {
        let unclaimed = cursor(&store, id);
        assert_eq!((unclaimed.count, unclaimed.local_stamp), (0, stamp(id)));
        assert_eq!(asked(id), 1);
    }

    // An unchanged database is not asked for again.
    f.calls.lock().clear();
    collect_home(
        &f.api,
        &mut store,
        dir.path(),
        &pricing,
        &settings,
        &mut Report::default(),
        false,
    )
    .await
    .unwrap();
    for id in ["a-unlisted", "a-foreign", "a-empty"] {
        assert_eq!(asked(id), 0, "{id}");
    }

    // A changed one is, and so is one whose write-ahead log alone changed.
    std::fs::write(conversations.join("a-foreign.db"), b"fixture changed").unwrap();
    std::fs::write(conversations.join("a-unlisted.db-wal"), b"fixture changed").unwrap();
    collect_home(
        &f.api,
        &mut store,
        dir.path(),
        &pricing,
        &settings,
        &mut Report::default(),
        false,
    )
    .await
    .unwrap();
    assert_eq!((asked("a-foreign"), asked("a-empty")), (1, 0));
    assert!(asked("a-unlisted") > 0);
    for id in ["a-foreign", "a-unlisted"] {
        assert_eq!(cursor(&store, id).local_stamp, stamp(id), "{id}");
    }
    assert_eq!(
        store.dashboard(&Filter::default()).unwrap().totals.requests,
        4
    );
}

#[test]
fn omitted_zero_counters_complete_a_reported_usage_but_never_invent_one() {
    let mut v = entry("one", 0);
    v["chatModel"].as_object_mut().unwrap().remove("retryInfos");
    for key in [
        "outputTokens",
        "cacheReadTokens",
        "thinkingOutputTokens",
        "responseOutputTokens",
    ] {
        v["chatModel"]["usage"].as_object_mut().unwrap().remove(key);
    }
    let r = parsed(&v);
    let a = &r.attempts[0];
    assert_eq!(
        (a.outcome.as_str(), r.known_success()),
        ("success", Some(true))
    );
    assert_eq!(
        (
            a.tokens.input,
            a.tokens.output,
            a.tokens.cache_read,
            a.tokens.cache_write
        ),
        (Some(100), Some(0), Some(0), None)
    );
    assert_eq!(a.usage_status, "reported");

    // A failed request keeps exactly what it reported.
    v["error"] = json!("fixture rejected");
    let r = parsed(&v);
    assert_eq!(
        (r.attempts[0].outcome.as_str(), r.attempts[0].tokens.output),
        ("failed", None)
    );
    v.as_object_mut().unwrap().remove("error");

    // So does a request that is still running.
    for key in ["timeToFirstToken", "streamingDuration"] {
        v["chatModel"].as_object_mut().unwrap().remove(key);
    }
    let running = Steps::from(
        [json!({"metadata":{"executionId":"exec","startedAt":"2026-01-01T00:00:00Z"}})].as_slice(),
    );
    let r = record("fixture", &v, &running, 0, &Learned::default())
        .unwrap()
        .unwrap();
    assert_eq!(
        (r.attempts[0].outcome.as_str(), r.attempts[0].tokens.output),
        ("in_progress", None)
    );

    // Completion without any usage reports no tokens at all.
    let mut silent = entry("one", 0);
    for key in ["retryInfos", "usage"] {
        silent["chatModel"].as_object_mut().unwrap().remove(key);
    }
    let r = parsed(&silent);
    assert_eq!(
        (r.attempts[0].outcome.as_str(), r.tokens().total()),
        ("unknown", None)
    );
}

#[test]
fn an_error_on_the_entry_fails_the_request_and_drops_clocks_nothing_measured() {
    let mut v = entry("one", 0);
    for key in ["retryInfos", "usage"] {
        v["chatModel"].as_object_mut().unwrap().remove(key);
    }
    v["chatModel"]["timeToFirstToken"] = json!("0s");
    v["chatModel"]["streamingDuration"] = json!("0s");
    v["error"] = json!("stream reading error: unexpected EOF");
    let r = parsed(&v);
    let a = r.final_attempt().unwrap();
    assert_eq!(
        (a.outcome.as_str(), r.known_success()),
        ("failed", Some(false))
    );
    assert_eq!(a.error_code.as_deref(), Some("STREAM_INTERRUPTED"));
    assert_eq!(
        (a.first_token_ms, a.generation_ms, a.measured_duration_ms()),
        (None, None, None)
    );
    assert!(r.finished_at.is_some());

    // Any other text on the entry still fails the request.
    v["error"] = json!("fixture rejected");
    assert_eq!(
        parsed(&v).final_attempt().unwrap().error_code.as_deref(),
        Some("ANTIGRAVITY_ERROR")
    );

    // Once tokens were consumed the reported clocks stand.
    v["chatModel"]["usage"] = json!({"responseId":"response-one","inputTokens":"100"});
    let r = parsed(&v);
    let a = r.final_attempt().unwrap();
    assert_eq!(
        (
            a.outcome.as_str(),
            a.first_token_ms,
            a.measured_duration_ms()
        ),
        ("failed", Some(0), Some(0))
    );

    for (text, class) in [
        ("unexpected EOF", Some("STREAM_INTERRUPTED")),
        ("stream reading error: closed", Some("STREAM_INTERRUPTED")),
        (
            "context deadline exceeded: unexpected EOF",
            Some("STREAM_TIMEOUT"),
        ),
        ("fixture rejected", None),
    ] {
        assert_eq!(classify_error_text(text), class, "{text}");
    }
}
