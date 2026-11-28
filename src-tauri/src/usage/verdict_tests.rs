//! Which requests count as a success, which as an error and which as neither.
//!
//! The table below is what the success rate is meant to say of each source,
//! written out case by case rather than worked out from the code that decides
//! it. Every fixture is synthetic.

use super::super::{bodies, query, storage_v12, store::Store};
use super::*;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

const SUCCESS: Option<bool> = Some(true);
const ERROR: Option<bool> = Some(false);
const NEITHER: Option<bool> = None;

/// Source, the last attempt's status and terminal state, whether the request
/// completed, and what it counts as.
type Case = (&'static str, Option<u16>, &'static str, bool, Option<bool>);

const CASES: &[Case] = &[
    // The gateway saw the answer: a 2xx or a protocol switch that ended well.
    ("proxy", Some(200), "success", true, SUCCESS),
    ("proxy", Some(201), "success", true, SUCCESS),
    ("proxy", Some(101), "success", true, SUCCESS),
    // Ending at the output limit is an answer in full.
    ("proxy", Some(200), "limited", true, SUCCESS),
    // The provider refused, has no such model, or failed, whatever it answered
    // with and even when nothing came back at all.
    ("proxy", Some(429), "rejected", false, ERROR),
    ("proxy", Some(401), "rejected", false, ERROR),
    ("proxy", Some(200), "rejected", false, ERROR),
    ("proxy", Some(404), "model_unavailable", false, ERROR),
    ("proxy", Some(503), "model_unavailable", false, ERROR),
    ("proxy", Some(500), "failure", false, ERROR),
    ("proxy", Some(200), "failure", false, ERROR),
    ("proxy", None, "failure", false, ERROR),
    ("proxy", None, "rejected", false, ERROR),
    ("proxy", None, "model_unavailable", false, ERROR),
    // An answer that was not a success, of a request kept before terminal
    // states were, is an error by its status alone.
    ("proxy", Some(500), "", false, ERROR),
    ("proxy", Some(200), "", false, ERROR),
    ("proxy", Some(200), "", true, SUCCESS),
    // The client gave the request up, or nothing was seen of its end: that is
    // not the provider's doing, whatever status had come by then.
    ("proxy", Some(200), "cancelled", false, NEITHER),
    ("proxy", None, "cancelled", false, NEITHER),
    ("proxy", Some(200), "unknown", false, NEITHER),
    ("proxy", None, "unknown", false, NEITHER),
    // Without a status and without a failure nothing is known.
    ("proxy", None, "", false, NEITHER),
    // Antigravity, from the app and from both kinds of session file, says
    // which requests ended well and which failed.
    ("antigravity", None, "success", true, SUCCESS),
    ("antigravity", None, "failed", false, ERROR),
    ("antigravity", None, "unknown", false, NEITHER),
    ("antigravity", None, "in_progress", false, NEITHER),
    ("antigravity", None, "cancelled", false, NEITHER),
    // A Claude Code session: a message that ended, an API error it wrote
    // down, and nothing of a message cut short or of one imported before
    // either was told apart.
    ("claude", None, "success", true, SUCCESS),
    ("claude", Some(429), "failed", false, ERROR),
    ("claude", Some(529), "failed", false, ERROR),
    ("claude", None, "failed", false, ERROR),
    ("claude", None, "completed", true, NEITHER),
    ("claude", None, "reported", false, NEITHER),
    // A Codex or Grok session keeps no reliable word of how a request ended,
    // so none of theirs counts, whatever a record of one happens to hold.
    ("codex", None, "failed", false, NEITHER),
    ("codex", None, "completed", true, NEITHER),
    ("codex", None, "reported", false, NEITHER),
    ("codex", Some(200), "success", true, NEITHER),
    ("grok", Some(200), "completed", true, NEITHER),
    ("grok", Some(200), "success", true, NEITHER),
    ("grok", Some(500), "failed", false, NEITHER),
    ("grok", None, "completed", true, NEITHER),
];

