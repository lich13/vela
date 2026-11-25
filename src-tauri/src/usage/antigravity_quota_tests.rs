use super::*;
use crate::usage::{model::*, store::Store};

fn summary() -> Value {
    json!({"response":{"groups":[
        {"buckets":[{"bucketId":"gemini-5h","window":"5h","resetTime":"2026-01-08T05:00:00Z","remainingFraction":0.745},{"bucketId":"gemini-weekly","window":"weekly","resetTime":"2026-01-09T00:00:00Z","remainingFraction":0.86}]},
        {"buckets":[{"bucketId":"3p-5h","window":"5h","resetTime":"2026-01-08T06:00:00Z","remainingFraction":0.42},{"bucketId":"3p-weekly","window":"weekly","resetTime":"2026-01-10T00:00:00Z","remainingFraction":0.685}]}
    ]}})
}

#[test]
fn each_group_uses_its_observed_reset_and_half_open_window() {
    let p = parse(&summary());
    assert_eq!(p.len(), 4);
    let gemini = &p["gemini-5h"];
    let third_party = &p["3p-5h"];
    assert_eq!(gemini.end - gemini.start, 5 * 3_600_000);
    assert_eq!(gemini.remaining_percent, Some(74.5));
    assert_eq!(third_party.remaining_percent, Some(42.0));
    assert_eq!(third_party.start - gemini.start, 3_600_000);
    assert_eq!(
        p["gemini-weekly"].end - p["gemini-weekly"].start,
        7 * 86_400_000
    );
    assert_eq!(p["gemini-weekly"].remaining_percent, Some(86.0));
    assert_eq!(p["3p-weekly"].remaining_percent, Some(68.5));
    assert_ne!(p["gemini-weekly"], p["3p-weekly"]);
    assert!(gemini.contains(gemini.start));
    assert!(!gemini.contains(gemini.start - 1));
    assert!(!gemini.contains(gemini.end));
    assert!(!gemini.current(gemini.end));
}

