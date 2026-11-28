// 覆盖即时速率：衰减与窗口、按耗时摊开、各客户端与合计、Antigravity 账号与轨道、未定价、返回字段。
use super::super::{model::*, store::Store};
use super::{level, Rate, Speed, FULL};
use serde_json::json;
use std::collections::BTreeMap;

const SECOND: i64 = 1_000;
const BASE: i64 = 20_000 * 86_400_000;
const ACCOUNT: &str = "00000000-0000-4000-8000-00000000000a";

/// One request of `client` that ended at `at` having taken `took`, of the
/// model `model` at `cost`; unpriced where `cost` is none.
fn request(
    client: &str,
    id: &str,
    at: i64,
    took: u64,
    model: Option<&str>,
    cost: Option<&str>,
) -> Record {
    Record {
        id: id.into(),
        client: client.into(),
        source: "proxy".into(),
        started_at: at,
        completed: true,
        attempts: vec![Attempt {
            id: format!("{id}-attempt"),
            provider: Some(ACCOUNT.into()),
            requested_model: model.map(str::to_owned),
            pricing_model: model.map(str::to_owned),
            response_id: Some(format!("{id}-response")),
            status: Some(200),
            outcome: "success".into(),
            started_at: at,
            duration_ms: took,
            stream: true,
            transport: "http".into(),
            tokens: Tokens {
                input: Some(1_000),
                output: Some(100),
                ..Tokens::default()
            },
            price: cost.map(|cost| PriceSnapshot {
                version: "fixture-price".into(),
                source: "fixture".into(),
                model: "fixture-model".into(),
                multiplier: "1".into(),
                rates: BTreeMap::from([("input".into(), "0.00001".into())]),
                cost: cost.into(),
                basis: None,
            }),
            ..Attempt::default()
        }],
        ..Record::default()
    }
}
fn store(records: &[Record]) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.write_batch(records, None).unwrap();
    (dir, store)
}
fn per_hour(rate: &Rate) -> f64 {
    rate.cost_per_hour.parse().unwrap()
}
fn of<'a>(speed: &'a Speed, client: &str) -> &'a Rate {
    &speed
        .clients
        .iter()
        .find(|named| named.id == client)
        .unwrap()
        .rate
}
/// What a cost that ended `age` seconds ago reads as, per dollar of it,
/// worked out here from the definition.
fn kick(age: f64) -> f64 {
    let edge = (-5.0f64).exp();
    3_600.0 * ((-age / 60.0).exp() - edge) / (60.0 * (1.0 - edge) - 300.0 * edge)
}
fn near(read: f64, expected: f64) -> bool {
    (read - expected).abs() <= 0.0001 + expected.abs() * 1e-9
}

#[test]
fn a_request_moves_the_dial_as_it_ends_and_less_the_longer_ago_it_was() {
    let (_dir, store) = store(&[request(
        "codex",
        "one",
        BASE,
        0,
        Some("fixture-model"),
        Some("0.6"),
    )]);
    let at = |seconds: i64| per_hour(&store.speed(BASE + seconds * SECOND).unwrap().total);
    // Sixty cents the moment they are known read as some thirty-seven
    // dollars an hour, and a minute later as about a third of that.
    assert!(near(at(0), 0.6 * kick(0.0)), "{}", at(0));
    assert!((37.0..37.5).contains(&at(0)));
    assert!(near(at(60), 0.6 * kick(60.0)), "{}", at(60));
    assert!((0.35..0.38).contains(&(at(60) / at(0))));
    let read: Vec<f64> = [0, 10, 30, 60, 120, 240, 299].map(at).to_vec();
    assert!(read.windows(2).all(|pair| pair[0] > pair[1]), "{read:?}");
    // The window's last second still reads, and its end no longer.
    assert!(at(299) > 0.0);
    let ended = store.speed(BASE + 300 * SECOND).unwrap();
    assert_eq!(
        ended.total,
        Rate {
            cost_per_hour: "0".into(),
            level: 0.0,
            unpriced: false
        }
    );
    // Nor does a request that has not ended by then.
    assert_eq!(at(-1), 0.0);
}

#[test]
fn spending_that_holds_steady_reads_as_itself() {
    // A cent every ten seconds for ten minutes is $3.60 an hour.
    let records: Vec<Record> = (0..60)
        .map(|n| {
            request(
                "claude",
                &format!("steady-{n}"),
                BASE - 5 * SECOND - n * 10 * SECOND,
                0,
                Some("fixture-model"),
                Some("0.01"),
            )
        })
        .collect();
    let (_dir, store) = store(&records);
    let read = per_hour(&store.speed(BASE).unwrap().total);
    assert!((3.57..3.63).contains(&read), "{read}");
}

