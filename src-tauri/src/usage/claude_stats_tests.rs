use super::{model::*, pricing::Pricing, store::Store};

const AT: i64 = 1_793_900_000_000;

fn request(id: &str, source: &str, model: &str, operation: Operation) -> Record {
    Record {
        id: safe_id(id),
        client: "claude".into(),
        source: source.into(),
        session_id: Some(safe_id("session")),
        started_at: AT,
        finished_at: Some(AT + 1000),
        completed: true,
        attempts: vec![Attempt {
            id: safe_id(&format!("{id}-attempt")),
            provider: (source == "proxy").then(|| "last".into()),
            requested_model: Some(model.into()),
            response_model: Some(model.into()),
            pricing_model: Some(model.into()),
            response_id: Some(format!("response-{id}")),
            started_at: AT,
            duration_ms: 1000,
            status: Some(200),
            outcome: "success".into(),
            operation,
            tokens: Tokens {
                input: Some(100),
                output: Some(20),
                cache_read: Some(80),
                cache_write: Some(0),
                ..Default::default()
            },
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn independent_stats_keep_dedup_retries_model_ids_and_filters_aligned() {
    for native in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        if native {
            while store.compact_storage_batch().unwrap() {}
        }
        let prices = Pricing::new(dir.path()).unwrap();
        let mut gateway = request("gateway", "proxy", "claude-sonnet-4-6", Operation::Model);
        let mut duplicate = gateway.clone();
        duplicate.id = safe_id("duplicate");
        duplicate.source = "claude".into();
        duplicate.attempts[0].provider = None;
        let mut retry = gateway.attempts[0].clone();
        retry.id = safe_id("retry");
        retry.provider = Some("first".into());
        retry.response_id = Some("retry-response".into());
        retry.status = Some(503);
        gateway.attempts.insert(0, retry);
        let local = request(
            "local",
            "claude",
            "claude-sonnet-4-20250514",
            Operation::Model,
        );
        let compression = request(
            "compression",
            "proxy",
            "claude-sonnet-4-6",
            Operation::Compaction,
        );
        let search = request("search", "proxy", "web_search", Operation::WebSearch);
        let mut records = [gateway, duplicate, local, compression, search];
        for record in &mut records {
            for attempt in &mut record.attempts {
                attempt.price = prices
                    .quote(Some("claude-sonnet-4-6"), "2")
                    .calculate(&attempt.tokens, None);
            }
        }
        records[0].attempts[0].price = None;
        store.write_batch(&records, None).unwrap();
        if native {
            while store.compact_storage_batch().unwrap() {}
        }
        let filter = Filter {
            client: Some("claude".into()),
            end: Some(AT + 2000),
            ..Default::default()
        };
        let all = store.claude_stats(&filter, AT + 2000).unwrap();
        assert_eq!(all.requests, 4);
        assert_eq!(all.sessions, 1);
        assert_eq!(all.unpriced_requests, 1);
        assert_eq!(
            all.providers
                .iter()
                .find(|p| p.id == "first")
                .unwrap()
                .unpriced_requests,
            1
        );
        assert_eq!(
            all.providers
                .iter()
                .find(|p| p.id == "last")
                .unwrap()
                .unpriced_requests,
            0
        );
        assert!(!all.token_mix.reasoning_reported);
        assert_eq!(all.token_mix.thinking_share_percent, None);
        assert_eq!(
            all.families
                .iter()
                .filter(|f| f.family.starts_with("claude-sonnet"))
                .count(),
            2
        );
        assert!(all.accounts.is_empty());
        assert!(decimal(&all.token_mix.cache_savings_usd).unwrap() > rust_decimal::Decimal::ZERO);
        for selected in [
            filter.clone(),
            Filter {
                provider: Some("last".into()),
                ..filter.clone()
            },
            Filter {
                provider: Some("session".into()),
                ..filter.clone()
            },
            Filter {
                provider: Some("first".into()),
                ..filter.clone()
            },
            Filter {
                operation: Some("model".into()),
                ..filter.clone()
            },
            Filter {
                operation: Some("compaction".into()),
                ..filter.clone()
            },
            Filter {
                operation: Some("web_search".into()),
                ..filter.clone()
            },
            Filter {
                session_id: Some(safe_id("session")),
                ..filter.clone()
            },
            Filter {
                start: Some(AT + 2001),
                ..filter.clone()
            },
        ] {
            let dashboard = store.dashboard(&selected).unwrap();
            let stats = store.claude_stats(&selected, AT + 2000).unwrap();
            assert_eq!(stats.requests, dashboard.totals.requests);
            assert_eq!(stats.unpriced_requests, dashboard.totals.unpriced);
            assert_eq!(decimal(&stats.cost_usd), decimal(&dashboard.totals.cost));
            assert_eq!(
                stats.token_mix.total,
                dashboard.totals.tokens.total().unwrap_or(0)
            );
        }
        // Endpoint ownership overrides a stale caller-side client selection.
        assert_eq!(
            store
                .claude_stats(
                    &Filter {
                        client: Some("codex".into()),
                        ..filter
                    },
                    AT + 2000
                )
                .unwrap()
                .requests,
            4
        );
    }
}

#[test]
fn saved_tariff_controls_savings_and_missing_basis_keeps_known_sum() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let prices = Pricing::new(dir.path()).unwrap();
    let mut row = request("saved", "claude", "claude-sonnet-4-6", Operation::Model);
    let attempt = &mut row.attempts[0];
    attempt.price = prices
        .quote(Some("claude-sonnet-4-6"), "2")
        .calculate(&attempt.tokens, Some("fast"));
    attempt.service_tier = Some("fast".into());
    let price = attempt.price.as_ref().unwrap();
    let expected = super::pricing::cache_savings_rates(&price.rates, &price.multiplier)
        .and_then(|rates| super::pricing::cache_savings(rates, attempt.tokens.cache_read.unwrap()))
        .unwrap();
    let cost = price.cost.clone();
    store.write_batch(std::slice::from_ref(&row), None).unwrap();
    let selected = Filter {
        end: Some(AT + 2000),
        ..Default::default()
    };
    let before = store.claude_stats(&selected, AT + 2000).unwrap();
    assert_eq!(decimal(&before.token_mix.cache_savings_usd), Some(expected));
    assert_eq!(before.cost_usd, cost);
    let mut priced = row.clone();
    priced.id = safe_id("priced");
    priced.attempts[0].id = safe_id("priced-attempt");
    priced.attempts[0].response_id = Some("priced-response".into());
    row.attempts[0].price.as_mut().unwrap().basis = None;
    row.attempts[0].price.as_mut().unwrap().rates.clear();
    let missing = tempfile::tempdir().unwrap();
    let mut store = Store::open(missing.path()).unwrap();
    store.write_batch(&[row], None).unwrap();
    let after = store.claude_stats(&selected, AT + 2000).unwrap();
    assert_eq!(after.token_mix.cache_savings_usd, "0");
    assert_eq!(after.cost_usd, cost);
    assert_eq!(after.token_mix.total, before.token_mix.total);
    store.write_batch(&[priced], None).unwrap();
    let mixed = store.claude_stats(&selected, AT + 2000).unwrap();
    assert_eq!(mixed.requests, 2);
    assert_eq!(decimal(&mixed.token_mix.cache_savings_usd), Some(expected));
}

#[test]
fn cache_savings_uses_applied_rates_for_compacted_retries() {
    use super::pricing::{cache_savings, cache_savings_rates, Quote};
    let quote = Quote {
        model: "fixture-model".into(),
        data: Some(serde_json::json!({
            "input_cost_per_token": "0.01", "input_cost_per_token_above_200k_tokens": "0.02",
            "cache_read_input_token_cost": "0.001", "cache_read_input_token_cost_above_200k_tokens": "0.002",
            "output_cost_per_token": "0.02"
        })),
        version: "fixture".into(),
        source: "fixed".into(),
        multiplier: "2".into(),
    };
    let tokens = Tokens {
        input: Some(100_000),
        output: Some(0),
        cache_read: Some(50_000),
        ..Default::default()
    };
    let price = quote.calculate(&tokens, None).unwrap();
    let mut aggregate = tokens.clone();
    aggregate.add(&tokens);
    assert_eq!(
        cache_savings_rates(&price.rates, &price.multiplier)
            .and_then(|rates| cache_savings(rates, aggregate.cache_read.unwrap())),
        decimal("1800")
    );
    let zero_fresh = Tokens {
        input: Some(0),
        ..tokens
    };
    let saved = quote.calculate(&zero_fresh, None).unwrap();
    assert_eq!(
        cache_savings_rates(&saved.rates, &saved.multiplier)
            .and_then(|rates| cache_savings(rates, zero_fresh.cache_read.unwrap())),
        None
    );
}

#[test]
fn missing_retries_preserve_reported_totals_and_unpriced_providers_count_once() {
    for native in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let mut row = request(
            "missing-retries",
            "proxy",
            "claude-sonnet-4-6",
            Operation::Model,
        );
        row.attempts[0].tokens.reasoning = Some(8);
        let prices = Pricing::new(dir.path()).unwrap();
        row.attempts[0].price = prices
            .quote(Some("claude-sonnet-4-6"), "2")
            .calculate(&row.attempts[0].tokens, None);
        for index in 0..2 {
            let mut retry = row.attempts[0].clone();
            retry.id = safe_id(&format!("empty-{index}"));
            retry.provider = Some("first".into());
            retry.status = Some(503);
            retry.tokens = Tokens::default();
            retry.price = None;
            row.attempts.insert(0, retry);
        }
        store.write_batch(&[row], None).unwrap();
        if native {
            while store.compact_storage_batch().unwrap() {}
        }
        let stats = store.claude_stats(&Filter::default(), AT + 2000).unwrap();
        assert_eq!(stats.token_mix.input, 100);
        assert_eq!(stats.token_mix.cache_write, 0);
        assert!(stats.token_mix.reasoning_reported);
        assert_eq!(stats.token_mix.reasoning, 8);
        assert_eq!(stats.token_mix.thinking_share_percent, Some(40.0));
        assert!(decimal(&stats.token_mix.cache_savings_usd).unwrap() > rust_decimal::Decimal::ZERO);
        assert!(stats.families[0].reasoning_reported);
        assert!(
            !stats
                .providers
                .iter()
                .find(|p| p.id == "first")
                .unwrap()
                .reasoning_reported
        );
        assert!(
            stats
                .providers
                .iter()
                .find(|p| p.id == "last")
                .unwrap()
                .reasoning_reported
        );
        assert_eq!(stats.unpriced_requests, 1);
        assert_eq!(
            stats
                .providers
                .iter()
                .find(|p| p.id == "first")
                .unwrap()
                .unpriced_requests,
            1
        );
        assert_eq!(
            stats
                .providers
                .iter()
                .find(|p| p.id == "last")
                .unwrap()
                .unpriced_requests,
            0
        );
        let mut absent = request("all-missing", "claude", "fixture-model", Operation::Model);
        absent.session_id = Some(safe_id("absent-session"));
        absent.attempts[0].tokens = Tokens::default();
        store.write_batch(&[absent], None).unwrap();
        let stats = store
            .claude_stats(
                &Filter {
                    session_id: Some(safe_id("absent-session")),
                    ..Default::default()
                },
                AT + 2000,
            )
            .unwrap();
        assert_eq!(stats.token_mix.input, 0);
        assert_eq!(stats.token_mix.output, 0);
        assert_eq!(stats.token_mix.cache_read, 0);
        assert_eq!(stats.token_mix.cache_write, 0);
        assert_eq!(stats.token_mix.cache_savings_usd, "0");
        assert!(!stats.token_mix.reasoning_reported);
        let empty = store
            .claude_stats(
                &Filter {
                    start: Some(AT + 3000),
                    ..Default::default()
                },
                AT + 4000,
            )
            .unwrap();
        assert_eq!(empty.requests, 0);
        assert_eq!(empty.token_mix.total, 0);
        assert_eq!(empty.token_mix.cache_savings_usd, "0");
        assert!(!empty.token_mix.reasoning_reported);
    }
}

#[test]
fn claude_thinking_tokens_guard_false_zeroes_and_backfill_via_source_generation_without_losing_history(
) {
    use crate::gateway::metadata::Projector;
    use serde_json::json;

    // 1) Live SSE Meter: content_block_start(redacted_thinking) prevents false 0, while positive thinking_tokens wins.
    let mut meter = Meter::default();
    for raw in [
        json!({"type":"message_start","message":{"id":"msg_1","model":"claude-opus-4-6","usage":{"input_tokens":50,"output_tokens":0,"output_tokens_details":{"thinking_tokens":0}}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"redacted_thinking","data":"enc"}}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":20,"output_tokens_details":{"thinking_tokens":0}}}),
    ] {
        let mut proj = Projector::default();
        proj.feed(raw.to_string().as_bytes());
        let projected = proj.finish().unwrap();
        meter.observe(&projected, 0);
    }
    assert_eq!(meter.tokens.reasoning, None);

    let mut meter_pos = Meter::default();
    let mut proj = Projector::default();
    proj.feed(
        json!({"type":"message","id":"msg_2","model":"claude-opus-4-6","content":[{"type":"thinking","thinking":"abc"},{"type":"text","text":"hi"}],"usage":{"input_tokens":10,"output_tokens":30,"output_tokens_details":{"thinking_tokens":18}}})
            .to_string()
            .as_bytes(),
    );
    meter_pos.observe(&proj.finish().unwrap(), 0);
    assert_eq!(meter_pos.tokens.reasoning, Some(18));

    // 2) Existing v13 DB upgrade backfills live .jsonl via source_generation_claude without deleting cleaned history.
    let dir = tempfile::tempdir().unwrap();
    let claude_home = dir.path().join("claude_home");
    let proj_dir = claude_home.join("projects/demo");
    std::fs::create_dir_all(&proj_dir).unwrap();
    let jsonl = proj_dir.join("session.jsonl");
    let line1 = json!({"type":"assistant","sessionId":"sess-1","timestamp":"2026-11-16T10:00:00Z","message":{"id":"msg_live","model":"claude-opus-4-6","stop_reason":null,"content":[{"type":"thinking","thinking":"step"}],"usage":{"input_tokens":100,"output_tokens":1,"output_tokens_details":{"thinking_tokens":0}}}});
    let line2 = json!({"type":"assistant","sessionId":"sess-1","timestamp":"2026-11-16T10:00:02Z","message":{"id":"msg_live","model":"claude-opus-4-6","stop_reason":"end_turn","content":[{"type":"text","text":"answer"}],"usage":{"input_tokens":100,"output_tokens":40,"output_tokens_details":{"thinking_tokens":25}}}});
    let line3 = json!({"type":"assistant","sessionId":"sess-1","timestamp":"2026-11-16T10:00:05Z","message":{"id":"msg_false_zero","model":"claude-opus-4-6","stop_reason":null,"content":[{"type":"thinking","thinking":"hidden"}],"usage":{"input_tokens":50,"output_tokens":1,"output_tokens_details":{"thinking_tokens":0}}}});
    let line4 = json!({"type":"assistant","sessionId":"sess-1","timestamp":"2026-11-16T10:00:06Z","message":{"id":"msg_false_zero","model":"claude-opus-4-6","stop_reason":"end_turn","content":[{"type":"text","text":"done"}],"usage":{"input_tokens":50,"output_tokens":10,"output_tokens_details":{"thinking_tokens":0}}}});
    std::fs::write(&jsonl, format!("{line1}\n{line2}\n{line3}\n{line4}\n")).unwrap();

    let prices = Pricing::new(dir.path()).unwrap();
    let settings = Settings::default();
    {
        let mut store = Store::open(dir.path()).unwrap();
        // Seed an old cleaned-up session record and a pre-v0.30 proxy record + old session record without reasoning.
        let mut archived = request(
            "cleaned-history",
            "claude",
            "claude-opus-4-6",
            Operation::Model,
        );
        archived.attempts[0].tokens.reasoning = None;
        let mut proxy = request("proxy-live", "proxy", "claude-opus-4-6", Operation::Model);
        proxy.started_at = 1_794_823_200_000;
        proxy.finished_at = Some(1_794_823_202_000);
        proxy.attempts[0].started_at = 1_794_823_200_000;
        proxy.attempts[0].response_id = Some(safe_id("msg_live"));
        proxy.attempts[0].tokens = Tokens {
            input: Some(100),
            output: Some(40),
            ..Default::default()
        };
        store.write_batch(&[archived, proxy], None).unwrap();
        // Simulate pre-v0.30 state: synced at generation 0 before claude_thinking_backfill existed.
        super::sessions::sync(
            &mut store,
            &prices,
            &settings,
            "claude",
            &claude_home,
            false,
        )
        .unwrap();
        store
            .connection
            .execute(
                "DELETE FROM metadata WHERE key='claude_thinking_backfill'",
                [],
            )
            .unwrap();
        // Clear reasoning on proxy & session to simulate pre-v0.30 stored state while keeping cursor at EOF.
        let mut stale_session = store
            .detail(&safe_id(&format!("claude:{}", safe_id("msg_live"))))
            .unwrap();
        stale_session.attempts[0].tokens.reasoning = None;
        let mut stale_proxy = store.detail(&safe_id("proxy-live")).unwrap();
        stale_proxy.attempts[0].tokens.reasoning = None;
        store
            .write_batch(&[stale_session, stale_proxy], None)
            .unwrap();
    }

    // Reopen Store: triggers one-time claude_thinking_backfill + source_generation_claude increment.
    let mut reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.source_generation("claude").unwrap(), 1);
    super::sessions::sync(
        &mut reopened,
        &prices,
        &settings,
        "claude",
        &claude_home,
        false,
    )
    .unwrap();

    // Cleaned historical record survives with reasoning == None.
    let kept_history = reopened.detail(&safe_id("cleaned-history")).unwrap();
    assert_eq!(kept_history.attempts[0].tokens.reasoning, None);

    // Rescanned live message & deduplicated proxy record both get reasoning == Some(25).
    let live_proxy = reopened.detail(&safe_id("proxy-live")).unwrap();
    assert_eq!(live_proxy.attempts[0].tokens.reasoning, Some(25));

    // False-zero message across two lines keeps reasoning == None.
    let false_zero = reopened
        .detail(&safe_id(&format!("claude:{}", safe_id("msg_false_zero"))))
        .unwrap();
    assert_eq!(false_zero.attempts[0].tokens.reasoning, None);

    // Subsequent reopen does NOT bump source_generation_claude again.
    drop(reopened);
    let third = Store::open(dir.path()).unwrap();
    assert_eq!(third.source_generation("claude").unwrap(), 1);
}
