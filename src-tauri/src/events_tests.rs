use crate::{
    events::{Action, Change, Filter, Reason, Record, Service},
    gateway::ClientId,
};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PROVIDER: &str = "00000000-0000-0000-0000-000000000001";
const RETENTION: u64 = 7 * 86400;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn record(model: &str, reason: Reason) -> Record {
    Record::new(
        Some(ClientId::Codex),
        Some(PROVIDER),
        Some(model),
        reason,
        Action::TryingNext,
        Some(503),
        Some(1),
    )
}

#[tokio::test]
async fn complete_errors_survive_restart_and_size_growth_and_do_not_merge_different_bodies() {
    let temp = tempfile::tempdir().unwrap();
    let service = Service::new(temp.path());
    let mut rx = service.subscribe();
    let text = format!(
        "{} 'quoted fixture' https://example.invalid/model?detail=429\nend",
        "fixture ".repeat(700_000)
    );
    let mut event = record("fixture-raw", Reason::ProtocolError);
    let id = event.id.clone();
    event.status = Some(200);
    event.details.message = Some(text.clone());
    event.details.raw = Some(std::sync::Arc::new(crate::event_raw::Raw::bytes(
        text.as_bytes(),
        vec![crate::event_raw::Header {
            name: "x-fixture".into(),
            value: "keep all".into(),
            base64: false,
        }],
        None,
    )));
    emit(&service, &mut rx, event.clone()).await;
    assert_eq!(service.raw(&id).unwrap().unwrap().body, text);
    let mut second = event.clone();
    second.id = uuid::Uuid::new_v4().to_string();
    let different = format!("{text} different");
    second.details.raw = Some(std::sync::Arc::new(crate::event_raw::Raw::bytes(
        different.as_bytes(),
        event.details.raw.as_ref().unwrap().headers.clone(),
        None,
    )));
    emit(&service, &mut rx, second).await;
    assert_eq!(service.query(Filter::default()).total, 2);
    drop(service);
    let restored = Service::new(temp.path());
    assert_eq!(restored.raw(&id).unwrap().unwrap().body, text);
    assert_eq!(
        restored.raw(&id).unwrap().unwrap().headers[0].value,
        "keep all"
    );
    restored.clear(None, None).await.unwrap();
    assert!(restored.raw(&id).unwrap().is_none());
    assert_eq!(
        fs::read_dir(temp.path().join("diagnostic-errors"))
            .unwrap()
            .count(),
        0
    );
}

