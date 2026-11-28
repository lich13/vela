//! Claude Code writes an API error into its session file in place of an
//! answer. These read such files as a sync does and say what is left of them.
use super::super::{model::*, pricing::Pricing, store::Store};
use super::{sync, Report};
use std::{fs, path::Path};

const DAY: i64 = 86_400_000;
const BASE: i64 = 1_700_000_000_000;
/// A turn that was answered, one that ended in a 429 and one that ended with
/// no status at all, as Claude Code 2 writes them.
const SESSION: &str = include_str!("fixtures/v015/claude-api-error.jsonl");

fn fixture() -> (tempfile::TempDir, Store, Pricing) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let prices = Pricing::new(dir.path()).unwrap();
    (dir, store, prices)
}

fn write(home: &Path, relative: &str, contents: &str) {
    let path = home.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn read(store: &mut Store, prices: &Pricing, home: &Path, rebuild: bool) -> Report {
    sync(store, prices, &Settings::default(), "claude", home, rebuild).unwrap()
}

fn all_time() -> Filter {
    Filter {
        start: Some(BASE - DAY),
        end: Some(BASE + 100 * DAY),
        ..Filter::default()
    }
}

fn rows(store: &Store) -> Vec<Record> {
    let page = store.logs(&Filter::default()).unwrap();
    assert_eq!(page.total as usize, page.rows.len());
    page.rows
}

fn row<'a>(rows: &'a [Record], id: &str) -> &'a Record {
    rows.iter()
        .find(|row| row.id == id)
        .unwrap_or_else(|| panic!("no record {id}"))
}

fn error_id(line: &str) -> String {
    safe_id(&format!("claude:error:fixture-claude-session:{line}"))
}

/// What a dashboard counts: requests, those with a known outcome, successes.
fn counted(store: &Store) -> (u64, u64, u64) {
    let totals = store.dashboard(&all_time()).unwrap().totals;
    (totals.requests, totals.status_known, totals.success)
}

#[test]
fn an_api_error_in_a_session_file_is_a_failed_request_and_an_ended_message_a_success() {
    let (dir, mut store, prices) = fixture();
    let home = dir.path().join("home");
    write(&home, "projects/fixture/main.jsonl", SESSION);
    let report = read(&mut store, &prices, &home, false);
    assert_eq!((report.imported, report.skipped, report.errors), (3, 0, 0));

    let rows = rows(&store);
    assert_eq!(rows.len(), 3);
    let session = Some(safe_id("fixture-claude-session"));

    let answer = row(
        &rows,
        &safe_id(&format!("claude:{}", safe_id("fixture-claude-answer"))),
    );
    let attempt = answer.final_attempt().unwrap();
    assert!(answer.completed);
    assert_eq!(attempt.outcome, "success");
    assert_eq!(attempt.status, None);
    assert_eq!(answer.session_id, session);
    assert_eq!(answer.known_success(), Some(true));

    let limited = row(&rows, &error_id("fixture-error-limited"));
    let attempt = limited.final_attempt().unwrap();
    assert_eq!(limited.attempts.len(), 1);
    assert!(!limited.completed);
    assert_eq!(attempt.outcome, "failed");
    assert_eq!(attempt.status, Some(429));
    assert_eq!(attempt.error_code.as_deref(), Some("rate_limit"));
    // The line's model is a placeholder and its usage all zeros: neither is
    // a model that answered or a consumption that was reported.
    assert_eq!(
        (&attempt.requested_model, &attempt.response_model),
        (&None, &None)
    );
    assert_eq!(attempt.tokens, Tokens::default());
    assert_eq!(attempt.response_id, None);
    assert!(attempt.price.is_none());
    assert_eq!(attempt.transport, "session");
    assert_eq!(limited.session_id, session);
    // It began when the turn it answers was written and ended with the line.
    assert_eq!(
        (limited.started_at, limited.finished_at, attempt.duration_ms),
        (BASE + 60_000, Some(BASE + 62_500), 2_500)
    );
    assert_eq!(limited.known_success(), Some(false));

    let dropped = row(&rows, &error_id("fixture-error-dropped"));
    let attempt = dropped.final_attempt().unwrap();
    assert!(!dropped.completed);
    assert_eq!(attempt.outcome, "failed");
    // No answer came back, so there is no status; it failed all the same.
    assert_eq!(attempt.status, None);
    assert_eq!(attempt.error_code.as_deref(), Some("server_error"));
    assert_eq!(dropped.session_id, session);
    assert_eq!(dropped.finished_at, Some(BASE + 121_000));
    assert_eq!(dropped.known_success(), Some(false));

    // An id is a digest, and nothing the file identifies itself or its lines
    // by, nor anything it says in words, is in what was kept.
    let kept = serde_json::to_string(&rows).unwrap();
    for row in &rows {
        assert_eq!(row.id.len(), 64, "{}", row.id);
        assert!(row.id.bytes().all(|b| b.is_ascii_hexdigit()), "{}", row.id);
    }
    for told in [
        "fixture-claude-session",
        "fixture-error-",
        "fixture-turn-",
        "fixture-synthetic-",
        "synthetic",
        "example.invalid",
        "API Error",
        "connection_failed",
        "/fixture/project",
    ] {
        assert!(!kept.contains(told), "{told}");
    }

    assert_eq!(counted(&store), (3, 3, 1));
    let totals = store.dashboard(&all_time()).unwrap().totals;
    assert_eq!(
        (totals.tokens.input, totals.tokens.output),
        (Some(100), Some(8))
    );
    assert_eq!(totals.cost, "0");

    // Nothing is read twice, and reading everything again changes nothing.
    assert_eq!(read(&mut store, &prices, &home, false).imported, 0);
    assert_eq!(read(&mut store, &prices, &home, true).imported, 3);
    let again = self::rows(&store);
    assert_eq!(
        serde_json::to_value(&again).unwrap(),
        serde_json::to_value(&rows).unwrap()
    );
    assert_eq!(counted(&store), (3, 3, 1));
}