/// A request of `source` whose only attempt ended as the case says.
fn request((source, status, outcome, completed, _): Case) -> Record {
    Record {
        id: format!("fixture-{source}-{status:?}-{outcome}-{completed}"),
        client: if source == "proxy" { "claude" } else { source }.into(),
        source: source.into(),
        completed,
        attempts: vec![Attempt {
            status,
            outcome: outcome.into(),
            ..Attempt::default()
        }],
        ..Record::default()
    }
}

#[test]
fn each_source_counts_what_it_can_tell_and_nothing_else() {
    for case in CASES {
        let (source, status, outcome, completed, expected) = *case;
        assert_eq!(
            verdict(source, status, outcome, completed),
            expected,
            "{source} {status:?} {outcome:?} completed={completed}"
        );
    }
}

#[test]
fn a_request_counts_as_its_last_attempt_ended() {
    for case in CASES {
        let (source, status, outcome, completed, expected) = *case;
        let mut record = request(*case);
        assert_eq!(
            record.known_success(),
            expected,
            "{source} {status:?} {outcome:?} completed={completed}"
        );
        let mut totals = Totals::default();
        totals.add_record(&record);
        assert_eq!(
            (totals.requests, totals.status_known, totals.success),
            (
                1,
                u64::from(expected.is_some()),
                u64::from(expected == SUCCESS)
            ),
            "{source} {status:?} {outcome:?} completed={completed}"
        );
        // An earlier attempt that failed decides nothing once another followed.
        record.attempts.insert(
            0,
            Attempt {
                status: Some(500),
                outcome: "failure".into(),
                ..Attempt::default()
            },
        );
        assert_eq!(record.known_success(), expected);
    }
    // A request with no attempt has nothing to go by.
    let empty = Record {
        source: "proxy".into(),
        completed: true,
        ..Record::default()
    };
    assert_eq!(empty.known_success(), NEITHER);
}

// What follows stores the same cases and reads them back through the three
// places that count them: a record, the view over the records kept in full,
// and the daily rollups.

const DAY: i64 = 86_400_000;
const BASE: i64 = 20_000 * DAY;
/// Every summary a store gives: of all sources but Grok, of the gateway, of
/// the sessions and of the Grok sessions.
const SCOPES: [Option<&str>; 4] = [None, Some("proxy"), Some("sessions"), Some("grok_sessions")];

