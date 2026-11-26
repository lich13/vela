// 覆盖网关与会话来源的去重和用量汇总。
use super::{model::*, pricing::Pricing, sessions, store::Store};
use std::{
    fs,
    path::{Path, PathBuf},
};

const DAY: i64 = 86_400_000;
const BASE: i64 = 1_700_000_000_000;
const FIXTURE_CODEX_RESET: &str = include_str!("fixtures/v015/codex-counter-reset.jsonl");
const FIXTURE_CODEX_LANES: &str = include_str!("fixtures/v015/codex-multi-channel.jsonl");
const FIXTURE_CODEX_PARENT: &str = include_str!("fixtures/v015/codex-fork-parent.jsonl");
const FIXTURE_CODEX_FORK: &str = include_str!("fixtures/v015/codex-fork-child.jsonl");
const FIXTURE_CLAUDE_CACHE: &str = include_str!("fixtures/v015/claude-cache-completion.jsonl");
const FIXTURE_CLAUDE_SUBAGENT: &str =
    include_str!("fixtures/v015/claude-subagent-interrupted.jsonl");

fn fixture() -> (tempfile::TempDir, Store, Pricing) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let pricing = Pricing::new(dir.path()).unwrap();
    (dir, store, pricing)
}

fn all_time() -> Filter {
    Filter {
        start: Some(BASE - DAY),
        end: Some(BASE + 100 * DAY),
        ..Filter::default()
    }
}

fn write_fixture(root: &Path, relative: &str, contents: &str) -> PathBuf {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, contents).unwrap();
    path
}

fn sync_fixture(
    store: &mut Store,
    pricing: &Pricing,
    client: &str,
    root: &Path,
    relative: &str,
    contents: &str,
) -> sessions::Report {
    write_fixture(root, relative, contents);
    sessions::sync(store, pricing, &Settings::default(), client, root, false).unwrap()
}

#[allow(clippy::too_many_arguments)]
fn record(
    id: &str,
    client: &str,
    source: &str,
    at: i64,
    provider: Option<&str>,
    model: Option<&str>,
    tokens: Tokens,
    status: Option<u16>,
) -> Record {
    let attempt = Attempt {
        id: format!("{id}-attempt"),
        provider: provider.map(str::to_owned),
        requested_model: model.map(str::to_owned),
        response_model: model.map(str::to_owned),
        pricing_model: model.map(str::to_owned),
        status,
        outcome: if status.is_some_and(|s| (200..300).contains(&s)) {
            "completed"
        } else {
            "failed"
        }
        .into(),
        started_at: at,
        transport: if source == "proxy" { "http" } else { "session" }.into(),
        tokens,
        ..Attempt::default()
    };
    Record {
        id: id.into(),
        client: client.into(),
        source: source.into(),
        started_at: at,
        attempts: vec![attempt],
        completed: true,
        ..Record::default()
    }
}

fn usage(input: u64, output: u64) -> Tokens {
    Tokens {
        input: Some(input),
        output: Some(output),
        ..Tokens::default()
    }
}

fn codex_snapshot(at: i64, total: [u64; 3], last: [u64; 3]) -> String {
    serde_json::json!({
        "type": "event_msg",
        "timestamp": at,
        "payload": {
            "type": "token_count",
            "info": {
                "model": "gpt-fixture",
                "total_token_usage": {
                    "input_tokens": total[0],
                    "cached_input_tokens": total[1],
                    "output_tokens": total[2]
                },
                "last_token_usage": {
                    "input_tokens": last[0],
                    "cached_input_tokens": last[1],
                    "output_tokens": last[2]
                }
            }
        }
    })
    .to_string()
}

fn price_snapshot(cost: &str) -> PriceSnapshot {
    PriceSnapshot {
        version: "fixture-price-v1".into(),
        source: "fixture-catalog".into(),
        model: "fixture-model".into(),
        multiplier: "1.25".into(),
        rates: std::collections::BTreeMap::from([("input".into(), "0.00001".into())]),
        cost: cost.into(),
        basis: Some(serde_json::json!({"input_cost_per_token":"0.00001"})),
    }
}

#[test]
fn retry_usage_stays_with_the_p1_and_p2_attempts() {
    let (_dir, mut store, _pricing) = fixture();
    let mut request = record(
        "fixture-retry",
        "codex",
        "proxy",
        BASE,
        Some("fixture-p1"),
        Some("fixture-model"),
        usage(12, 2),
        Some(503),
    );
    request.attempts.push(Attempt {
        id: "fixture-retry-p2".into(),
        provider: Some("fixture-p2".into()),
        requested_model: Some("fixture-model".into()),
        response_model: Some("fixture-model".into()),
        pricing_model: Some("fixture-model".into()),
        status: Some(200),
        outcome: "completed".into(),
        started_at: BASE + 50,
        transport: "http".into(),
        tokens: usage(18, 3),
        ..Attempt::default()
    });
    store.write_batch(&[request], None).unwrap();

    let dashboard = store.dashboard(&all_time()).unwrap();
    assert_eq!(dashboard.totals.requests, 1);
    assert_eq!(dashboard.totals.attempts, 2);
    assert_eq!(dashboard.totals.tokens.input, Some(30));
    assert_eq!(dashboard.totals.tokens.output, Some(5));

    for (provider, input, output) in [("fixture-p1", 12, 2), ("fixture-p2", 18, 3)] {
        let group = dashboard
            .providers
            .iter()
            .find(|g| g.id == provider)
            .unwrap_or_else(|| panic!("missing provider group {provider}"));
        assert_eq!(group.totals.attempts, 1, "provider={provider}");
        assert_eq!(
            group.totals.tokens.input,
            Some(input),
            "provider={provider}"
        );
        assert_eq!(
            group.totals.tokens.output,
            Some(output),
            "provider={provider}"
        );
    }
}

#[test]
fn retry_pricing_counts_each_logical_request_once_and_keeps_provider_attribution() {
    let (_dir, mut store, _pricing) = fixture();
    let retry = |id: &str,
                 model: &str,
                 first_price: Option<PriceSnapshot>,
                 final_price: Option<PriceSnapshot>| {
        let first_provider = format!("{model}-p1");
        let final_provider = format!("{model}-p2");
        let mut request = record(
            id,
            "codex",
            "proxy",
            BASE,
            Some(&first_provider),
            Some(model),
            usage(12, 2),
            Some(503),
        );
        request.attempts[0].price = first_price;
        request.attempts.push(Attempt {
            id: format!("{id}-attempt-2"),
            provider: Some(final_provider),
            requested_model: Some(model.into()),
            response_model: Some(model.into()),
            pricing_model: Some(model.into()),
            status: Some(200),
            outcome: "completed".into(),
            started_at: BASE + 50,
            transport: "http".into(),
            tokens: usage(18, 3),
            price: final_price,
            ..Attempt::default()
        });
        request
    };

    let rows = [
        retry(
            "fixture-retry-unpriced-earlier",
            "fixture-retry-unpriced-earlier-model",
            None,
            Some(price_snapshot("0.25")),
        ),
        retry(
            "fixture-retry-unpriced-both",
            "fixture-retry-unpriced-both-model",
            None,
            None,
        ),
        retry(
            "fixture-retry-priced-both",
            "fixture-retry-priced-both-model",
            Some(price_snapshot("0.1")),
            Some(price_snapshot("0.2")),
        ),
        retry(
            "fixture-retry-zero-cost",
            "fixture-retry-zero-cost-model",
            Some(price_snapshot("0")),
            Some(price_snapshot("0")),
        ),
    ];
    store.write_batch(&rows, None).unwrap();

    for (model, total_unpriced, p1_unpriced, p2_unpriced) in [
        ("fixture-retry-unpriced-earlier-model", 1, 1, 0),
        ("fixture-retry-unpriced-both-model", 1, 1, 1),
        ("fixture-retry-priced-both-model", 0, 0, 0),
        ("fixture-retry-zero-cost-model", 0, 0, 0),
    ] {
        let dashboard = store
            .dashboard(&Filter {
                model: Some(model.into()),
                ..all_time()
            })
            .unwrap();
        assert_eq!(dashboard.totals.requests, 1, "model={model}");
        assert_eq!(dashboard.totals.attempts, 2, "model={model}");
        assert_eq!(dashboard.totals.unpriced, total_unpriced, "model={model}");
        assert_eq!(dashboard.sources.get("proxy"), Some(&1), "model={model}");

        for (suffix, expected) in [("p1", p1_unpriced), ("p2", p2_unpriced)] {
            let provider = format!("{model}-{suffix}");
            let group = dashboard
                .providers
                .iter()
                .find(|group| group.id == provider)
                .unwrap_or_else(|| panic!("missing provider group {provider}"));
            assert_eq!(group.totals.unpriced, expected, "provider={provider}");
        }
    }
}

#[test]
fn logs_page_and_filter_keep_the_requested_rows_and_order() {
    let (_dir, mut store, _pricing) = fixture();
    let rows: Vec<_> = (0..25)
        .map(|i| {
            record(
                &format!("fixture-request-{i}"),
                "codex",
                "proxy",
                BASE + i * 1000,
                Some(if i % 2 == 0 {
                    "fixture-p1"
                } else {
                    "fixture-p2"
                }),
                Some(if i % 3 == 0 {
                    "fixture-gpt"
                } else {
                    "fixture-claude"
                }),
                usage(10 + i as u64, 1),
                Some(if i % 4 == 0 { 503 } else { 200 }),
            )
        })
        .collect();
    store.write_batch(&rows, None).unwrap();

    let first = store.logs(&Filter::default()).unwrap();
    assert_eq!((first.total, first.page, first.rows.len()), (25, 1, 20));
    assert_eq!(first.rows[0].id, "fixture-request-24");
    let second = store
        .logs(&Filter {
            page: 2,
            ..Filter::default()
        })
        .unwrap();
    assert_eq!((second.total, second.page, second.rows.len()), (25, 2, 5));
    assert_eq!(second.rows[0].id, "fixture-request-4");
    assert_eq!(second.rows[4].id, "fixture-request-0");

    let filtered = store
        .logs(&Filter {
            start: Some(BASE + 10_000),
            end: Some(BASE + 20_000),
            client: Some("codex".into()),
            provider: Some("fixture-p1".into()),
            model: Some("fixture-gpt".into()),
            status: Some("5xx".into()),
            ..Filter::default()
        })
        .unwrap();
    assert_eq!(filtered.total, 1);
    assert_eq!(filtered.rows[0].id, "fixture-request-12");
}

