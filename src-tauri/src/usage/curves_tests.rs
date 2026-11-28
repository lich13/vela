// 覆盖供应商曲线：分桶、失败分类、首字分位数、日志覆盖范围、同域合并与无数据。
use super::super::{commands::get_usage_curves, model::*, query, store::Store};
use super::{spread, step, Bucket, Curves, Failures, FirstToken, Grid, Provider};
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
/// The provider `id` as `curves` has it.
fn of<'a>(curves: &'a Curves, id: &str) -> &'a Provider {
    curves.providers.iter().find(|p| p.id == id).unwrap()
}
/// The providers on `host` as `curves` has them taken as one.
fn group<'a>(curves: &'a Curves, host: &str) -> &'a Provider {
    let id = format!("host:{host}");
    curves.groups.iter().find(|g| g.id == id).unwrap()
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
    let (_dir, store) = store();
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
    for row in [provider, group] {
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