async fn next(rx: &mut tokio::sync::broadcast::Receiver<Change>) -> Change {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn emit(service: &Service, rx: &mut tokio::sync::broadcast::Receiver<Change>, value: Record) {
    service.emit(value);
    let _ = next(rx).await;
}

#[tokio::test]
async fn clearing_external_logs_survives_restart_append_and_unscanned_files() {
    use std::io::Write;
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let logs = home.join(".gemini/antigravity/logs");
    let claude = home.join("claude");
    fs::create_dir_all(&logs).unwrap();
    fs::create_dir_all(claude.join("projects/fixture")).unwrap();
    let old_log = logs.join("language_server.log");
    let partial = logs.join("unscanned.log");
    fs::write(&old_log, "[ERROR] 503 old fixture\n").unwrap();
    let session = claude.join("projects/fixture/session.jsonl");
    let row = |id: &str, at: chrono::DateTime<chrono::Utc>| {
        format!(
            "{}\n",
            serde_json::json!({"type":"assistant","isApiErrorMessage":true,"uuid":id,"sessionId":"fixture-session","timestamp":at.to_rfc3339(),"message":{"content":[{"type":"text","text":"API Error: fixture"}]}})
        )
    };
    fs::write(&session, row("old", chrono::Utc::now())).unwrap();
    let data = temp.path().join("data");
    let service = Service::new(&data);
    service.sync_antigravity_logs(&home);
    service.sync_claude_logs(&claude);
    assert_eq!(service.query(Filter::default()).total, 2);
    // This file was never scanned and ends in a partial line at clear time.
    fs::write(&partial, "[ERROR] 401 old partial").unwrap();
    service
        .clear(Some(home.clone()), Some(claude.clone()))
        .await
        .unwrap();
    drop(service);
    let marker = fs::read_to_string(data.join("diagnostics.jsonl")).unwrap();
    assert!(!marker.contains("language_server"));
    assert!(!marker.contains("old partial"));
    let mut f = fs::OpenOptions::new().append(true).open(&old_log).unwrap();
    writeln!(f, "[ERROR] 429 new fixture").unwrap();
    let mut f = fs::OpenOptions::new().append(true).open(&partial).unwrap();
    writeln!(f, " completed").unwrap();
    writeln!(f, "[ERROR] 503 another new fixture").unwrap();
    let mut f = fs::OpenOptions::new().append(true).open(&session).unwrap();
    f.write_all(row("new", chrono::Utc::now()).as_bytes())
        .unwrap();
    f.write_all(
        row(
            "backfilled",
            chrono::Utc::now() - chrono::Duration::hours(1),
        )
        .as_bytes(),
    )
    .unwrap();
    // The persisted mark, rather than in-memory cursors, determines the boundary.
    let restored = Service::new(&data);
    restored.sync_antigravity_logs(&home);
    restored.sync_claude_logs(&claude);
    let page = restored.query(Filter::default());
    assert_eq!(page.total, 3);
    assert!(page
        .items
        .iter()
        .all(|r| !r.details.message.as_deref().unwrap_or("").contains("old")));
    let mut rx = restored.subscribe();
    restored.emit(record("after-clear", Reason::ProtocolError));
    while next(&mut rx)
        .await
        .record
        .is_none_or(|r| r.model.as_deref() != Some("after-clear"))
    {}
    let again = Service::new(&data);
    again.sync_antigravity_logs(&home);
    again.sync_claude_logs(&claude);
    assert_eq!(again.query(Filter::default()).total, 4);
    assert!(fs::read_to_string(data.join("diagnostics.jsonl"))
        .unwrap()
        .starts_with("{\"cleared\":"));
}

#[tokio::test]
async fn records_merge_within_300_seconds_and_expose_subscribe_detail_and_clear() {
    let temp = tempfile::tempdir().unwrap();
    let service = Service::new(temp.path());
    let mut rx = service.subscribe();
    let first = record("fixture-model", Reason::UpstreamService);
    let id = first.id.clone();
    emit(&service, &mut rx, first.clone()).await;
    let mut merged = record("fixture-model", Reason::UpstreamService);
    merged.last_at = first.last_at + 299;
    emit(&service, &mut rx, merged).await;
    let page = service.query(Filter::default());
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].count, 2);
    assert_eq!(service.detail(&id).unwrap().count, 2);

    let mut separate = record("fixture-model", Reason::UpstreamService);
    separate.last_at = first.last_at + 600;
    emit(&service, &mut rx, separate).await;
    assert_eq!(service.query(Filter::default()).total, 2);

    service.clear(None, None).await.unwrap();
    assert_eq!(service.query(Filter::default()).total, 0);
    assert!(service.detail(&id).is_none());
    let saved: serde_json::Value =
        serde_json::from_slice(&fs::read(temp.path().join("diagnostics.jsonl")).unwrap()).unwrap();
    assert!(saved.get("cleared").is_some());
}

#[tokio::test]
async fn sensitive_models_are_omitted_and_pages_are_filtered_and_bounded() {
    assert!(crate::events::safe_model("fixture/model-v1").is_some());
    for value in [
        "sk-secret-fixture",
        "Bearer secret-fixture",
        "eyJ.fixture.token",
        "https://example.invalid/secret",
        "/fixture/private",
        "users/fixture/private",
    ] {
        assert!(crate::events::safe_model(value).is_none(), "{value}");
    }
    let secret = Record::new(
        Some(ClientId::Codex),
        Some(PROVIDER),
        Some("sk-secret-fixture"),
        Reason::Authentication,
        Action::Stopped,
        Some(401),
        None,
    );
    assert!(secret.model.is_none());
    assert!(!secret.notification().contains("sk-secret"));

    let temp = tempfile::tempdir().unwrap();
    let service = Service::new(temp.path());
    let mut rx = service.subscribe();
    for i in 0..55 {
        emit(
            &service,
            &mut rx,
            record(
                &format!("fixture-model-{i}"),
                if i % 2 == 0 {
                    Reason::Network
                } else {
                    Reason::UpstreamService
                },
            ),
        )
        .await;
    }
    let page_one = service.query(Filter {
        client_id: Some(ClientId::Codex.into()),
        page: Some(1),
        ..Filter::default()
    });
    assert_eq!(page_one.total, 55);
    assert_eq!(page_one.page, 1);
    assert_eq!(page_one.items.len(), 50);
    let page_two = service.query(Filter {
        reason: Some(Reason::Network),
        page: Some(99),
        ..Filter::default()
    });
    assert_eq!(page_two.total, 28);
    assert_eq!(page_two.page, 1);
    assert_eq!(page_two.items.len(), 28);

    let server_errors = service.query(Filter {
        status_group: Some(crate::events::StatusGroup::ServerError),
        ..Filter::default()
    });
    assert_eq!(server_errors.total, 55);
}