#[test]
fn unique_cross_source_fallback_pairs_within_ten_minutes() {
    let (_dir, mut store, _pricing) = fixture();
    let mut proxy = record(
        "fixture-proxy-unique",
        "codex",
        "proxy",
        BASE,
        Some("fixture-p1"),
        Some("fixture-model"),
        usage(100, 20),
        Some(200),
    );
    let mut session = record(
        "fixture-session-unique",
        "codex",
        "codex",
        BASE + 599_999,
        None,
        Some("fixture-model"),
        usage(100, 20),
        None,
    );
    proxy.attempts[0].response_id = None;
    session.attempts[0].response_id = None;
    store.write_batch(&[proxy, session], None).unwrap();

    let page = store.logs(&Filter::default()).unwrap();
    assert_eq!(page.total, 1);
    let row = &page.rows[0];
    assert_eq!(row.id, "fixture-proxy-unique");
    assert_eq!(row.deduplication, "strict_match");
    assert!(row.merged_sources.contains(&"codex".into()));
    assert!(row.merged_sources.contains(&"proxy".into()));
}

#[test]
fn distinct_response_ids_block_time_based_cross_source_merging() {
    let (_dir, mut store, _pricing) = fixture();
    let mut proxy = record(
        "fixture-proxy-distinct",
        "codex",
        "proxy",
        BASE,
        Some("fixture-p1"),
        Some("fixture-model"),
        usage(100, 20),
        Some(200),
    );
    let mut session = record(
        "fixture-session-distinct",
        "codex",
        "codex",
        BASE + 100,
        None,
        Some("fixture-model"),
        usage(100, 20),
        None,
    );
    proxy.attempts[0].response_id = Some(safe_id("fixture-response-p1"));
    session.attempts[0].response_id = Some(safe_id("fixture-response-p2"));
    store.write_batch(&[proxy, session], None).unwrap();

    assert_eq!(store.logs(&Filter::default()).unwrap().total, 2);
}

fn cache_pair() -> (Record, Record) {
    let mut proxy = record(
        "fixture-cache-proxy",
        "codex",
        "proxy",
        BASE,
        Some("fixture-provider"),
        Some("fixture-model"),
        usage(100, 20),
        Some(200),
    );
    proxy.attempts[0].tokens.cache_read = None;
    proxy.attempts[0].duration_ms = 1_234;
    proxy.attempts[0].first_token_ms = Some(234);
    proxy.attempts[0].price = Some(price_snapshot("0.06283"));
    let mut session = record(
        "fixture-cache-session",
        "codex",
        "codex",
        BASE + 1_200,
        None,
        Some("fixture-model"),
        usage(100, 20),
        None,
    );
    session.attempts[0].tokens.cache_read = Some(0);
    session.attempts[0].price = Some(price_snapshot("0.06283"));
    (proxy, session)
}

#[test]
fn missing_and_zero_cache_match_in_both_layouts_and_import_orders_without_changing_raw_usage() {
    for reverse in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let (proxy, session) = cache_pair();
        let rows = if reverse {
            [session, proxy]
        } else {
            [proxy, session]
        };
        for row in rows {
            store.write_batch(&[row], None).unwrap();
        }
        let totals = store.dashboard(&all_time()).unwrap().totals;
        assert_eq!(totals.requests, 1);
        assert_eq!(totals.tokens.input, Some(100));
        assert_eq!(totals.tokens.output, Some(20));
        assert_eq!(totals.cost, "0.06283");
        let proxy = store.detail("fixture-cache-proxy").unwrap();
        let session = store.detail("fixture-cache-session").unwrap();
        assert_eq!(proxy.attempts[0].tokens.cache_read, None);
        assert_eq!(session.attempts[0].tokens.cache_read, Some(0));
        assert_eq!(proxy.attempts[0].duration_ms, 1_234);
        assert_eq!(proxy.attempts[0].first_token_ms, Some(234));
        assert_eq!(proxy.attempts[0].price.as_ref().unwrap().cost, "0.06283");
        assert_eq!(session.duplicate_of.as_deref(), Some("fixture-cache-proxy"));
        for source in ["proxy", "sessions"] {
            assert_eq!(
                store
                    .dashboard(&Filter {
                        source: Some(source.into()),
                        ..all_time()
                    })
                    .unwrap()
                    .totals
                    .requests,
                1
            );
        }
    }
}

#[test]
fn cache_alias_keeps_known_conflicts_identity_guards_and_bidirectional_uniqueness() {
    for mismatch in [
        "cache",
        "model",
        "provider",
        "operation",
        "response",
        "window",
        "multiple_proxies",
        "multiple_sessions",
    ] {
        let (_dir, mut store, _) = fixture();
        let (mut proxy, mut session) = cache_pair();
        let mut extra = None;
        match mismatch {
            "cache" => session.attempts[0].tokens.cache_read = Some(1),
            "model" => session.attempts[0].response_model = Some("fixture-other".into()),
            "provider" => session.attempts[0].provider = Some("fixture-other".into()),
            "operation" => session.attempts[0].operation = Operation::Compaction,
            "response" => {
                proxy.attempts[0].response_id = Some("fixture-first".into());
                session.attempts[0].response_id = Some("fixture-second".into());
            }
            "window" => session.attempts[0].started_at += 601_000,
            "multiple_proxies" => {
                let mut p = proxy.clone();
                p.id = "fixture-other-proxy".into();
                extra = Some(p);
            }
            "multiple_sessions" => {
                let mut s = session.clone();
                s.id = "fixture-other-session".into();
                extra = Some(s);
            }
            _ => unreachable!(),
        }
        store.write_batch(&[proxy, session], None).unwrap();
        if let Some(extra) = extra {
            store.write_batch(&[extra], None).unwrap();
        }
        let session = store.detail("fixture-cache-session").unwrap();
        assert!(session.duplicate_of.is_none(), "{mismatch}");
        assert_eq!(
            session.deduplication,
            if mismatch.starts_with("multiple") {
                "ambiguous"
            } else {
                ""
            },
            "{mismatch}"
        );
    }
}

#[test]
fn archived_cache_alias_prevents_double_billing_without_guessing_a_pair() {
    let (_dir, mut store, _) = fixture();
    let (proxy, session) = cache_pair();
    store.connection.execute("INSERT INTO receipts(id,client,source,time,provider,response,signature,ended,effective,operation) VALUES(usage_key(?1),'codex','proxy',?2,usage_key(?3),NULL,usage_key(?4),?5,1,'model')",
        rusqlite::params![proxy.id, BASE, proxy.attempts[0].provider, proxy.signature(), BASE + 1_234]).unwrap();
    store.write_batch(&[session], None).unwrap();
    let row = store.detail("fixture-cache-session").unwrap();
    assert_eq!(row.deduplication, "ambiguous");
    assert!(row.duplicate_of.is_none());
    assert_eq!(store.dashboard(&all_time()).unwrap().totals.requests, 0);
}

/// Puts a database back into the shape it had before a record, and the
/// receipt left of it, kept the key it came in with: the column is gone from
/// both, and the two views that list a record's columns, with the triggers of
/// the first, are made again from their own text without it.
fn without_entry_column(dir: &Path) {
    let c = rusqlite::Connection::open(dir.join("usage.sqlite")).unwrap();
    super::query::register(&c).unwrap();
    // The views come before the triggers of one of them.
    let made = c
        .prepare("SELECT sql FROM sqlite_master WHERE tbl_name IN ('v12_records','records') ORDER BY type DESC,name DESC")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(made.len(), 4, "two views and the two triggers of one");
    let made = made.join(";\n");
    assert_eq!(
        made.matches(",r.entry").count(),
        2,
        "each view names the column once"
    );
    // Dropping the column checks every view, so they are whole again first.
    c.execute_batch(&format!(
        "BEGIN;
         DROP VIEW records; DROP VIEW v12_records;
         {};
         ALTER TABLE v12_rows DROP COLUMN entry;
         ALTER TABLE receipts DROP COLUMN entry;
         COMMIT;",
        made.replace(",r.entry", "")
    ))
    .unwrap();
}

