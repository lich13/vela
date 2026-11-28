// 覆盖供应商曲线：分桶、失败分类、首字分位数、日志覆盖范围、同域合并、不经网关的序列与无数据。
use super::super::{commands::get_usage_curves, model::*, query, store::Store};
use super::{kind, spread, step, Bucket, Curves, Failures, FirstToken, Grid, Provider};
use crate::{
    events::{FailureKind, FailureKind::*, UpstreamFailure},
    gateway::{form, ClientId},
};
use std::collections::BTreeMap;

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
const BASE: i64 = 20_000 * DAY;
const A: &str = "00000000-0000-4000-8000-00000000000a";
const B: &str = "00000000-0000-4000-8000-00000000000b";
const C: &str = "00000000-0000-4000-8000-00000000000c";
const D: &str = "00000000-0000-4000-8000-00000000000d";
/// Two accounts Antigravity is signed in to, which no gateway serves.
const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000a1";
const OTHER: &str = "00000000-0000-4000-8000-0000000000a2";
const RELAY: &str = "relay.example.invalid";
/// A subdomain of `RELAY`, which makes it another host.
const REGIONAL: &str = "eu.relay.example.invalid";
/// No provider is said to be on any host.
static NOWHERE: BTreeMap<String, String> = BTreeMap::new();

fn request(
    client: &str,
    id: &str,
    provider: &str,
    at: i64,
    status: Option<u16>,
    first_token_ms: Option<u64>,
) -> Record {
    let succeeded = status.is_some_and(|status| (200..300).contains(&status));
    Record {
        id: id.into(),
        client: client.into(),
        source: "proxy".into(),
        started_at: at,
        completed: succeeded,
        attempts: vec![Attempt {
            id: format!("{id}-attempt"),
            provider: Some(provider.into()),
            requested_model: Some("fixture-model".into()),
            response_id: Some(format!("{id}-response")),
            status,
            outcome: if succeeded { "success" } else { "failure" }.into(),
            started_at: at,
            duration_ms: 1_000,
            first_token_ms,
            stream: true,
            transport: "http".into(),
            ..Attempt::default()
        }],
        ..Record::default()
    }
}
/// An attempt of a request that reached no gateway: the account that made it,
/// where it names one, how it ended and the error it gave.
fn tried(account: Option<&str>, outcome: &str, error_code: Option<&str>) -> Attempt {
    Attempt {
        provider: account.map(Into::into),
        outcome: outcome.into(),
        error_code: error_code.map(Into::into),
        ..Attempt::default()
    }
}
/// A request of `client` that reached no gateway, as its app or its session
/// files tell of it, with its attempts in order. It completed where the last
/// of them succeeded. All of a client's are of one session, so that an error
/// Claude Code wrote down is told to have reached no gateway by the request
/// before it, whatever a gateway recorded at that time.
fn told(client: &str, id: &str, at: i64, attempts: Vec<Attempt>) -> Record {
    Record {
        id: id.into(),
        client: client.into(),
        source: client.into(),
        session_id: Some(format!("{client}-session")),
        started_at: at,
        completed: attempts.last().is_some_and(|a| a.outcome == "success"),
        attempts: attempts
            .into_iter()
            .enumerate()
            .map(|(index, attempt)| Attempt {
                id: format!("{id}-{index}"),
                started_at: at,
                ..attempt
            })
            .collect(),
        ..Record::default()
    }
}
/// A journal row of `count` failures from `first` to `last`, in milliseconds.
fn failed(provider: &str, kind: FailureKind, first: i64, last: i64, count: u32) -> UpstreamFailure {
    UpstreamFailure {
        client: ClientId::Claude,
        provider: provider.into(),
        first_at: (first / 1000) as u64,
        last_at: (last / 1000) as u64,
        count,
        kind,
    }
}
fn range() -> Filter {
    Filter {
        start: Some(BASE),
        // Twenty buckets of five minutes.
        end: Some(BASE + 100 * MINUTE - 1),
        client: Some("claude".into()),
        ..Filter::default()
    }
}
/// `range`, of every client.
fn every_client() -> Filter {
    Filter {
        client: None,
        ..range()
    }
}
/// The provider `id` as `curves` has it.
fn of<'a>(curves: &'a Curves, id: &str) -> &'a Provider {
    curves.providers.iter().find(|p| p.id == id).unwrap()
}
/// The providers on `host` as `curves` has them taken as one.
fn group<'a>(curves: &'a Curves, host: &str) -> &'a Provider {
    let id = format!("host:{host}");
    curves.groups.iter().find(|g| g.id == id).unwrap()
}
/// What reached no gateway under `id`, as `curves` has it.
fn direct<'a>(curves: &'a Curves, id: &str) -> &'a Provider {
    curves.direct.iter().find(|p| p.id == id).unwrap()
}
/// Which host each of `providers` is on.
fn on(providers: &[(&str, &str)]) -> BTreeMap<String, String> {
    providers
        .iter()
        .map(|(id, host)| ((*id).into(), (*host).into()))
        .collect()
}
fn store() -> (tempfile::TempDir, Store) {
    store_of("claude")
}
fn store_of(client: &str) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let request = |id, provider, minutes, status, first_token_ms| {
        request(
            client,
            id,
            provider,
            BASE + minutes * MINUTE,
            status,
            first_token_ms,
        )
    };
    store
        .write_batch(
            &[
                request("a-1", A, 1, Some(200), Some(100)),
                request("a-2", A, 2, Some(200), Some(300)),
                request("a-3", A, 3, Some(200), Some(200)),
                // Its last attempt was rejected, which the journal also says.
                request("a-4", A, 31, Some(429), None),
                // Nothing was recorded of how this one ended.
                request("a-5", A, 61, None, None),
                request("b-1", B, 10, Some(200), None),
            ],
            None,
        )
        .unwrap();
    (dir, store)
}
/// A store of nothing but `records`.
fn holding(records: &[Record]) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    store.write_batch(records, None).unwrap();
    (dir, store)
}
/// `store`, and beside its requests what reached no gateway: an account of
/// Antigravity answered in the first five minutes and, half an hour on, was
/// limited once before it did; a session of Claude Code ended two messages in
/// the first five minutes and wrote down an error an hour on.
fn store_and_direct() -> (tempfile::TempDir, Store) {
    let (dir, mut store) = store();
    let at = |minutes: i64| BASE + minutes * MINUTE;
    let ok = |account| tried(account, "success", None);
    let limited = tried(Some(ACCOUNT), "failed", Some("429"));
    let overloaded = Attempt {
        status: Some(529),
        ..tried(None, "failed", None)
    };
    store
        .write_batch(
            &[
                told("antigravity", "g-1", at(1), vec![ok(Some(ACCOUNT))]),
                told(
                    "antigravity",
                    "g-2",
                    at(31),
                    vec![limited, ok(Some(ACCOUNT))],
                ),
                told("claude", "s-1", at(2), vec![ok(None)]),
                told("claude", "s-2", at(3), vec![ok(None)]),
                told("claude", "s-3", at(61), vec![overloaded]),
            ],
            None,
        )
        .unwrap();
    (dir, store)
}
fn journal() -> Vec<UpstreamFailure> {
    vec![
        // Six rejections four minutes apart in all, across two buckets.
        failed(A, Limited, BASE + 29 * MINUTE, BASE + 33 * MINUTE, 6),
        failed(A, Server, BASE + 31 * MINUTE, BASE + 31 * MINUTE, 1),
        failed(A, Broken, BASE + 62 * MINUTE, BASE + 62 * MINUTE, 2),
        // An endless provider: rejected over and over, never a usage record.
        failed(C, Limited, BASE + 40 * MINUTE, BASE + 40 * MINUTE, 9),
        UpstreamFailure {
            client: ClientId::Codex,
            ..failed(B, Server, BASE + 10 * MINUTE, BASE + 10 * MINUTE, 4)
        },
    ]
}
fn none() -> FirstToken {
    FirstToken::default()
}
/// Four providers whose times to the first token tell a percentile of all
/// of them from an average of each one's percentiles. A's are 100 and 300 in
/// the first five minutes and 200 half an hour on; B's are 400, 600, 800 and
/// 1000, and then 500, 700 and 900. C and D each answered once.
fn shared_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let ok = |provider: &str, minutes: i64, first_token_ms: u64| {
        let id = format!("{provider}-{minutes}");
        let at = BASE + minutes * MINUTE;
        request("claude", &id, provider, at, Some(200), Some(first_token_ms))
    };
    store
        .write_batch(
            &[
                ok(A, 1, 100),
                ok(A, 2, 300),
                ok(A, 31, 200),
                ok(B, 1, 400),
                ok(B, 2, 600),
                ok(B, 3, 800),
                ok(B, 4, 1000),
                ok(B, 31, 500),
                ok(B, 32, 700),
                ok(B, 33, 900),
                ok(C, 10, 50),
                ok(D, 10, 60),
            ],
            None,
        )
        .unwrap();
    (dir, store)
}
/// What the journal says failed of the providers of `shared_store`. By it
/// they are listed B, C, D, A.
fn shared_journal() -> Vec<UpstreamFailure> {
    let failed = |provider, kind, minutes: i64, count| {
        let at = BASE + minutes * MINUTE;
        failed(provider, kind, at, at, count)
    };
    vec![
        failed(B, Limited, 3, 6),
        failed(A, Rejected, 32, 1),
        failed(B, Server, 32, 2),
        // In five minutes in which nothing was answered.
        failed(B, Broken, 50, 1),
        failed(C, Limited, 10, 1),
        failed(D, Server, 10, 3),
    ]
}

