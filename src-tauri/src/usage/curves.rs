//! A provider's curves over a range: how often its attempts failed, and how
//! soon it began to answer.
//!
//! What succeeded and how long the first token took are read from the usage
//! records. What failed is read from the journal: a provider in endless mode
//! leaves no usage record of a rejection, so the records alone would show the
//! provider that is rejected most as one that never fails. The journal is
//! kept for less time than the records, and where it no longer reaches
//! nothing is said of failures, which is not the same as there having been
//! none. Where it begins within a bucket, the bucket counts what succeeded
//! from there as well, so that a rate is of attempts of one time.
use super::{model::Filter, query};
use crate::{
    events::{FailureKind, UpstreamFailure},
    storage::Result,
};
use chrono::TimeZone;
use rusqlite::{types::Value, Connection};
use serde::Serialize;
use std::collections::BTreeMap;

const MINUTE: i64 = 60_000;
const DAY: i64 = 86_400_000;
/// The widths a bucket may have. A range is divided by the narrowest that
/// keeps it within `BUCKETS`; the narrowest of all is no shorter than the
/// time over which the journal merges like failures into one row.
const STEPS: [i64; 10] = [
    5 * MINUTE,
    10 * MINUTE,
    15 * MINUTE,
    30 * MINUTE,
    60 * MINUTE,
    120 * MINUTE,
    180 * MINUTE,
    360 * MINUTE,
    720 * MINUTE,
    DAY,
];
const BUCKETS: i64 = 60;
/// How many stretches a provider's trend divides the range into at most.
const TREND: usize = 20;

/// Where the buckets of one answer begin and end.
#[derive(Clone, Copy)]
struct Grid {
    step: i64,
    /// How far local time stood ahead of UTC at the end of the range. Every
    /// bucket shorter than a day keeps to it: taken anew at each moment,
    /// those on either side of a change of the clocks would overlap.
    offset: i64,
}
impl Grid {
    fn new(step: i64, end: i64) -> Self {
        let offset = chrono::Local
            .timestamp_millis_opt(end)
            .single()
            .map_or(0, |local| {
                i64::from(local.offset().local_minus_utc()) * 1000
            });
        Self { step, offset }
    }
    /// Where the bucket `at` falls in begins. A day begins at its own
    /// midnight, whatever the clocks did within it.
    fn bucket(self, at: i64) -> i64 {
        if self.step == DAY {
            query::bucket(at, DAY)
        } else {
            (at + self.offset).div_euclid(self.step) * self.step - self.offset
        }
    }
    /// Where the bucket that begins at `bucket` ends.
    fn end(self, bucket: i64) -> i64 {
        if self.step == DAY {
            day_end(bucket)
        } else {
            bucket.saturating_add(self.step)
        }
    }
}
/// Where the day that begins at `day` ends. A day lasts from 23 hours to 25,
/// so a day and a half on is in the next one.
fn day_end(day: i64) -> i64 {
    query::bucket(day.saturating_add(DAY + DAY / 2), DAY)
}

/// Failed attempts by how they ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Failures {
    /// Rate limited or out of capacity: a 429, or the same told within a
    /// response that had begun.
    pub limited: u64,
    /// Any other 4xx.
    pub rejected: u64,
    /// 5xx.
    pub server: u64,
    /// Neither a limit nor an error status: it could not be reached, did not
    /// answer in time, or began a response and did not finish it.
    pub broken: u64,
}
impl Failures {
    fn add(&mut self, kind: FailureKind, count: u64) {
        *match kind {
            FailureKind::Limited => &mut self.limited,
            FailureKind::Rejected => &mut self.rejected,
            FailureKind::Server => &mut self.server,
            FailureKind::Broken => &mut self.broken,
        } += count;
    }
    fn merge(&mut self, other: &Self) {
        self.limited += other.limited;
        self.rejected += other.rejected;
        self.server += other.server;
        self.broken += other.broken;
    }
    fn total(&self) -> u64 {
        self.limited + self.rejected + self.server + self.broken
    }
}

