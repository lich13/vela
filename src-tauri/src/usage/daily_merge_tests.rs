//! Priced requests of a day that differ only in what they consumed share one
//! archived rollup, and no summary may tell.
//!
//! Every fixture is synthetic and lives in a private temporary directory.

use super::{bodies, model::*, pricing::Pricing, query, storage_v12, store::Store};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const DAY: i64 = 86_400_000;
const BASE: i64 = 20_000 * DAY;

/// An hour into the local day `offset` days after the one holding `BASE`.
fn day(offset: i64) -> i64 {
    use chrono::TimeZone;
    let local = chrono::Local.timestamp_millis_opt(BASE).single().unwrap();
    let date = local.date_naive() + chrono::Duration::days(offset);
    chrono::Local
        .from_local_datetime(&date.and_hms_opt(1, 0, 0).unwrap())
        .earliest()
        .unwrap()
        .timestamp_millis()
}

fn price(cost: &str, version: &str) -> PriceSnapshot {
    PriceSnapshot {
        version: version.into(),
        source: "fixture".into(),
        model: "fixture-model".into(),
        multiplier: "1".into(),
        rates: BTreeMap::from([
            ("input".into(), "0.01".into()),
            ("output".into(), "0.02".into()),
        ]),
        cost: cost.into(),
        basis: Some(json!({
            "input_cost_per_token": "0.01",
            "output_cost_per_token": "0.02"
        })),
    }
}

fn tokens(input: u64, output: u64) -> Tokens {
    Tokens {
        input: Some(input),
        output: Some(output),
        ..Tokens::default()
    }
}

fn attempt(source: &str, model: &str, tokens: Tokens, cost: Option<&str>) -> Attempt {
    let gateway = source == "proxy";
    Attempt {
        provider: gateway.then(|| "fixture-provider".into()),
        requested_model: Some(model.into()),
        response_model: Some(model.into()),
        pricing_model: Some(model.into()),
        status: gateway.then_some(200),
        outcome: if gateway { "success" } else { "completed" }.into(),
        duration_ms: 100 + tokens.output.unwrap_or(0),
        first_token_ms: gateway.then_some(10 + tokens.input.unwrap_or(0) % 7),
        stream: gateway,
        transport: if gateway { "http" } else { "session" }.into(),
        tokens,
        price: cost.map(|cost| price(cost, "fixture-price-1")),
        ..Attempt::default()
    }
}

fn record(id: &str, source: &str, at: i64, attempts: Vec<Attempt>) -> Record {
    let mut record = Record {
        id: id.into(),
        client: if source == "grok" { "grok" } else { "codex" }.into(),
        source: source.into(),
        started_at: at - 100,
        finished_at: Some(at),
        attempts,
        completed: true,
        ..Record::default()
    };
    for (i, attempt) in record.attempts.iter_mut().enumerate() {
        attempt.id = format!("{id}-attempt-{i}");
        attempt.response_id = Some(format!("{id}-response-{i}"));
        attempt.started_at = at - 100;
    }
    record
}

/// A gateway request of one priced attempt.
fn gateway(id: &str, at: i64, model: &str, usage: Tokens, cost: &str) -> Record {
    record(
        id,
        "proxy",
        at,
        vec![attempt("proxy", model, usage, Some(cost))],
    )
}