#[test]
fn a_range_is_divided_into_at_most_sixty_buckets() {
    let of = |span: i64| step(Some(BASE), BASE + span);
    assert_eq!(of(0), 5 * MINUTE);
    // Five hours less a moment end in the sixtieth bucket of five minutes,
    // and the moment after in a sixty-first.
    assert_eq!(of(300 * MINUTE - 1), 5 * MINUTE);
    assert_eq!(of(300 * MINUTE), 10 * MINUTE);
    assert_eq!(of(DAY), 30 * MINUTE);
    assert_eq!(of(7 * DAY), 180 * MINUTE);
    assert_eq!(of(14 * DAY), 360 * MINUTE);
    assert_eq!(of(29 * DAY), 720 * MINUTE);
    // Thirty days end in a sixty-first bucket of twelve hours. From there a
    // bucket stays a day, as does one of a range that has no beginning to
    // measure from.
    assert_eq!(of(30 * DAY), DAY);
    assert_eq!(of(400 * DAY), DAY);
    assert_eq!(step(None, BASE), DAY);
}

#[test]
fn a_range_that_begins_within_a_bucket_is_still_within_sixty() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    // The last five hours, day, week and thirty days as a page asks for
    // them: from a moment that is the edge of no bucket. Divided by their
    // length alone, the first and the last would reach into a sixty-first.
    let start = BASE + 7 * MINUTE + 13_000;
    for (span, step) in [
        (300 * MINUTE, 10 * MINUTE),
        (DAY, 30 * MINUTE),
        (7 * DAY, 180 * MINUTE),
        (30 * DAY, DAY),
    ] {
        let filter = Filter {
            start: Some(start),
            end: Some(start + span),
            ..Filter::default()
        };
        let curves = store
            .curves(&filter, 0, &[], &NOWHERE, start + span)
            .unwrap();
        assert_eq!(curves.step, step);
        // A day may be an hour longer or shorter than the step.
        let drawn = (curves.end - curves.start + step / 2) / step;
        assert!(drawn <= 60, "{drawn} buckets of {step}");
        if step < DAY {
            // One more than its length fills.
            assert_eq!(drawn, span / step + 1);
        }
    }
}

#[test]
fn buckets_shorter_than_a_day_keep_one_offset_where_the_clocks_change() {
    const HOUR: i64 = 60 * MINUTE;
    // New York on 3 November 2024, at 06:00 UTC: 01:30 came twice an hour
    // apart, four hours behind UTC and then five. Each moment by its own
    // offset, the second would begin a bucket an hour into the first one's.
    let change = 1_730_613_600_000;
    let grid = Grid {
        step: 3 * HOUR,
        offset: -4 * HOUR,
    };
    let bucket = change - 2 * HOUR;
    assert_eq!(grid.bucket(change - HOUR / 2), bucket);
    assert_eq!(grid.bucket(change + HOUR / 2), bucket);
    // One ends where the next begins.
    assert_eq!(grid.end(bucket), change + HOUR);
    assert_eq!(grid.bucket(change + HOUR), change + HOUR);
}

#[test]
fn percentiles_are_values_that_were_measured() {
    let of = |values: &[u64]| FirstToken::of(&mut values.to_vec());
    assert_eq!(of(&[]), none());
    assert_eq!(
        of(&[70]),
        FirstToken {
            samples: 1,
            p50: Some(70),
            p90: Some(70)
        }
    );
    // The lower of the middle two, never a value between them.
    assert_eq!(
        of(&[400, 100]),
        FirstToken {
            samples: 2,
            p50: Some(100),
            p90: Some(400)
        }
    );
    assert_eq!(
        of(&[10, 9, 8, 7, 6, 5, 4, 3, 2, 1]),
        FirstToken {
            samples: 10,
            p50: Some(5),
            p90: Some(9)
        }
    );
}

#[test]
fn failures_are_counted_by_how_the_journal_says_they_ended() {
    let mut failures = Failures::default();
    for (kind, count) in [
        (Limited, 1),
        (Rejected, 2),
        (Rejected, 4),
        (Server, 8),
        (Server, 16),
        (Broken, 32),
        (Broken, 64),
    ] {
        failures.add(kind, count);
    }
    assert_eq!(
        failures,
        Failures {
            limited: 1,
            rejected: 6,
            server: 24,
            broken: 96
        }
    );
}

#[test]
fn a_merged_row_is_spread_over_the_buckets_it_reaches() {
    let shares = |row: &UpstreamFailure, start: i64, end: i64| {
        let mut shares = Vec::new();
        let grid = Grid::new(5 * MINUTE, BASE);
        spread(row, start, end, grid, |bucket, count| {
            shares.push((bucket - BASE, count))
        });
        shares
    };
    // At 29:00, 29:48, 30:36, 31:24, 32:12 and 33:00.
    let row = failed(A, Limited, BASE + 29 * MINUTE, BASE + 33 * MINUTE, 6);
    assert_eq!(
        shares(&row, i64::MIN, i64::MAX),
        [(25 * MINUTE, 2), (30 * MINUTE, 4)]
    );
    // Only what fell in the range asked for.
    assert_eq!(
        shares(&row, BASE + 30 * MINUTE, i64::MAX),
        [(30 * MINUTE, 4)]
    );
    assert_eq!(
        shares(&row, BASE + 29 * MINUTE + 1, BASE + 31 * MINUTE),
        [(25 * MINUTE, 1), (30 * MINUTE, 1)]
    );
    assert_eq!(shares(&row, BASE + 34 * MINUTE, i64::MAX), []);
    // One second holds them all.
    let burst = failed(A, Limited, BASE + 29 * MINUTE, BASE + 29 * MINUTE, 7);
    assert_eq!(shares(&burst, i64::MIN, i64::MAX), [(25 * MINUTE, 7)]);
    assert_eq!(shares(&burst, BASE + 29 * MINUTE + 1, i64::MAX), []);
}

#[test]
fn a_provider_s_curves_join_what_succeeded_with_what_the_journal_says_failed() {
    let (_dir, store) = store();
    let curves = store
        .curves(&range(), 0, &journal(), &NOWHERE, BASE + DAY)
        .unwrap();
    assert_eq!(
        (curves.start, curves.end, curves.step),
        (BASE, BASE + 100 * MINUTE, 5 * MINUTE)
    );
    let provider = of(&curves, A);
    assert_eq!(
        provider.first_token,
        FirstToken {
            samples: 3,
            p50: Some(200),
            p90: Some(300)
        }
    );
    assert_eq!(
        provider.buckets,
        [
            Bucket {
                at: BASE,
                ok: 3,
                failures: Some(Failures::default()),
                first_token: FirstToken {
                    samples: 3,
                    p50: Some(200),
                    p90: Some(300)
                },
            },
            Bucket {
                at: BASE + 25 * MINUTE,
                ok: 0,
                failures: Some(Failures {
                    limited: 2,
                    ..Failures::default()
                }),
                first_token: none(),
            },
            // The rejected request is one of the journal's failures and is
            // not counted a second time from its usage record.
            Bucket {
                at: BASE + 30 * MINUTE,
                ok: 0,
                failures: Some(Failures {
                    limited: 4,
                    server: 1,
                    ..Failures::default()
                }),
                first_token: none(),
            },
            // The request nothing is known of is neither a success nor a failure.
            Bucket {
                at: BASE + 60 * MINUTE,
                ok: 0,
                failures: Some(Failures {
                    broken: 2,
                    ..Failures::default()
                }),
                first_token: none(),
            },
        ]
    );
}

#[test]
fn providers_are_listed_most_rate_limited_first_whichever_the_filter_names() {
    let (_dir, store) = store();
    // Naming one narrows nothing: all of them are there to choose among.
    let named = Filter {
        provider: Some(B.into()),
        ..range()
    };
    let curves = store
        .curves(&named, 0, &journal(), &NOWHERE, BASE + DAY)
        .unwrap();
    let summary: Vec<_> = curves
        .providers
        .iter()
        .map(|p| (p.id.as_str(), p.requests, p.ok, p.failures))
        .collect();
    assert_eq!(
        summary,
        [
            // Known only to the journal, and still the first to look at.
            (
                C,
                0,
                0,
                Failures {
                    limited: 9,
                    ..Failures::default()
                }
            ),
            (
                A,
                5,
                3,
                Failures {
                    limited: 6,
                    server: 1,
                    broken: 2,
                    ..Failures::default()
                }
            ),
            // Another client's failures on it are not this client's.
            (B, 1, 1, Failures::default()),
        ]
    );
    let trend = &curves.providers[1].trend;
    assert_eq!(trend.len(), 20);
    assert_eq!(
        (trend[0], trend[1], trend[5], trend[6], trend[12]),
        (Some(0.0), None, Some(1.0), Some(1.0), Some(1.0))
    );
    // Every client's, where none is asked for.
    let every = Filter {
        client: None,
        ..range()
    };
    let curves = store
        .curves(&every, 0, &journal(), &NOWHERE, BASE + DAY)
        .unwrap();
    let other = curves.providers.iter().find(|p| p.id == B).unwrap();
    assert_eq!(other.failures.server, 4);
    assert_eq!(other.trend[2], Some(0.8));
}