/// Times to the first token in milliseconds, of the requests that have one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirstToken {
    pub samples: u64,
    pub p50: Option<u64>,
    pub p90: Option<u64>,
}
impl FirstToken {
    fn of(values: &mut [u64]) -> Self {
        values.sort_unstable();
        Self {
            samples: values.len() as u64,
            p50: percentile(values, 50),
            p90: percentile(values, 90),
        }
    }
}
/// The value `percent` of `sorted` is at or below, by nearest rank: one of
/// the values themselves, the lower of the middle two for an even median.
fn percentile(sorted: &[u64], percent: usize) -> Option<u64> {
    let rank = (sorted.len() * percent).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    pub at: i64,
    /// Requests the provider completed. Where the journal begins within the
    /// bucket, those from where it begins, as its failures are.
    pub ok: u64,
    /// None where the journal no longer reaches.
    pub failures: Option<Failures>,
    pub first_token: FirstToken,
}

/// What one provider did over the range: the totals to choose it by and the
/// buckets to draw it with.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    /// Requests it was the last to be asked for, over the whole range.
    pub requests: u64,
    /// Requests it completed in the time failures are known for, so that the
    /// two are of the same time.
    pub ok: u64,
    pub failures: Failures,
    /// The share of attempts that failed in each stretch of the range, none
    /// where no attempt is known to have ended.
    pub trend: Vec<Option<f64>>,
    /// Over the whole range.
    pub first_token: FirstToken,
    /// Where the last day ends of which only a total is kept of some of its
    /// requests: up to there a percentile of the first token is of the rest
    /// alone, if any are kept one by one. None where all of them in the
    /// range are.
    pub summed_until: Option<i64>,
    /// Only those in which something ended, in order of time.
    pub buckets: Vec<Bucket>,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Curves {
    /// Where the first bucket begins and the last one ends.
    pub start: i64,
    pub end: i64,
    pub step: i64,
    /// From when failures are known. The bucket this falls within counts
    /// what succeeded and what failed from here on.
    pub failures_since: i64,
    /// The providers that had traffic or failed, the most rate-limited first.
    /// Each comes with its buckets, so that choosing among them asks nothing
    /// more.
    pub providers: Vec<Provider>,
}

/// What the usage records say of one provider in one bucket.
#[derive(Default)]
struct Seen {
    requests: u64,
    ok: u64,
    /// Those of `ok` that began once the journal had.
    ok_since: u64,
    first_token: Vec<u64>,
}
/// What the usage records say of one provider.
#[derive(Default)]
struct Answered {
    buckets: BTreeMap<i64, Seen>,
    /// Where the last day begins of which only a total is kept.
    summed: Option<i64>,
}

/// The narrowest width at which `start..=end` reaches into no more than
/// `BUCKETS`, the buckets its two ends fall in both counted: a range that
/// does not begin where a bucket does reaches into one more than its length
/// fills. One too long for any width is divided into days, however many, as
/// is one with no beginning to measure from.
fn step(start: Option<i64>, end: i64) -> i64 {
    let Some(start) = start else {
        return DAY;
    };
    STEPS
        .into_iter()
        .find(|&step| {
            let grid = Grid::new(step, end);
            (grid.bucket(end) - grid.bucket(start)) / step < BUCKETS
        })
        .unwrap_or(DAY)
}

/// How many of a row's attempts came before `at`, taking them as evenly
/// spread from its first to its last.
fn before(row: &UpstreamFailure, at: i64) -> u64 {
    let (first, last) = (row.first_at as i64 * 1000, row.last_at as i64 * 1000);
    let count = u64::from(row.count);
    if at <= first {
        0
    } else if at > last || count < 2 {
        count
    } else {
        // Numbered from nothing, attempt k of n came at
        // first + k × (last − first) ÷ (n − 1).
        ((at - first) as u128 * u128::from(count - 1)).div_ceil((last - first) as u128) as u64
    }
}
/// Gives each bucket the attempts of `row` that fell in it and in
/// `start..=end`. A row is merged for less time than the narrowest bucket
/// lasts, so it reaches into two at most.
fn spread(row: &UpstreamFailure, start: i64, end: i64, grid: Grid, mut add: impl FnMut(i64, u64)) {
    let within = |from: i64, to: i64| before(row, to.max(from)) - before(row, from);
    let until = end.saturating_add(1);
    let first = grid.bucket(row.first_at as i64 * 1000);
    let last = grid.bucket(row.last_at as i64 * 1000);
    let shares = if first == last {
        [(first, within(start, until)), (last, 0)]
    } else {
        [
            (first, within(start, last.min(until))),
            (last, within(start.max(last), until)),
        ]
    };
    for (bucket, count) in shares {
        if count > 0 {
            add(bucket, count);
        }
    }
}

