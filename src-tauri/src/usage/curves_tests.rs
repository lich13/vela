// 覆盖供应商曲线：分桶、失败分类、首字分位数、日志覆盖范围与无数据。
use super::super::{model::*, query, store::Store};
use super::{spread, step, Bucket, Curves, Failures, FirstToken, Grid, Provider};
use crate::{
    events::{FailureKind, FailureKind::*, UpstreamFailure},
    gateway::ClientId,
};

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
        let curves = store.curves(&filter, 0, &[], start + span).unwrap();
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
fn the_answer_names_its_parts_as_the_page_reads_them() {
    let (_dir, store) = store();
    let curves = store.curves(&range(), 0, &journal(), BASE + DAY).unwrap();
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
            "id",
            "ok",
            "requests",
            "summedUntil",
            "trend"
        ]
    );
    assert_eq!(
        names(&provider["buckets"][0]),
        ["at", "failures", "firstToken", "ok"]
    );
    assert_eq!(
        names(&provider["failures"]),
        ["broken", "limited", "rejected", "server"]
    );
    assert_eq!(names(&provider["firstToken"]), ["p50", "p90", "samples"]);
    // The journal has said nothing went wrong, and every request is kept
    // one by one.
    assert!(json["journalError"].is_null());
    assert!(provider["summedUntil"].is_null());
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
fn the_bucket_the_journal_begins_within_counts_from_where_it_begins() {
    let (_dir, store) = store();
    // After the first of the three requests of the first five minutes.
    let since = BASE + 2 * MINUTE;
    let curves = store
        .curves(&range(), since, &journal(), BASE + DAY)
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
        .curves(&filter, since, &journal, day + 2 * DAY)
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
    let curves = store.curves(&filter, 0, &[], BASE + 40 * DAY).unwrap();
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
    let curves = store.curves(&filter, 0, &[], BASE + 40 * DAY).unwrap();
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
        .curves(&old("codex"), 0, &[], BASE + 40 * DAY)
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
    let curves = store.curves(&recent, 0, &[], BASE + 40 * DAY).unwrap();
    assert_eq!(curves.step, 5 * MINUTE);
    assert_eq!(of(&curves, A).first_token.samples, 1);
    // Requests of Claude Code are kept one by one for good, so the same old
    // days of them have no total among them.
    let (_dir, mut store) = store_of("claude");
    store.compact(BASE + 40 * DAY).unwrap();
    let curves = store
        .curves(&old("claude"), 0, &[], BASE + 40 * DAY)
        .unwrap();
    assert_eq!(curves.step, 120 * MINUTE);
    assert_eq!(of(&curves, A).first_token.samples, 3);
}