#[test]
fn seven_day_retention_and_damaged_tail_keep_valid_records() {
    let temp = tempfile::tempdir().unwrap();
    let mut old = record("old-model", Reason::Network);
    old.first_at = now().saturating_sub(RETENTION + 1);
    old.last_at = old.first_at;
    let fresh = record("fresh-model", Reason::Network);
    let mut bytes = serde_json::to_vec(&old).unwrap();
    bytes.push(b'\n');
    bytes.extend(serde_json::to_vec(&fresh).unwrap());
    bytes.extend_from_slice(b"\n{damaged-tail\n");
    fs::write(temp.path().join("diagnostics.jsonl"), bytes).unwrap();
    let service = Service::new(temp.path());
    let page = service.query(Filter::default());
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].model.as_deref(), Some("fresh-model"));
    assert!(page
        .error
        .as_deref()
        .is_some_and(|message| message.contains("损坏记录")));
}

#[test]
fn damaged_large_files_report_corruption_without_a_size_limit() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("diagnostics.jsonl");
    fs::write(path, vec![b'x'; 5 * 1024 * 1024 + 1]).unwrap();
    let service = Service::new(temp.path());
    let page = service.query(Filter::default());
    assert_eq!(page.total, 0);
    assert!(page
        .error
        .as_deref()
        .is_some_and(|message| message.contains("损坏记录")));
}

#[tokio::test]
async fn write_failures_are_visible_through_subscription_and_query() {
    let temp = tempfile::tempdir().unwrap();
    let data_file = temp.path().join("data-file");
    fs::write(&data_file, b"fixture").unwrap();
    let service = Service::new(&data_file);
    let mut rx = service.subscribe();
    service.emit(record("fixture-model", Reason::Network));
    let change = next(&mut rx).await;
    assert!(change
        .error
        .as_deref()
        .is_some_and(|message| message.contains("写入失败")));
    assert!(service
        .query(Filter::default())
        .error
        .as_deref()
        .is_some_and(|message| message.contains("写入失败")));
}

#[test]
fn serialization_uses_the_public_camel_case_contract() {
    let value = serde_json::to_value(record("fixture-model", Reason::Network)).unwrap();
    assert!(value.get("firstAt").is_some());
    assert!(value.get("lastAt").is_some());
    assert!(value.get("clientId").is_some());
    assert!(value.get("providerId").is_some());
    assert!(value.get("notifiedAt").is_some());
    assert_eq!(
        value.get("errorCode").and_then(|v| v.as_str()),
        Some("NETWORK_ERROR")
    );
    assert!(value.get("first_at").is_none());
}

#[test]
fn record_ids_are_unique_for_distinct_events() {
    static SEED: AtomicU64 = AtomicU64::new(0);
    let a = record(
        &format!("fixture-{}", SEED.fetch_add(1, Ordering::Relaxed)),
        Reason::Network,
    );
    let b = record(
        &format!("fixture-{}", SEED.fetch_add(1, Ordering::Relaxed)),
        Reason::Network,
    );
    assert_ne!(a.id, b.id);
}