#[test]
fn every_provider_comes_with_its_own_buckets() {
    let (_dir, store) = store();
    let curves = store
        .curves(&range(), 0, &journal(), &NOWHERE, BASE + DAY)
        .unwrap();
    // One the usage records know nothing of is drawn from the journal alone.
    assert_eq!(
        of(&curves, C).buckets,
        [Bucket {
            at: BASE + 40 * MINUTE,
            ok: 0,
            failures: Some(Failures {
                limited: 9,
                ..Failures::default()
            }),
            first_token: none(),
        }]
    );
    // One that answered and never failed has a rate of nothing, known.
    let quiet = of(&curves, B);
    assert_eq!(
        quiet.buckets,
        [Bucket {
            at: BASE + 10 * MINUTE,
            ok: 1,
            failures: Some(Failures::default()),
            first_token: none(),
        }]
    );
    assert_eq!(quiet.first_token, none());
}

#[test]
fn the_answer_names_its_parts_as_the_page_reads_them() {
    let (_dir, mut store) = store();
    // A session of Claude Code tells of one request no gateway saw.
    let session = told(
        "claude",
        "session",
        BASE + MINUTE,
        vec![tried(None, "success", None)],
    );
    store.write_batch(&[session], None).unwrap();
    // C and A are on one host, and B no longer exists.
    let hosts = on(&[(A, RELAY), (C, RELAY)]);
    let curves = store
        .curves(&range(), 0, &journal(), &hosts, BASE + DAY)
        .unwrap();
    let json = serde_json::to_value(&curves).unwrap();
    let names = |value: &serde_json::Value| {
        let mut names: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        names.sort();
        names
    };
    assert_eq!(
        names(&json),
        [
            "direct",
            "end",
            "failuresSince",
            "groups",
            "journalError",
            "providers",
            "start",
            "step"
        ]
    );
    let provider = &json["providers"][1];
    assert_eq!(
        names(provider),
        [
            "buckets",
            "failures",
            "firstToken",
            "host",
            "id",
            "ok",
            "requests",
            "summedUntil",
            "trend"
        ]
    );
    assert_eq!(provider["host"], RELAY);
    // One that no longer exists says nothing of a host, and none of them
    // anything of members: a page that knows of neither reads what it did.
    let gone = &json["providers"][2];
    assert_eq!(gone["id"], B);
    assert_eq!(
        names(gone),
        [
            "buckets",
            "failures",
            "firstToken",
            "id",
            "ok",
            "requests",
            "summedUntil",
            "trend"
        ]
    );
    let group = &json["groups"][0];
    assert_eq!(
        names(group),
        [
            "buckets",
            "failures",
            "firstToken",
            "host",
            "id",
            "members",
            "ok",
            "requests",
            "summedUntil",
            "trend"
        ]
    );
    assert_eq!(group["id"], "host:relay.example.invalid");
    assert_eq!(group["host"], RELAY);
    assert_eq!(group["members"], serde_json::json!([C, A]));
    // What reached no gateway says that the records tell what failed of it,
    // which no row of a gateway's does, and nothing of a host or of members.
    let direct = &json["direct"][0];
    assert_eq!(
        names(direct),
        [
            "buckets",
            "failures",
            "firstToken",
            "id",
            "ok",
            "recorded",
            "requests",
            "summedUntil",
            "trend"
        ]
    );
    assert_eq!(direct["id"], "session:claude");
    assert_eq!(direct["recorded"], true);
    assert!(direct["summedUntil"].is_null());
    assert!(direct["buckets"][0]["failures"].is_object());
    for row in [provider, group, direct] {
        assert_eq!(
            names(&row["buckets"][0]),
            ["at", "failures", "firstToken", "ok"]
        );
        assert_eq!(
            names(&row["failures"]),
            ["broken", "limited", "rejected", "server"]
        );
        assert_eq!(names(&row["firstToken"]), ["p50", "p90", "samples"]);
    }
    // The journal has said nothing went wrong, and every request is kept
    // one by one.
    assert!(json["journalError"].is_null());
    assert!(provider["summedUntil"].is_null());
    // Where no two providers are on one host the list is there, and empty.
    let apart = store
        .curves(&range(), 0, &journal(), &NOWHERE, BASE + DAY)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&apart).unwrap()["groups"],
        serde_json::json!([])
    );
    // So is that of what reached no gateway, where nothing did.
    let (_dir, store) = self::store();
    let through = store
        .curves(&range(), 0, &journal(), &NOWHERE, BASE + DAY)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&through).unwrap()["direct"],
        serde_json::json!([])
    );
}

#[test]
fn nothing_is_said_of_failures_before_the_journal_reaches() {
    let (_dir, store) = store();
    let since = BASE + 30 * MINUTE;
    let curves = store
        .curves(&range(), since, &journal(), &NOWHERE, BASE + DAY)
        .unwrap();
    assert_eq!(curves.failures_since, since);
    let failures: Vec<_> = of(&curves, A)
        .buckets
        .iter()
        .map(|bucket| {
            (
                bucket.at - BASE,
                bucket.ok,
                bucket.failures.map(|f| f.total()),
            )
        })
        .collect();
    // Three requests succeeded in the first bucket; whether others failed
    // there is not known, so it has no error rate rather than one of zero.
    // Of the six rejections that began a minute before the journal does,
    // the two of that minute are not counted, and nothing else ended there.
    assert_eq!(
        failures,
        [
            (0, 3, None),
            (30 * MINUTE, 0, Some(5)),
            (60 * MINUTE, 0, Some(2)),
        ]
    );
    let provider = of(&curves, A);
    // A rate compares successes and failures of the same time.
    assert_eq!((provider.requests, provider.ok), (5, 0));
    assert_eq!(provider.failures.total(), 7);
    assert_eq!(provider.trend[0], None);
}

#[test]
fn a_journal_that_begins_after_the_range_knows_none_of_it() {
    let (_dir, store) = store();
    // The range stops two minutes into a bucket, and the journal begins a
    // minute later in the same one.
    let filter = Filter {
        end: Some(BASE + 32 * MINUTE - 1),
        ..range()
    };
    let curves = store
        .curves(
            &filter,
            BASE + 33 * MINUTE,
            &journal(),
            &NOWHERE,
            BASE + DAY,
        )
        .unwrap();
    assert_eq!(curves.end, BASE + 35 * MINUTE);
    // Nothing drawn is of a time the journal has, and it says so: it is
    // not left to begin before the last bucket ends.
    assert_eq!(curves.failures_since, curves.end);
    for provider in &curves.providers {
        assert_eq!((provider.ok, provider.failures.total()), (0, 0));
        assert!(provider.buckets.iter().all(|b| b.failures.is_none()));
    }
    assert_eq!(of(&curves, A).requests, 4);
}

#[test]
fn the_bucket_the_journal_begins_within_counts_from_where_it_begins() {
    let (_dir, store) = store();
    // After the first of the three requests of the first five minutes.
    let since = BASE + 2 * MINUTE;
    let curves = store
        .curves(&range(), since, &journal(), &NOWHERE, BASE + DAY)
        .unwrap();
    let provider = of(&curves, A);
    let first = &provider.buckets[0];
    assert_eq!((first.ok, first.failures), (2, Some(Failures::default())));
    // What it says of the first token is of all of it.
    assert_eq!(first.first_token.samples, 3);
    assert_eq!((provider.requests, provider.ok), (5, 2));
    assert_eq!(provider.trend[0], Some(0.0));
}

#[test]
fn a_day_the_journal_begins_within_still_counts_what_failed_after() {
    const HOUR: i64 = 60 * MINUTE;
    let day = query::bucket(BASE, DAY);
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let ok = |id, hours| request("claude", id, A, day + hours * HOUR, Some(200), Some(100));
    store
        .write_batch(&[ok("m-1", 9), ok("m-2", 10), ok("e-1", 15)], None)
        .unwrap();
    // The journal was cleared at noon. It has the rejections of the
    // afternoon, and a row of five a minute apart that began two minutes
    // before: three of them came since.
    let since = day + 12 * HOUR;
    let journal = [
        failed(A, Limited, since - 2 * MINUTE, since + 2 * MINUTE, 5),
        failed(A, Limited, day + 14 * HOUR, day + 14 * HOUR, 4),
    ];
    let filter = Filter {
        start: None,
        end: Some(day + DAY - 1),
        client: Some("claude".into()),
        ..Filter::default()
    };
    let curves = store
        .curves(&filter, since, &journal, &NOWHERE, day + 2 * DAY)
        .unwrap();
    assert_eq!(
        (curves.start, curves.end, curves.step),
        (day, day + DAY, DAY)
    );
    let provider = of(&curves, A);
    // The two successes of the morning are not set against failures the
    // journal no longer has.
    assert_eq!(
        provider.buckets,
        [Bucket {
            at: day,
            ok: 1,
            failures: Some(Failures {
                limited: 7,
                ..Failures::default()
            }),
            first_token: FirstToken {
                samples: 3,
                p50: Some(100),
                p90: Some(100)
            },
        }]
    );
    assert_eq!(
        (provider.requests, provider.ok, provider.failures.limited),
        (3, 1, 7)
    );
    assert_eq!(provider.trend, [Some(0.875)]);
}