#[test]
fn a_long_answer_is_weighed_over_the_time_it_took() {
    let (_dir, store) = store(&[
        request(
            "codex",
            "at-once",
            BASE,
            0,
            Some("fixture-model"),
            Some("1"),
        ),
        request(
            "claude",
            "two-minutes",
            BASE,
            120_000,
            Some("fixture-model"),
            Some("1"),
        ),
    ]);
    let speed = store.speed(BASE).unwrap();
    let (at_once, long) = (
        per_hour(of(&speed, "codex")),
        per_hour(of(&speed, "claude")),
    );
    assert!(near(at_once, kick(0.0)), "{at_once}");
    // The mean of what each second of the two minutes reads as now.
    let edge = (-5.0f64).exp();
    let gathered = |age: f64| {
        (60.0 * (1.0 - (-age / 60.0).exp()) - age * edge) / (60.0 * (1.0 - edge) - 300.0 * edge)
    };
    let expected = 3_600.0 * gathered(120.0) / 120.0;
    assert!(near(long, expected), "{long} {expected}");
    assert!(long < at_once * 0.5);
    // An answer longer than the window is not shown for more than was spent.
    let (_dir, store) = store_of_one(request(
        "codex",
        "ten-minutes",
        BASE,
        600_000,
        Some("fixture-model"),
        Some("1"),
    ));
    let read = per_hour(&store.speed(BASE).unwrap().total);
    assert!(near(read, 3_600.0 / 600.0), "{read}");
}
fn store_of_one(record: Record) -> (tempfile::TempDir, Store) {
    store(&[record])
}

#[test]
fn each_client_reads_for_itself_and_the_total_is_all_of_them() {
    let (_dir, store) = store(&[
        request(
            "codex",
            "codex-1",
            BASE,
            0,
            Some("fixture-model"),
            Some("0.1"),
        ),
        request(
            "codex",
            "codex-2",
            BASE - 60 * SECOND,
            0,
            Some("fixture-model"),
            Some("0.1"),
        ),
        request(
            "grok",
            "grok-1",
            BASE,
            0,
            Some("fixture-model"),
            Some("0.3"),
        ),
        request(
            "dsh",
            "dsh-1",
            BASE - 400 * SECOND,
            0,
            Some("fixture-model"),
            Some("9"),
        ),
        // A client the dial has no place for is still spending.
        request(
            "other",
            "other-1",
            BASE,
            0,
            Some("fixture-model"),
            Some("0.2"),
        ),
    ]);
    let speed = store.speed(BASE).unwrap();
    assert_eq!(speed.at, BASE);
    assert_eq!(
        speed
            .clients
            .iter()
            .map(|named| named.id.as_str())
            .collect::<Vec<_>>(),
        ["codex", "claude", "antigravity", "grok", "dsh"]
    );
    assert!(near(
        per_hour(of(&speed, "codex")),
        0.1 * (kick(0.0) + kick(60.0))
    ));
    assert!(near(per_hour(of(&speed, "grok")), 0.3 * kick(0.0)));
    for idle in ["claude", "antigravity", "dsh"] {
        assert_eq!(of(&speed, idle).cost_per_hour, "0", "{idle}");
        assert_eq!(of(&speed, idle).level, 0.0, "{idle}");
    }
    assert!(near(
        per_hour(&speed.total),
        0.6 * kick(0.0) + 0.1 * kick(60.0)
    ));
    assert!(speed.accounts.is_empty());
}

#[test]
fn antigravity_reads_by_account_and_quota_track() {
    let named_only = Record {
        attempts: vec![Attempt {
            requested_model: None,
            pricing_model: None,
            model_display_name: Some("Gemini 3 Pro".into()),
            ..request("antigravity", "shown", BASE, 0, None, Some("0.4")).attempts[0].clone()
        }],
        ..request("antigravity", "shown", BASE, 0, None, Some("0.4"))
    };
    let nobody = Record {
        attempts: vec![Attempt {
            provider: None,
            ..request(
                "antigravity",
                "nobody",
                BASE,
                0,
                Some("gemini-fixture"),
                Some("5"),
            )
            .attempts[0]
                .clone()
        }],
        ..request(
            "antigravity",
            "nobody",
            BASE,
            0,
            Some("gemini-fixture"),
            Some("5"),
        )
    };
    let (_dir, store) = store(&[
        request(
            "antigravity",
            "gemini",
            BASE,
            0,
            Some("gemini-fixture"),
            Some("0.1"),
        ),
        request(
            "antigravity",
            "claude",
            BASE,
            0,
            Some("claude-fixture"),
            Some("0.2"),
        ),
        // No track is known for it: the client spends it, no track does.
        request(
            "antigravity",
            "mystery",
            BASE,
            0,
            Some("fixture-model"),
            Some("0.3"),
        ),
        named_only,
        // Nobody was signed in that Vela knows of.
        nobody,
        // Another client's request of the same model is not an account's.
        request(
            "claude",
            "elsewhere",
            BASE,
            0,
            Some("claude-fixture"),
            Some("7"),
        ),
    ]);
    let speed = store.speed(BASE).unwrap();
    assert!(near(per_hour(of(&speed, "antigravity")), 6.0 * kick(0.0)));
    assert_eq!(speed.accounts.len(), 1);
    assert_eq!(speed.accounts[0].id, ACCOUNT);
    let tracks: BTreeMap<&str, f64> = speed.accounts[0]
        .tracks
        .iter()
        .map(|named| (named.id.as_str(), per_hour(&named.rate)))
        .collect();
    assert_eq!(
        tracks.keys().copied().collect::<Vec<_>>(),
        ["claude_gpt", "gemini"]
    );
    assert!(near(tracks["claude_gpt"], 0.2 * kick(0.0)));
    assert!(near(tracks["gemini"], 0.5 * kick(0.0)));
}