#[test]
fn sync_antigravity_logs_ingests_and_deduplicates_app_errors() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let transcript_dir = home.join(".gemini/antigravity/brain/conv-1/.system_generated/logs");
    fs::create_dir_all(&transcript_dir).unwrap();
    fs::write(
        transcript_dir.join("transcript.jsonl"),
        concat!(
            r#"{"step_index":1,"source":"MODEL","type":"GENERIC","status":"ERROR","content":"Error: edit_file target not found"}"#, "\n",
            r#"{"type":"ERROR_MESSAGE","status":"DONE","model":"claude-opus-4-6","error":"stream was interrupted after 5m0s"}"#, "\n",
            r#"{"type":"ERROR_MESSAGE","status":"DONE","model":"claude-opus-4-6","error":"HTTP 401 unauthenticated token"}"#, "\n",
        ),
    )
    .unwrap();
    let log_dir = home.join("Library/Logs/Antigravity");
    fs::create_dir_all(&log_dir).unwrap();
    let mut large_log =
        "I0414 10:00:00.000000 123 main.go:1] normal info line padding\n".repeat(5200);
    let now_prefix = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    large_log.push_str(&format!(
        "[{now_prefix}.401] [error] upstream connection reset by peer\n"
    ));
    large_log.push_str("[error] RESOURCE_EXHAUSTED: quota exceeded for project\n");
    assert!(large_log.len() > 256 * 1024);
    fs::write(log_dir.join("language_server.log"), large_log).unwrap();

    let service = Service::new(&temp.path().join("data"));
    service.sync_antigravity_logs(&home);
    service.sync_antigravity_logs(&home);

    let upstream_page = service.query(Filter {
        origin: Some(crate::events::Origin::Upstream),
        ..Filter::default()
    });
    assert_eq!(upstream_page.total, 0);

    let antigravity_filter = Filter {
        client_id: Some(crate::events::JournalClient::External(
            crate::events::ExternalClient::Antigravity,
        )),
        origin: Some(crate::events::Origin::Antigravity),
        ..Filter::default()
    };
    let page = service.query(antigravity_filter);
    assert_eq!(page.total, 4);
    assert!(page
        .items
        .iter()
        .any(|r| r.reason == Reason::RequestInterrupted));
    assert_eq!(
        page.items
            .iter()
            .filter(|r| r.reason == Reason::Authentication)
            .count(),
        1,
        "timestamp .401 must not be misclassified as 401 Authentication"
    );
    assert!(page
        .items
        .iter()
        .any(|r| r.reason == Reason::UpstreamService));
    assert!(page.items.iter().any(|r| r.reason == Reason::RateLimit));
}