#[test]
fn an_error_line_gives_a_status_and_a_code_only_where_it_names_them() {
    let (dir, mut store, prices) = fixture();
    let home = dir.path().join("home");
    let line = |uuid: &str, second: u32, extra: &str| {
        format!(
            r#"{{"type":"assistant","sessionId":"fixture-claude-session","uuid":"{uuid}","timestamp":"2023-11-14T22:13:{second:02}.000Z","message":{{"id":"fixture-synthetic","model":"<synthetic>","stop_reason":"stop_sequence","usage":{{"input_tokens":0,"output_tokens":0}}}},"isApiErrorMessage":true{extra}}}"#
        )
    };
    let lines = [
        line("fixture-error-overloaded", 21, r#","error":"server_error","apiErrorStatus":529"#),
        // Neither is a status a server answers with.
        line("fixture-error-low", 22, r#","error":"unknown","apiErrorStatus":0"#),
        line("fixture-error-high", 23, r#","error":"unknown","apiErrorStatus":600"#),
        // A sentence is not a code.
        line("fixture-error-worded", 24, r#","error":"fixture-content.example.invalid said no","apiErrorStatus":500"#),
        line("fixture-error-bare", 25, ""),
        // Not marked as an error, and with no usage: no request at all.
        r#"{"type":"assistant","sessionId":"fixture-claude-session","uuid":"fixture-plain","timestamp":"2023-11-14T22:13:26.000Z","message":{"id":"fixture-synthetic","model":"<synthetic>","stop_reason":"stop_sequence","usage":{"input_tokens":0,"output_tokens":0}},"isApiErrorMessage":false,"apiErrorStatus":500}"#.to_owned(),
    ];
    write(
        &home,
        "projects/fixture/main.jsonl",
        &lines.map(|line| line + "\n").concat(),
    );
    let report = read(&mut store, &prices, &home, false);
    assert_eq!((report.imported, report.skipped, report.errors), (5, 0, 0));

    let rows = rows(&store);
    let told = |line: &str| {
        let row = row(&rows, &error_id(line));
        let attempt = row.final_attempt().unwrap();
        assert_eq!(attempt.outcome, "failed", "{line}");
        assert!(!row.completed, "{line}");
        // With no turn before it in the file, it began as it ended.
        assert_eq!(
            (row.finished_at, attempt.duration_ms),
            (Some(row.started_at), 0),
            "{line}"
        );
        (attempt.status, attempt.error_code.clone())
    };
    assert_eq!(
        told("fixture-error-overloaded"),
        (Some(529), Some("server_error".into()))
    );
    assert_eq!(told("fixture-error-low"), (None, Some("unknown".into())));
    assert_eq!(told("fixture-error-high"), (None, Some("unknown".into())));
    assert_eq!(told("fixture-error-worded"), (Some(500), None));
    assert_eq!(told("fixture-error-bare"), (None, None));
    assert_eq!(counted(&store), (5, 5, 0));
}