#[test]
fn a_range_without_a_beginning_starts_where_anything_is_known() {
    let (_dir, store) = store();
    let filter = Filter {
        start: None,
        ..range()
    };
    let curves = store
        .curves(&filter, 0, &journal(), &NOWHERE, BASE + DAY)
        .unwrap();
    assert_eq!(curves.step, DAY);
    assert_eq!(curves.start, query::bucket(BASE + MINUTE, DAY));
    assert_eq!(
        curves.end,
        query::bucket(BASE + 100 * MINUTE - 1, DAY) + DAY
    );
    let provider = of(&curves, A);
    assert_eq!(provider.buckets.iter().map(|b| b.ok).sum::<u64>(), 3);
    assert_eq!(provider.first_token.samples, 3);
}

#[test]
fn nothing_recorded_is_nothing_to_draw() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let curves = store
        .curves(&range(), 0, &[], &NOWHERE, BASE + DAY)
        .unwrap();
    assert!(curves.providers.is_empty());
    assert_eq!((curves.start, curves.end), (BASE, BASE + 100 * MINUTE));
    let all = store
        .curves(&Filter::default(), 0, &[], &NOWHERE, BASE + DAY)
        .unwrap();
    assert_eq!(all.start + DAY, all.end);
}

#[test]
fn requests_kept_only_as_daily_totals_count_without_a_percentile() {
    // Requests of Claude Code are kept one by one for good.
    let (_dir, mut store) = store_of("codex");
    store.compact(BASE + 40 * DAY).unwrap();
    let filter = Filter {
        start: Some(BASE - 2 * DAY),
        end: Some(BASE + 2 * DAY),
        client: Some("codex".into()),
        ..range()
    };
    let curves = store
        .curves(&filter, 0, &[], &NOWHERE, BASE + 40 * DAY)
        .unwrap();
    let provider = of(&curves, A);
    assert_eq!((provider.requests, provider.ok), (5, 3));
    assert_eq!(provider.buckets.iter().map(|b| b.ok).sum::<u64>(), 3);
    assert_eq!(provider.first_token, none());
}

#[test]
fn each_provider_says_up_to_where_only_totals_are_kept_of_it() {
    let (_dir, mut store) = store_of("codex");
    store.compact(BASE + 40 * DAY).unwrap();
    // One request of A since then, kept as it came, and all there is of C.
    let late = |id, provider, first_token_ms| {
        let at = BASE + 39 * DAY;
        request("codex", id, provider, at, Some(200), Some(first_token_ms))
    };
    store
        .write_batch(&[late("late-a", A, 70), late("late-c", C, 90)], None)
        .unwrap();
    let filter = Filter {
        start: Some(BASE - 2 * DAY),
        end: Some(BASE + 40 * DAY),
        client: Some("codex".into()),
        ..Filter::default()
    };
    let curves = store
        .curves(&filter, 0, &[], &NOWHERE, BASE + 40 * DAY)
        .unwrap();
    // The end of the day of its last request that was summed.
    let day_after = |minutes: i64| Some(query::bucket(BASE + minutes * MINUTE, DAY) + DAY);
    let provider = of(&curves, A);
    assert_eq!(provider.summed_until, day_after(61));
    // Its percentiles are of the one kept as it came.
    assert_eq!(
        provider.first_token,
        FirstToken {
            samples: 1,
            p50: Some(70),
            p90: Some(70)
        }
    );
    assert_eq!(of(&curves, B).summed_until, day_after(10));
    assert_eq!(of(&curves, C).summed_until, None);
    // Requests of Claude Code are never summed, however old.
    let (_dir, mut store) = store_of("claude");
    store.compact(BASE + 40 * DAY).unwrap();
    let filter = Filter {
        client: Some("claude".into()),
        ..filter
    };
    let curves = store
        .curves(&filter, 0, &[], &NOWHERE, BASE + 40 * DAY)
        .unwrap();
    assert_eq!(of(&curves, A).summed_until, None);
    assert_eq!(of(&curves, A).first_token.samples, 3);
}

#[test]
fn days_kept_only_as_totals_are_drawn_by_the_day() {
    // Four days are divided into buckets of two hours, in which a day's
    // total, having no hour, would all stand at midnight.
    let old = |client: &str| Filter {
        start: Some(BASE - 2 * DAY),
        end: Some(BASE + 2 * DAY),
        client: Some(client.into()),
        ..Filter::default()
    };
    let (_dir, mut store) = store_of("codex");
    store.compact(BASE + 40 * DAY).unwrap();
    let curves = store
        .curves(&old("codex"), 0, &[], &NOWHERE, BASE + 40 * DAY)
        .unwrap();
    assert_eq!(curves.step, DAY);
    assert_eq!(curves.start, query::bucket(BASE - 2 * DAY, DAY));
    assert_eq!(of(&curves, A).buckets.iter().map(|b| b.ok).sum::<u64>(), 3);
    // What came since is still kept one by one, and a range of that alone
    // is divided as finely as its length allows.
    let late = request("codex", "late", A, BASE + 39 * DAY, Some(200), Some(70));
    store.write_batch(&[late], None).unwrap();
    let recent = Filter {
        start: Some(BASE + 39 * DAY - 60 * MINUTE),
        end: Some(BASE + 39 * DAY + 60 * MINUTE),
        ..old("codex")
    };
    let curves = store
        .curves(&recent, 0, &[], &NOWHERE, BASE + 40 * DAY)
        .unwrap();
    assert_eq!(curves.step, 5 * MINUTE);
    assert_eq!(of(&curves, A).first_token.samples, 1);
    // Requests of Claude Code are kept one by one for good, so the same old
    // days of them have no total among them.
    let (_dir, mut store) = store_of("claude");
    store.compact(BASE + 40 * DAY).unwrap();
    let curves = store
        .curves(&old("claude"), 0, &[], &NOWHERE, BASE + 40 * DAY)
        .unwrap();
    assert_eq!(curves.step, 120 * MINUTE);
    assert_eq!(of(&curves, A).first_token.samples, 3);
}

#[test]
fn providers_on_one_host_are_added_up_before_a_rate_is_taken_of_them() {
    let (_dir, store) = shared_store();
    // C is on a subdomain, which is another host, and D no longer exists:
    // nothing says where it was.
    let hosts = on(&[(A, RELAY), (B, RELAY), (C, REGIONAL)]);
    let curves = store
        .curves(&range(), 0, &shared_journal(), &hosts, BASE + DAY)
        .unwrap();
    let listed: Vec<_> = curves
        .providers
        .iter()
        .map(|p| (p.id.as_str(), p.host.as_deref()))
        .collect();
    assert_eq!(
        listed,
        [
            (B, Some(RELAY)),
            (C, Some(REGIONAL)),
            (D, None),
            (A, Some(RELAY))
        ]
    );
    assert!(curves.providers.iter().all(|p| p.members.is_empty()));
    // One host has two of them, and the two as one are all there is.
    assert_eq!(curves.groups.len(), 1);
    let both = group(&curves, RELAY);
    assert_eq!(both.host.as_deref(), Some(RELAY));
    // In the order they are listed in, which is not that of their ids.
    assert_eq!(both.members, [B, A]);
    let (a, b) = (of(&curves, A), of(&curves, B));
    assert_eq!((a.requests, a.ok, a.failures.total()), (3, 3, 1));
    assert_eq!((b.requests, b.ok, b.failures.total()), (7, 7, 9));
    assert_eq!((both.requests, both.ok), (10, 10));
    assert_eq!(
        both.failures,
        Failures {
            limited: 6,
            rejected: 1,
            server: 2,
            broken: 1
        }
    );
    assert_eq!(
        both.buckets,
        [
            Bucket {
                at: BASE,
                ok: 6,
                failures: Some(Failures {
                    limited: 6,
                    ..Failures::default()
                }),
                first_token: FirstToken {
                    samples: 6,
                    p50: Some(400),
                    p90: Some(1000)
                },
            },
            Bucket {
                at: BASE + 30 * MINUTE,
                ok: 4,
                failures: Some(Failures {
                    rejected: 1,
                    server: 2,
                    ..Failures::default()
                }),
                first_token: FirstToken {
                    samples: 4,
                    p50: Some(500),
                    p90: Some(900)
                },
            },
            // Only B has anything here.
            Bucket {
                at: BASE + 50 * MINUTE,
                ok: 0,
                failures: Some(Failures {
                    broken: 1,
                    ..Failures::default()
                }),
                first_token: none(),
            },
        ]
    );
    // In the first five minutes six of B's ten attempts failed and none of
    // A's two: six of the twelve, where the two rates average to 0.3. Half
    // an hour on it is one of A's two and two of B's five: three of seven.
    let trend = |p: &Provider| (p.trend[0], p.trend[6], p.trend[10]);
    assert_eq!(trend(a), (Some(0.0), Some(0.5), None));
    assert_eq!(trend(b), (Some(0.6), Some(0.4), Some(1.0)));
    assert_eq!(trend(both), (Some(0.5), Some(0.4286), Some(1.0)));
    assert_eq!(both.trend.iter().flatten().count(), 3);
}