#[test]
fn account_periods_ignore_history_filters_and_expire_without_extrapolation() {
    let periods = parse(&summary());
    let now = periods["gemini-5h"].start + 2 * 3_600_000;
    for native in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        if native {
            while store.compact_storage_batch().unwrap() {}
        }
        let entries: Vec<_> = ["b", "a"].iter().map(|id| json!({"id":id,"name":format!("Fixture {id}"),"kind":"oauth","sourceType":"fixture","payload":"{}","updatedAt":0})).collect();
        std::fs::write(
            dir.path().join("antigravity-accounts.json"),
            serde_json::to_vec(&json!({"schema":1,"entries":entries})).unwrap(),
        )
        .unwrap();
        store.save_antigravity_quota("a", &periods).unwrap();
        let writes = store.connection.total_changes();
        store.save_antigravity_quota("a", &periods).unwrap();
        assert_eq!(
            store.connection.total_changes(),
            writes,
            "unchanged boundaries must not write metadata"
        );
        let make = |id: &str, provider: &str, model: &str, at: i64| {
            let mut r = Record {
                id: id.into(),
                client: "antigravity".into(),
                source: "antigravity".into(),
                started_at: at,
                finished_at: Some(at),
                completed: true,
                attempts: vec![Attempt {
                    id: format!("{id}-attempt"),
                    provider: Some(provider.into()),
                    requested_model: Some(model.into()),
                    response_model: Some(model.into()),
                    pricing_model: Some(model.into()),
                    started_at: at,
                    outcome: "success".into(),
                    usage_status: "reported".into(),
                    tokens: Tokens {
                        input: Some(100),
                        output: Some(20),
                        ..Tokens::default()
                    },
                    ..Attempt::default()
                }],
                ..Record::default()
            };
            super::super::normalize_record(&mut r);
            r
        };
        let start = periods["gemini-5h"].start;
        store
            .write_batch(
                &[
                    make("before", "a", "gemini-3-flash", start - 1),
                    make("included", "a", "gemini-3-flash", start),
                    make("current", "a", "gemini-3-flash", now),
                    make("reset", "a", "gemini-3-flash", periods["gemini-5h"].end),
                    make("claude-before", "a", "claude-opus-4-6", start),
                    make(
                        "claude-included",
                        "a",
                        "claude-opus-4-6",
                        periods["3p-5h"].start,
                    ),
                    make("other-account", "b", "gemini-3-flash", now),
                    // Usage of an account that no longer exists adds no account.
                    make("deleted-account", "gone", "gemini-3-flash", now),
                ],
                None,
            )
            .unwrap();
        let filter = Filter {
            start: Some(now),
            end: Some(now + 1000),
            ..Filter::default()
        };
        let stats = store.antigravity_stats(&filter, now).unwrap();
        assert_eq!(
            stats
                .accounts
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        let a = stats.accounts.iter().find(|a| a.id == "a").unwrap();
        assert_eq!(a.total.requests, 0);
        assert_eq!(a.tracks[0].burst_5h.requests, 2);
        assert_eq!(a.tracks[0].burst_5h.tokens, 240);
        assert_eq!(a.tracks[0].weekly_7d.requests, 3);
        assert_eq!(a.tracks[1].burst_5h.requests, 1);
        assert_eq!(a.tracks[1].weekly_7d.requests, 2);
        let b = stats.accounts.iter().find(|a| a.id == "b").unwrap();
        assert!(b
            .tracks
            .iter()
            .all(|t| t.burst_5h.period.is_none() && t.weekly_7d.period.is_none()));
        // A previously seen inactive account can use its still-current periods.
        store.save_antigravity_quota("b", &periods).unwrap();
        let other = store.antigravity_stats(&filter, now).unwrap();
        assert_eq!(
            other.accounts.iter().find(|a| a.id == "b").unwrap().tracks[0]
                .burst_5h
                .requests,
            1
        );
        let mut retry = make("retry", "b", "gemini-3-flash", now);
        let mut first = retry.attempts[0].clone();
        first.id = "retry-first".into();
        first.provider = Some("a".into());
        let mut second = first.clone();
        second.id = "retry-second".into();
        retry.attempts.splice(0..0, [first, second]);
        super::super::normalize_record(&mut retry);
        store.write_batch(&[retry], None).unwrap();
        if native {
            while store.compact_storage_batch().unwrap() {}
        }
        let retried = store.antigravity_stats(&filter, now).unwrap();
        assert_eq!(retried.unpriced_requests, other.unpriced_requests + 1);
        for id in ["a", "b"] {
            let before = other.accounts.iter().find(|a| a.id == id).unwrap();
            let after = retried.accounts.iter().find(|a| a.id == id).unwrap();
            assert_eq!(
                after.tracks[0].burst_5h.unpriced_requests,
                before.tracks[0].burst_5h.unpriced_requests + 1,
                "each account owns its unpriced attempts, once per request"
            );
        }
        let expired_at = periods["3p-weekly"].end;
        let expired = store.antigravity_stats(&filter, expired_at).unwrap();
        assert!(expired.accounts.iter().flat_map(|a| &a.tracks).all(|t| t
            .burst_5h
            .period
            .is_none()
            && t.weekly_7d.period.is_none()));
    }
}