/// What of the usage records the journal's failures are compared with, as
/// `query::selection` words it. The journal knows a failure's client,
/// provider and time and nothing else of the request, so nothing else
/// narrows them.
fn scope(f: &Filter) -> (String, Vec<Value>) {
    query::selection(
        &Filter {
            source: Some("proxy".into()),
            start: f.start,
            end: f.end,
            client: f.client.clone(),
            ..Filter::default()
        },
        "",
    )
}
/// Whether any of `scope` is a day of which only a total is kept.
fn summed(c: &Connection, (cte, args): &(String, Vec<Value>)) -> Result<bool> {
    let sql = query::native_sql(format!(
        "{cte} SELECT 1 FROM m WHERE kind=1 AND provider IS NOT NULL AND requests>0 LIMIT 1"
    ));
    c.prepare(&sql)
        .and_then(|mut statement| statement.exists(rusqlite::params_from_iter(args.iter())))
        .map_err(|_| failed())
}
/// What the usage records have of each provider: its requests by bucket, and
/// up to where only totals are kept of them.
fn usage(
    c: &Connection,
    (cte, args): &(String, Vec<Value>),
    grid: Grid,
    failures_since: i64,
) -> Result<BTreeMap<String, Answered>> {
    // Only the last attempt of a request says how the request ended.
    let sql = query::native_sql(format!(
        "{cte} SELECT provider,time,kind,requests,success,first_token_sum_ms,first_token_samples FROM m WHERE provider IS NOT NULL AND requests>0"
    ));
    let mut statement = c.prepare(&sql).map_err(|_| failed())?;
    let mut rows = statement
        .query(rusqlite::params_from_iter(args.iter()))
        .map_err(|_| failed())?;
    let mut providers: BTreeMap<String, Answered> = BTreeMap::new();
    while let Some(row) = rows.next().map_err(|_| failed())? {
        let read = || -> rusqlite::Result<_> {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, u64>(3)?,
                row.get::<_, u64>(4)?,
                row.get::<_, u64>(5)?,
                row.get::<_, u64>(6)?,
            ))
        };
        let (provider, time, kind, requests, ok, first_token, samples) =
            read().map_err(|_| failed())?;
        let answered = providers.entry(provider).or_default();
        // A day's total keeps the sum of its requests' times, which no
        // percentile can be read from.
        let total = kind == 1;
        if total {
            answered.summed = answered.summed.max(Some(query::bucket(time, DAY)));
        }
        let seen = answered.buckets.entry(grid.bucket(time)).or_default();
        seen.requests += requests;
        seen.ok += ok;
        if time >= failures_since {
            seen.ok_since += ok;
        }
        if !total && samples == 1 {
            seen.first_token.push(first_token);
        }
    }
    Ok(providers)
}
fn failed() -> crate::storage::AppError {
    super::model::failure("用量曲线查询失败")
}