/// Requests of every source and operation over two days. Within a group they
/// differ in nothing but what they consumed.
fn fixture() -> Vec<Record> {
    let mut rows = Vec::new();
    // One model at one price: counts and costs of every kind, a cache count
    // unreported, reported as zero, and reported.
    for (i, (cache_read, cost)) in [
        (None, "0.012"),
        (Some(0), "0.5"),
        (Some(40), "1.25"),
        (None, "0"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut usage = tokens(100 + i as u64, 20 + i as u64);
        usage.cache_read = cache_read;
        let at = day(0) + i as i64 * 1_000;
        rows.push(gateway(
            &format!("fixture-plain-{i}"),
            at,
            "fixture-model",
            usage,
            cost,
        ));
    }
    // The same model at another version of its price.
    for i in 0..2u64 {
        let at = day(0) + 10_000 + i as i64;
        let mut row = gateway(
            &format!("fixture-repriced-{i}"),
            at,
            "fixture-model",
            tokens(200 + i, 30),
            "0.07",
        );
        row.attempts[0].price = Some(price("0.07", "fixture-price-2"));
        rows.push(row);
    }
    // Another model, and the first one on another day. Neither day's requests
    // report a cache count for the other model; one of the next day's reports
    // a zero, which a summary must show as zero.
    for i in 0..2u64 {
        rows.push(gateway(
            &format!("fixture-other-model-{i}"),
            day(0) + 20_000 + i as i64,
            "fixture-other-model",
            tokens(300 + i, 40),
            "0.3",
        ));
        let mut usage = tokens(400 + i, 50);
        usage.cache_read = (i == 0).then_some(0);
        rows.push(gateway(
            &format!("fixture-next-day-{i}"),
            day(1) + i as i64,
            "fixture-model",
            usage,
            "0.25",
        ));
    }
    // A supplier that failed before another one answered, both priced.
    for i in 0..2u64 {
        let mut failed = attempt("proxy", "fixture-model", tokens(5 + i, 0), Some("0.001"));
        failed.provider = Some("fixture-fallback".into());
        failed.status = Some(503);
        failed.outcome = "failed".into();
        let answered = attempt("proxy", "fixture-model", tokens(500 + i, 60), Some("0.9"));
        rows.push(record(
            &format!("fixture-retried-{i}"),
            "proxy",
            day(0) + 30_000 + i as i64,
            vec![failed, answered],
        ));
    }
    // A request that failed, and the two other operations.
    let mut failed = gateway(
        "fixture-failed",
        day(0) + 40_000,
        "fixture-model",
        tokens(600, 0),
        "0.006",
    );
    failed.attempts[0].status = Some(500);
    failed.attempts[0].outcome = "failed".into();
    rows.push(failed);
    for (name, operation) in [
        ("compaction", Operation::Compaction),
        ("search", Operation::WebSearch),
    ] {
        let mut row = gateway(
            &format!("fixture-{name}"),
            day(0) + 50_000,
            "fixture-model",
            tokens(700, 70),
            "0.01",
        );
        row.attempts[0].operation = operation;
        rows.push(row);
    }
    // Session files: three requests of their own, and one the gateway saw too.
    for i in 0..3u64 {
        rows.push(record(
            &format!("fixture-session-{i}"),
            "codex",
            day(0) + 60_000 + i as i64,
            vec![attempt(
                "codex",
                "fixture-model",
                tokens(800 + i, 80),
                Some("0.4"),
            )],
        ));
    }
    let both = gateway(
        "fixture-gateway-seen",
        day(0) + 65_000,
        "fixture-model",
        tokens(850, 85),
        "0.45",
    );
    let mut seen = both.clone();
    seen.id = "fixture-session-seen".into();
    seen.source = "codex".into();
    seen.attempts[0].id = "fixture-session-seen-attempt".into();
    rows.extend([both, seen]);
    // A Grok prompt answered by two models.
    for i in 0..3u64 {
        rows.push(record(
            &format!("fixture-grok-{i}"),
            "grok",
            day(0) + 70_000 + i as i64,
            vec![
                attempt(
                    "grok",
                    "fixture-grok-fast",
                    tokens(900 + i, 90),
                    Some("0.02"),
                ),
                attempt(
                    "grok",
                    "fixture-grok-deep",
                    tokens(950 + i, 95),
                    Some("0.08"),
                ),
            ],
        ));
    }
    // No price: two requests alike in everything, and one that is not.
    for (i, input) in [1_000, 1_000, 1_001].into_iter().enumerate() {
        let mut row = record(
            &format!("fixture-unpriced-{i}"),
            "proxy",
            day(0) + 80_000 + i as i64,
            vec![attempt(
                "proxy",
                "fixture-unlisted",
                tokens(input, 10),
                None,
            )],
        );
        row.attempts[0].first_token_ms = Some(10);
        row.attempts[0].duration_ms = 100;
        rows.push(row);
    }
    rows
}

/// Every summary a view can ask for, each as the text the interface receives.
fn summaries(store: &Store) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let whole_day = |offset: i64| (day(offset) - 3_600_000, day(offset + 1) - 3_600_001);
    for source in [None, Some("proxy"), Some("sessions"), Some("grok_sessions")] {
        let view = Filter {
            source: source.map(str::to_owned),
            ..Filter::default()
        };
        let mut filters = vec![("everything".to_string(), view.clone())];
        for client in ["codex", "grok"] {
            filters.push((
                format!("client {client}"),
                Filter {
                    client: Some(client.into()),
                    ..view.clone()
                },
            ));
        }
        for provider in ["fixture-provider", "fixture-fallback", "session"] {
            filters.push((
                format!("provider {provider}"),
                Filter {
                    provider: Some(provider.into()),
                    ..view.clone()
                },
            ));
        }
        for model in [
            "fixture-model",
            "fixture-other-model",
            "fixture-unlisted",
            "fixture-grok-fast",
            "fixture-grok-deep",
            "web_search",
        ] {
            filters.push((
                format!("model {model}"),
                Filter {
                    model: Some(model.into()),
                    ..view.clone()
                },
            ));
        }
        for operation in ["model", "compaction", "web_search"] {
            filters.push((
                format!("operation {operation}"),
                Filter {
                    operation: Some(operation.into()),
                    ..view.clone()
                },
            ));
        }
        for (name, (start, end)) in [
            ("first day", whole_day(0)),
            ("second day", whole_day(1)),
            ("both days", (whole_day(0).0, whole_day(1).1)),
            ("later", whole_day(2)),
        ] {
            filters.push((
                name.to_string(),
                Filter {
                    start: Some(start),
                    end: Some(end),
                    ..view.clone()
                },
            ));
        }
        for (name, filter) in filters {
            out.insert(
                format!("{source:?} {name}"),
                serde_json::to_string(&store.dashboard(&filter).unwrap()).unwrap(),
            );
        }
    }
    for client in ["", "codex", "grok"] {
        let models = query::calculator_models(&store.connection, client).unwrap();
        out.insert(
            format!("calculator models {client}"),
            serde_json::to_string(&models).unwrap(),
        );
    }
    out
}