#[test]
fn the_first_token_of_providers_taken_as_one_is_a_percentile_of_all_their_times() {
    let (_dir, store) = shared_store();
    let hosts = on(&[(A, RELAY), (B, RELAY)]);
    let curves = store.curves(&range(), 0, &[], &hosts, BASE + DAY).unwrap();
    let (a, b, both) = (of(&curves, A), of(&curves, B), group(&curves, RELAY));
    let first_token = |samples, p50, p90| FirstToken {
        samples,
        p50: Some(p50),
        p90: Some(p90),
    };
    assert_eq!(a.first_token, first_token(3, 200, 300));
    assert_eq!(b.first_token, first_token(7, 700, 1000));
    // Of the ten times from 100 to 1000, the fifth and the ninth. The
    // percentiles of the two average to 450 and 650, and weighed by how many
    // times each is of, to 550 and 790.
    assert_eq!(both.first_token, first_token(10, 500, 900));
    let mut times: Vec<u64> = (1..=10).map(|n| n * 100).collect();
    assert_eq!(both.first_token, FirstToken::of(&mut times));
    // So it is in each bucket: of 100, 300, 400, 600, 800 and 1000 in the
    // first, and of 200, 500, 700 and 900 in the second.
    let buckets =
        |p: &Provider| -> Vec<FirstToken> { p.buckets.iter().map(|b| b.first_token).collect() };
    assert_eq!(
        buckets(a),
        [first_token(2, 100, 300), first_token(1, 200, 200)]
    );
    assert_eq!(
        buckets(b),
        [first_token(4, 600, 1000), first_token(3, 700, 900)]
    );
    assert_eq!(
        buckets(both),
        [first_token(6, 400, 1000), first_token(4, 500, 900)]
    );
}

#[test]
fn a_host_is_taken_as_one_only_where_two_of_its_providers_have_something_in_the_range() {
    let (_dir, store) = shared_store();
    let journal = shared_journal();
    // E is on the host of A and has never answered or failed. Nothing is
    // drawn of it, which leaves A the only one there.
    const E: &str = "00000000-0000-4000-8000-00000000000e";
    let hosts = on(&[(A, RELAY), (E, RELAY), (B, REGIONAL)]);
    let curves = store
        .curves(&range(), 0, &journal, &hosts, BASE + DAY)
        .unwrap();
    assert!(curves.groups.is_empty());
    assert_eq!(curves.providers.len(), 4);
    assert_eq!(of(&curves, A).host.as_deref(), Some(RELAY));
    // A and B are on one host, and in these ten minutes B alone failed.
    let hosts = on(&[(A, RELAY), (B, RELAY)]);
    let late = Filter {
        start: Some(BASE + 45 * MINUTE),
        end: Some(BASE + 55 * MINUTE - 1),
        ..range()
    };
    let curves = store
        .curves(&late, 0, &journal, &hosts, BASE + DAY)
        .unwrap();
    assert!(curves.groups.is_empty());
    let alone: Vec<_> = curves
        .providers
        .iter()
        .map(|p| (p.id.as_str(), p.host.as_deref()))
        .collect();
    assert_eq!(alone, [(B, Some(RELAY))]);
    // Over the whole of the range they are two.
    let curves = store
        .curves(&range(), 0, &journal, &hosts, BASE + DAY)
        .unwrap();
    assert_eq!(group(&curves, RELAY).members, [B, A]);
}

#[test]
fn a_provider_that_no_longer_exists_is_on_no_host_and_taken_with_no_other() {
    let (_dir, store) = shared_store();
    let journal = shared_journal();
    // C and D were deleted: no address is left to say where they were.
    let hosts = on(&[(A, RELAY), (B, RELAY)]);
    let curves = store
        .curves(&range(), 0, &journal, &hosts, BASE + DAY)
        .unwrap();
    for gone in [C, D] {
        assert_eq!(of(&curves, gone).host, None);
    }
    // What it did is drawn as before.
    let gone = of(&curves, D);
    assert_eq!((gone.requests, gone.ok, gone.failures.server), (1, 1, 3));
    // Having no host alike makes nothing of the two of them.
    let hosts: Vec<_> = curves.groups.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(hosts, ["host:relay.example.invalid"]);
    assert_eq!(group(&curves, RELAY).members, [B, A]);
    // Nor of all four, where none of them exists any more.
    let curves = store
        .curves(&range(), 0, &journal, &NOWHERE, BASE + DAY)
        .unwrap();
    assert!(curves.groups.is_empty());
    assert!(curves.providers.iter().all(|p| p.host.is_none()));
}

#[test]
fn hosts_are_listed_as_providers_are() {
    let (_dir, store) = shared_store();
    let hosts = on(&[(A, RELAY), (B, RELAY), (C, REGIONAL), (D, REGIONAL)]);
    let curves = store
        .curves(&range(), 0, &shared_journal(), &hosts, BASE + DAY)
        .unwrap();
    let listed: Vec<_> = curves
        .groups
        .iter()
        .map(|g| {
            (
                g.id.as_str(),
                g.failures.limited,
                g.failures.total(),
                g.requests,
            )
        })
        .collect();
    // The most rate-limited first, though by its name the other would be.
    assert_eq!(
        listed,
        [
            ("host:relay.example.invalid", 6, 10, 10),
            ("host:eu.relay.example.invalid", 1, 4, 2)
        ]
    );
    assert_eq!(group(&curves, REGIONAL).members, [C, D]);
}

#[test]
fn providers_taken_as_one_count_from_where_the_journal_begins_as_each_of_them_does() {
    let (_dir, store) = shared_store();
    let journal = shared_journal();
    let hosts = on(&[(A, RELAY), (B, RELAY)]);
    let since = BASE + 30 * MINUTE;
    let curves = store
        .curves(&range(), since, &journal, &hosts, BASE + DAY)
        .unwrap();
    assert_eq!(curves.failures_since, since);
    let both = group(&curves, RELAY);
    // The six rejections of the first five minutes came before the journal
    // begins, and what succeeded then is set against nothing.
    assert_eq!((both.requests, both.ok, both.failures.total()), (10, 4, 4));
    let buckets: Vec<_> = both
        .buckets
        .iter()
        .map(|b| (b.at - BASE, b.ok, b.failures.map(|f| f.total())))
        .collect();
    assert_eq!(
        buckets,
        [
            (0, 6, None),
            (30 * MINUTE, 4, Some(3)),
            (50 * MINUTE, 0, Some(1))
        ]
    );
    assert_eq!((both.trend[0], both.trend[6]), (None, Some(0.4286)));
    // A range that stops two minutes into a bucket, a minute before the
    // journal begins within it: none of it is known, of the two as of each.
    let filter = Filter {
        end: Some(BASE + 32 * MINUTE - 1),
        ..range()
    };
    let curves = store
        .curves(&filter, BASE + 33 * MINUTE, &journal, &hosts, BASE + DAY)
        .unwrap();
    assert_eq!(curves.failures_since, curves.end);
    let both = group(&curves, RELAY);
    assert_eq!((both.requests, both.ok, both.failures.total()), (8, 0, 0));
    let buckets: Vec<_> = both.buckets.iter().map(|b| (b.ok, b.failures)).collect();
    assert_eq!(buckets, [(6, None), (2, None)]);
}

#[test]
fn providers_taken_as_one_say_the_last_day_only_a_total_is_kept_of_any_of_them() {
    let (_dir, mut store) = store_of("codex");
    // B answered once more three days on.
    let later = request("codex", "b-later", B, BASE + 3 * DAY, Some(200), Some(80));
    store.write_batch(&[later], None).unwrap();
    store.compact(BASE + 40 * DAY).unwrap();
    // All there is of C came since, and is kept as it came.
    let late = request("codex", "late-c", C, BASE + 39 * DAY, Some(200), Some(90));
    store.write_batch(&[late], None).unwrap();
    let filter = Filter {
        start: Some(BASE - 2 * DAY),
        end: Some(BASE + 40 * DAY),
        client: Some("codex".into()),
        ..Filter::default()
    };
    let hosts = on(&[(A, RELAY), (B, RELAY), (C, RELAY)]);
    let curves = store
        .curves(&filter, 0, &[], &hosts, BASE + 40 * DAY)
        .unwrap();
    let until = |id| of(&curves, id).summed_until;
    assert!(until(A).is_some() && until(B) > until(A));
    assert_eq!(until(C), None);
    let all = group(&curves, RELAY);
    assert_eq!(all.members.len(), 3);
    assert_eq!(all.summed_until, until(B));
    assert_eq!((all.requests, all.ok), (8, 6));
    // Its percentiles are of the one request kept as it came.
    assert_eq!(
        all.first_token,
        FirstToken {
            samples: 1,
            p50: Some(90),
            p90: Some(90)
        }
    );
}