/// The curves of every provider in the filter's range and client, whichever
/// provider it names. `failures` are the journal's rows that reach into the
/// range, and `failures_since` the time the journal holds them all from, in
/// milliseconds. Before `detail_since` a day's requests may be kept only as
/// a total.
pub fn read(
    c: &Connection,
    f: &Filter,
    failures_since: i64,
    failures: &[UpstreamFailure],
    detail_since: i64,
    now: i64,
) -> Result<Curves> {
    let asked_end = f.end.unwrap_or(now);
    let scope = scope(f);
    let mut step = step(f.start, asked_end);
    // A day kept only as a total has no time of day to stand at: in narrower
    // buckets the whole of it would be put at midnight. Such days are all
    // before `detail_since`, so a range that begins later is not searched
    // for one.
    if step < DAY && f.start.is_some_and(|start| start < detail_since) && summed(c, &scope)? {
        step = DAY;
    }
    let grid = Grid::new(step, asked_end);
    let usage = usage(c, &scope, grid, failures_since)?;
    let mut failed: BTreeMap<&str, BTreeMap<i64, Failures>> = BTreeMap::new();
    let client = f.client.as_deref().filter(|client| !client.is_empty());
    // Of a row the journal begins within, only what came since: the rest of
    // that time has gone from it.
    let failed_from = f.start.unwrap_or(i64::MIN).max(failures_since);
    for row in failures {
        if client.is_some_and(|client| client != row.client.usage_name()) {
            continue;
        }
        spread(row, failed_from, asked_end, grid, |bucket, count| {
            failed
                .entry(&row.provider)
                .or_default()
                .entry(bucket)
                .or_default()
                .add(row.kind, count)
        });
    }
    let last = grid.bucket(asked_end);
    // With no beginning asked for, the range begins where anything is known.
    let first = f.start.map_or_else(
        || {
            let earliest = |buckets: Option<&i64>| buckets.copied().unwrap_or(last);
            usage
                .values()
                .map(|answered| earliest(answered.buckets.keys().next()))
                .chain(
                    failed
                        .values()
                        .map(|buckets| earliest(buckets.keys().next())),
                )
                .min()
                .unwrap_or(last)
                .min(last)
        },
        |start| grid.bucket(start),
    );
    // Days differ in length where clocks change, so a bucket's place is the
    // nearest whole number of steps from the first.
    let place = |bucket: i64| ((bucket - first) as f64 / step as f64).round().max(0.0) as usize;
    let count = place(last) + 1;
    let stretches = count.min(TREND);
    // The journal says what failed in a bucket where the part of it that was
    // asked for reaches past where the journal begins.
    let known = |bucket: i64| grid.end(bucket).min(asked_end.saturating_add(1)) > failures_since;
    let ids: std::collections::BTreeSet<&str> = usage
        .keys()
        .map(String::as_str)
        .chain(failed.keys().copied())
        .collect();
    let mut providers: Vec<Provider> = ids
        .into_iter()
        .map(|id| {
            let (answered, lost) = (usage.get(id), failed.get(id));
            let buckets = answered.map(|answered| &answered.buckets);
            let mut provider = Provider {
                id: id.to_owned(),
                requests: 0,
                ok: 0,
                failures: Failures::default(),
                trend: Vec::new(),
                first_token: FirstToken::default(),
                summed_until: answered.and_then(|answered| answered.summed).map(day_end),
                buckets: Vec::new(),
            };
            let mut trend = vec![(0u64, 0u64); stretches];
            let stretch = |bucket: i64| (place(bucket) * stretches / count).min(stretches - 1);
            // Both are of the time since the journal began, so that a rate
            // compares attempts of one time.
            for (bucket, seen) in buckets.into_iter().flatten() {
                provider.requests += seen.requests;
                provider.ok += seen.ok_since;
                trend[stretch(*bucket)].0 += seen.ok_since;
            }
            for (bucket, failures) in lost.into_iter().flatten() {
                provider.failures.merge(failures);
                trend[stretch(*bucket)].1 += failures.total();
            }
            provider.trend = trend
                .into_iter()
                .map(|(ok, failed)| {
                    (ok + failed > 0)
                        .then(|| (failed as f64 / (ok + failed) as f64 * 1e4).round() / 1e4)
                })
                .collect();
            let times: std::collections::BTreeSet<i64> = buckets
                .into_iter()
                .flat_map(|buckets| buckets.keys().copied())
                .chain(lost.into_iter().flat_map(|buckets| buckets.keys().copied()))
                .collect();
            let mut all = Vec::new();
            provider.buckets = times
                .into_iter()
                .map(|at| {
                    let seen = buckets.and_then(|buckets| buckets.get(&at));
                    let mut first_token =
                        seen.map_or_else(Vec::new, |seen| seen.first_token.clone());
                    all.extend_from_slice(&first_token);
                    let known = known(at);
                    Bucket {
                        at,
                        ok: seen.map_or(0, |seen| if known { seen.ok_since } else { seen.ok }),
                        failures: known.then(|| {
                            lost.and_then(|buckets| buckets.get(&at))
                                .copied()
                                .unwrap_or_default()
                        }),
                        first_token: FirstToken::of(&mut first_token),
                    }
                })
                .collect();
            provider.first_token = FirstToken::of(&mut all);
            provider
        })
        .collect();
    providers.sort_by(|a, b| {
        (b.failures.limited, b.failures.total(), b.requests)
            .cmp(&(a.failures.limited, a.failures.total(), a.requests))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(Curves {
        start: first,
        end: grid.end(last),
        step,
        failures_since,
        providers,
    })
}

#[cfg(test)]
#[path = "curves_tests.rs"]
mod tests;