/// What the calculator totals by supplier and model. Compared between archived
/// forms only: a live record leaves the measured output out of these.
fn calculator(store: &Store) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for provider in [None, Some("fixture-provider"), Some("fixture-fallback")] {
        for model in [None, Some("fixture-model"), Some("gpt-4o")] {
            let filter = Filter {
                provider: provider.map(str::to_owned),
                model: model.map(str::to_owned),
                ..Filter::default()
            };
            let totals = query::calculator_totals(&store.connection, &filter).unwrap();
            out.insert(
                format!("{provider:?} {model:?}"),
                serde_json::to_string(&totals).unwrap(),
            );
        }
    }
    out
}

/// The texts are compared whole, so that a count reported as zero is not
/// taken for one that is unreported, nor a cost for an equal one written
/// otherwise.
#[track_caller]
fn assert_unchanged(now: &BTreeMap<String, String>, before: &BTreeMap<String, String>) {
    assert_eq!(now.len(), before.len());
    for (name, text) in before {
        assert_eq!(now[name], *text, "{name}");
    }
}

fn count(store: &Store, sql: &str) -> i64 {
    store.connection.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// The rollups of the view of everything, as they are stored in full.
fn rollups(store: &Store, condition: &str) -> Vec<Value> {
    let mut statement = store
        .connection
        .prepare(&format!(
            "SELECT body FROM daily WHERE json_extract(body,'$.scope')='all' AND {condition} ORDER BY day,id"
        ))
        .unwrap();
    let bodies: Vec<String> = statement
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    bodies
        .iter()
        .map(|body| bodies::unpack(&store.connection, body).unwrap())
        .collect()
}

/// Every rollup as it is stored in full, and every row a rollup projects.
fn stored(store: &Store) -> (Vec<(String, i64, Value)>, Vec<String>) {
    let mut statement = store
        .connection
        .prepare("SELECT id,day,body FROM daily ORDER BY id")
        .unwrap();
    let rollups = statement
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(|row| {
            let (id, day, body) = row.unwrap();
            (id, day, bodies::unpack(&store.connection, &body).unwrap())
        })
        .collect();
    let mut statement = store
        .connection
        .prepare("SELECT * FROM usage_daily_metrics ORDER BY owner,ordinal")
        .unwrap();
    let columns = statement.column_count();
    let projected = statement
        .query_map([], |r| {
            (0..columns)
                .map(|i| Ok(format!("{:?}", r.get::<_, rusqlite::types::Value>(i)?)))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .map(|row| row.unwrap().join(","))
        .collect();
    (rollups, projected)
}

/// Archives the requests as an earlier version did: into a rollup for each
/// exact usage.
fn archive_exact(store: &mut Store, rows: &[Record]) {
    store.write_batch(rows, None).unwrap();
    let ids: Vec<_> = rows.iter().map(|r| r.id.clone()).collect();
    for chunk in ids.chunks(3) {
        store.archive_batch_exact(chunk).unwrap();
    }
}

/// A database of an earlier version says nothing of shared rollups.
fn unmark(store: &Store) {
    store
        .connection
        .execute("DELETE FROM metadata WHERE key='storage_daily'", [])
        .unwrap();
}

/// How many payloads are kept beyond one for each rollup.
fn spare_payloads(store: &Store) -> i64 {
    count(
        store,
        "SELECT (SELECT count(*) FROM v12_archives)-(SELECT count(*) FROM daily)",
    )
}

/// Merges what is left, in batches of `limit` rollups.
fn merge_rest(store: &mut Store, limit: usize) {
    let mut batches = 0;
    while store.merge_daily_batch(limit).unwrap() {
        batches += 1;
        assert!(batches < 64, "the rollups were not all merged");
    }
}

/// The rows of each table a rollup takes room in, and the pages of the file.
fn sizes(store: &Store) -> [(&'static str, i64); 5] {
    [
        ("daily", "SELECT count(*) FROM daily"),
        (
            "usage_daily_metrics",
            "SELECT count(*) FROM usage_daily_metrics",
        ),
        ("v12_archives", "SELECT count(*) FROM v12_archives"),
        ("v12_templates", "SELECT count(*) FROM v12_templates"),
        ("pages", "PRAGMA page_count"),
    ]
    .map(|(name, sql)| (name, count(store, sql)))
}

const RECLAIMING: Option<&str> = Some("用量存储正在回收空间");
const RETIRED: &str = "SELECT count(*) FROM v12_dead_diagnostics";
const MERGED: &str = "json_extract(body,'$.merged')";
const EXACT: &str = "json_extract(body,'$.merged') IS NULL";
const EXACT_PRICED: &str =
    "json_extract(body,'$.merged') IS NULL AND json_extract(body,'$.totals.unpriced')=0";

#[test]
fn priced_requests_differing_only_in_usage_share_a_rollup_and_no_summary_changes() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let rows = fixture();
    store.write_batch(&rows, None).unwrap();
    assert!(store
        .detail("fixture-session-seen")
        .unwrap()
        .duplicate_of
        .is_some());
    let before = summaries(&store);

    // A few at a time, so that a rollup is written again and again.
    let (unpriced, priced): (Vec<_>, Vec<_>) = rows
        .iter()
        .map(|r| r.id.clone())
        .partition(|id| id.starts_with("fixture-unpriced"));
    for chunk in priced.chunks(3) {
        store.archive_batch(chunk).unwrap();
    }
    // Each shared rollup keeps one payload: the ones it replaced went with them.
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM v12_archives WHERE rid NOT IN (SELECT json_extract(body,'$._usageArchive') FROM daily)"
        ),
        0
    );
    store.archive_batch(&unpriced).unwrap();
    assert_eq!(store.logs(&Filter::default()).unwrap().total, 0);
    assert_unchanged(&summaries(&store), &before);

    // One model at one price, the same at another price, another model, the
    // next day, a retried request, a failed one, either other operation, what
    // the session files hold and what both saw: ten rollups for nineteen requests.
    let merged = rollups(&store, MERGED);
    assert_eq!(merged.len(), 10);
    let requests: u64 = merged
        .iter()
        .map(|rollup| {
            let sums = rollup["sums"].as_array().unwrap();
            sums.last().unwrap()["totals"]["requests"].as_u64().unwrap()
        })
        .sum();
    assert_eq!(requests, 19);
    let plain = merged
        .iter()
        .find(|rollup| rollup["sums"][0]["totals"]["requests"] == 4)
        .unwrap();
    let sum = &plain["sums"][0];
    assert_eq!(sum["provider"], "fixture-provider");
    assert_eq!(sum["model"], "fixture-model");
    assert_eq!(sum["totals"]["tokens"]["input"], 406);
    assert_eq!(sum["totals"]["tokens"]["cacheRead"], 40);
    assert!(sum["totals"]["tokens"]["cacheWrite"].is_null());
    assert_eq!(sum["totals"]["cost"].to_string(), "1.762");
    // What a request alone held is gone from the example; what parts rollups stays.
    let kept = &plain["example"]["attempts"][0];
    for key in ["tokens", "durationMs", "firstTokenMs", "repeatCount", "id"] {
        assert!(kept.get(key).is_none(), "{key}");
    }
    assert!(kept["price"].get("cost").is_none());
    assert_eq!(kept["price"]["version"], "fixture-price-1");
    assert_eq!(kept["status"], 200);
    assert!(plain.get("totals").is_none());
    let retried = merged
        .iter()
        .find(|rollup| rollup["sums"].as_array().unwrap().len() == 2)
        .unwrap();
    assert_eq!(retried["sums"][0]["provider"], "fixture-fallback");
    assert_eq!(retried["sums"][0]["totals"]["requests"], 0);
    assert_eq!(retried["sums"][0]["totals"]["attempts"], 2);
    assert_eq!(retried["sums"][0]["totals"]["cost"].to_string(), "0.002");
    assert_eq!(retried["sums"][1]["totals"]["requests"], 2);
    assert_eq!(retried["sums"][1]["totals"]["success"], 2);
    // No request of the other model reported a cache count, and none is made
    // up; one of the next day's reported a zero, and that is what is kept.
    let consumed = |input: u64| {
        merged
            .iter()
            .map(|rollup| &rollup["sums"][0]["totals"]["tokens"])
            .find(|tokens| tokens["input"] == input)
            .unwrap()
    };
    assert!(consumed(601)["cacheRead"].is_null());
    assert_eq!(consumed(801)["cacheRead"], 0);

    // Requests without a price are rolled up as they were: alike in everything.
    let exact = rollups(&store, EXACT);
    assert_eq!(exact.len(), 2);
    assert!(exact.iter().all(|rollup| rollup.get("sums").is_none()));
    let mut counts: Vec<_> = exact
        .iter()
        .map(|rollup| rollup["totals"]["requests"].as_u64().unwrap())
        .collect();
    counts.sort_unstable();
    assert_eq!(counts, [1, 2]);
    assert_eq!(exact[0]["example"]["attempts"][0]["tokens"]["output"], 10);

    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert_unchanged(&summaries(&store), &before);
}

#[test]
fn a_shared_rollup_keeps_one_payload_and_one_shape_for_all_its_days() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let mut rows = Vec::new();
    for offset in 0..3 {
        for i in 0..5u64 {
            rows.push(gateway(
                &format!("fixture-shape-{offset}-{i}"),
                day(offset) + i as i64,
                "fixture-model",
                tokens(100 + i + offset as u64, 20 + i),
                &format!("0.{}{i}", offset + 1),
            ));
        }
    }
    store.write_batch(&rows, None).unwrap();
    let before = summaries(&store);
    for row in &rows {
        store.archive_batch(std::slice::from_ref(&row.id)).unwrap();
    }
    assert_unchanged(&summaries(&store), &before);
    // A rollup for each day in either view that holds gateway requests.
    assert_eq!(count(&store, "SELECT count(*) FROM daily"), 6);
    assert_eq!(count(&store, "SELECT count(*) FROM v12_archives"), 6);
    assert_eq!(count(&store, "SELECT count(*) FROM usage_daily_metrics"), 6);
    // Costs are numbers there, so the days of a view differ in numbers alone.
    assert_eq!(count(&store, "SELECT count(*) FROM v12_templates"), 2);
    let merged = rollups(&store, MERGED);
    assert_eq!(merged.len(), 3);
    assert_eq!(merged[0]["sums"][0]["totals"]["requests"], 5);
    assert_eq!(merged[0]["sums"][0]["totals"]["cost"].to_string(), "0.60");
    assert_eq!(merged[2]["sums"][0]["totals"]["cost"].to_string(), "1.60");
}