#[test]
fn saying_where_providers_are_changes_nothing_else_that_is_said_of_them() {
    let (_dir, store) = store();
    let hosts = on(&[(A, RELAY), (C, RELAY)]);
    // With all of the range known, and with the journal beginning within it.
    for since in [0, BASE + 30 * MINUTE] {
        let curves = |hosts| {
            store
                .curves(&range(), since, &journal(), hosts, BASE + DAY)
                .unwrap()
        };
        let (apart, mut placed) = (curves(&NOWHERE), curves(&hosts));
        assert!(apart.groups.is_empty());
        assert_eq!(group(&placed, RELAY).members, [C, A]);
        let said: Vec<_> = placed
            .providers
            .iter_mut()
            .map(|p| (p.id.clone(), p.host.take()))
            .collect();
        assert_eq!(
            said,
            [
                (C.to_owned(), Some(RELAY.to_owned())),
                (A.to_owned(), Some(RELAY.to_owned())),
                (B.to_owned(), None)
            ]
        );
        assert_eq!(placed.providers, apart.providers);
        assert_eq!(
            (placed.start, placed.end, placed.step, placed.failures_since),
            (apart.start, apart.end, apart.step, apart.failures_since)
        );
    }
}

#[test]
fn a_request_that_reached_no_gateway_is_drawn_from_its_record_attempt_by_attempt() {
    // Antigravity was limited once and answered when it asked again: one
    // request, which succeeded, and two attempts, of which one failed.
    let streamed = |first_token_ms, attempt| Attempt {
        stream: true,
        first_token_ms: Some(first_token_ms),
        ..attempt
    };
    let retried = told(
        "antigravity",
        "retried",
        BASE + MINUTE,
        vec![
            streamed(900, tried(Some(ACCOUNT), "failed", Some("429"))),
            streamed(120, tried(Some(ACCOUNT), "success", None)),
        ],
    );
    let (_dir, store) = holding(&[retried]);
    let filter = Filter {
        client: Some("antigravity".into()),
        ..range()
    };
    // The totals count the request, as its last attempt ended.
    let totals = store.dashboard(&filter).unwrap().totals;
    assert_eq!(
        (totals.requests, totals.success, totals.status_known),
        (1, 1, 1)
    );
    let curves = store.curves(&filter, 0, &[], &NOWHERE, BASE + DAY).unwrap();
    // It is none of a gateway's.
    assert!(curves.providers.is_empty() && curves.groups.is_empty());
    assert_eq!(curves.direct.len(), 1);
    let account = direct(&curves, ACCOUNT);
    assert!(account.recorded);
    assert_eq!((account.host.as_deref(), account.members.len()), (None, 0));
    assert_eq!(
        (account.requests, account.ok, account.summed_until),
        (1, 1, None)
    );
    let limited = Failures {
        limited: 1,
        ..Failures::default()
    };
    assert_eq!(account.failures, limited);
    // The time to the first token is that of the attempt that answered.
    let first_token = FirstToken {
        samples: 1,
        p50: Some(120),
        p90: Some(120),
    };
    assert_eq!(account.first_token, first_token);
    assert_eq!(
        account.buckets,
        [Bucket {
            at: BASE,
            ok: 1,
            failures: Some(limited),
            first_token,
        }]
    );
    // One of its two attempts failed.
    assert_eq!(account.trend.len(), 20);
    assert_eq!(account.trend[0], Some(0.5));
    assert_eq!(account.trend.iter().flatten().count(), 1);
}

#[test]
fn an_attempt_that_failed_is_counted_against_the_account_that_made_it() {
    let (_dir, store) = holding(&[
        // One account was limited and the other answered.
        told(
            "antigravity",
            "moved",
            BASE + MINUTE,
            vec![
                tried(Some(OTHER), "failed", Some("RESOURCE_EXHAUSTED")),
                tried(Some(ACCOUNT), "success", None),
            ],
        ),
        // An attempt that answered, and one whose end nothing tells, are no
        // failures for the request having failed after them.
        told(
            "antigravity",
            "lost",
            BASE + 2 * MINUTE,
            vec![
                tried(Some(OTHER), "success", None),
                tried(Some(OTHER), "unknown", None),
                tried(Some(ACCOUNT), "failed", Some("503")),
            ],
        ),
    ]);
    let curves = store
        .curves(&every_client(), 0, &[], &NOWHERE, BASE + DAY)
        .unwrap();
    // The most rate-limited first, as a gateway's providers are listed.
    let listed: Vec<_> = curves.direct.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(listed, [OTHER, ACCOUNT]);
    let (account, other) = (direct(&curves, ACCOUNT), direct(&curves, OTHER));
    // A request is one of the account that was asked last.
    assert_eq!((account.requests, account.ok), (2, 1));
    assert_eq!(
        account.failures,
        Failures {
            server: 1,
            ..Failures::default()
        }
    );
    // Of the other there is the attempt that failed, and no request.
    assert_eq!((other.requests, other.ok), (0, 0));
    let limited = Failures {
        limited: 1,
        ..Failures::default()
    };
    assert_eq!(other.failures, limited);
    assert_eq!(
        other.buckets,
        [Bucket {
            at: BASE,
            ok: 0,
            failures: Some(limited),
            first_token: none(),
        }]
    );
    assert_eq!((account.trend[0], other.trend[0]), (Some(0.5), Some(1.0)));
}

#[test]
fn a_failure_the_records_tell_of_is_of_the_kind_its_status_or_its_error_says() {
    for (status, error, expected) in [
        (Some(429), None, Limited),
        (Some(400), None, Rejected),
        (Some(500), None, Server),
        // The status first, whatever the error is called.
        (Some(403), Some("server_error"), Rejected),
        (Some(529), Some("rate_limit"), Server),
        // Antigravity writes the status it was answered with as the error.
        (None, Some("429"), Limited),
        (None, Some("403"), Rejected),
        (None, Some("503"), Server),
        // What it calls the rest, and what Claude Code calls its own.
        (None, Some("RESOURCE_EXHAUSTED"), Limited),
        (None, Some("rate_limit"), Limited),
        (None, Some("UNAUTHENTICATED"), Rejected),
        (None, Some("authentication_failed"), Rejected),
        (None, Some("invalid_request"), Rejected),
        (None, Some("billing_error"), Rejected),
        (None, Some("UPSTREAM_ERROR"), Server),
        (None, Some("server_error"), Server),
        // Anything else says only that the attempt did not end well.
        (None, Some("STREAM_TIMEOUT"), Broken),
        (None, Some("unknown"), Broken),
        (None, None, Broken),
        // Three digits are a status, and no more or fewer are.
        (None, Some("4290"), Broken),
        (None, Some("42"), Broken),
        (None, Some("200"), Broken),
        // A status that is not an error's leaves it to the error.
        (Some(200), Some("rate_limit"), Limited),
        (Some(200), None, Broken),
    ] {
        assert_eq!(kind(status, error), expected, "{status:?} {error:?}");
    }
}

#[test]
fn what_failed_is_counted_by_the_status_and_the_error_its_record_keeps() {
    // What Antigravity wrote of eight attempts that failed, and what Claude
    // Code did of eight more, with the status of those it kept one of.
    let antigravity = [
        Some("429"),
        Some("RESOURCE_EXHAUSTED"),
        Some("403"),
        Some("UNAUTHENTICATED"),
        Some("503"),
        Some("UPSTREAM_ERROR"),
        Some("STREAM_TIMEOUT"),
        None,
    ];
    let claude = [
        (Some(429), Some("rate_limit")),
        (None, Some("rate_limit")),
        (None, Some("authentication_failed")),
        (None, Some("invalid_request")),
        (None, Some("billing_error")),
        (Some(529), Some("server_error")),
        (None, Some("server_error")),
        (None, Some("unknown")),
    ];
    let mut records = Vec::new();
    for (index, error) in antigravity.into_iter().enumerate() {
        let failed = tried(Some(ACCOUNT), "failed", error);
        let id = format!("g-{index}");
        records.push(told("antigravity", &id, BASE + MINUTE, vec![failed]));
    }
    for (index, (status, error)) in claude.into_iter().enumerate() {
        let failed = Attempt {
            status,
            ..tried(None, "failed", error)
        };
        let id = format!("c-{index}");
        records.push(told("claude", &id, BASE + MINUTE, vec![failed]));
    }
    let (_dir, store) = holding(&records);
    let curves = store
        .curves(&every_client(), 0, &[], &NOWHERE, BASE + DAY)
        .unwrap();
    let account = direct(&curves, ACCOUNT);
    assert_eq!((account.requests, account.ok), (8, 0));
    assert_eq!(
        account.failures,
        Failures {
            limited: 2,
            rejected: 2,
            server: 2,
            broken: 2
        }
    );
    let session = direct(&curves, "session:claude");
    assert_eq!((session.requests, session.ok), (8, 0));
    assert_eq!(
        session.failures,
        Failures {
            limited: 2,
            rejected: 3,
            server: 2,
            broken: 1
        }
    );
}