#[test]
fn sync_claude_logs_reads_api_errors_from_nested_session_files_once() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("claude-home");
    let project = home.join("projects/fixture-project");
    let nested = project.join("fixture-session/subagents");
    fs::create_dir_all(&nested).unwrap();
    let at = chrono::Utc::now().to_rfc3339();
    let error = |uuid: &str, extra: &str| {
        format!(
            r#"{{"type":"assistant","isApiErrorMessage":true,"uuid":"{uuid}","sessionId":"fixture-session","timestamp":"{at}","message":{{"model":"<synthetic>","content":[{{"type":"text","text":"API Error: fixture"}}]}}{extra}}}"#
        )
    };
    fs::write(
        project.join("fixture-session.jsonl"),
        [
            r#"{"type":"user","message":{"content":"mentions \"isApiErrorMessage\":true in text"}}"#.to_owned(),
            r#"{"type":"assistant","message":{"id":"msg_fixture","usage":{"output_tokens":1}}}"#.to_owned(),
            error("uuid-1", r#","error":"rate_limit","apiErrorStatus":429"#),
            "{ not json \"isApiErrorMessage\":true".to_owned(),
            String::new(),
        ]
        .join("\n"),
    )
    .unwrap();
    fs::write(
        nested.join("agent-fixture.jsonl"),
        error(
            "uuid-2",
            r#","error":"server_error","apiError":"connection_failed""#,
        ) + "\n",
    )
    .unwrap();
    fs::write(project.join("notes.txt"), error("uuid-3", "") + "\n").unwrap();

    let service = Service::new(&temp.path().join("data"));
    service.sync_claude_logs(&home);
    service.sync_claude_logs(&home);

    let page = service.query(Filter {
        origin: Some(crate::events::Origin::ClaudeCode),
        ..Filter::default()
    });
    assert_eq!(page.total, 2);
    assert!(page.items.iter().any(|r| r.reason == Reason::RateLimit));
    assert!(page.items.iter().any(|r| r.reason == Reason::Network));
    // They stay out of the gateway's own views and of the Claude gateway client.
    for origin in [
        crate::events::Origin::Upstream,
        crate::events::Origin::Gateway,
        crate::events::Origin::Antigravity,
    ] {
        let other = service.query(Filter {
            origin: Some(origin),
            ..Filter::default()
        });
        assert_eq!(other.total, 0);
    }
    let gateway_client = service.query(Filter {
        client_id: Some(ClientId::Claude.into()),
        ..Filter::default()
    });
    assert_eq!(gateway_client.total, 0);

    // A line appended later is picked up without re-reading the old ones.
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(nested.join("agent-fixture.jsonl"))
        .unwrap();
    use std::io::Write;
    writeln!(
        file,
        "{}",
        error("uuid-4", r#","error":"rate_limit","apiErrorStatus":429"#)
    )
    .unwrap();
    drop(file);
    service.sync_claude_logs(&home);
    let page = service.query(Filter {
        origin: Some(crate::events::Origin::ClaudeCode),
        ..Filter::default()
    });
    assert_eq!(page.total, 3);
}

#[test]
fn claude_log_scan_covers_all_recent_sessions_and_keeps_incremental_cursors() {
    use std::{
        io::Write,
        time::{Duration, SystemTime},
    };
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("claude");
    let project = home.join("projects/fixture");
    fs::create_dir_all(&project).unwrap();
    let recent = chrono::Utc::now().to_rfc3339();
    let old = (chrono::Utc::now() - chrono::Duration::days(8)).to_rfc3339();
    let line = |id: usize, at: &str| {
        serde_json::json!({
        "type":"assistant", "isApiErrorMessage":true,"uuid":format!("fixture-{id}"),
        "sessionId":"fixture", "timestamp":at, "apiErrorStatus":429,
        "error":"rate_limit", "message":{"content":[{"type":"text","text":"API Error: fixture"}]}
    }).to_string() + "\n"
    };
    for i in 0..64 {
        fs::write(project.join(format!("{i}.jsonl")), line(i, &recent)).unwrap();
    }
    fs::write(project.join("old-event.jsonl"), line(64, &old)).unwrap();
    let stale = project.join("old-file.jsonl");
    fs::write(&stale, line(65, &recent)).unwrap();
    fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(8 * 86400))
        .unwrap();
    let service = Service::new(&dir.path().join("data"));
    let filter = || Filter {
        origin: Some(crate::events::Origin::ClaudeCode),
        ..Filter::default()
    };
    service.sync_claude_logs(&home);
    assert_eq!(service.query(filter()).total, 64);
    service.sync_claude_logs(&home);
    assert_eq!(service.query(filter()).total, 64);
    let path = project.join("0.jsonl");
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(line(66, &recent).as_bytes())
        .unwrap();
    service.sync_claude_logs(&home);
    assert_eq!(service.query(filter()).total, 65);
    service.sync_claude_logs(&home);
    assert_eq!(service.query(filter()).total, 65);
}

#[test]
fn antigravity_reads_all_recent_files_above_former_caps_without_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let logs = home.join(".gemini/antigravity/logs");
    fs::create_dir_all(&logs).unwrap();
    let at = chrono::Utc::now().to_rfc3339();
    for i in 0..55 {
        let dir = home.join(format!(
            ".gemini/antigravity/brain/fixture-{i}/.system_generated/logs"
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("transcript.jsonl"), format!("{}\n",serde_json::json!({"type":"ERROR_MESSAGE","errorMessage":"fixture 503","step_index":0,"created_at":at}))).unwrap();
    }
    for i in 0..30 {
        fs::write(
            logs.join(format!("fixture-{i}.log")),
            "[ERROR] 503 fixture\n",
        )
        .unwrap();
    }
    let service = Service::new(&tmp.path().join("data"));
    service.sync_antigravity_logs(&home);
    assert_eq!(service.query(Filter::default()).total, 85);
    service.sync_antigravity_logs(&home);
    assert_eq!(service.query(Filter::default()).total, 85);
}
