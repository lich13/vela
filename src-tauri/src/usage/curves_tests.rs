// 覆盖供应商曲线：分桶、失败分类、首字分位数、日志覆盖范围与无数据。
use super::super::{model::*, query, store::Store};
use super::{spread, step, Bucket, Curves, Failures, FirstToken, Provider};
use crate::{events::UpstreamFailure, gateway::ClientId};

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
const BASE: i64 = 20_000 * DAY;
const A: &str = "00000000-0000-4000-8000-00000000000a";
const B: &str = "00000000-0000-4000-8000-00000000000b";
const C: &str = "00000000-0000-4000-8000-00000000000c";

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
fn failed(
    provider: &str,
    status: Option<u16>,
    first: i64,
    last: i64,
    count: u32,
) -> UpstreamFailure {
    UpstreamFailure {
        client: ClientId::Claude,
        provider: provider.into(),
        first_at: (first / 1000) as u64,
        last_at: (last / 1000) as u64,
        count,
        status,
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
        failed(A, Some(429), BASE + 29 * MINUTE, BASE + 33 * MINUTE, 6),
        failed(A, Some(503), BASE + 31 * MINUTE, BASE + 31 * MINUTE, 1),
        failed(A, None, BASE + 62 * MINUTE, BASE + 62 * MINUTE, 2),
        // An endless provider: rejected over and over, never a usage record.
        failed(C, Some(429), BASE + 40 * MINUTE, BASE + 40 * MINUTE, 9),
        UpstreamFailure {
            client: ClientId::Codex,
            ..failed(B, Some(500), BASE + 10 * MINUTE, BASE + 10 * MINUTE, 4)
        },
    ]
}
fn none() -> FirstToken {
    FirstToken::default()
}

#[test]
fn a_range_is_divided_into_at_most_sixty_buckets() {
    let of = |span: i64| step(Some(BASE), BASE + span);
    assert_eq!(of(0), 5 * MINUTE);
    assert_eq!(of(300 * MINUTE), 5 * MINUTE);
    assert_eq!(of(300 * MINUTE + 1), 10 * MINUTE);
    assert_eq!(of(DAY), 30 * MINUTE);
    assert_eq!(of(7 * DAY), 180 * MINUTE);
    assert_eq!(of(14 * DAY), 360 * MINUTE);
    assert_eq!(of(30 * DAY), 720 * MINUTE);
    assert_eq!(of(60 * DAY), DAY);
    // Beyond that a bucket stays a day, and so does one of a range that has
    // no beginning to measure from.
    assert_eq!(of(400 * DAY), DAY);
    assert_eq!(step(None, BASE), DAY);
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
fn failures_are_classed_by_the_status_answered() {
    let mut failures = Failures::default();
    for (status, count) in [
        (Some(429), 1),
        (Some(400), 2),
        (Some(404), 4),
        (Some(500), 8),
        (Some(529), 16),
        // Never reached, or a response that began and was cut short.
        (None, 32),
        (Some(200), 64),
    ] {
        failures.add(status, count);
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
        spread(row, start, end, 5 * MINUTE, |bucket, count| {
            shares.push((bucket - BASE, count))
        });
        shares
    };
    // At 29:00, 29:48, 30:36, 31:24, 32:12 and 33:00.
    let row = failed(A, Some(429), BASE + 29 * MINUTE, BASE + 33 * MINUTE, 6);
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
    let burst = failed(A, Some(429), BASE + 29 * MINUTE, BASE + 29 * MINUTE, 7);
    assert_eq!(shares(&burst, i64::MIN, i64::MAX), [(25 * MINUTE, 7)]);
    assert_eq!(shares(&burst, BASE + 29 * MINUTE + 1, i64::MAX), []);
}

#[test]
fn a_provider_s_curves_join_what_succeeded_with_what_the_journal_says_failed() {
    let (_dir, store) = store();
    let curves = store.curves(&range(), 0, &journal(), BASE + DAY).unwrap();
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
    let curves = store.curves(&named, 0, &journal(), BASE + DAY).unwrap();
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
    let curves = store.curves(&every, 0, &journal(), BASE + DAY).unwrap();
    let other = curves.providers.iter().find(|p| p.id == B).unwrap();
    assert_eq!(other.failures.server, 4);
    assert_eq!(other.trend[2], Some(0.8));
}

#[test]
fn every_provider_comes_with_its_own_buckets() {
    let (_dir, store) = store();
    let curves = store.curves(&range(), 0, &journal(), BASE + DAY).unwrap();
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
fn nothing_is_said_of_failures_before_the_journal_reaches() {
    let (_dir, store) = store();
    let since = BASE + 30 * MINUTE;
    let curves = store
        .curves(&range(), since, &journal(), BASE + DAY)
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
    assert_eq!(
        failures,
        [
            (0, 3, None),
            (25 * MINUTE, 0, None),
            (30 * MINUTE, 0, Some(5)),
            (60 * MINUTE, 0, Some(2)),
        ]
    );
    let provider = curves.providers.iter().find(|p| p.id == A).unwrap();
    // A rate compares successes and failures of the same time.
    assert_eq!((provider.requests, provider.ok), (5, 0));
    assert_eq!(provider.failures.total(), 7);
    assert_eq!(provider.trend[0], None);
    // The journal's week and the page's are measured a moment apart.
    let curves = store
        .curves(&range(), BASE + MINUTE, &journal(), BASE + DAY)
        .unwrap();
    assert!(of(&curves, A).buckets[0].failures.is_some());
}

#[test]
fn a_range_without_a_beginning_starts_where_anything_is_known() {
    let (_dir, store) = store();
    let filter = Filter {
        start: None,
        ..range()
    };
    let curves = store.curves(&filter, 0, &journal(), BASE + DAY).unwrap();
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
    let curves = store.curves(&range(), 0, &[], BASE + DAY).unwrap();
    assert!(curves.providers.is_empty());
    assert_eq!((curves.start, curves.end), (BASE, BASE + 100 * MINUTE));
    let all = store
        .curves(&Filter::default(), 0, &[], BASE + DAY)
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
    let curves = store.curves(&filter, 0, &[], BASE + 40 * DAY).unwrap();
    let provider = of(&curves, A);
    assert_eq!((provider.requests, provider.ok), (5, 3));
    assert_eq!(provider.buckets.iter().map(|b| b.ok).sum::<u64>(), 3);
    assert_eq!(provider.first_token, none());
}