#[test]
fn what_names_no_account_is_drawn_under_its_client() {
    let (_dir, mut store) = store();
    let hosts = on(&[(A, RELAY), (C, RELAY)]);
    let read = |store: &Store| {
        store
            .curves(&every_client(), 0, &journal(), &hosts, BASE + DAY)
            .unwrap()
    };
    let before = read(&store);
    assert!(before.direct.is_empty());
    let ok = |client, id, account| {
        let answered = tried(account, "success", None);
        told(client, id, BASE + MINUTE, vec![answered])
    };
    let overloaded = Attempt {
        status: Some(529),
        ..tried(None, "failed", None)
    };
    let limited = tried(Some(OTHER), "failed", Some("429"));
    store
        .write_batch(
            &[
                // A session of Claude Code names no account.
                ok("claude", "c-1", None),
                ok("claude", "c-2", None),
                told("claude", "c-3", BASE + 6 * MINUTE, vec![overloaded]),
                // Nor does what Antigravity wrote while nothing told which
                // account it was signed in to.
                ok("antigravity", "g-1", None),
                ok("antigravity", "g-2", Some(ACCOUNT)),
                told("antigravity", "g-3", BASE + MINUTE, vec![limited]),
            ],
            None,
        )
        .unwrap();
    let curves = read(&store);
    let listed: Vec<_> = curves
        .direct
        .iter()
        .map(|p| (p.id.as_str(), p.requests, p.ok, p.failures.total()))
        .collect();
    // The most rate-limited first, then the one that failed most, and the
    // rest by their ids.
    assert_eq!(
        listed,
        [
            (OTHER, 1, 0, 1),
            ("session:claude", 3, 2, 1),
            (ACCOUNT, 1, 1, 0),
            ("session:antigravity", 1, 1, 0),
        ]
    );
    assert!(curves
        .direct
        .iter()
        .all(|p| p.recorded && p.host.is_none() && p.members.is_empty()));
    // What is said of a gateway's providers, and of those on one host taken
    // as one, is what it was.
    assert_eq!(curves.providers, before.providers);
    assert_eq!(curves.groups, before.groups);
    assert_eq!(curves.groups.len(), 1);
    let mut gateways = curves.providers.iter().chain(&curves.groups);
    assert!(gateways.all(|p| !p.recorded));
    assert_eq!(
        (curves.start, curves.end, curves.step, curves.failures_since),
        (before.start, before.end, before.step, before.failures_since)
    );
}

#[test]
fn what_antigravity_wrote_naming_no_account_is_the_account_s_signed_in_at_its_time() {
    let dir = tempfile::tempdir().unwrap();
    // The first account was signed in two minutes in, the other ten.
    let accounts = serde_json::json!({
        "schema": 1,
        "entries": [],
        "activations": [
            { "accountId": ACCOUNT, "sinceMs": BASE + 2 * MINUTE },
            { "accountId": OTHER, "sinceMs": BASE + 10 * MINUTE }
        ]
    });
    std::fs::write(
        dir.path().join("antigravity-accounts.json"),
        serde_json::to_vec(&accounts).unwrap(),
    )
    .unwrap();
    let mut store = Store::open(dir.path()).unwrap();
    let ok = |client, id, minutes, account| {
        let answered = tried(account, "success", None);
        told(client, id, BASE + minutes * MINUTE, vec![answered])
    };
    store
        .write_batch(
            &[
                ok("antigravity", "g-1", 3, None),
                ok("antigravity", "g-2", 12, None),
                // One that names its account is that account's whenever it was.
                ok("antigravity", "g-3", 13, Some(ACCOUNT)),
                // No account is signed in to Claude Code.
                ok("claude", "c-1", 3, None),
            ],
            None,
        )
        .unwrap();
    let curves = store
        .curves(&every_client(), 0, &[], &NOWHERE, BASE + DAY)
        .unwrap();
    let listed: Vec<_> = curves
        .direct
        .iter()
        .map(|p| (p.id.as_str(), p.requests, p.ok))
        .collect();
    assert_eq!(
        listed,
        [(ACCOUNT, 2, 2), (OTHER, 1, 1), ("session:claude", 1, 1)]
    );
}

#[test]
fn only_the_client_narrows_what_reached_no_gateway() {
    let ok = |client, id, account| {
        let answered = tried(account, "success", None);
        told(client, id, BASE + MINUTE, vec![answered])
    };
    let (_dir, store) = holding(&[
        ok("claude", "c-1", None),
        ok("antigravity", "g-1", None),
        ok("antigravity", "g-2", Some(ACCOUNT)),
    ]);
    let listed = |filter: Filter| -> Vec<String> {
        let curves = store.curves(&filter, 0, &[], &NOWHERE, BASE + DAY).unwrap();
        curves.direct.into_iter().map(|p| p.id).collect()
    };
    let of_client = |client: &str| Filter {
        client: Some(client.into()),
        ..range()
    };
    let all = [ACCOUNT, "session:antigravity", "session:claude"];
    assert_eq!(listed(every_client()), all);
    // A client with no name is none asked for.
    assert_eq!(listed(of_client("")), all);
    assert_eq!(listed(of_client("claude")), ["session:claude"]);
    assert_eq!(
        listed(of_client("antigravity")),
        [ACCOUNT, "session:antigravity"]
    );
    // All that is drawn of the other clients went through a gateway.
    for client in ["codex", "grok", "dsh"] {
        assert!(listed(of_client(client)).is_empty(), "{client}");
    }
    // Nothing else that is asked for narrows them, as nothing else narrows
    // what a gateway's providers are compared by.
    let named = Filter {
        provider: Some(A.into()),
        model: Some("fixture-model".into()),
        source: Some("proxy".into()),
        status: Some("5xx".into()),
        ..every_client()
    };
    assert_eq!(listed(named), all);
}

#[test]
fn a_request_whose_end_its_record_does_not_tell_is_counted_and_nothing_more() {
    let ended = |client: &str, account, outcome: &str| {
        let id = format!("{client}-{outcome}");
        let attempt = tried(account, outcome, None);
        told(client, &id, BASE + MINUTE, vec![attempt])
    };
    let (_dir, store) = holding(&[
        ended("antigravity", Some(ACCOUNT), "unknown"),
        ended("antigravity", Some(ACCOUNT), "in_progress"),
        ended("antigravity", Some(ACCOUNT), "cancelled"),
        // A message of Claude Code that was cut short, and one imported
        // before its sessions told how a message ended.
        ended("claude", None, "reported"),
        Record {
            completed: true,
            ..ended("claude", None, "completed")
        },
    ]);
    let curves = store
        .curves(&every_client(), 0, &[], &NOWHERE, BASE + DAY)
        .unwrap();
    for (id, requests) in [(ACCOUNT, 3), ("session:claude", 2)] {
        let row = direct(&curves, id);
        assert_eq!(
            (row.requests, row.ok, row.failures),
            (requests, 0, Failures::default()),
            "{id}"
        );
        // Nothing failed there, which is known, and nothing succeeded: there
        // is no rate to draw of it.
        assert_eq!(
            row.buckets,
            [Bucket {
                at: BASE,
                ok: 0,
                failures: Some(Failures::default()),
                first_token: none(),
            }]
        );
        assert!(row.trend.iter().all(Option::is_none));
    }
}

#[test]
fn what_reached_no_gateway_is_known_wherever_the_journal_begins() {
    let (_dir, store) = store_and_direct();
    let read = |since| {
        store
            .curves(&every_client(), since, &journal(), &NOWHERE, BASE + DAY)
            .unwrap()
    };
    let drawn = |provider: &Provider| -> Vec<_> {
        let buckets = provider.buckets.iter();
        buckets
            .map(|b| (b.at - BASE, b.ok, b.failures.map(|f| f.total())))
            .collect()
    };
    let known = read(0);
    let account = direct(&known, ACCOUNT);
    assert_eq!(
        (account.requests, account.ok, account.failures.limited),
        (2, 2, 1)
    );
    assert_eq!(drawn(account), [(0, 1, Some(0)), (30 * MINUTE, 1, Some(1))]);
    assert_eq!((account.trend[0], account.trend[6]), (Some(0.0), Some(0.5)));
    let session = direct(&known, "session:claude");
    assert_eq!(
        (session.requests, session.ok, session.failures.server),
        (3, 2, 1)
    );
    assert_eq!(drawn(session), [(0, 2, Some(0)), (60 * MINUTE, 0, Some(1))]);
    // The journal begins half an hour in. Of a gateway's provider nothing is
    // said of what failed before, and what it completed then is set against
    // nothing.
    let since = BASE + 30 * MINUTE;
    let partly = read(since);
    assert_eq!(partly.failures_since, since);
    let provider = of(&partly, A);
    assert_eq!(
        drawn(provider),
        [
            (0, 3, None),
            (30 * MINUTE, 0, Some(5)),
            (60 * MINUTE, 0, Some(2))
        ]
    );
    assert_eq!((provider.ok, provider.trend[0]), (0, None));
    // The records have all of what reached no gateway, of the time before
    // the journal as of the time since.
    assert_eq!(partly.direct, known.direct);
    // So they do where the journal begins after all of the range.
    let since = BASE + 2 * DAY;
    let after = read(since);
    assert_eq!(after.failures_since, since);
    for provider in &after.providers {
        assert_eq!((provider.ok, provider.failures.total()), (0, 0));
        assert!(provider.buckets.iter().all(|b| b.failures.is_none()));
    }
    assert_eq!(after.direct, known.direct);
    let mut buckets = after.direct.iter().flat_map(|p| &p.buckets);
    assert!(buckets.all(|b| b.failures.is_some()));
}