#[test]
fn requests_priced_after_they_were_archived_match_those_priced_before() {
    let dir = tempfile::tempdir().unwrap();
    let pricing = Pricing::new(&dir.path().join("prices")).unwrap();
    let rows: Vec<_> = [100, 100, 137]
        .into_iter()
        .enumerate()
        .map(|(i, input)| {
            let mut usage = attempt("proxy", "gpt-4o", tokens(input, 20), None);
            usage.first_token_ms = Some(10);
            record(
                &format!("fixture-late-price-{i}"),
                "proxy",
                day(0) + i as i64,
                vec![usage],
            )
        })
        .collect();
    let ids: Vec<_> = rows.iter().map(|r| r.id.clone()).collect();

    // Archived without a price: alike requests share an exact rollup.
    let mut late = Store::open(&dir.path().join("late")).unwrap();
    late.write_batch(&rows, None).unwrap();
    late.archive_batch(&ids).unwrap();
    assert_eq!(
        late.dashboard(&Filter::default()).unwrap().totals.unpriced,
        3
    );
    assert_eq!(rollups(&late, MERGED).len(), 0);
    late.backfill(&pricing).unwrap();
    let exact = rollups(&late, EXACT);
    assert_eq!(exact.len(), 2);
    assert!(exact
        .iter()
        .all(|rollup| rollup["totals"]["unpriced"] == 0 && rollup.get("sums").is_none()));

    // Priced first, they share one rollup; the summaries are the same.
    let mut early = Store::open(&dir.path().join("early")).unwrap();
    early.write_batch(&rows, None).unwrap();
    early.backfill(&pricing).unwrap();
    let live = summaries(&early);
    assert_unchanged(&summaries(&late), &live);
    early.archive_batch(&ids).unwrap();
    assert_eq!(rollups(&early, MERGED).len(), 1);
    assert_unchanged(&summaries(&early), &live);
    assert_unchanged(&calculator(&early), &calculator(&late));
    let totals = early.dashboard(&Filter::default()).unwrap().totals;
    assert_eq!((totals.requests, totals.unpriced), (3, 0));
    assert!(decimal(&totals.cost).is_some_and(|cost| !cost.is_zero()));

    // Rollups priced late are merged as any other written for an exact usage.
    unmark(&late);
    let totals = calculator(&late);
    merge_rest(&mut late, 4);
    assert_eq!(rollups(&late, MERGED).len(), 1);
    assert_eq!(rollups(&late, EXACT_PRICED).len(), 0);
    assert_unchanged(&summaries(&late), &live);
    assert_unchanged(&calculator(&late), &totals);
}

