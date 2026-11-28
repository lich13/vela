//! Claude Code writes an API error into its session file in place of an
//! answer. These read such files as a sync does and say what is left of them.
use super::super::{model::*, pricing::Pricing, query, storage_v12, store::Store};
use super::{sync, Report};
use std::{fs, path::Path};

const DAY: i64 = 86_400_000;
const HOUR: i64 = 3_600_000;
const BASE: i64 = 1_700_000_000_000;
/// A turn that was answered, one that ended in a 429 and one that ended with
/// no status at all, as Claude Code 2 writes them.
const SESSION: &str = include_str!("fixtures/v015/claude-api-error.jsonl");
/// The session of that file.
const FIXTURE: &str = "fixture-claude-session";

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

/// The id of the record of the error line `line` of `session`.
fn error_id(session: &str, line: &str) -> String {
    safe_id(&format!("claude:error:{session}:{line}"))
}

/// The id of the record of what `message` answered.
fn answer_id(message: &str) -> String {
    safe_id(&format!("claude:{}", safe_id(message)))
}

/// Lines of a session file: a turn, an answer that ended, and an API error
/// written down in place of an answer.
fn turn(session: &str, line: &str, at: i64) -> String {
    format!(
        r#"{{"type":"user","sessionId":"{session}","uuid":"{line}","timestamp":{at},"message":{{"role":"user","content":"fixture-content.example.invalid"}}}}"#
    )
}
fn answer(session: &str, message: &str, at: i64) -> String {
    format!(
        r#"{{"type":"assistant","sessionId":"{session}","uuid":"{message}-line","timestamp":{at},"message":{{"id":"{message}","model":"claude-fixture","stop_reason":"end_turn","usage":{{"input_tokens":100,"output_tokens":8,"cache_read_input_tokens":20,"cache_creation_input_tokens":5}}}}}}"#
    )
}
fn error(session: &str, line: &str, at: i64) -> String {
    format!(
        r#"{{"type":"assistant","sessionId":"{session}","uuid":"{line}","timestamp":{at},"message":{{"id":"fixture-synthetic","model":"<synthetic>","stop_reason":"stop_sequence","usage":{{"input_tokens":0,"output_tokens":0}}}},"error":"server_error","isApiErrorMessage":true,"apiErrorStatus":529}}"#
    )
}
fn file(lines: &[String]) -> String {
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// What the gateway records of a request of `client` that ended at `at`.
fn gateway(id: &str, client: &str, at: i64, status: Option<u16>, outcome: &str) -> Record {
    Record {
        id: id.into(),
        client: client.into(),
        source: "proxy".into(),
        started_at: at - 500,
        finished_at: Some(at),
        completed: outcome == "success",
        attempts: vec![Attempt {
            id: format!("{id}-attempt"),
            provider: Some("fixture-provider".into()),
            requested_model: Some("claude-fixture".into()),
            response_model: Some("claude-fixture".into()),
            pricing_model: Some("claude-fixture".into()),
            cost_multiplier: "1".into(),
            status,
            outcome: outcome.into(),
            started_at: at - 500,
            duration_ms: 500,
            transport: "http".into(),
            ..Attempt::default()
        }],
        ..Record::default()
    }
}

/// The gateway's record of the request `message` answered.
fn answered(id: &str, message: &str, at: i64) -> Record {
    let mut record = gateway(id, "claude", at, Some(200), "success");
    record.attempts[0].response_id = Some(safe_id(message));
    record.attempts[0].tokens = Tokens {
        input: Some(100),
        output: Some(8),
        cache_read: Some(20),
        cache_write: Some(5),
        ..Tokens::default()
    };
    record
}

/// Whether the store holds a record of that id, counted or not.
fn kept(store: &Store, id: &str) -> bool {
    store.detail(id).is_ok()
}

/// The gateway record the answer to `message` is one request with, if any.
fn joined(store: &Store, message: &str) -> Option<String> {
    store.detail(&answer_id(message)).unwrap().duplicate_of
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
    let session = Some(safe_id(FIXTURE));

    let answer = row(&rows, &answer_id("fixture-claude-answer"));
    let attempt = answer.final_attempt().unwrap();
    assert!(answer.completed);
    assert_eq!(attempt.outcome, "success");
    assert_eq!(attempt.status, None);
    assert_eq!(answer.session_id, session);
    assert_eq!(answer.known_success(), Some(true));

    let limited = row(&rows, &error_id(FIXTURE, "fixture-error-limited"));
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

    let dropped = row(&rows, &error_id(FIXTURE, "fixture-error-dropped"));
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
        let row = row(&rows, &error_id(FIXTURE, line));
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

#[test]
fn a_failure_after_a_request_that_went_through_the_gateway_is_left_to_the_gateway() {
    let (dir, mut store, prices) = fixture();
    let home = dir.path().join("home");
    // What the gateway recorded of the session's three requests.
    store
        .write_batch(
            &[
                answered(
                    "fixture-gateway-answer",
                    "fixture-claude-answer",
                    BASE + 1_000,
                ),
                gateway(
                    "fixture-gateway-limited",
                    "claude",
                    BASE + 62_000,
                    Some(429),
                    "rejected",
                ),
                gateway(
                    "fixture-gateway-dropped",
                    "claude",
                    BASE + 120_500,
                    None,
                    "failure",
                ),
            ],
            None,
        )
        .unwrap();
    write(&home, "projects/fixture/main.jsonl", SESSION);
    for rebuild in [false, true] {
        assert_eq!(read(&mut store, &prices, &home, rebuild).errors, 0);
        // The answer is one request with the gateway's, so the session went
        // through the gateway, and neither failure after it is written: the
        // second has the same request before it as the first.
        assert_eq!(
            joined(&store, "fixture-claude-answer").as_deref(),
            Some("fixture-gateway-answer")
        );
        for line in ["fixture-error-limited", "fixture-error-dropped"] {
            assert!(
                !kept(&store, &error_id(FIXTURE, line)),
                "{line} rebuild={rebuild}"
            );
        }
        let mut listed: Vec<_> = rows(&store).into_iter().map(|row| row.id).collect();
        listed.sort();
        assert_eq!(
            listed,
            [
                "fixture-gateway-answer",
                "fixture-gateway-dropped",
                "fixture-gateway-limited"
            ]
        );
        // Each request once: one answered and two failed.
        assert_eq!(counted(&store), (3, 3, 1), "rebuild={rebuild}");
    }
}

#[test]
fn a_failure_after_a_request_that_went_straight_to_the_api_is_counted_whatever_the_gateway_recorded(
) {
    let (dir, mut store, prices) = fixture();
    let home = dir.path().join("home");
    // Another conversation of the client went through the gateway, and failed
    // there within the minute before each failure of this one.
    store
        .write_batch(
            &[
                gateway(
                    "fixture-gateway-limited",
                    "claude",
                    BASE + 62_000,
                    Some(429),
                    "rejected",
                ),
                gateway(
                    "fixture-gateway-dropped",
                    "claude",
                    BASE + 120_500,
                    None,
                    "failure",
                ),
            ],
            None,
        )
        .unwrap();
    write(&home, "projects/fixture/main.jsonl", SESSION);
    for rebuild in [false, true] {
        assert_eq!(read(&mut store, &prices, &home, rebuild).errors, 0);
        assert_eq!(joined(&store, "fixture-claude-answer"), None);
        for line in ["fixture-error-limited", "fixture-error-dropped"] {
            assert!(
                kept(&store, &error_id(FIXTURE, line)),
                "{line} rebuild={rebuild}"
            );
        }
        // The session's answer and its two failures, and the gateway's two.
        assert_eq!(counted(&store), (5, 5, 1), "rebuild={rebuild}");
    }
}

#[test]
fn with_no_request_before_it_a_failure_is_the_gateways_where_the_gateway_recorded_one_in_the_minute_before(
) {
    // What the gateway recorded, for which client and how long before the
    // line was written; then whether the failure is written.
    type Recorded = Option<(&'static str, i64, Option<u16>, &'static str)>;
    let cases: [(&str, Recorded, bool); 11] = [
        ("nothing", None, true),
        (
            "a rejection a second before",
            Some(("claude", -1_000, Some(429), "rejected")),
            false,
        ),
        (
            "a failure with no status",
            Some(("claude", -1_000, None, "failure")),
            false,
        ),
        (
            "a model that was not there",
            Some(("claude", -1_000, Some(404), "model_unavailable")),
            false,
        ),
        (
            "a rejection a minute before",
            Some(("claude", -60_000, Some(429), "rejected")),
            false,
        ),
        (
            "a rejection as the line was written",
            Some(("claude", 0, Some(429), "rejected")),
            false,
        ),
        (
            "a rejection more than a minute before",
            Some(("claude", -60_001, Some(429), "rejected")),
            true,
        ),
        (
            "a rejection after the line was written",
            Some(("claude", 1, Some(429), "rejected")),
            true,
        ),
        (
            "an answer",
            Some(("claude", -1_000, Some(200), "success")),
            true,
        ),
        (
            "a request the client gave up",
            Some(("claude", -1_000, Some(200), "cancelled")),
            true,
        ),
        (
            "a rejection of another client",
            Some(("codex", -1_000, Some(429), "rejected")),
            true,
        ),
    ];
    let (dir, mut store, prices) = fixture();
    let home = dir.path().join("home");
    // Each in a session of its own, ten minutes from the next.
    let session = |index: usize| format!("fixture-session-{index}");
    for (index, (_, recorded, _)) in cases.iter().enumerate() {
        let at = BASE + index as i64 * 600_000;
        write(
            &home,
            &format!("projects/fixture/{}.jsonl", session(index)),
            &file(&[error(&session(index), "fixture-error", at)]),
        );
        if let Some((client, offset, status, outcome)) = recorded {
            let id = format!("fixture-gateway-{index}");
            store
                .write_batch(&[gateway(&id, client, at + offset, *status, outcome)], None)
                .unwrap();
        }
    }
    for rebuild in [false, true] {
        assert_eq!(read(&mut store, &prices, &home, rebuild).errors, 0);
        for (index, (name, _, written)) in cases.iter().enumerate() {
            assert_eq!(
                kept(&store, &error_id(&session(index), "fixture-error")),
                *written,
                "{name} rebuild={rebuild}"
            );
        }
    }
}

#[test]
fn a_request_more_than_six_hours_before_tells_nothing_of_the_way_a_failure_went() {
    // A session made a request through the gateway and failed this long
    // after; whether the gateway recorded a failure a second before that;
    // then whether the session's failure is written.
    let cases = [
        ("six hours after", 6 * HOUR, false, false),
        ("more than six hours after", 6 * HOUR + 1, false, true),
        (
            "more than six hours after, the gateway failing too",
            6 * HOUR + 1,
            true,
            false,
        ),
    ];
    let (dir, mut store, prices) = fixture();
    let home = dir.path().join("home");
    let session = |index: usize| format!("fixture-session-{index}");
    let message = |index: usize| format!("fixture-message-{index}");
    for (index, (_, after, failed, _)) in cases.iter().enumerate() {
        let at = BASE + index as i64 * DAY;
        let mut recorded = vec![answered(
            &format!("fixture-gateway-{index}"),
            &message(index),
            at,
        )];
        if *failed {
            recorded.push(gateway(
                &format!("fixture-gateway-failure-{index}"),
                "claude",
                at + after - 1_000,
                Some(429),
                "rejected",
            ));
        }
        store.write_batch(&recorded, None).unwrap();
        write(
            &home,
            &format!("projects/fixture/{}.jsonl", session(index)),
            &file(&[
                answer(&session(index), &message(index), at),
                error(&session(index), "fixture-error", at + after),
            ]),
        );
    }
    for rebuild in [false, true] {
        assert_eq!(read(&mut store, &prices, &home, rebuild).errors, 0);
        for (index, (name, _, _, written)) in cases.iter().enumerate() {
            assert_eq!(
                joined(&store, &message(index)),
                Some(format!("fixture-gateway-{index}")),
                "{name}"
            );
            assert_eq!(
                kept(&store, &error_id(&session(index), "fixture-error")),
                *written,
                "{name} rebuild={rebuild}"
            );
        }
    }
}

#[test]
fn a_failure_is_judged_after_everything_else_of_the_write_it_came_in() {
    let message = "fixture-message";
    for first in [true, false] {
        // A write goes in the order of its ids, as does what a rebuild
        // staged: an error line whose record comes before the answer's, and
        // one whose record comes after.
        let line = (0..)
            .map(|n| format!("fixture-error-{n}"))
            .find(|line| (error_id(FIXTURE, line) < answer_id(message)) == first)
            .unwrap();
        for through_gateway in [true, false] {
            for rebuild in [false, true] {
                let (dir, mut store, prices) = fixture();
                let home = dir.path().join("home");
                // Either the gateway recorded the answer, or the session went
                // straight to the API while another conversation failed at
                // the gateway a second before it did. Taken before the
                // answer is written, the failure would be judged by the
                // gateway's failures alone, and wrongly both times.
                let recorded = if through_gateway {
                    answered("fixture-gateway-answer", message, BASE)
                } else {
                    gateway(
                        "fixture-gateway-other",
                        "claude",
                        BASE + 59_000,
                        Some(429),
                        "rejected",
                    )
                };
                store.write_batch(&[recorded], None).unwrap();
                write(
                    &home,
                    "projects/fixture/main.jsonl",
                    &file(&[
                        answer(FIXTURE, message, BASE),
                        error(FIXTURE, &line, BASE + 60_000),
                    ]),
                );
                assert_eq!(read(&mut store, &prices, &home, rebuild).errors, 0);
                assert_eq!(
                    kept(&store, &error_id(FIXTURE, &line)),
                    !through_gateway,
                    "first={first} through_gateway={through_gateway} rebuild={rebuild}"
                );
            }
        }
    }
}

#[test]
fn failures_of_one_write_are_judged_in_the_order_they_happened() {
    // A session went straight to the API and failed twice, half a minute
    // apart; between the two, another conversation failed at the gateway.
    // The later failure's record comes first in the order of ids.
    let early = "fixture-error-early";
    let late = (0..)
        .map(|n| format!("fixture-error-late-{n}"))
        .find(|line| error_id(FIXTURE, line) < error_id(FIXTURE, early))
        .unwrap();
    let other = gateway(
        "fixture-gateway-other",
        "claude",
        BASE + 20_000,
        Some(429),
        "rejected",
    );
    for rebuild in [false, true] {
        let (dir, mut store, prices) = fixture();
        let home = dir.path().join("home");
        store
            .write_batch(std::slice::from_ref(&other), None)
            .unwrap();
        write(
            &home,
            "projects/fixture/main.jsonl",
            &file(&[
                error(FIXTURE, early, BASE),
                error(FIXTURE, &late, BASE + 30_000),
            ]),
        );
        assert_eq!(read(&mut store, &prices, &home, rebuild).errors, 0);
        // The later one has the earlier before it, which went straight.
        for line in [early, late.as_str()] {
            assert!(
                kept(&store, &error_id(FIXTURE, line)),
                "{line} rebuild={rebuild}"
            );
        }
    }
    // Alone, the later one has only the gateway's failure before it.
    let (dir, mut store, prices) = fixture();
    let home = dir.path().join("home");
    store.write_batch(&[other], None).unwrap();
    write(
        &home,
        "projects/fixture/main.jsonl",
        &file(&[error(FIXTURE, &late, BASE + 30_000)]),
    );
    assert_eq!(read(&mut store, &prices, &home, false).errors, 0);
    assert!(!kept(&store, &error_id(FIXTURE, &late)));
}

#[test]
fn a_failure_written_before_its_session_was_found_at_the_gateway_is_removed_when_read_again() {
    let (dir, mut store, prices) = fixture();
    let home = dir.path().join("home");
    let failures =
        ["fixture-error-limited", "fixture-error-dropped"].map(|line| error_id(FIXTURE, line));
    write(&home, "projects/fixture/main.jsonl", SESSION);
    assert_eq!(read(&mut store, &prices, &home, false).errors, 0);
    assert!(failures.iter().all(|id| kept(&store, id)));
    assert_eq!(counted(&store), (3, 3, 1));

    // The gateway's records of the same three requests are written late.
    store
        .write_batch(
            &[
                answered(
                    "fixture-gateway-answer",
                    "fixture-claude-answer",
                    BASE + 1_000,
                ),
                gateway(
                    "fixture-gateway-limited",
                    "claude",
                    BASE + 62_000,
                    Some(429),
                    "rejected",
                ),
                gateway(
                    "fixture-gateway-dropped",
                    "claude",
                    BASE + 120_500,
                    None,
                    "failure",
                ),
            ],
            None,
        )
        .unwrap();
    // No line is read again, so what was written of the failures stays.
    assert_eq!(read(&mut store, &prices, &home, false).imported, 0);
    assert!(failures.iter().all(|id| kept(&store, id)));
    assert_eq!(counted(&store), (5, 5, 1));

    // The file is replaced by one that begins otherwise, so all of it is
    // read again: the answer is the gateway's by now, and the failures go.
    write(
        &home,
        "projects/fixture/main.jsonl",
        &format!(
            "{}\n{SESSION}",
            turn(FIXTURE, "fixture-turn-zero", BASE - 1_000)
        ),
    );
    assert_eq!(read(&mut store, &prices, &home, false).errors, 0);
    assert_eq!(
        joined(&store, "fixture-claude-answer").as_deref(),
        Some("fixture-gateway-answer")
    );
    assert!(!failures.iter().any(|id| kept(&store, id)));
    assert_eq!(counted(&store), (3, 3, 1));
}

#[test]
fn the_requests_before_a_failure_are_found_by_their_time() {
    let (dir, _store, _prices) = fixture();
    let c = rusqlite::Connection::open(dir.path().join("usage.sqlite")).unwrap();
    query::register(&c).unwrap();
    for sql in [storage_v12::CLAUDE_ROUTE, storage_v12::GATEWAY_BEFORE] {
        let mut statement = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let mut steps = statement.raw_query();
        let mut plan = Vec::new();
        while let Some(step) = steps.next().unwrap() {
            plan.push(step.get::<_, String>(3).unwrap());
        }
        // The range of the index and nothing else: no row outside it is
        // read, and the nearest comes first with nothing sorted.
        assert_eq!(
            plan,
            ["SEARCH v12_rows USING INDEX v12_time (time>? AND time<?)"],
            "{sql}"
        );
    }
}