#[test]
fn a_row_stored_without_an_account_takes_the_one_in_use_then_and_a_quota_without_a_period_says_why()
{
    let mut periods = parse(&summary());
    let now = periods["gemini-5h"].start + 3_600_000;
    periods.insert(
        "3p-5h".into(),
        Period {
            without: Some(Without::Disabled),
            ..Period::default()
        },
    );
    periods.insert(
        "3p-weekly".into(),
        Period {
            remaining_percent: Some(100.0),
            without: Some(Without::NoReset),
            ..Period::default()
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    std::fs::write(
        dir.path().join("antigravity-accounts.json"),
        serde_json::to_vec(&json!({
            "schema": 1,
            "entries": [{"id":"a","name":"Fixture a","kind":"oauth","sourceType":"fixture","payload":"{}","updatedAt":0}],
            "activations": [{"accountId": "a", "sinceMs": 0}]
        }))
        .unwrap(),
    )
    .unwrap();
    store.save_antigravity_quota("a", &periods).unwrap();
    let mut row = Record {
        id: "unowned".into(),
        client: "antigravity".into(),
        source: "antigravity".into(),
        started_at: now,
        finished_at: Some(now),
        completed: true,
        attempts: vec![Attempt {
            id: "unowned-attempt".into(),
            requested_model: Some("gemini-3-flash".into()),
            response_model: Some("gemini-3-flash".into()),
            pricing_model: Some("gemini-3-flash".into()),
            started_at: now,
            outcome: "success".into(),
            usage_status: "reported".into(),
            tokens: Tokens {
                input: Some(100),
                output: Some(20),
                ..Tokens::default()
            },
            ..Attempt::default()
        }],
        ..Record::default()
    };
    super::super::normalize_record(&mut row);
    store.write_batch(&[row], None).unwrap();

    let stats = store.antigravity_stats(&Filter::default(), now).unwrap();
    let account = &stats.accounts[0];
    assert_eq!(account.tracks[0].burst_5h.requests, 1);
    assert_eq!(
        stats
            .providers
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["a"]
    );
    // The periods that say nothing do not widen what is read or counted.
    let third_party = &account.tracks[1];
    assert_eq!(
        (&third_party.burst_5h.period, third_party.burst_5h.without),
        (&None, Some(Without::Disabled))
    );
    assert_eq!(
        (
            &third_party.weekly_7d.period,
            third_party.weekly_7d.without,
            third_party.weekly_7d.remaining_percent
        ),
        (&None, Some(Without::NoReset), Some(100.0))
    );
    assert_eq!(third_party.weekly_7d.requests, 0);
}

#[test]
fn an_untouched_quota_reset_at_the_unix_epoch_has_no_reset_time() {
    // `agy /usage` gives the Unix epoch as the reset of a quota nothing was
    // spent from.
    let epoch = json!({"id": "gemini-5h", "reset_time": "1970-01-01T00:00:00Z", "remaining_fraction": 1.0});
    let real = json!({"bucketId": "gemini-5h", "resetTime": "2026-01-08T05:00:00Z", "remainingFraction": 0.745});
    let read = periods(&json!([{"buckets": [
        epoch.clone(),
        {"bucketId": "gemini-weekly", "resetTime": "1970-01-01T00:00:00Z", "remainingFraction": 1.0},
        {"bucketId": "3p-5h", "resetTime": "2026-01-08T06:00:00Z", "remainingFraction": 0.42},
    ]}]));
    let untouched = Period {
        remaining_percent: Some(100.0),
        without: Some(Without::NoReset),
        ..Period::default()
    };
    assert_eq!(read["gemini-5h"], untouched);
    assert_eq!(read["gemini-weekly"], untouched);
    assert_eq!(read["3p-5h"].without, None);
    // It never expires, and no usage falls into it.
    assert!(untouched.current(1) && untouched.current(i64::MAX));
    assert!(!untouched.contains(0));

    // A period reported for the same quota elsewhere stands, in either order.
    for buckets in [
        json!([epoch.clone(), real.clone()]),
        json!([real, epoch]),
    ] {
        let read = periods(&json!([{ "buckets": buckets }]));
        assert_eq!(
            (read["gemini-5h"].without, read["gemini-5h"].remaining_percent),
            (None, Some(74.5))
        );
    }
}

#[test]
fn a_period_saved_as_ending_at_the_unix_epoch_reads_as_one_without_a_reset_time() {
    let body = json!({
        "gemini-5h": {"start": -18_000_000, "end": 0, "remainingPercent": 100.0},
        "gemini-weekly": {"start": -604_800_000, "end": 0, "remainingPercent": 100.0},
        "3p-5h": {"start": 0, "end": 0, "without": "disabled"},
        "3p-weekly": {"start": 1_767_398_400_000_i64, "end": 1_768_003_200_000_i64, "remainingPercent": 68.5},
    });
    let saved: Periods = serde_json::from_value(body.clone()).unwrap();
    let untouched = Period {
        remaining_percent: Some(100.0),
        without: Some(Without::NoReset),
        ..Period::default()
    };
    assert_eq!(saved["gemini-5h"], untouched);
    assert_eq!(saved["gemini-weekly"], untouched);
    let now = 1_767_484_800_000;
    assert!(saved["gemini-5h"].current(now));
    // A reason or a real period is read as it was saved.
    assert_eq!(
        saved["3p-5h"],
        Period {
            without: Some(Without::Disabled),
            ..Period::default()
        }
    );
    assert_eq!(
        saved["3p-weekly"],
        Period {
            start: 1_767_398_400_000,
            end: 1_768_003_200_000,
            remaining_percent: Some(68.5),
            without: None,
        }
    );

    // The statistics show it at once, before the quota is asked for again.
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    std::fs::write(
        dir.path().join("antigravity-accounts.json"),
        serde_json::to_vec(&json!({
            "schema": 1,
            "entries": [{"id":"a","name":"Fixture a","kind":"oauth","sourceType":"fixture","payload":"{}","updatedAt":0}],
        }))
        .unwrap(),
    )
    .unwrap();
    store
        .connection
        .execute(
            "INSERT INTO metadata(key,value) VALUES('antigravity_quota:a',?1)",
            [body.to_string()],
        )
        .unwrap();
    let stats = store.antigravity_stats(&Filter::default(), now).unwrap();
    let [gemini, third_party] = &stats.accounts[0].tracks[..] else {
        panic!("two tracks");
    };
    for window in [&gemini.burst_5h, &gemini.weekly_7d] {
        assert_eq!(
            (&window.period, window.without, window.remaining_percent),
            (&None, Some(Without::NoReset), Some(100.0))
        );
    }
    assert_eq!(third_party.burst_5h.without, Some(Without::Disabled));
    assert!(third_party.weekly_7d.period.is_some());

    // Asked again, the quota is written the way it is read now, once.
    assert!(store.save_antigravity_quota("a", &saved).unwrap());
    assert!(!store.save_antigravity_quota("a", &saved).unwrap());
}

#[test]
fn hourly_cost_is_the_period_spend_over_the_time_since_it_began() {
    let periods = parse(&summary());
    let start = periods["gemini-5h"].start;
    let usage_at = |now: i64| {
        let account = Account {
            id: "a".into(),
            name: "Fixture".into(),
        };
        let mut acc = AccountAccumulator::new(account, periods.clone(), now);
        let spent = WindowAcc {
            requests: 1,
            cost: rust_decimal::Decimal::from(3),
            ..WindowAcc::default()
        };
        acc.ingest(start, Some("gemini"), &spent);
        acc.finish()
    };
    let two_hours_in = usage_at(start + 2 * 3_600_000);
    let gemini = &two_hours_in.tracks[0];
    assert_eq!(gemini.burst_5h.cost_usd, "3");
    assert_eq!(gemini.burst_5h.cost_per_hour, "1.5");
    // The weekly period began six days before the five-hour one: 3 / 146.
    assert_eq!(gemini.weekly_7d.cost_per_hour, "0.0205");
    // A period without spend, and the account as a whole.
    assert_eq!(two_hours_in.tracks[1].burst_5h.cost_per_hour, "0");
    assert_eq!(two_hours_in.total.cost_per_hour, "0");
    assert_eq!(
        serde_json::to_value(&gemini.burst_5h).unwrap()["costPerHour"],
        "1.5"
    );

    // The first minute counts as a whole one.
    assert_eq!(usage_at(start + 1).tracks[0].burst_5h.cost_per_hour, "180");

    // Past its reset a period is not shown, and neither is a rate.
    let later = usage_at(periods["gemini-5h"].end);
    assert!(later.tracks[0].burst_5h.period.is_none());
    assert_eq!(later.tracks[0].burst_5h.cost_per_hour, "0");
}

#[tokio::test]
async fn quota_requires_the_app_identity_to_match_the_selected_account() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        for method in [
            "GetUserStatus",
            "GetUserStatus",
            "RetrieveUserQuotaSummary",
            "GetUserStatus",
            "RetrieveUserQuotaSummary",
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let len = socket.read(&mut request).await.unwrap();
            assert!(String::from_utf8_lossy(&request[..len]).contains(method));
            let value = if method == "GetUserStatus" {
                json!({"userStatus":{"email":"fixture@example.invalid"}})
            } else {
                summary()
            };
            let body = value.to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let api = Api::from_single(
        super::super::loopback_client().unwrap(),
        origin.clone(),
        "fixture".into(),
    );
    assert!(api
        .quota_for("another@example.invalid")
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        api.quota_for("FIXTURE@example.invalid")
            .await
            .unwrap()
            .unwrap(),
        parse(&summary())
    );

    // An instance that cannot answer does not hide the one that can; when
    // none answers, that is a failure rather than "no quota".
    let stale = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stale_origin = format!("http://{}", stale.local_addr().unwrap());
    let closing = tokio::spawn(async move {
        while let Ok((socket, _)) = stale.accept().await {
            drop(socket);
        }
    });
    let api = Api::from_endpoints(
        super::super::loopback_client().unwrap(),
        vec![
            (stale_origin.clone(), "fixture".into()),
            (origin, "fixture".into()),
        ],
    );
    assert_eq!(
        api.quota_for("fixture@example.invalid")
            .await
            .unwrap()
            .unwrap(),
        parse(&summary())
    );
    let api = Api::from_single(
        super::super::loopback_client().unwrap(),
        stale_origin,
        "fixture".into(),
    );
    assert!(api.quota_for("fixture@example.invalid").await.is_err());
    closing.abort();
    server.await.unwrap();
}