/// Gateway requests of the first day and the first model, at its first price.
fn plain(name: &str, count: u64) -> Vec<Record> {
    (0..count)
        .map(|i| {
            gateway(
                &format!("fixture-plain-{name}-{i}"),
                day(0) + 5_000 + i as i64,
                "fixture-model",
                tokens(150 + i, 25),
                "0.33",
            )
        })
        .collect()
}

#[test]
fn rollups_of_identical_requests_join_the_shared_ones_in_batches_that_can_be_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let rows = fixture();
    let ids: Vec<_> = rows.iter().map(|r| r.id.clone()).collect();

    // What the same requests leave when they are archived into shared rollups.
    let mut direct = Store::open(&dir.path().join("direct")).unwrap();
    direct.write_batch(&rows, None).unwrap();
    let live = summaries(&direct);
    direct.archive_batch(&ids).unwrap();

    let path = dir.path().join("earlier");
    let mut store = Store::open(&path).unwrap();
    archive_exact(&mut store, &rows);
    unmark(&store);
    assert_eq!(rollups(&store, MERGED).len(), 0);
    assert_unchanged(&summaries(&store), &live);
    let totals = calculator(&store);
    assert_unchanged(&totals, &calculator(&direct));
    let spare = spare_payloads(&store);

    // One small batch, then the process stops before the rest.
    assert!(store.merge_daily_batch(4).unwrap());
    assert!(!rollups(&store, EXACT_PRICED).is_empty());
    assert_unchanged(&summaries(&store), &live);
    assert_unchanged(&calculator(&store), &totals);
    drop(store);

    // It goes on where it stopped. Requests that grow old meanwhile are
    // archived into shared rollups, which the earlier ones then join.
    let mut store = Store::open(&path).unwrap();
    assert_unchanged(&summaries(&store), &live);
    assert!(store.merge_daily_batch(4).unwrap());
    let meanwhile = plain("meanwhile", 3);
    let meanwhile_ids: Vec<_> = meanwhile.iter().map(|r| r.id.clone()).collect();
    for store in [&mut store, &mut direct] {
        store.write_batch(&meanwhile, None).unwrap();
        store.archive_batch(&meanwhile_ids).unwrap();
    }
    let live = summaries(&direct);
    let totals = calculator(&direct);
    assert_unchanged(&summaries(&store), &live);
    assert_unchanged(&calculator(&store), &totals);
    let mut batches = 0;
    while store.merge_daily_batch(4).unwrap() {
        batches += 1;
        assert!(batches < 64, "the rollups were not all merged");
        assert_unchanged(&summaries(&store), &live);
        assert_unchanged(&calculator(&store), &totals);
    }
    assert!(batches > 2);
    // The rollups are those the requests would have been archived into, each
    // with one payload; the ones without a price are as they were.
    assert_eq!(stored(&store), stored(&direct));
    assert_eq!(rollups(&store, EXACT_PRICED).len(), 0);
    assert_eq!(spare_payloads(&store), spare);

    // Done: nothing is read again, now or after a restart.
    let merged = stored(&store);
    assert!(!store.merge_daily_batch(4).unwrap());
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert!(!store.merge_daily_batch(4).unwrap());
    assert_eq!(stored(&store), merged);
    assert_eq!(
        count(
            &store,
            "SELECT count(*) FROM metadata WHERE key LIKE 'storage_daily%'"
        ),
        1
    );

    // Requests archived from now on join the rollups that were merged.
    let later = plain("later", 3);
    let later_ids: Vec<_> = later.iter().map(|r| r.id.clone()).collect();
    for store in [&mut store, &mut direct] {
        store.write_batch(&later, None).unwrap();
        store.archive_batch(&later_ids).unwrap();
    }
    assert_eq!(stored(&store).0.len(), merged.0.len());
    assert_eq!(stored(&store), stored(&direct));
}