fn in_scope(scope: Option<&str>, source: &str) -> bool {
    match scope {
        None => source != "grok",
        Some("proxy") => source == "proxy",
        Some("sessions") => !matches!(source, "proxy" | "grok"),
        _ => source == "grok",
    }
}
fn filter(scope: Option<&str>) -> Filter {
    Filter {
        source: scope.map(str::to_owned),
        start: Some(BASE - DAY),
        end: Some(BASE + 2 * DAY),
        ..Filter::default()
    }
}
/// The model a case's requests name, which is how a summary tells the cases
/// apart.
fn model(index: usize) -> String {
    format!("fixture-case-{index:02}")
}
fn price(model: &str) -> PriceSnapshot {
    PriceSnapshot {
        version: "fixture-price-1".into(),
        source: "fixture".into(),
        model: model.into(),
        multiplier: "1".into(),
        rates: BTreeMap::from([
            ("input".into(), "0.01".into()),
            ("output".into(), "0.02".into()),
        ]),
        cost: "0.5".into(),
        basis: Some(json!({
            "input_cost_per_token": "0.01",
            "output_cost_per_token": "0.02"
        })),
    }
}
/// The `index`th case as a request to store, the `copy`th of it. One priced
/// in full is archived into a rollup it shares with others; one with no price
/// into a rollup of its exact usage.
fn stored(index: usize, case: Case, copy: u64, priced: bool) -> Record {
    let (source, status, outcome, completed, _) = case;
    let at = BASE + (index as i64 * 8 + copy as i64) * 1_000;
    let model = model(index);
    Record {
        id: format!("fixture-case-{index:02}-{copy}"),
        client: if source == "proxy" { "codex" } else { source }.into(),
        source: source.into(),
        started_at: at,
        finished_at: Some(at + 100),
        completed,
        attempts: vec![Attempt {
            provider: (source == "proxy").then(|| "fixture-provider".into()),
            requested_model: Some(model.clone()),
            response_model: Some(model.clone()),
            pricing_model: Some(model.clone()),
            status,
            outcome: outcome.into(),
            started_at: at,
            duration_ms: 100,
            transport: match source {
                "proxy" => "http",
                "antigravity" => "local_api",
                _ => "session",
            }
            .into(),
            tokens: Tokens {
                input: Some(100 + index as u64),
                output: Some(10 + copy),
                ..Tokens::default()
            },
            price: priced.then(|| price(&model)),
            ..Attempt::default()
        }],
        ..Record::default()
    }
}
/// Two requests of every case, one priced and one not.
fn requests() -> Vec<Record> {
    CASES
        .iter()
        .enumerate()
        .flat_map(|(index, case)| {
            [
                stored(index, *case, 0, true),
                stored(index, *case, 1, false),
            ]
        })
        .collect()
}
/// What the table says a summary of `scope` counts of `copies` requests of
/// each case: requests, known outcomes and successes, by model.
fn expected(scope: Option<&str>, copies: u64) -> BTreeMap<String, (u64, u64, u64)> {
    CASES
        .iter()
        .enumerate()
        .filter(|(_, case)| in_scope(scope, case.0))
        .map(|(index, case)| {
            let counts = (
                copies,
                copies * u64::from(case.4.is_some()),
                copies * u64::from(case.4 == SUCCESS),
            );
            (model(index), counts)
        })
        .collect()
}
/// What the store's summary of `scope` counts, by model.
fn counted(store: &Store, scope: Option<&str>) -> BTreeMap<String, (u64, u64, u64)> {
    store
        .dashboard(&filter(scope))
        .unwrap()
        .models
        .into_iter()
        .map(|group| {
            let totals = group.totals;
            (
                group.id,
                (totals.requests, totals.status_known, totals.success),
            )
        })
        .collect()
}
/// How many rollups are shared ones, and how many are of one exact usage.
fn rollup_kinds(c: &Connection) -> (u64, u64) {
    c.query_row(
        "SELECT count(*) FILTER (WHERE json_extract(body,'$.merged')),count(*) FILTER (WHERE json_extract(body,'$.merged') IS NULL) FROM daily",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

#[test]
fn a_record_the_view_and_the_daily_rollups_count_every_case_alike() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let requests = requests();
    store.write_batch(&requests, None).unwrap();

    // A record, as it was written and as it is read back.
    for scope in SCOPES {
        let mut by_model = BTreeMap::<String, Totals>::new();
        for request in requests.iter().filter(|r| in_scope(scope, &r.source)) {
            let kept = store.detail(&request.id).unwrap();
            assert_eq!(kept.known_success(), request.known_success());
            let model = kept.attempts[0].pricing_model.clone().unwrap();
            by_model.entry(model).or_default().add_record(&kept);
        }
        let by_model: BTreeMap<_, _> = by_model
            .into_iter()
            .map(|(model, totals)| {
                (
                    model,
                    (totals.requests, totals.status_known, totals.success),
                )
            })
            .collect();
        assert_eq!(by_model, expected(scope, 2), "records of {scope:?}");
    }
    // The view over the records kept in full.
    for scope in SCOPES {
        assert_eq!(
            counted(&store, scope),
            expected(scope, 2),
            "view of {scope:?}"
        );
    }
    // The daily rollups, of both kinds, once no record is left.
    let ids: Vec<_> = requests.iter().map(|r| r.id.clone()).collect();
    store.archive_batch(&ids).unwrap();
    let left: u64 = store
        .connection
        .query_row("SELECT count(*) FROM v12_rows", [], |row| row.get(0))
        .unwrap();
    assert_eq!(left, 0);
    let (shared, exact) = rollup_kinds(&store.connection);
    assert!(shared > 0 && exact > 0);
    for scope in SCOPES {
        assert_eq!(
            counted(&store, scope),
            expected(scope, 2),
            "rollups of {scope:?}"
        );
    }
}

// A database of version 14 is one this version wrote, put back: its rollups
// count as that version did, its view is that version's, and so is its number.

/// What version 14 took a request for: it went by the status wherever there
/// was one, and by the outcome only for Antigravity.
fn version_14(source: &str, status: Option<u16>, outcome: &str, completed: bool) -> Option<bool> {
    if let Some(status) = status {
        return Some(((200..300).contains(&status) || status == 101) && completed);
    }
    if source == "antigravity" {
        return match outcome {
            "success" => Some(completed),
            "failed" | "cancelled" | "rejected" => Some(false),
            _ => None,
        };
    }
    None
}
fn connection(dir: &Path) -> Connection {
    let c = Connection::open(dir.join("usage.sqlite")).unwrap();
    query::register(&c).unwrap();
    c
}
fn into_version_14(dir: &Path) {
    let c = connection(dir);
    let rollups: Vec<(String, String)> = c
        .prepare("SELECT id,body FROM daily ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    for (id, body) in rollups {
        let mut payload = bodies::unpack(&c, &body).unwrap();
        let example = payload["example"].clone();
        let attempts = example["attempts"].as_array().unwrap();
        let last = attempts.last().unwrap();
        let judged = version_14(
            example["source"].as_str().unwrap(),
            last["status"].as_u64().map(|status| status as u16),
            last["outcome"].as_str().unwrap(),
            example["completed"].as_bool().unwrap(),
        );
        let ordinal = attempts.len() - 1;
        let requests: u64 = c
            .query_row(
                "SELECT requests FROM usage_daily_metrics WHERE kind=1 AND owner=?1 AND ordinal=?2",
                params![id, ordinal as i64],
                |row| row.get(0),
            )
            .unwrap();
        let success = if judged == Some(true) { requests } else { 0 };
        let known = if judged.is_some() { requests } else { 0 };
        let totals = if payload.get("sums").is_some() {
            &mut payload["sums"][ordinal]["totals"]
        } else {
            &mut payload["totals"]
        };
        totals["success"] = success.into();
        totals["statusKnown"] = known.into();
        bodies::retire(&c, &body).unwrap();
        c.execute(
            "UPDATE daily SET body=?2 WHERE id=?1",
            params![id, bodies::encode(&c, &payload).unwrap()],
        )
        .unwrap();
        c.execute(
            "UPDATE usage_daily_metrics SET success=?3,status_known=?4 WHERE kind=1 AND owner=?1 AND ordinal=?2",
            params![id, ordinal as i64, success, known],
        )
        .unwrap();
    }
    storage_v12::metrics_view_of_version_14(&c);
    c.pragma_update(None, "user_version", 14).unwrap();
}
/// Each rollup's day and stored body, by its id.
fn rollups(c: &Connection) -> BTreeMap<String, (i64, String)> {
    c.prepare("SELECT id,day,body FROM daily")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}
/// Each rollup's payload in full, by its id.
fn payloads(c: &Connection) -> BTreeMap<String, Value> {
    rollups(c)
        .into_iter()
        .map(|(id, (_, body))| (id, bodies::unpack(c, &body).unwrap()))
        .collect()
}
/// Every row the rollups project, every column of it.
fn projected(c: &Connection) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = c
        .prepare("SELECT * FROM usage_daily_metrics ORDER BY kind,owner,ordinal")
        .unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}