#[test]
fn a_range_without_a_beginning_starts_where_what_reached_no_gateway_does() {
    let (_dir, mut store) = store();
    // Three days before a gateway had a request, a session of Claude Code
    // wrote down an error.
    let early = BASE - 3 * DAY + MINUTE;
    let limited = tried(None, "failed", Some("rate_limit"));
    store
        .write_batch(&[told("claude", "early", early, vec![limited])], None)
        .unwrap();
    let filter = Filter {
        start: None,
        ..range()
    };
    let curves = store
        .curves(&filter, 0, &journal(), &NOWHERE, BASE + DAY)
        .unwrap();
    assert_eq!(curves.step, DAY);
    assert_eq!(curves.start, query::bucket(early, DAY));
    let session = direct(&curves, "session:claude");
    assert_eq!(session.buckets.len(), 1);
    assert_eq!(session.buckets[0].at, curves.start);
    assert_eq!(session.trend[0], Some(1.0));
    // What a gateway's provider did is drawn where it was.
    let provider = of(&curves, A);
    assert_eq!(provider.buckets[0].at, query::bucket(BASE + MINUTE, DAY));
    assert_eq!(provider.trend[0], None);
}

#[test]
fn a_session_s_copy_of_a_request_a_gateway_recorded_is_not_drawn_apart() {
    let (_dir, mut store) = store();
    // Antigravity asked a gateway once, in a terminal.
    let through = request("antigravity", "p-1", D, BASE + MINUTE, Some(200), None);
    store.write_batch(&[through], None).unwrap();
    // The session of each client wrote the request down too, with the id
    // its answer had. That of Antigravity names the account that was signed
    // in, which the gateway's provider is not.
    let copy = |client, id, account, response: &str, transport: &str| {
        let attempt = Attempt {
            response_id: Some(response.into()),
            transport: transport.into(),
            ..tried(account, "success", None)
        };
        told(client, id, BASE + MINUTE, vec![attempt])
    };
    // And each wrote down one more, which no gateway saw.
    let own = |client, id, account| {
        let answered = tried(account, "success", None);
        told(client, id, BASE + 2 * MINUTE, vec![answered])
    };
    store
        .write_batch(
            &[
                copy("claude", "copy-c", None, "a-1-response", "session"),
                copy(
                    "antigravity",
                    "copy-g",
                    Some(ACCOUNT),
                    "p-1-response",
                    "cli_file",
                ),
                own("claude", "own-c", None),
                own("antigravity", "own-g", Some(ACCOUNT)),
            ],
            None,
        )
        .unwrap();
    for (copy, original) in [("copy-c", "a-1"), ("copy-g", "p-1")] {
        let copy = store.detail(copy).unwrap();
        assert_eq!(copy.duplicate_of.as_deref(), Some(original));
    }
    let curves = store
        .curves(&every_client(), 0, &journal(), &NOWHERE, BASE + DAY)
        .unwrap();
    // Each is the gateway's request, and is counted there once.
    assert_eq!((of(&curves, A).requests, of(&curves, D).requests), (5, 1));
    let listed: Vec<_> = curves
        .direct
        .iter()
        .map(|p| (p.id.as_str(), p.requests, p.ok))
        .collect();
    assert_eq!(listed, [(ACCOUNT, 1, 1), ("session:claude", 1, 1)]);
}

#[test]
fn what_reached_no_gateway_is_kept_one_by_one_however_old() {
    let (_dir, mut store) = store_and_direct();
    let filter = Filter {
        start: Some(BASE - 2 * DAY),
        end: Some(BASE + 2 * DAY),
        ..Filter::default()
    };
    let read = |store: &Store| {
        store
            .curves(&filter, 0, &[], &NOWHERE, BASE + 40 * DAY)
            .unwrap()
    };
    let before = read(&store);
    assert_eq!(before.direct.len(), 2);
    store.compact(BASE + 40 * DAY).unwrap();
    // No day of them is kept as a total: they are drawn as finely as they
    // were, and nothing is missing from their percentiles.
    let after = read(&store);
    assert_eq!((after.step, before.step), (120 * MINUTE, 120 * MINUTE));
    assert_eq!(after.direct, before.direct);
    assert!(after.direct.iter().all(|p| p.summed_until.is_none()));
}

/// A runtime whose every directory is under `root`. Nothing of it runs in
/// the background.
fn runtime(root: &std::path::Path) -> std::sync::Arc<crate::runtime::Runtime> {
    let client = |name: &str| root.join(name).to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join(".vela")).unwrap();
    std::fs::write(
        root.join(".vela/accounts.json"),
        serde_json::json!({
            "schema": 1,
            "profiles": [],
            "preferences": {
                "codexHome": client(".codex"),
                "claudeHome": client(".claude"),
                "grokHome": client(".grok"),
                "dshHome": client(".dsh"),
                "cliPath": "",
                "theme": "system",
            },
        })
        .to_string(),
    )
    .unwrap();
    crate::runtime::Runtime::at(
        std::sync::Arc::new(crate::runtime::Silent),
        root.to_path_buf(),
        root.join(".codex"),
    )
    .unwrap()
}

#[tokio::test]
async fn the_hosts_are_those_of_the_addresses_every_client_s_gateway_has() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let edit = |client: ClientId, edit| {
        let gateway = runtime.gateway(client);
        let home = runtime.home(client).unwrap();
        gateway.edit(edit, &gateway.view().revision, &home).unwrap()
    };
    let add = |client: ClientId, address: &str| {
        let mut fields = serde_json::json!({ "baseUrl": address });
        match client {
            ClientId::Grok => {
                fields["op"] = "saveGrokProvider".into();
                fields["protocol"] = "responses".into();
            }
            ClientId::Dsh => {
                fields["op"] = "saveDshProvider".into();
                fields["protocol"] = "chat_completions".into();
            }
            ClientId::Codex | ClientId::Claude | ClientId::Agy => {}
        }
        let view = edit(client, form(fields));
        view.providers.last().unwrap().id.clone()
    };
    let plain = add(ClientId::Codex, "https://relay.example.invalid/v1");
    // Another scheme, port and path are still the same host.
    let ported = add(
        ClientId::Codex,
        "http://relay.example.invalid:8443/openai/v1",
    );
    // A subdomain is not.
    let regional = add(ClientId::Codex, "https://eu.relay.example.invalid/v1");
    let gone = add(ClientId::Codex, "https://relay.example.invalid/old/v1");
    let claude = add(ClientId::Claude, "https://RELAY.example.invalid/anthropic");
    let agy = add(ClientId::Agy, "https://relay.example.invalid/gemini");
    let grok = add(ClientId::Grok, "https://relay.example.invalid/grok/v1");
    let dsh = add(ClientId::Dsh, "https://relay.example.invalid/deepseek/v1");
    edit(
        ClientId::Codex,
        crate::gateway::Edit::DeleteProvider { id: gone.clone() },
    );

    // Each answered once, ten minutes ago.
    let at = now() - 10 * MINUTE;
    let answered = |client: ClientId, provider: &String| {
        let id = format!("{provider}-request");
        request(client.usage_name(), &id, provider, at, Some(200), Some(100))
    };
    let requests = [
        answered(ClientId::Codex, &plain),
        answered(ClientId::Codex, &ported),
        answered(ClientId::Codex, &regional),
        answered(ClientId::Codex, &gone),
        answered(ClientId::Claude, &claude),
        answered(ClientId::Agy, &agy),
        answered(ClientId::Grok, &grok),
        answered(ClientId::Dsh, &dsh),
    ];
    runtime
        .usage
        .query(|store| store.write_batch(&requests, None))
        .unwrap();
    let of_client = |client: Option<&str>| Filter {
        start: Some(at - 50 * MINUTE),
        client: client.map(Into::into),
        ..Filter::default()
    };

    // Every client's: the six on the host are one, whichever client's they
    // are. None of them failed and each was asked once, so they are listed
    // by id.
    let curves = get_usage_curves(&runtime, of_client(None)).await.unwrap();
    assert_eq!(curves.providers.len(), 8);
    assert_eq!(of(&curves, &regional).host.as_deref(), Some(REGIONAL));
    assert_eq!(of(&curves, &gone).host, None);
    let mut members = vec![&plain, &ported, &claude, &agy, &grok, &dsh];
    members.sort();
    for member in &members {
        assert_eq!(of(&curves, member).host.as_deref(), Some(RELAY));
    }
    assert_eq!(curves.groups.len(), 1);
    let all = group(&curves, RELAY);
    assert_eq!(all.host.as_deref(), Some(RELAY));
    assert_eq!(all.members.iter().collect::<Vec<_>>(), members);
    assert_eq!((all.requests, all.ok, all.first_token.samples), (6, 6, 6));

    // One client's: only its own have anything in the range.
    let curves = get_usage_curves(&runtime, of_client(Some("codex")))
        .await
        .unwrap();
    assert_eq!(curves.providers.len(), 4);
    let mut members = vec![&plain, &ported];
    members.sort();
    assert_eq!(curves.groups.len(), 1);
    let two = group(&curves, RELAY);
    assert_eq!(two.members.iter().collect::<Vec<_>>(), members);
    assert_eq!((two.requests, two.ok), (2, 2));
    // And one provider alone is not taken as one with those of other clients.
    let curves = get_usage_curves(&runtime, of_client(Some("claude")))
        .await
        .unwrap();
    assert_eq!(curves.providers.len(), 1);
    assert_eq!(of(&curves, &claude).host.as_deref(), Some(RELAY));
    assert!(curves.groups.is_empty());
}