#[test]
fn a_merged_rollup_adds_up_what_was_projected_and_works_nothing_out_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    archive_exact(&mut store, &fixture());
    unmark(&store);
    // What a rollup projects is what every summary reads, also where its
    // example would give something else: a count it reports and the summaries
    // never had, a zero they had and it lacks, a time measured otherwise.
    store
        .connection
        .execute_batch(
            "UPDATE usage_daily_metrics SET cache_read=NULL,duration_ms=duration_ms+7 WHERE model='fixture-model' AND requests>0;
             UPDATE usage_daily_metrics SET cache_write=0 WHERE model='fixture-other-model';",
        )
        .unwrap();
    let before = summaries(&store);
    let totals = calculator(&store);
    assert!(before["None model fixture-model"].contains("\"cacheRead\":null"));
    assert!(before["None model fixture-other-model"].contains("\"cacheWrite\":0"));

    merge_rest(&mut store, 4);
    assert_eq!(rollups(&store, EXACT_PRICED).len(), 0);
    assert_unchanged(&summaries(&store), &before);
    assert_unchanged(&calculator(&store), &totals);
}

#[test]
fn a_rollup_that_does_not_project_a_priced_row_for_each_attempt_stays_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    archive_exact(&mut store, &fixture());
    unmark(&store);
    let owner =
        |sql: &str| -> String { store.connection.query_row(sql, [], |r| r.get(0)).unwrap() };
    // One of two rows is gone; another row says that a price is lacking.
    let halved = owner("SELECT owner FROM usage_daily_metrics WHERE ordinal=1 AND provider='fixture-provider' ORDER BY owner LIMIT 1");
    let unpriced = owner("SELECT owner FROM usage_daily_metrics WHERE input=100 AND model='fixture-model' ORDER BY owner LIMIT 1");
    store
        .connection
        .execute(
            "DELETE FROM usage_daily_metrics WHERE owner=?1 AND ordinal=0",
            [&halved],
        )
        .unwrap();
    store
        .connection
        .execute(
            "UPDATE usage_daily_metrics SET unpriced=1 WHERE owner=?1",
            [&unpriced],
        )
        .unwrap();
    let before = summaries(&store);
    let kept: Vec<_> = stored(&store)
        .0
        .into_iter()
        .filter(|(id, _, _)| *id == halved || *id == unpriced)
        .collect();
    assert_eq!(kept.len(), 2);

    merge_rest(&mut store, 4);
    assert_unchanged(&summaries(&store), &before);
    let (rollups, _) = stored(&store);
    assert!(kept.iter().all(|rollup| rollups.contains(rollup)));
    assert_eq!(
        count(
            &store,
            &format!("SELECT count(*) FROM daily WHERE {EXACT_PRICED}")
        ),
        2
    );
    assert!(!store.merge_daily_batch(4).unwrap());
}