/// What the gateway's rollups project of a model: requests, known outcomes
/// and successes.
fn projected_of(c: &Connection, model: &str) -> (u64, u64, u64) {
    c.query_row(
        "SELECT sum(m.requests),sum(m.status_known),sum(m.success) FROM usage_daily_metrics m JOIN daily d ON d.id=m.owner WHERE m.model=?1 AND json_extract(d.body,'$.scope')='proxy'",
        [model],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .unwrap()
}
/// The store's number, its generation of Claude sessions, the schema's cookie
/// and what makes up the schema.
fn state(c: &Connection) -> (i64, Option<String>, i64, Vec<String>) {
    let version = c
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let generation = c
        .query_row(
            "SELECT (SELECT value FROM metadata WHERE key='source_generation_claude')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let cookie = c
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .unwrap();
    let made = c
        .prepare(
            "SELECT concat_ws('|',type,name,tbl_name,sql) FROM sqlite_master ORDER BY type,name",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    (version, generation, cookie, made)
}
fn summaries(store: &Store) -> Vec<Value> {
    SCOPES
        .iter()
        .map(|scope| serde_json::to_value(store.dashboard(&filter(*scope)).unwrap()).unwrap())
        .collect()
}
fn case(source: &str, status: Option<u16>, outcome: &str) -> usize {
    CASES
        .iter()
        .position(|case| (case.0, case.1, case.2) == (source, status, outcome))
        .unwrap()
}

#[test]
fn a_database_of_version_14_is_taken_over_with_its_rollups_counted_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    // Two requests of every case go into the rollups, a third stays a record.
    let mut archived = requests();
    // A request whose first attempt failed and whose second the client gave
    // up, priced and not: only what its last attempt adds is counted again.
    for copy in 0..2 {
        let mut retried = stored(
            case("proxy", Some(200), "cancelled"),
            CASES[0],
            copy,
            copy == 0,
        );
        retried.id = format!("fixture-retried-{copy}");
        retried.completed = false;
        let mut first = retried.attempts[0].clone();
        (first.status, first.outcome) = (Some(500), "failure".into());
        retried.attempts[0].outcome = "cancelled".into();
        for attempt in [&mut first, &mut retried.attempts[0]] {
            attempt.requested_model = Some("fixture-retried".into());
            attempt.response_model = Some("fixture-retried".into());
            attempt.pricing_model = Some("fixture-retried".into());
            if let Some(price) = &mut attempt.price {
                price.model = "fixture-retried".into();
            }
        }
        retried.attempts.insert(0, first);
        archived.push(retried);
    }
    let live: Vec<_> = CASES
        .iter()
        .enumerate()
        .map(|(index, case)| stored(index, *case, 2, true))
        .collect();
    store.write_batch(&archived, None).unwrap();
    store.write_batch(&live, None).unwrap();
    let ids: Vec<_> = archived.iter().map(|r| r.id.clone()).collect();
    store.archive_batch(&ids).unwrap();
    let (shared, exact) = rollup_kinds(&store.connection);
    assert!(shared > 0 && exact > 0);
    // What this version makes of them, which taking the older one over must
    // arrive at.
    let written = (
        summaries(&store),
        payloads(&store.connection),
        projected(&store.connection),
    );
    let view: String = store
        .connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='usage_metrics'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    store
        .connection
        .execute(
            "INSERT INTO metadata VALUES('source_generation_claude','3')",
            [],
        )
        .unwrap();
    drop(store);

    into_version_14(dir.path());
    let cancelled = model(case("proxy", Some(200), "cancelled"));
    let unanswered = model(case("proxy", None, "failure"));
    let old = connection(dir.path());
    let (old_rollups, old_payloads) = (rollups(&old), payloads(&old));
    assert_ne!(projected(&old), written.2);
    // Version 14 held a request the client gave up for a failure, and knew
    // nothing of a failure that came with no status.
    assert_eq!(projected_of(&old, &cancelled), (2, 2, 0));
    assert_eq!(projected_of(&old, &unanswered), (2, 0, 0));
    assert_eq!(projected_of(&old, "fixture-retried"), (2, 2, 0));
    assert_eq!(state(&old).0, 14);
    drop(old);

    let store = Store::open(dir.path()).unwrap();
    let c = &store.connection;
    let (version, generation, _, made) = state(c);
    assert_eq!((version, generation.as_deref()), (15, Some("4")));
    assert!(made.contains(&format!("view|usage_metrics|usage_metrics|{view}")));
    assert_eq!(projected_of(c, &cancelled), (2, 0, 0));
    assert_eq!(projected_of(c, &unanswered), (2, 2, 0));
    assert_eq!(projected_of(c, "fixture-retried"), (2, 0, 0));
    // Every summary, every rollup and every row a rollup projects is what
    // this version writes: nothing but the two counts was changed.
    assert_eq!(summaries(&store), written.0);
    assert_eq!(payloads(c), written.1);
    assert_eq!(projected(c), written.2);
    // A rollup that counted so already was not written again; every other
    // kept its id and its day.
    let new_rollups = rollups(c);
    assert_eq!(new_rollups.len(), old_rollups.len());
    let mut rewritten = 0;
    for (id, (day, body)) in &new_rollups {
        let (old_day, old_body) = &old_rollups[id];
        assert_eq!(day, old_day);
        let same = old_payloads[id] == written.1[id];
        assert_eq!(body == old_body, same, "rollup {id}");
        rewritten += u64::from(!same);
    }
    assert!(rewritten > 0 && rewritten < new_rollups.len() as u64);
    for scope in SCOPES {
        let counts = counted(&store, scope);
        for (model, per_copy) in expected(scope, 1) {
            let (requests, known, success) = per_copy;
            assert_eq!(
                counts[&model],
                (3 * requests, 3 * known, 3 * success),
                "{model} of {scope:?}"
            );
        }
    }

    // Opening it again does none of it again.
    let taken = (state(c), new_rollups);
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(
        (state(&store.connection), rollups(&store.connection)),
        taken
    );
    assert_eq!(summaries(&store), written.0);
}

#[test]
fn counting_a_rollup_again_leaves_whatever_else_it_projects() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let requests = requests();
    store.write_batch(&requests, None).unwrap();
    let ids: Vec<_> = requests.iter().map(|r| r.id.clone()).collect();
    store.archive_batch(&ids).unwrap();
    let written = projected(&store.connection);
    drop(store);

    into_version_14(dir.path());
    let old = connection(dir.path());
    // What a rollup projects is what every summary reads, also where its
    // example would give something else: a count the summaries never had, a
    // time measured otherwise, a cost and a number of attempts of its own.
    old.execute_batch(
        "UPDATE usage_daily_metrics SET cache_read=7,duration_ms=duration_ms+7,cost='0.25',attempts=attempts+1",
    )
    .unwrap();
    let before = projected(&old);
    let counts: Vec<usize> = old
        .prepare("SELECT cid FROM pragma_table_info('usage_daily_metrics') WHERE name IN ('success','status_known')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(counts.len(), 2);
    drop(old);

    let store = Store::open(dir.path()).unwrap();
    let after = projected(&store.connection);
    assert_eq!(after.len(), before.len());
    let mut counted_again = 0;
    for ((after, before), written) in after.iter().zip(&before).zip(&written) {
        for (column, value) in after.iter().enumerate() {
            // The two counts are what this version writes; all else is as it
            // was found.
            let expected = if counts.contains(&column) {
                &written[column]
            } else {
                &before[column]
            };
            assert_eq!(value, expected, "column {column} of {:?}", &after[..3]);
        }
        counted_again += usize::from(after != before);
    }
    assert!(counted_again > 0 && counted_again < after.len());
}

#[test]
fn a_database_of_version_14_that_holds_nothing_is_taken_over_too() {
    let dir = tempfile::tempdir().unwrap();
    drop(Store::open(dir.path()).unwrap());
    let made = state(&connection(dir.path())).3;
    into_version_14(dir.path());
    assert_eq!(state(&connection(dir.path())).0, 14);

    let store = Store::open(dir.path()).unwrap();
    let (version, generation, _, now) = state(&store.connection);
    // The Claude sessions are read again whether or not any was read before.
    assert_eq!((version, generation.as_deref()), (15, Some("1")));
    assert_eq!(now, made);
    assert_eq!(store.dashboard(&filter(None)).unwrap().totals.requests, 0);
}