/// How many of the two, a record and a receipt, have the column for the key
/// a request came in with.
fn entry_columns(c: &rusqlite::Connection) -> i64 {
    c.query_row(
        "SELECT (SELECT count(*) FROM pragma_table_info('v12_rows') WHERE name='entry')+(SELECT count(*) FROM pragma_table_info('receipts') WHERE name='entry')",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

/// Puts the receipts and cursors of a database back into the tables that
/// version 13 kept them in: identifiers and cursor bodies as text. No record
/// of that version kept a gateway key either.
fn into_version_13(dir: &Path) {
    without_entry_column(dir);
    let c = rusqlite::Connection::open(dir.join("usage.sqlite")).unwrap();
    super::query::register(&c).unwrap();
    c.execute_batch(
        "BEGIN;
         DROP INDEX receipt_response; DROP INDEX receipt_signature;
         ALTER TABLE receipts RENAME TO receipts_now; ALTER TABLE cursors RENAME TO cursors_now;
         CREATE TABLE cursors(id TEXT PRIMARY KEY,body TEXT NOT NULL);
         CREATE TABLE receipts(id TEXT PRIMARY KEY,client TEXT NOT NULL,source TEXT NOT NULL,time INTEGER NOT NULL,provider TEXT,response TEXT,signature TEXT,ended INTEGER NOT NULL,effective INTEGER NOT NULL,operation TEXT NOT NULL DEFAULT 'model');
         CREATE INDEX receipt_response ON receipts(client,response);
         CREATE INDEX receipt_signature ON receipts(client,signature,ended);
         INSERT INTO receipts SELECT usage_text(id),client,source,time,usage_text(provider),usage_text(response),usage_text(signature),ended,effective,operation FROM receipts_now;
         INSERT INTO cursors SELECT id,usage_expand(body) FROM cursors_now;
         DROP TABLE receipts_now; DROP TABLE cursors_now;
         PRAGMA user_version=13;
         COMMIT;",
    )
    .unwrap();
}

#[test]
fn receipts_and_cursors_of_version_13_are_taken_over_and_still_match() {
    let (dir, mut store, _pricing) = fixture();
    let gateway = |id: &str, at: i64, tokens: Tokens| {
        record(
            id,
            "codex",
            "proxy",
            at,
            Some("00000000-0000-4000-8000-0000000000a1"),
            Some("fixture-model"),
            tokens,
            Some(200),
        )
    };
    let session = |id: &str, at: i64, tokens: Tokens| {
        record(
            id,
            "codex",
            "codex",
            at,
            None,
            Some("fixture-model"),
            tokens,
            None,
        )
    };
    // Three archived gateway requests: one a later session names by its
    // response, one a session can only resemble, one that arrives again.
    let mut named = gateway("fixture-v13-named", BASE, usage(100, 20));
    named.attempts[0].response_id = Some(safe_id("fixture-v13-response"));
    let alike = gateway("fixture-v13-alike", BASE + DAY, usage(300, 40));
    let again = gateway(
        "00000000-0000-4000-8000-000000000013",
        BASE + 2 * DAY,
        usage(7, 9),
    );
    let codex_cursor = r#"{"source":"codex","offset":13}"#;
    let claude_cursor = r#"{"source":"claude","offset":14}"#;
    store
        .write_batch(
            &[named.clone(), alike.clone(), again.clone()],
            Some(("fixture-cursor-codex", codex_cursor)),
        )
        .unwrap();
    store
        .write_batch(&[], Some(("fixture-cursor-claude", claude_cursor)))
        .unwrap();
    store
        .archive_batch(&[named.id.clone(), alike.id.clone(), again.id.clone()])
        .unwrap();
    let receipts = |store: &Store| -> Vec<String> {
        let mut stmt = store
            .connection
            .prepare("SELECT concat_ws('|',usage_text(id),client,source,time,usage_text(provider),usage_text(response),usage_text(signature),ended,effective,operation) FROM receipts ORDER BY 1")
            .unwrap();
        let rows = stmt.query_map([], |row| row.get(0)).unwrap();
        rows.map(Result::unwrap).collect()
    };
    let tables = |store: &Store| -> Vec<(String, Option<String>)> {
        let mut stmt = store
            .connection
            .prepare("SELECT name,sql FROM sqlite_master WHERE tbl_name IN ('receipts','cursors') ORDER BY name")
            .unwrap();
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        rows.map(Result::unwrap).collect()
    };
    let kinds = |store: &Store| -> (String, String) {
        store
            .connection
            .query_row(
                "SELECT (SELECT group_concat(DISTINCT typeof(id)||typeof(provider)||typeof(response)||typeof(signature)) FROM receipts WHERE response IS NOT NULL),(SELECT group_concat(DISTINCT typeof(body)) FROM cursors)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    };
    let (kept, made) = (receipts(&store), tables(&store));
    assert_eq!(kept.len(), 3);
    assert!(kept[2].starts_with("fixture-v13-named|codex|proxy|"));
    assert!(kept[2].contains("|00000000-0000-4000-8000-0000000000a1|"));
    assert!(kept[2].contains(&safe_id("fixture-v13-response")));
    assert_eq!(kinds(&store), ("blobblobblobblob".into(), "blob".into()));
    let requests = store.dashboard(&all_time()).unwrap().totals.requests;
    assert_eq!(requests, 3);
    drop(store);

    into_version_13(dir.path());
    let old = rusqlite::Connection::open(dir.path().join("usage.sqlite")).unwrap();
    let texts: (String, String) = old
        .query_row(
            "SELECT (SELECT group_concat(DISTINCT typeof(id)||typeof(provider)||typeof(response)||typeof(signature)) FROM receipts WHERE response IS NOT NULL),(SELECT group_concat(DISTINCT typeof(body)) FROM cursors)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(texts, ("texttexttexttext".into(), "text".into()));
    assert_eq!(entry_columns(&old), 0);
    drop(old);

    let mut store = Store::open(dir.path()).unwrap();
    let version: i64 = store
        .connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 14);
    // Version 13 goes all the way: its records and receipts have the column
    // for a key too.
    assert_eq!(entry_columns(&store.connection), 2);
    assert_eq!(receipts(&store), kept);
    assert_eq!(tables(&store), made);
    assert_eq!(kinds(&store), ("blobblobblobblob".into(), "blob".into()));
    assert_eq!(
        store.cursor("fixture-cursor-codex").unwrap().as_deref(),
        Some(codex_cursor)
    );
    assert_eq!(store.dashboard(&all_time()).unwrap().totals.requests, 3);

    // An archived request that arrives again is not stored or counted again.
    store
        .write_batch(std::slice::from_ref(&again), None)
        .unwrap();
    assert!(store.detail(&again.id).is_err());
    // A session that names an archived response is that request; one that
    // only resembles an archived request waits for review. Neither is billed.
    let mut naming = session("fixture-v13-session-naming", BASE + 1_000, usage(100, 20));
    naming.attempts[0].response_id = Some(safe_id("fixture-v13-response"));
    let resembling = session(
        "fixture-v13-session-resembling",
        BASE + DAY + 1_000,
        usage(300, 40),
    );
    store.write_batch(&[naming, resembling], None).unwrap();
    let naming = store.detail("fixture-v13-session-naming").unwrap();
    assert_eq!(
        (
            naming.deduplication.as_str(),
            naming.duplicate_of.as_deref()
        ),
        ("archived_match", Some("fixture-v13-named"))
    );
    assert_eq!(
        store
            .detail("fixture-v13-session-resembling")
            .unwrap()
            .deduplication,
        "ambiguous"
    );
    assert_eq!(store.dashboard(&all_time()).unwrap().totals.requests, 3);
    // The session view counts the one that stands for a gateway request.
    let sessions = Filter {
        source: Some("sessions".into()),
        ..all_time()
    };
    assert_eq!(store.dashboard(&sessions).unwrap().totals.requests, 1);

    // Rebuilding a source replaces its cursors and leaves the others.
    let rebuilt = r#"{"source":"codex","offset":15}"#;
    store
        .rebuild(
            "codex",
            &[],
            &[("fixture-cursor-rebuilt".into(), rebuilt.into())],
        )
        .unwrap();
    assert_eq!(store.cursor("fixture-cursor-codex").unwrap(), None);
    assert_eq!(
        store.cursor("fixture-cursor-rebuilt").unwrap().as_deref(),
        Some(rebuilt)
    );
    assert_eq!(
        store.cursor("fixture-cursor-claude").unwrap().as_deref(),
        Some(claude_cursor)
    );
    assert_eq!(receipts(&store), kept);
}

#[test]
fn ambiguous_cross_source_candidate_is_reviewable_but_not_effective() {
    let (_dir, mut store, _pricing) = fixture();
    let first = record(
        "fixture-proxy-ambiguous-a",
        "codex",
        "proxy",
        BASE + 100,
        Some("fixture-p1"),
        Some("fixture-model"),
        usage(100, 20),
        Some(200),
    );
    let second = record(
        "fixture-proxy-ambiguous-b",
        "codex",
        "proxy",
        BASE + 200,
        Some("fixture-p1"),
        Some("fixture-model"),
        usage(100, 20),
        Some(200),
    );
    let session = record(
        "fixture-session-ambiguous",
        "codex",
        "codex",
        BASE + 300,
        None,
        Some("fixture-model"),
        usage(100, 20),
        None,
    );
    store.write_batch(&[first, second, session], None).unwrap();

    let ambiguous = store.detail("fixture-session-ambiguous").unwrap();
    assert_eq!(ambiguous.deduplication, "ambiguous");
    let dashboard = store.dashboard(&all_time()).unwrap();
    assert_eq!(dashboard.totals.requests, 2);
    assert_eq!(dashboard.totals.tokens.input, Some(200));
    assert_eq!(dashboard.totals.tokens.output, Some(40));
}

#[test]
fn codex_total_counter_reset_starts_a_new_usage_delta() {
    let (dir, mut store, pricing) = fixture();
    let home = dir.path().join("home");
    let report = sync_fixture(
        &mut store,
        &pricing,
        "codex",
        &home,
        "sessions/reset.jsonl",
        FIXTURE_CODEX_RESET,
    );
    assert_eq!((report.imported, report.errors), (2, 0));

    let totals = store.dashboard(&all_time()).unwrap().totals;
    assert_eq!(totals.requests, 2);
    assert_eq!(totals.tokens.input, Some(450));
    assert_eq!(totals.tokens.cache_read, Some(650));
    assert_eq!(totals.tokens.output, Some(50));
}

#[test]
fn codex_multi_channel_snapshots_ignore_replays_and_keep_new_usage() {
    let (dir, mut store, pricing) = fixture();
    let report = sync_fixture(
        &mut store,
        &pricing,
        "codex",
        &dir.path().join("home"),
        "sessions/channels.jsonl",
        FIXTURE_CODEX_LANES,
    );
    assert_eq!((report.imported, report.errors), (3, 0));
    let totals = store.dashboard(&all_time()).unwrap().totals;
    assert_eq!(totals.requests, 3);
    assert_eq!(totals.tokens.input, Some(180));
    assert_eq!(totals.tokens.cache_read, Some(120));
    assert_eq!(totals.tokens.output, Some(30));
}

#[test]
fn codex_fork_skips_parent_prefix_and_counts_only_new_consumption() {
    let (dir, mut store, pricing) = fixture();
    let home = dir.path().join("home");
    write_fixture(
        &home,
        "sessions/2023/11/14/rollout-2023-11-14T22-13-20-00000000-0000-4000-8000-000000000001.jsonl",
        FIXTURE_CODEX_PARENT,
    );
    write_fixture(
        &home,
        "sessions/2023/11/14/rollout-2023-11-14T22-13-20-00000000-0000-4000-8000-000000000002.jsonl",
        FIXTURE_CODEX_FORK,
    );
    let report = sessions::sync(
        &mut store,
        &pricing,
        &Settings::default(),
        "codex",
        &home,
        false,
    )
    .unwrap();
    assert_eq!((report.imported, report.errors), (3, 0));

    let totals = store.dashboard(&all_time()).unwrap().totals;
    assert_eq!(totals.requests, 3);
    assert_eq!(totals.tokens.input, Some(140));
    assert_eq!(totals.tokens.cache_read, Some(110));
    assert_eq!(totals.tokens.output, Some(30));
}

#[test]
fn codex_fork_filtered_parent_events_use_subsequence_prefix_alignment() {
    let (dir, mut store, pricing) = fixture();
    let home = dir.path().join("home");
    let parent = [
        serde_json::json!({
            "type":"session_meta", "timestamp":BASE,
            "payload":{"id":"fixture-parent-subsequence"}
        })
        .to_string(),
        codex_snapshot(BASE + 100, [100, 50, 10], [100, 50, 10]),
        codex_snapshot(BASE + 200, [200, 100, 20], [100, 50, 10]),
        codex_snapshot(BASE + 300, [300, 150, 30], [100, 50, 10]),
    ]
    .join("\n")
        + "\n";
    let child = [
        serde_json::json!({
            "type":"session_meta", "timestamp":BASE + 350,
            "payload":{
                "id":"fixture-child-subsequence",
                "forked_from_id":"fixture-parent-subsequence"
            }
        })
        .to_string(),
        // Replayed child snapshots can be timestamped after the fork cutoff.
        codex_snapshot(BASE + 500, [100, 50, 10], [100, 50, 10]),
        codex_snapshot(BASE + 600, [300, 150, 30], [100, 50, 10]),
        codex_snapshot(BASE + 700, [450, 220, 45], [150, 70, 15]),
    ]
    .join("\n")
        + "\n";
    write_fixture(&home, "sessions/fixture-parent.jsonl", &parent);
    write_fixture(&home, "sessions/fixture-child.jsonl", &child);

    let report = sessions::sync(
        &mut store,
        &pricing,
        &Settings::default(),
        "codex",
        &home,
        false,
    )
    .unwrap();
    assert_eq!((report.imported, report.errors), (4, 0));

    let totals = store.dashboard(&all_time()).unwrap().totals;
    assert_eq!(totals.requests, 4);
    // The new 150 input includes 70 cached tokens; only 80 are fresh input.
    assert_eq!(totals.tokens.input, Some(230));
    assert_eq!(totals.tokens.cache_read, Some(220));
    assert_eq!(totals.tokens.output, Some(45));
    let child_row = store
        .logs(&Filter::default())
        .unwrap()
        .rows
        .into_iter()
        .find(|row| row.session_id.as_deref() == Some(&safe_id("fixture-child-subsequence")))
        .unwrap();
    assert_eq!(child_row.tokens().input, Some(80));
    assert_eq!(child_row.tokens().cache_read, Some(70));
    assert_eq!(child_row.tokens().output, Some(15));
}

#[test]
fn complete_json_without_newline_is_imported_once_for_codex_and_claude() {
    for client in ["codex", "claude"] {
        let (dir, mut store, pricing) = fixture();
        let home = dir.path().join("home");
        let (relative, contents) = if client == "codex" {
            (
                "sessions/fixture-no-final-newline.jsonl",
                format!(
                    "{}\n{}",
                    serde_json::json!({
                        "type":"session_meta", "timestamp":BASE,
                        "payload":{"id":"fixture-no-final-newline-codex"}
                    }),
                    codex_snapshot(BASE + 1, [40, 20, 5], [40, 20, 5])
                ),
            )
        } else {
            (
                "projects/fixture/no-final-newline.jsonl",
                serde_json::json!({
                    "type":"assistant", "uuid":"fixture-assistant-event",
                    "sessionId":"fixture-no-final-newline-claude", "timestamp":BASE,
                    "message":{
                        "id":"fixture-no-final-newline-message", "type":"message",
                        "model":"claude-fixture", "stop_reason":"end_turn",
                        "usage":{"input_tokens":8,"output_tokens":3,"cache_read_input_tokens":2}
                    }
                })
                .to_string(),
            )
        };
        let path = write_fixture(&home, relative, &contents);

        let first = sessions::sync(
            &mut store,
            &pricing,
            &Settings::default(),
            client,
            &home,
            false,
        )
        .unwrap();
        assert_eq!((first.imported, first.errors), (1, 0), "client={client}");
        assert_eq!(store.logs(&Filter::default()).unwrap().total, 1);

        let mut completed = fs::read(&path).unwrap();
        completed.push(b'\n');
        fs::write(&path, completed).unwrap();
        let appended = sessions::sync(
            &mut store,
            &pricing,
            &Settings::default(),
            client,
            &home,
            false,
        )
        .unwrap();
        assert_eq!(appended.errors, 0, "client={client}");

        let page = store.logs(&Filter::default()).unwrap();
        assert_eq!(page.total, 1, "client={client}");
        let totals = store.dashboard(&all_time()).unwrap().totals;
        assert_eq!(totals.requests, 1, "client={client}");
        if client == "codex" {
            assert_eq!(totals.tokens.input, Some(20));
            assert_eq!(totals.tokens.cache_read, Some(20));
            assert_eq!(totals.tokens.output, Some(5));
        } else {
            assert_eq!(totals.tokens.input, Some(8));
            assert_eq!(totals.tokens.cache_read, Some(2));
            assert_eq!(totals.tokens.output, Some(3));
        }
    }
}

#[test]
fn reader_keeps_a_committed_snapshot_while_writer_transaction_is_open() {
    let (dir, mut store, _pricing) = fixture();
    let initial = record(
        "fixture-reader-initial",
        "codex",
        "proxy",
        BASE,
        Some("fixture-provider"),
        Some("fixture-model"),
        usage(10, 1),
        Some(200),
    );
    store.write_batch(&[initial], None).unwrap();
    drop(store);

    let mut writer = rusqlite::Connection::open(dir.path().join("usage.sqlite")).unwrap();
    super::query::register(&writer).unwrap();
    let reader = Store::reader(dir.path()).unwrap();
    reader
        .snapshot(|snapshot| {
            assert_eq!(snapshot.logs(&Filter::default()).unwrap().total, 1);
            let second = record(
                "fixture-reader-committed-later",
                "codex",
                "proxy",
                BASE + 1,
                Some("fixture-provider"),
                Some("fixture-model"),
                usage(20, 2),
                Some(200),
            );
            let tx = writer.transaction().unwrap();
            super::storage_v12::save(&tx, &second, 1, 2).unwrap();
            super::query::changed(&tx).unwrap();
            assert_eq!(snapshot.logs(&Filter::default()).unwrap().total, 1);
            tx.commit().unwrap();
            assert_eq!(snapshot.logs(&Filter::default()).unwrap().total, 1);
            Ok(())
        })
        .unwrap();
    assert_eq!(reader.logs(&Filter::default()).unwrap().total, 2);
}

#[test]
fn v2_migration_and_codex_rebuild_preserve_gateway_price_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let mut gateway = record(
        "fixture-v2-gateway-record",
        "codex",
        "proxy",
        BASE,
        Some("fixture-gateway-provider"),
        Some("fixture-model"),
        usage(25, 4),
        Some(200),
    );
    gateway.attempts[0].price = Some(price_snapshot("0.1234"));
    let mut session = record(
        "fixture-v2-codex-session",
        "codex",
        "codex",
        BASE + 1,
        None,
        Some("fixture-model"),
        usage(25, 4),
        None,
    );
    session.attempts[0].price = Some(price_snapshot("0.5678"));
    store
        .write_batch(&[gateway.clone(), session.clone()], None)
        .unwrap();
    drop(store);

    let mut migrated = Store::open(dir.path()).unwrap();
    while migrated.compact_storage_batch().unwrap() {}
    migrated.reclaim_space().unwrap();
    let version: i64 = rusqlite::Connection::open(dir.path().join("usage.sqlite"))
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 14);
    assert_eq!(
        migrated.detail(&gateway.id).unwrap().attempts[0]
            .price
            .as_ref()
            .unwrap()
            .cost,
        "0.1234"
    );
    assert_eq!(
        migrated.detail(&session.id).unwrap().attempts[0]
            .price
            .as_ref()
            .unwrap()
            .cost,
        "0.5678"
    );
    let gateway_after_migration = migrated.detail(&gateway.id).unwrap();
    let session_after_migration = migrated.detail(&session.id).unwrap();
    assert_eq!(
        gateway_after_migration.merged_sources,
        vec!["codex", "proxy"]
    );
    assert_eq!(gateway_after_migration.deduplication, "strict_match");
    assert_eq!(
        session_after_migration.duplicate_of.as_deref(),
        Some(gateway.id.as_str())
    );
    assert_eq!(session_after_migration.deduplication, "strict_match");
    assert_eq!(migrated.logs(&Filter::default()).unwrap().total, 1);

    migrated
        .rebuild("codex", std::slice::from_ref(&session), &[])
        .unwrap();
    let rebuilt_session = migrated.detail(&session.id).unwrap();
    let preserved_gateway = migrated.detail(&gateway.id).unwrap();
    assert_eq!(
        rebuilt_session.attempts[0].price.as_ref().unwrap().cost,
        "0.5678"
    );
    assert_eq!(
        preserved_gateway.attempts[0].price.as_ref().unwrap().cost,
        "0.1234"
    );
    assert_eq!(preserved_gateway.merged_sources, vec!["codex", "proxy"]);
    assert_eq!(preserved_gateway.deduplication, "strict_match");
    assert_eq!(
        rebuilt_session.duplicate_of.as_deref(),
        Some(gateway.id.as_str())
    );
    assert_eq!(rebuilt_session.deduplication, "strict_match");
    assert_eq!(migrated.logs(&Filter::default()).unwrap().total, 1);
}

#[test]
fn claude_same_output_completion_keeps_updated_cache_usage() {
    let (dir, mut store, pricing) = fixture();
    let report = sync_fixture(
        &mut store,
        &pricing,
        "claude",
        &dir.path().join("home"),
        "projects/fixture/main.jsonl",
        FIXTURE_CLAUDE_CACHE,
    );
    assert_eq!((report.imported, report.errors), (1, 0));

    let page = store.logs(&Filter::default()).unwrap();
    assert_eq!(page.total, 1);
    let row = &page.rows[0];
    assert!(row.completed);
    assert_eq!(row.final_attempt().unwrap().tokens.output, Some(8));
    assert_eq!(row.final_attempt().unwrap().tokens.cache_read, Some(60));
    assert_eq!(row.final_attempt().unwrap().tokens.cache_write, Some(25));
}

#[test]
fn claude_same_output_completion_updates_cache_during_incremental_sync() {
    let (dir, mut store, pricing) = fixture();
    let mut lines = FIXTURE_CLAUDE_CACHE.lines();
    let user_line = lines.next().unwrap();
    let partial_line = lines.next().unwrap();
    let completed_line = lines.next().unwrap();
    assert!(lines.next().is_none());

    let home = dir.path().join("home");
    let path = write_fixture(
        &home,
        "projects/fixture/incremental.jsonl",
        &format!("{user_line}\n{partial_line}\n"),
    );
    let first = sessions::sync(
        &mut store,
        &pricing,
        &Settings::default(),
        "claude",
        &home,
        false,
    )
    .unwrap();
    assert_eq!((first.imported, first.errors), (1, 0));

    let partial = store.logs(&Filter::default()).unwrap();
    assert_eq!(partial.total, 1);
    assert!(!partial.rows[0].completed);
    let attempt = partial.rows[0].final_attempt().unwrap();
    assert_eq!(attempt.tokens.output, Some(8));
    assert_eq!(attempt.tokens.cache_read, Some(20));
    assert_eq!(attempt.tokens.cache_write, Some(5));

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    std::io::Write::write_all(&mut file, format!("{completed_line}\n").as_bytes()).unwrap();
    let second = sessions::sync(
        &mut store,
        &pricing,
        &Settings::default(),
        "claude",
        &home,
        false,
    )
    .unwrap();
    assert_eq!((second.imported, second.errors), (1, 0));

    let completed = store.logs(&Filter::default()).unwrap();
    assert_eq!(completed.total, 1);
    assert!(completed.rows[0].completed);
    let attempt = completed.rows[0].final_attempt().unwrap();
    assert_eq!(attempt.tokens.output, Some(8));
    assert_eq!(attempt.tokens.cache_read, Some(60));
    assert_eq!(attempt.tokens.cache_write, Some(25));
    assert_eq!(store.dashboard(&all_time()).unwrap().totals.requests, 1);
}

#[test]
fn claude_subagent_interruption_still_counts_reported_consumption() {
    let (dir, mut store, pricing) = fixture();
    let report = sync_fixture(
        &mut store,
        &pricing,
        "claude",
        &dir.path().join("home"),
        "projects/fixture/main/subagents/agent-fixture.jsonl",
        FIXTURE_CLAUDE_SUBAGENT,
    );
    assert_eq!((report.imported, report.errors), (1, 0));

    let page = store.logs(&Filter::default()).unwrap();
    assert_eq!(page.total, 1);
    let row = &page.rows[0];
    assert!(!row.completed);
    assert_eq!(row.final_attempt().unwrap().status, None);
    assert_eq!(row.final_attempt().unwrap().tokens.input, Some(100));
    assert_eq!(row.final_attempt().unwrap().tokens.cache_read, Some(40));
    assert_eq!(row.final_attempt().unwrap().tokens.cache_write, Some(20));
    assert_eq!(row.final_attempt().unwrap().tokens.output, Some(4));
    assert_eq!(store.dashboard(&all_time()).unwrap().totals.requests, 1);
}

#[test]
fn dashboard_cache_refreshes_after_writes_and_keeps_filters_isolated() {
    let (_dir, mut store, _pricing) = fixture();
    let initial = [
        record(
            "fixture-cache-codex-a",
            "codex",
            "proxy",
            BASE,
            Some("fixture-provider"),
            Some("fixture-model-a"),
            usage(10, 1),
            Some(200),
        ),
        record(
            "fixture-cache-claude-b",
            "claude",
            "proxy",
            BASE + 1,
            Some("fixture-provider"),
            Some("fixture-model-b"),
            usage(20, 2),
            Some(200),
        ),
    ];
    store.write_batch(&initial, None).unwrap();

    let codex_model_a = Filter {
        start: Some(BASE - DAY),
        end: Some(BASE + 100 * DAY),
        client: Some("codex".into()),
        model: Some("fixture-model-a".into()),
        ..Filter::default()
    };
    let cached_before_write = store.dashboard(&codex_model_a).unwrap();
    assert_eq!(cached_before_write.totals.requests, 1);

    let added = record(
        "fixture-cache-codex-a-added",
        "codex",
        "proxy",
        BASE + 2,
        Some("fixture-provider"),
        Some("fixture-model-a"),
        usage(30, 3),
        Some(200),
    );
    store.write_batch(&[added], None).unwrap();

    let refreshed = store.dashboard(&codex_model_a).unwrap();
    assert_eq!(refreshed.totals.requests, 2);
    assert_eq!(store.logs(&codex_model_a).unwrap().total, 2);

    let claude_model_a = Filter {
        client: Some("claude".into()),
        ..codex_model_a.clone()
    };
    let codex_model_b = Filter {
        model: Some("fixture-model-b".into()),
        ..codex_model_a.clone()
    };
    let claude_model_b = Filter {
        client: Some("claude".into()),
        model: Some("fixture-model-b".into()),
        ..codex_model_a
    };
    assert_eq!(store.dashboard(&claude_model_a).unwrap().totals.requests, 0);
    assert_eq!(store.dashboard(&codex_model_b).unwrap().totals.requests, 0);
    assert_eq!(store.dashboard(&claude_model_b).unwrap().totals.requests, 1);
}

#[test]
fn dashboard_cache_key_keeps_the_two_day_granularity_boundary() {
    let (_dir, mut store, _pricing) = fixture();
    let row = record(
        "fixture-cache-granularity",
        "codex",
        "proxy",
        BASE + 1,
        Some("fixture-provider"),
        Some("fixture-model"),
        usage(10, 1),
        Some(200),
    );
    store.write_batch(&[row], None).unwrap();

    let hourly = store
        .dashboard(&Filter {
            start: Some(BASE),
            end: Some(BASE + 2 * DAY),
            ..Filter::default()
        })
        .unwrap();
    let daily = store
        .dashboard(&Filter {
            start: Some(BASE),
            end: Some(BASE + 2 * DAY + 1),
            ..Filter::default()
        })
        .unwrap();

    assert_eq!(hourly.totals.requests, 1);
    assert_eq!(daily.totals.requests, 1);
}

const KEY_A: &str = "00000000-0000-4000-8000-00000000e00a";
const KEY_B: &str = "00000000-0000-4000-8000-00000000e00b";

/// A gateway request that came in with the handed-out key `key`, or with the
/// client's own. It is priced in full, so archiving puts it in a rollup it
/// shares with the requests like it.
fn keyed(id: &str, at: i64, key: Option<&str>) -> Record {
    let mut request = record(
        id,
        "codex",
        "proxy",
        at,
        Some("fixture-provider"),
        Some("fixture-model"),
        usage(10, 1),
        Some(200),
    );
    request.attempts[0].price = Some(price_snapshot("0.0001"));
    request.entry_key = key.map(str::to_owned);
    request
}

/// The same request after a first attempt that failed and has no price.
fn retried(mut request: Record, final_price: Option<PriceSnapshot>) -> Record {
    let mut first = request.attempts[0].clone();
    first.id = format!("{}-first", request.id);
    first.status = Some(503);
    first.outcome = "failed".into();
    first.price = None;
    request.attempts[0].started_at += 50;
    request.attempts[0].price = final_price;
    request.attempts.insert(0, first);
    request
}

fn by_key(key: &str) -> Filter {
    Filter {
        entry_key: Some(key.into()),
        ..all_time()
    }
}

/// Totals as they compare: a sum made here may differ from one SQL made only
/// in the trailing zeros of its cost.
fn comparable(totals: &Totals) -> serde_json::Value {
    let mut value = serde_json::to_value(totals).unwrap();
    value["cost"] = decimal(&totals.cost)
        .unwrap_or_default()
        .normalize()
        .to_string()
        .into();
    value
}

/// Writes requests of two handed-out keys and of the client's own on two
/// days, and answers the ids of those of the first day. Those of the second
/// key have a provider and a model of their own.
fn write_keyed_requests(store: &mut Store) -> Vec<String> {
    let later = BASE + 2 * DAY;
    let second = |id: &str, at: i64, price: Option<PriceSnapshot>| {
        let mut request = keyed(id, at, Some(KEY_B));
        let attempt = &mut request.attempts[0];
        attempt.provider = Some("fixture-provider-b".into());
        attempt.requested_model = Some("fixture-model-b".into());
        attempt.response_model = Some("fixture-model-b".into());
        attempt.pricing_model = Some("fixture-model-b".into());
        attempt.price = price;
        request
    };
    let mut unpriced = keyed("fixture-keys-own-old-unpriced", BASE + 5, None);
    unpriced.attempts[0].price = None;
    let first_day = vec![
        keyed("fixture-keys-a-old-1", BASE, Some(KEY_A)),
        keyed("fixture-keys-a-old-2", BASE + 1, Some(KEY_A)),
        // Neither attempt has a price: one unpriced request, not two.
        retried(
            keyed("fixture-keys-a-old-retried", BASE + 2, Some(KEY_A)),
            None,
        ),
        second(
            "fixture-keys-b-old",
            BASE + 3,
            Some(price_snapshot("0.0002")),
        ),
        keyed("fixture-keys-own-old", BASE + 4, None),
        unpriced,
    ];
    let second_day = [
        keyed("fixture-keys-a-new", later, Some(KEY_A)),
        // Only the first attempt lacks a price, and the request with it.
        retried(
            keyed("fixture-keys-a-new-retried", later + 1, Some(KEY_A)),
            Some(price_snapshot("0.0001")),
        ),
        second("fixture-keys-b-new-unpriced", later + 2, None),
        keyed("fixture-keys-own-new", later + 3, None),
        // What a session reports came in with no key.
        record(
            "fixture-keys-session",
            "codex",
            "codex",
            later + 4,
            None,
            Some("fixture-model"),
            usage(7, 3),
            None,
        ),
    ];
    store.write_batch(&first_day, None).unwrap();
    store.write_batch(&second_day, None).unwrap();
    first_day.into_iter().map(|request| request.id).collect()
}

/// Every overview a key can narrow and what the calculator counts for it, as
/// they are answered.
fn overviews_by_key(store: &Store) -> Vec<serde_json::Value> {
    [None, Some(KEY_A), Some(KEY_B), Some(OWN_ENTRY_KEY)]
        .into_iter()
        .map(|key| {
            let filter = Filter {
                entry_key: key.map(str::to_owned),
                ..all_time()
            };
            let gateway = Filter {
                source: Some("proxy".into()),
                ..filter.clone()
            };
            serde_json::json!({
                "all": store.dashboard(&filter).unwrap(),
                "gateway": store.dashboard(&gateway).unwrap(),
                "calculator": super::query::calculator_totals(&store.connection, &filter).unwrap(),
            })
        })
        .collect()
}

#[test]
fn a_record_keeps_the_key_it_came_in_with_and_one_of_the_own_key_reads_as_before() {
    let (_dir, mut store, _pricing) = fixture();
    store
        .write_batch(
            &[
                keyed("fixture-key-kept", BASE, Some(KEY_A)),
                keyed("fixture-key-none", BASE + 1, None),
            ],
            None,
        )
        .unwrap();

    let kept = store.detail("fixture-key-kept").unwrap();
    assert_eq!(kept.entry_key.as_deref(), Some(KEY_A));
    let own = store.detail("fixture-key-none").unwrap();
    assert_eq!(own.entry_key, None);
    assert!(serde_json::to_value(&own)
        .unwrap()
        .get("entryKey")
        .is_none());
    // The key is a column of its own, as compact as an identifier; the two
    // records share everything else they keep.
    let stored: Vec<(Option<String>, Option<i64>, i64)> = store
        .connection
        .prepare("SELECT usage_text(entry),length(entry),shape FROM v12_rows ORDER BY rid")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(stored.len(), 2);
    assert_eq!(
        (stored[0].0.as_deref(), stored[0].1),
        (Some(KEY_A), Some(17))
    );
    assert_eq!((stored[1].0.as_deref(), stored[1].1), (None, None));
    assert_eq!(stored[0].2, stored[1].2);
}

#[test]
fn the_request_list_the_overview_and_the_calculator_narrow_to_one_key_or_the_own_key() {
    let (_dir, mut store, _pricing) = fixture();
    store
        .write_batch(
            &[
                keyed("fixture-list-a-1", BASE, Some(KEY_A)),
                keyed("fixture-list-a-2", BASE + 1, Some(KEY_A)),
                keyed("fixture-list-b", BASE + 2, Some(KEY_B)),
                keyed("fixture-list-own", BASE + 3, None),
                record(
                    "fixture-list-session",
                    "codex",
                    "codex",
                    BASE + 4,
                    None,
                    Some("fixture-model"),
                    usage(7, 3),
                    None,
                ),
            ],
            None,
        )
        .unwrap();

    let listed = |filter: &Filter| -> Vec<String> {
        let page = store.logs(filter).unwrap();
        assert_eq!(page.total as usize, page.rows.len());
        page.rows.into_iter().map(|row| row.id).collect()
    };
    assert_eq!(
        listed(&by_key(KEY_A)),
        ["fixture-list-a-2", "fixture-list-a-1"]
    );
    assert_eq!(listed(&by_key(KEY_B)), ["fixture-list-b"]);
    // Everything that came in with no handed-out key, a session's too.
    assert_eq!(
        listed(&by_key(OWN_ENTRY_KEY)),
        ["fixture-list-session", "fixture-list-own"]
    );
    assert_eq!(listed(&all_time()).len(), 5);
    // An empty choice is no choice, as with the other filters.
    assert_eq!(listed(&by_key("")).len(), 5);

    for (key, requests, forwarded) in [(KEY_A, 2, 2), (KEY_B, 1, 1), (OWN_ENTRY_KEY, 2, 1)] {
        let filter = by_key(key);
        assert_eq!(
            store.dashboard(&filter).unwrap().totals.requests,
            requests,
            "key={key}"
        );
        // The calculator counts what the gateway forwarded.
        assert_eq!(
            super::query::calculator_totals(&store.connection, &filter)
                .unwrap()
                .requests,
            forwarded,
            "key={key}"
        );
    }
}

#[test]
fn archived_requests_stay_with_their_key_and_each_key_is_one_group_that_adds_up_to_the_totals() {
    let (_dir, mut store, _pricing) = fixture();
    let first_day = write_keyed_requests(&mut store);
    let live = overviews_by_key(&store);

    // The first day goes into rollups: shared ones where a request is priced
    // in full, one of its exact usage where it is not.
    store.archive_batch(&first_day).unwrap();
    assert_eq!(store.logs(&all_time()).unwrap().total, 5);
    assert_eq!(overviews_by_key(&store), live);

    let overview = store.dashboard(&all_time()).unwrap();
    let groups: Vec<_> = overview
        .entry_keys
        .iter()
        .map(|group| {
            (
                group.id.as_str(),
                group.totals.requests,
                group.totals.attempts,
                group.totals.unpriced,
            )
        })
        .collect();
    // One group for a key whose requests are partly records and partly
    // rollups; a request counts as unpriced once, as in the totals.
    assert_eq!(
        groups,
        [(KEY_A, 5, 7, 2), (OWN_ENTRY_KEY, 4, 4, 2), (KEY_B, 2, 2, 1)]
    );
    assert_eq!(
        (overview.totals.requests, overview.totals.unpriced),
        (11, 5)
    );

    let narrowed = [
        all_time(),
        Filter {
            source: Some("proxy".into()),
            ..all_time()
        },
        Filter {
            source: Some("sessions".into()),
            ..all_time()
        },
        Filter {
            provider: Some("fixture-provider".into()),
            ..all_time()
        },
        Filter {
            model: Some("fixture-model-b".into()),
            ..all_time()
        },
        Filter {
            start: Some(BASE + DAY),
            ..all_time()
        },
    ];
    for (index, filter) in narrowed.iter().enumerate() {
        let overview = store.dashboard(filter).unwrap();
        let mut sum = Totals::default();
        for group in &overview.entry_keys {
            sum.add(&group.totals);
        }
        assert_eq!(
            comparable(&sum),
            comparable(&overview.totals),
            "filter {index}"
        );
    }
}

#[test]
fn a_key_filter_narrows_the_totals_and_the_other_groups_but_still_lists_every_key() {
    let (_dir, mut store, _pricing) = fixture();
    let first_day = write_keyed_requests(&mut store);
    store.archive_batch(&first_day).unwrap();
    let everything = store.dashboard(&all_time()).unwrap();
    let ids = |groups: &[super::store::Group]| -> Vec<String> {
        groups.iter().map(|group| group.id.clone()).collect()
    };

    for (key, providers, models, sources) in [
        (
            KEY_A,
            vec!["fixture-provider"],
            vec!["fixture-model"],
            vec![("proxy", 5)],
        ),
        (
            KEY_B,
            vec!["fixture-provider-b"],
            vec!["fixture-model-b"],
            vec![("proxy", 2)],
        ),
        (
            OWN_ENTRY_KEY,
            vec!["fixture-provider", "session"],
            vec!["fixture-model"],
            vec![("codex", 1), ("proxy", 3)],
        ),
    ] {
        let narrowed = store.dashboard(&by_key(key)).unwrap();
        // The choice does not remove the other choices.
        assert_eq!(
            serde_json::to_value(&narrowed.entry_keys).unwrap(),
            serde_json::to_value(&everything.entry_keys).unwrap(),
            "key={key}"
        );
        // The totals are those of the chosen key's own group.
        let group = everything
            .entry_keys
            .iter()
            .find(|group| group.id == key)
            .unwrap_or_else(|| panic!("missing key group {key}"));
        assert_eq!(
            comparable(&narrowed.totals),
            comparable(&group.totals),
            "key={key}"
        );
        assert_eq!(ids(&narrowed.providers), providers, "key={key}");
        assert_eq!(ids(&narrowed.models), models, "key={key}");
        let counted: Vec<(&str, u64)> = narrowed
            .sources
            .iter()
            .map(|(source, requests)| (source.as_str(), *requests))
            .collect();
        assert_eq!(counted, sources, "key={key}");
    }
}

#[test]
fn the_saved_overview_of_one_key_is_not_answered_for_another() {
    let (_dir, mut store, _pricing) = fixture();
    let first_day = write_keyed_requests(&mut store);
    store.archive_batch(&first_day).unwrap();
    let filters = [
        (all_time(), 11),
        (by_key(KEY_A), 5),
        (by_key(KEY_B), 2),
        (by_key(OWN_ENTRY_KEY), 4),
    ];
    let saved: std::collections::BTreeSet<String> = filters
        .iter()
        .map(|(filter, _)| super::query::overview_key(&store.connection, filter).unwrap())
        .collect();
    assert_eq!(saved.len(), filters.len());
    // One overview is saved at a time: each is asked right after every other.
    for (filter, requests) in &filters {
        for (other, other_requests) in &filters {
            assert_eq!(
                store.dashboard(other).unwrap().totals.requests,
                *other_requests
            );
            assert_eq!(store.dashboard(filter).unwrap().totals.requests, *requests);
        }
    }
}

#[test]
fn archived_requests_of_different_keys_have_rollups_apart_and_the_own_key_keeps_the_one_it_had() {
    let (_dir, mut store, _pricing) = fixture();
    // The shared rollups of every source: id, key and requests.
    let rollups = |store: &Store| -> Vec<(String, Option<String>, i64)> {
        store
            .connection
            .prepare("SELECT d.id,json_extract(d.body,'$.entryKey'),(SELECT sum(requests) FROM usage_daily_metrics m WHERE m.kind=1 AND m.owner=d.id) FROM daily d WHERE json_extract(d.body,'$.scope')='all' ORDER BY 2,1")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let archive = |store: &mut Store, requests: &[Record]| {
        store.write_batch(requests, None).unwrap();
        let ids: Vec<String> = requests.iter().map(|request| request.id.clone()).collect();
        store.archive_batch(&ids).unwrap();
    };

    archive(&mut store, &[keyed("fixture-rollup-own-1", BASE, None)]);
    let own = rollups(&store);
    assert_eq!(own.len(), 1);
    assert_eq!((own[0].1.as_deref(), own[0].2), (None, 1));
    // A rollup of the own key names no key anywhere, as before there were any.
    let body: String = store
        .connection
        .query_row("SELECT body FROM daily WHERE id=?1", [&own[0].0], |row| {
            row.get(0)
        })
        .unwrap();
    let payload = super::bodies::unpack(&store.connection, &body).unwrap();
    assert!(!body.contains("entryKey"));
    assert!(payload.get("entryKey").is_none());
    assert!(payload["example"].get("entryKey").is_none());

    // The same request once more joins it: the same rollup, and no other.
    archive(&mut store, &[keyed("fixture-rollup-own-2", BASE + 1, None)]);
    assert_eq!(rollups(&store), [(own[0].0.clone(), None, 2)]);

    // With a handed-out key it starts a rollup of that key.
    archive(
        &mut store,
        &[
            keyed("fixture-rollup-a-1", BASE + 2, Some(KEY_A)),
            keyed("fixture-rollup-b", BASE + 3, Some(KEY_B)),
            keyed("fixture-rollup-a-2", BASE + 4, Some(KEY_A)),
        ],
    );
    let apart = rollups(&store);
    assert_eq!(apart.len(), 3);
    assert_eq!(apart[0], (own[0].0.clone(), None, 2));
    assert_eq!(
        (apart[1].1.as_deref(), apart[1].2),
        (Some(KEY_A), 2),
        "one rollup for the requests of one key"
    );
    assert_eq!((apart[2].1.as_deref(), apart[2].2), (Some(KEY_B), 1));
}

#[test]
fn gateway_requests_with_the_same_response_merge_within_one_key_and_not_across_keys() {
    let (_dir, mut store, _pricing) = fixture();
    let answered = |id: &str, at: i64, key: Option<&str>, response: &str| {
        let mut request = keyed(id, at, key);
        request.attempts[0].response_id = Some(safe_id(response));
        request
    };
    let one = "fixture-response-one-key";
    let own = "fixture-response-own-key";
    let two = "fixture-response-two-keys";
    store
        .write_batch(
            &[
                answered("fixture-same-a-1", BASE, Some(KEY_A), one),
                answered("fixture-same-a-2", BASE + 1, Some(KEY_A), one),
                answered("fixture-same-own-1", BASE + 2, None, own),
                answered("fixture-same-own-2", BASE + 3, None, own),
                answered("fixture-apart-a", BASE + 4, Some(KEY_A), two),
                answered("fixture-apart-b", BASE + 5, Some(KEY_B), two),
                answered("fixture-apart-own", BASE + 6, None, two),
            ],
            None,
        )
        .unwrap();

    let merged = |store: &Store, id: &str| {
        let row = store.detail(id).unwrap();
        (row.deduplication, row.duplicate_of)
    };
    // Within one key, the client's own included, the second is the first.
    assert_eq!(
        merged(&store, "fixture-same-a-2"),
        ("response_id".into(), Some("fixture-same-a-1".into()))
    );
    assert_eq!(
        merged(&store, "fixture-same-own-2"),
        ("response_id".into(), Some("fixture-same-own-1".into()))
    );
    // Across keys each is a request of its own.
    for id in ["fixture-apart-a", "fixture-apart-b", "fixture-apart-own"] {
        assert_eq!(merged(&store, id), (String::new(), None), "id={id}");
    }
    let requests =
        |store: &Store, key: &str| store.dashboard(&by_key(key)).unwrap().totals.requests;
    assert_eq!(
        (
            requests(&store, KEY_A),
            requests(&store, KEY_B),
            requests(&store, OWN_ENTRY_KEY)
        ),
        (2, 1, 2)
    );

    // A session still names the gateway request it is, whatever key that came
    // in with; saving the request again with what the session adds keeps it.
    let mut session = record(
        "fixture-same-session",
        "codex",
        "codex",
        BASE + 8,
        None,
        Some("fixture-model"),
        usage(10, 1),
        None,
    );
    session.attempts[0].response_id = Some(safe_id("fixture-response-session"));
    store
        .write_batch(
            &[
                answered(
                    "fixture-same-keyed",
                    BASE + 7,
                    Some(KEY_B),
                    "fixture-response-session",
                ),
                session,
            ],
            None,
        )
        .unwrap();
    assert_eq!(
        merged(&store, "fixture-same-session"),
        ("response_id".into(), Some("fixture-same-keyed".into()))
    );
    let gateway = store.detail("fixture-same-keyed").unwrap();
    assert_eq!(gateway.merged_sources, ["codex", "proxy"]);
    assert_eq!(gateway.entry_key.as_deref(), Some(KEY_B));
    assert_eq!(requests(&store, KEY_B), 2);
}

#[test]
fn an_archived_gateway_request_is_another_request_to_one_of_another_key_with_its_response() {
    let (_dir, mut store, _pricing) = fixture();
    let answered = |id: &str, at: i64, key: Option<&str>, response: &str| {
        let mut request = keyed(id, at, key);
        request.attempts[0].response_id = Some(safe_id(response));
        request
    };
    let shared = "fixture-response-archived-shared";
    let own = "fixture-response-archived-own";
    let named = "fixture-response-archived-named";
    let archived = [
        answered("fixture-archived-a", BASE, Some(KEY_A), shared),
        answered("fixture-archived-own", BASE + 1, None, own),
        answered("fixture-archived-b", BASE + 2, Some(KEY_B), named),
    ];
    store.write_batch(&archived, None).unwrap();
    store
        .archive_batch(&archived.map(|request| request.id))
        .unwrap();
    // What is left of each names the key it came in with, as its record did.
    let receipts: Vec<(String, Option<String>)> = store
        .connection
        .prepare("SELECT usage_text(id),usage_text(entry) FROM receipts ORDER BY 1")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        receipts,
        [
            ("fixture-archived-a".into(), Some(KEY_A.into())),
            ("fixture-archived-b".into(), Some(KEY_B.into())),
            ("fixture-archived-own".into(), None),
        ]
    );

    let mut session = record(
        "fixture-later-session",
        "codex",
        "codex",
        BASE + DAY + 4,
        None,
        Some("fixture-model"),
        usage(10, 1),
        None,
    );
    session.attempts[0].response_id = Some(safe_id(named));
    store
        .write_batch(
            &[
                answered("fixture-later-b", BASE + DAY, Some(KEY_B), shared),
                answered("fixture-later-own", BASE + DAY + 1, None, shared),
                answered("fixture-later-a", BASE + DAY + 2, Some(KEY_A), shared),
                answered("fixture-later-own-again", BASE + DAY + 3, None, own),
                session,
            ],
            None,
        )
        .unwrap();
    let merged = |id: &str| {
        let row = store.detail(id).unwrap();
        (row.deduplication, row.duplicate_of)
    };
    // With another key, the client's own included, it is a request of its own.
    for id in ["fixture-later-b", "fixture-later-own"] {
        assert_eq!(merged(id), (String::new(), None), "id={id}");
    }
    // With the same key it is the archived request, as it was before keys.
    assert_eq!(
        merged("fixture-later-a"),
        ("archived_match".into(), Some("fixture-archived-a".into()))
    );
    assert_eq!(
        merged("fixture-later-own-again"),
        ("archived_match".into(), Some("fixture-archived-own".into()))
    );
    // A session names the archived request it is, whatever key that came in with.
    assert_eq!(
        merged("fixture-later-session"),
        ("archived_match".into(), Some("fixture-archived-b".into()))
    );
    let requests = |key: &str| store.dashboard(&by_key(key)).unwrap().totals.requests;
    assert_eq!(
        (requests(KEY_A), requests(KEY_B), requests(OWN_ENTRY_KEY)),
        (1, 2, 2)
    );
    assert_eq!(store.dashboard(&all_time()).unwrap().totals.requests, 5);
}

/// Opens a copy of the usage database of `dir` and checks what opening does
/// to one written before a record kept its key. `dir` is only read: the copy
/// is what a reader sees of it, made in a temporary directory. Every message
/// is fixed text, so nothing of the records is printed.
fn a_copy_written_before_keys_opens_with_its_records_under_the_own_key(dir: &Path) {
    let copy = tempfile::tempdir().unwrap();
    let target = copy.path().join("usage.sqlite");
    {
        let source = rusqlite::Connection::open_with_flags(
            dir.join("usage.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap_or_else(|_| panic!("the usage database could not be opened for reading"));
        // A writer may be at work on it, as it is for a reader of the app.
        source
            .busy_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        // The copy builds its indexes over these functions again.
        super::query::register(&source)
            .unwrap_or_else(|_| panic!("the usage functions could not be registered"));
        source
            .execute("VACUUM INTO ?1", [target.to_str().unwrap()])
            .unwrap_or_else(|_| panic!("the usage database could not be copied"));
    }
    // Version, how many of a record and a receipt have the column, records
    // and rollups.
    let state = |c: &rusqlite::Connection| -> (i64, i64, i64, i64) {
        c.query_row(
            "SELECT (SELECT user_version FROM pragma_user_version),(SELECT count(*) FROM pragma_table_info('v12_rows') WHERE name='entry')+(SELECT count(*) FROM pragma_table_info('receipts') WHERE name='entry'),(SELECT count(*) FROM v12_rows),(SELECT count(*) FROM daily)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap_or_else(|_| panic!("the copy could not be read"))
    };
    let (version, columns, records, rollups) = state(
        &rusqlite::Connection::open(&target)
            .unwrap_or_else(|_| panic!("the copy could not be opened")),
    );
    assert!(
        matches!(version, 13 | 14),
        "the copy is not of a version that is taken over"
    );
    assert!(columns == 0, "the copy already has the column");
    assert!(records > 0 || rollups > 0, "the copy holds no usage");

    let store =
        Store::open(copy.path()).unwrap_or_else(|_| panic!("the copy could not be taken over"));
    assert!(
        state(&store.connection) == (14, 2, records, rollups),
        "taking the copy over changed more than the two columns"
    );
    let overview = store
        .dashboard(&Filter::default())
        .unwrap_or_else(|_| panic!("the overview could not be read"));
    assert!(
        overview.totals.requests > 0,
        "the overview counts no request"
    );
    assert!(
        overview.entry_keys.len() == 1
            && overview.entry_keys[0].id == OWN_ENTRY_KEY
            && serde_json::to_value(&overview.entry_keys[0].totals).ok()
                == serde_json::to_value(&overview.totals).ok(),
        "the requests are not all under the client's own key"
    );
    if records > 0 {
        let latest: String = store
            .connection
            .query_row(
                "SELECT usage_text(key) FROM v12_rows ORDER BY rid DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or_else(|_| panic!("the latest record could not be found"));
        let record = store
            .detail(&latest)
            .unwrap_or_else(|_| panic!("a record could not be read"));
        assert!(record.entry_key.is_none(), "a record names a key");
    }
}

#[test]
fn a_database_written_before_keys_gains_the_column_and_keeps_its_records_under_the_own_key() {
    let (dir, mut store, _pricing) = fixture();
    let mut unpriced = keyed("fixture-before-archived-unpriced", BASE + 1, None);
    unpriced.attempts[0].price = None;
    let archived = [keyed("fixture-before-archived", BASE, None), unpriced];
    let live = [
        keyed("fixture-before-live", BASE + 2 * DAY, None),
        record(
            "fixture-before-session",
            "codex",
            "codex",
            BASE + 2 * DAY + 1,
            None,
            Some("fixture-model"),
            usage(7, 3),
            None,
        ),
    ];
    store.write_batch(&archived, None).unwrap();
    store.write_batch(&live, None).unwrap();
    store
        .archive_batch(&archived.map(|request| request.id))
        .unwrap();
    let held = serde_json::to_value(store.dashboard(&all_time()).unwrap()).unwrap();
    drop(store);

    let path = dir.path().join("usage.sqlite");
    let version = |c: &rusqlite::Connection| -> i64 {
        c.query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap()
    };
    without_entry_column(dir.path());
    {
        let old = rusqlite::Connection::open(&path).unwrap();
        assert_eq!((version(&old), entry_columns(&old)), (14, 0));
    }
    // A copy of it is taken over, as one of a real database is before a
    // release; the database itself is left as it was.
    a_copy_written_before_keys_opens_with_its_records_under_the_own_key(dir.path());
    assert_eq!(
        entry_columns(&rusqlite::Connection::open(&path).unwrap()),
        0
    );

    let mut store = Store::open(dir.path()).unwrap();
    assert_eq!(
        (version(&store.connection), entry_columns(&store.connection)),
        (14, 2)
    );
    // What it held reads as it did, all of it under the client's own key.
    assert_eq!(
        serde_json::to_value(store.dashboard(&all_time()).unwrap()).unwrap(),
        held
    );
    let overview = store.dashboard(&all_time()).unwrap();
    assert_eq!(overview.totals.requests, 4);
    assert_eq!(overview.entry_keys.len(), 1);
    assert_eq!(overview.entry_keys[0].id, OWN_ENTRY_KEY);
    assert_eq!(
        comparable(&overview.entry_keys[0].totals),
        comparable(&overview.totals)
    );
    for request in &live {
        assert_eq!(store.detail(&request.id).unwrap().entry_key, None);
    }
    assert_eq!(store.logs(&by_key(OWN_ENTRY_KEY)).unwrap().total, 2);
    assert_eq!(store.logs(&by_key(KEY_A)).unwrap().total, 0);

    // The columns of a record and of a receipt, and the views that list a
    // record's, are those of a database made new.
    let columns = |c: &rusqlite::Connection| -> Vec<String> {
        c.prepare("SELECT concat_ws('|',t.name,i.cid,i.name,i.type,i.\"notnull\",coalesce(i.dflt_value,''),i.pk) FROM sqlite_master t,pragma_table_info(t.name) i WHERE t.name IN ('v12_rows','receipts') ORDER BY t.name,i.cid")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let views = |c: &rusqlite::Connection| -> Vec<String> {
        c.prepare("SELECT concat_ws('|',type,name,sql) FROM sqlite_master WHERE tbl_name IN ('v12_records','records') ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let (_new_dir, new, _) = fixture();
    assert_eq!(columns(&store.connection), columns(&new.connection));
    assert_eq!(views(&store.connection), views(&new.connection));
    assert_eq!(views(&store.connection).len(), 4);

    // A request that comes in with a key afterwards keeps it.
    store
        .write_batch(
            &[keyed(
                "fixture-before-keyed",
                BASE + 2 * DAY + 2,
                Some(KEY_A),
            )],
            None,
        )
        .unwrap();
    assert_eq!(
        store
            .detail("fixture-before-keyed")
            .unwrap()
            .entry_key
            .as_deref(),
        Some(KEY_A)
    );
    assert_eq!(store.logs(&by_key(KEY_A)).unwrap().total, 1);

    // Opening it again changes nothing of its schema.
    let schema = |c: &rusqlite::Connection| -> (i64, Vec<String>) {
        let cookie = c
            .query_row("PRAGMA schema_version", [], |row| row.get(0))
            .unwrap();
        let made = c
            .prepare("SELECT concat_ws('|',type,name,tbl_name,sql) FROM sqlite_master ORDER BY type,name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        (cookie, made)
    };
    let made = schema(&store.connection);
    drop(store);
    let store = Store::open(dir.path()).unwrap();
    assert_eq!(schema(&store.connection), made);
    assert_eq!(store.logs(&all_time()).unwrap().total, 3);
}

#[test]
#[ignore = "Reads the usage database of an explicitly supplied directory; only a copy in a temporary directory is opened for writing"]
fn real_usage_database_is_taken_over_with_its_records_under_the_own_key() {
    let dir = std::env::var_os("VELA_REAL_USAGE_DIR").expect("usage directory required");
    // Never print what the database holds: it is the user's own usage.
    a_copy_written_before_keys_opens_with_its_records_under_the_own_key(Path::new(&dir));
}