#[test]
fn storage_batches_merge_the_rollups_and_their_space_is_reclaimed_after_the_last_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    // Requests of every kind, and more rollups than one batch reads.
    let mut rows = fixture();
    for offset in 0..2 {
        for i in 0..130u64 {
            rows.push(gateway(
                &format!("fixture-many-{offset}-{i}"),
                day(offset) + 90_000 + i as i64,
                "fixture-model",
                tokens(2_000 + i, 30),
                &format!("0.{:04}", 1 + i),
            ));
        }
    }
    archive_exact(&mut store, &rows);
    // An earlier version has packed every payload and reclaimed the space.
    while store.compact_storage_batch().unwrap() {}
    store.reclaim_space().unwrap();
    assert_eq!(store.maintenance_status(), None);
    unmark(&store);
    let before = summaries(&store);
    let totals = calculator(&store);
    let was = sizes(&store);
    let exact = |store: &Store| {
        count(
            store,
            &format!("SELECT count(*) FROM daily WHERE {EXACT_PRICED}"),
        )
    };
    let all = exact(&store);
    assert!(all > 520);

    // One small batch marks the space it leaves, and what it wrote is packed,
    // as archiving details does at any time. Space asked for now is still not
    // reclaimed: the payloads that went stay listed, and so does the mark.
    assert!(store.merge_daily_batch(4).unwrap());
    assert_eq!(store.maintenance_status().as_deref(), RECLAIMING);
    while storage_v12::compact_diagnostics(&store.connection).unwrap() {}
    store.reclaim_space().unwrap();
    assert_eq!(store.maintenance_status().as_deref(), RECLAIMING);
    let retired = count(&store, RETIRED);
    assert!(retired > 0);
    drop(store);

    // After a restart each call goes on by one bounded batch.
    let mut store = Store::open(dir.path()).unwrap();
    assert_unchanged(&summaries(&store), &before);
    assert!(store.compact_storage_batch().unwrap());
    let left = exact(&store);
    assert!(0 < left && left < all - 4, "{left} of {all}");
    store.reclaim_space().unwrap();
    assert_eq!(store.maintenance_status().as_deref(), RECLAIMING);
    assert!(count(&store, RETIRED) > retired);
    assert_unchanged(&summaries(&store), &before);
    let mut batches = 0;
    while store.compact_storage_batch().unwrap() {
        batches += 1;
        assert!(batches < 64, "the storage batches did not finish");
    }
    assert_eq!(exact(&store), 0);
    assert_unchanged(&summaries(&store), &before);
    assert_unchanged(&calculator(&store), &totals);

    // Now the space is reclaimed, and every table holds less than it did.
    store.reclaim_space().unwrap();
    assert_eq!(store.maintenance_status(), None);
    assert_eq!(count(&store, RETIRED), 0);
    assert_eq!(spare_payloads(&store), 0);
    assert_unchanged(&summaries(&store), &before);
    assert_unchanged(&calculator(&store), &totals);
    for ((name, now), (_, was)) in sizes(&store).into_iter().zip(was) {
        assert!(now < was, "{name}: {now} of {was}");
    }

    // Done: a further call changes nothing, now or after a restart.
    let merged = stored(&store);
    assert!(!store.compact_storage_batch().unwrap());
    drop(store);
    let mut store = Store::open(dir.path()).unwrap();
    assert!(!store.compact_storage_batch().unwrap());
    assert_eq!(stored(&store), merged);
    assert_unchanged(&summaries(&store), &before);
}
