//! Priced requests of a day that differ only in what they consumed share one
//! archived rollup, and no summary may tell.
//!
//! Every fixture is synthetic and lives in a private temporary directory.

use super::{bodies, model::*, pricing::Pricing, query, store::Store};
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
    // Another model, and the first one on another day.
    for i in 0..2u64 {
        rows.push(gateway(
            &format!("fixture-other-model-{i}"),
            day(0) + 20_000 + i as i64,
            "fixture-other-model",
            tokens(300 + i, 40),
            "0.3",
        ));
        rows.push(gateway(
            &format!("fixture-next-day-{i}"),
            day(1) + i as i64,
            "fixture-model",
            tokens(400 + i, 50),
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
    let merged = rollups(&store, "json_extract(body,'$.merged')");
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

    // Requests without a price are rolled up as they were: alike in everything.
    let exact = rollups(&store, "json_extract(body,'$.merged') IS NULL");
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
    let merged = rollups(&store, "json_extract(body,'$.merged')");
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
    assert_eq!(rollups(&late, "json_extract(body,'$.merged')").len(), 0);
    late.backfill(&pricing).unwrap();
    let exact = rollups(&late, "json_extract(body,'$.merged') IS NULL");
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
    assert_eq!(rollups(&early, "json_extract(body,'$.merged')").len(), 1);
    assert_unchanged(&summaries(&early), &live);
    assert_unchanged(&calculator(&early), &calculator(&late));
    let totals = early.dashboard(&Filter::default()).unwrap().totals;
    assert_eq!((totals.requests, totals.unpriced), (3, 0));
    assert!(decimal(&totals.cost).is_some_and(|cost| !cost.is_zero()));
}