#[test]
fn tokens_nothing_could_price_are_said_to_be_unpriced() {
    let failed = Record {
        completed: false,
        attempts: vec![Attempt {
            status: Some(429),
            outcome: "rejected".into(),
            tokens: Tokens::default(),
            ..request("grok", "failed", BASE, 0, Some("fixture-model"), None).attempts[0].clone()
        }],
        ..request("grok", "failed", BASE, 0, Some("fixture-model"), None)
    };
    let (_dir, store) = store(&[
        request("codex", "unpriced", BASE, 0, Some("fixture-model"), None),
        // Beside a priced request the reading is of what could be priced.
        request(
            "claude",
            "priced",
            BASE,
            0,
            Some("fixture-model"),
            Some("0.1"),
        ),
        request("claude", "unpriced-2", BASE, 0, Some("fixture-model"), None),
        // A request that used nothing has nothing left unpriced.
        failed,
    ]);
    let speed = store.speed(BASE).unwrap();
    assert_eq!(
        *of(&speed, "codex"),
        Rate {
            cost_per_hour: "0".into(),
            level: 0.0,
            unpriced: true
        }
    );
    assert!(!of(&speed, "claude").unpriced);
    assert!(per_hour(of(&speed, "claude")) > 0.0);
    assert!(!of(&speed, "grok").unpriced);
    assert_eq!(of(&speed, "grok").cost_per_hour, "0");
    assert!(!speed.total.unpriced);
}

#[test]
fn the_dial_is_even_for_cents_logarithmic_above_and_ends_at_its_last_reading() {
    assert_eq!(level(0.0), 0.0);
    assert_eq!(level(FULL), 1.0);
    assert_eq!(level(FULL * 10.0), 1.0);
    let read: Vec<f64> = [0.05, 0.25, 1.0, 5.0, 20.0, 100.0, 199.0]
        .map(level)
        .to_vec();
    assert!(read.windows(2).all(|pair| pair[0] < pair[1]), "{read:?}");
    // A dollar an hour stands near a quarter of the way, twenty at two thirds.
    assert!((0.23..0.25).contains(&level(1.0)));
    assert!((0.64..0.67).contains(&level(20.0)));
    let (_dir, store) = store(&[]);
    let speed = store.speed(BASE).unwrap();
    assert_eq!(
        speed
            .dial
            .iter()
            .map(|mark| (mark.label.as_str(), mark.level))
            .collect::<Vec<_>>(),
        [
            ("1", level(1.0)),
            ("5", level(5.0)),
            ("20", level(20.0)),
            ("100", level(100.0))
        ]
    );
}

#[test]
fn the_answer_is_written_as_the_pages_read_it() {
    let (_dir, store) = store(&[request(
        "antigravity",
        "gemini",
        BASE,
        0,
        Some("gemini-fixture"),
        Some("0.1"),
    )]);
    let speed = store.speed(BASE).unwrap();
    let written = serde_json::to_value(&speed).unwrap();
    let rate = json!({
        "costPerHour": speed.total.cost_per_hour,
        "level": speed.total.level,
        "unpriced": false,
    });
    let idle = json!({"costPerHour": "0", "level": 0.0, "unpriced": false});
    let with = |id: &str, rate: &serde_json::Value| {
        let mut named = rate.clone();
        named["id"] = json!(id);
        named
    };
    let mut expected = rate.clone();
    expected["at"] = json!(BASE);
    expected["dial"] = json!(speed
        .dial
        .iter()
        .map(|mark| json!({"label": mark.label, "level": mark.level}))
        .collect::<Vec<_>>());
    expected["clients"] = json!([
        with("codex", &idle),
        with("claude", &idle),
        with("antigravity", &rate),
        with("grok", &idle),
        with("dsh", &idle),
    ]);
    expected["accounts"] = json!([{"id": ACCOUNT, "tracks": [with("gemini", &rate)]}]);
    assert_eq!(written, expected);
    // What another instance answers is read back as it was.
    assert_eq!(serde_json::from_value::<Speed>(written).unwrap(), speed);
}
