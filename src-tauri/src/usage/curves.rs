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
//!
//! The providers on one host are given a second time as one, added up. Which
//! of the two is shown is for whoever reads the answer to choose.
//!
//! What reached no gateway is given apart: the requests of an Antigravity
//! account and those of a Claude Code session. The journal knows nothing of
//! them. The usage records tell of every attempt of theirs, of one that
//! failed as of one that succeeded, so what failed is read from the records
//! and is known over the whole range.
use super::{
    model::{verdict, Filter},
    query, storage_v12,
};
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
/// buckets to draw it with. The providers on one host, taken as one, are said
/// in the same.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    /// The host it is on now. Not said of a provider that no longer exists:
    /// where it was is kept nowhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The providers taken as one, in the order they are listed in. Not said
    /// of a provider by itself.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    /// Said of what reached no gateway alone: its failures are read from the
    /// usage records, as its successes are, and so are known over the whole
    /// range, wherever the journal begins. Every bucket of it has them, and
    /// counts all that it completed there.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub recorded: bool,
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
    /// what succeeded and what failed from here on. At `end` or after it,
    /// none of the range is known.
    pub failures_since: i64,
    /// What the journal says went wrong with it, if anything: failures it
    /// could not read or was never told are then missing from the counts.
    /// The records know nothing of it; whoever asked the journal says so.
    pub journal_error: Option<String>,
    /// The providers that had traffic or failed, the most rate-limited first.
    /// Each comes with its buckets, so that choosing among them asks nothing
    /// more.
    pub providers: Vec<Provider>,
    /// Every host two or more of those providers are on, with them taken as
    /// one: `host:` and the host for an id, in the same order. Each of them
    /// is still among `providers`, which is all a page that knows nothing of
    /// hosts reads.
    pub groups: Vec<Provider>,
    /// What reached no gateway, and so is none of `providers`: the attempts
    /// of an Antigravity account under the account's id, and those that name
    /// no account under `session:` and their client. In the same order.
    pub direct: Vec<Provider>,
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
/// What the usage records say of one account that reached no gateway, or of
/// the sessions of one client that name none.
#[derive(Default)]
struct Direct {
    answered: Answered,
    /// What failed, by bucket.
    lost: BTreeMap<i64, Failures>,
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
/// How an attempt the usage records say failed ended: by its status where
/// that is one of an error, and by the error it gave where not. Antigravity
/// writes a status it was answered with as that error; the names are those
/// it and Claude Code give a limit, a refusal and a fault of the service.
fn kind(status: Option<u16>, error_code: Option<&str>) -> FailureKind {
    let refused = |status: u16| match status {
        429 => Some(FailureKind::Limited),
        400..=499 => Some(FailureKind::Rejected),
        500..=599 => Some(FailureKind::Server),
        _ => None,
    };
    let written = error_code
        .filter(|code| code.len() == 3 && code.bytes().all(|digit| digit.is_ascii_digit()))
        .and_then(|code| code.parse().ok());
    if let Some(kind) = status
        .and_then(refused)
        .or_else(|| written.and_then(refused))
    {
        return kind;
    }
    match error_code {
        Some("RESOURCE_EXHAUSTED" | "rate_limit") => FailureKind::Limited,
        Some("UNAUTHENTICATED" | "authentication_failed" | "invalid_request" | "billing_error") => {
            FailureKind::Rejected
        }
        Some("UPSTREAM_ERROR" | "server_error") => FailureKind::Server,
        _ => FailureKind::Broken,
    }
}
/// What the usage records have of what reached no gateway in the filter's
/// range and client: each attempt under the account that made it, or under
/// `session:` and its client where it names none. A request is one of its
/// last attempt's and ended as `verdict` says of that. An attempt another
/// followed failed where its own status and outcome say so, whatever became
/// of the request: it is counted where the request is, against its own
/// account.
fn direct(
    c: &Connection,
    f: &Filter,
    client: Option<&str>,
    grid: Grid,
) -> Result<BTreeMap<String, Direct>> {
    let mut series: BTreeMap<String, Direct> = BTreeMap::new();
    // The same times `scope` has the gateway's requests of.
    let (start, end) = (f.start.unwrap_or(0), f.end.unwrap_or(i64::MAX));
    storage_v12::scan_direct_attempts(c, client, start, end, |attempt| {
        // Whether the request completed is said of its last attempt alone.
        let completed = attempt.completed || !attempt.is_final;
        let ended = verdict(attempt.source, attempt.status, attempt.outcome, completed);
        // An attempt another followed is counted only where it failed.
        if !attempt.is_final && ended != Some(false) {
            return Ok(());
        }
        let id = attempt
            .provider
            .unwrap_or_else(|| format!("session:{}", attempt.client));
        let told = series.entry(id).or_default();
        let bucket = grid.bucket(attempt.time);
        if attempt.is_final {
            let seen = told.answered.buckets.entry(bucket).or_default();
            seen.requests += 1;
            if ended == Some(true) {
                seen.ok += 1;
                // The records know all of the range, wherever the journal begins.
                seen.ok_since += 1;
            }
            seen.first_token.extend(attempt.first_token);
        }
        if ended == Some(false) {
            let lost = told.lost.entry(bucket).or_default();
            lost.add(kind(attempt.status, attempt.error_code), 1);
        }
        Ok(())
    })
    .map_err(|_| failed())?;
    Ok(series)
}
fn failed() -> crate::storage::AppError {
    super::model::failure("用量曲线查询失败")
}

/// One row of the answer from what `answered` and `lost` have of a provider,
/// or of several taken as one. Of several, what succeeded and what failed
/// are added up bucket by bucket before a share is taken of them, and the
/// times to the first token are put together before a percentile is: neither
/// is an average of what each of them had. `stretch` says which of the
/// trend's `stretches` a bucket falls in, and `known` whether what failed in
/// it is known.
fn series(
    id: String,
    answered: &[&Answered],
    lost: &[&BTreeMap<i64, Failures>],
    stretches: usize,
    stretch: impl Fn(i64) -> usize,
    known: impl Fn(i64) -> bool,
) -> Provider {
    let mut provider = Provider {
        id,
        host: None,
        members: Vec::new(),
        recorded: false,
        requests: 0,
        ok: 0,
        failures: Failures::default(),
        trend: Vec::new(),
        first_token: FirstToken::default(),
        // The last such day of any of them: up to there some of their times
        // are missing from the percentiles.
        summed_until: answered
            .iter()
            .filter_map(|answered| answered.summed)
            .max()
            .map(day_end),
        buckets: Vec::new(),
    };
    let mut trend = vec![(0u64, 0u64); stretches];
    // Both are of the time since the journal began, so that a rate
    // compares attempts of one time.
    for (bucket, seen) in answered.iter().flat_map(|answered| &answered.buckets) {
        provider.requests += seen.requests;
        provider.ok += seen.ok_since;
        trend[stretch(*bucket)].0 += seen.ok_since;
    }
    for (bucket, failures) in lost.iter().flat_map(|lost| lost.iter()) {
        provider.failures.merge(failures);
        trend[stretch(*bucket)].1 += failures.total();
    }
    provider.trend = trend
        .into_iter()
        .map(|(ok, failed)| {
            (ok + failed > 0).then(|| (failed as f64 / (ok + failed) as f64 * 1e4).round() / 1e4)
        })
        .collect();
    let times: std::collections::BTreeSet<i64> = answered
        .iter()
        .flat_map(|answered| answered.buckets.keys())
        .chain(lost.iter().flat_map(|lost| lost.keys()))
        .copied()
        .collect();
    let mut all = Vec::new();
    provider.buckets = times
        .into_iter()
        .map(|at| {
            let seen = || {
                answered
                    .iter()
                    .filter_map(|answered| answered.buckets.get(&at))
            };
            let mut first_token: Vec<u64> =
                seen().flat_map(|seen| &seen.first_token).copied().collect();
            all.extend_from_slice(&first_token);
            let known = known(at);
            Bucket {
                at,
                ok: seen()
                    .map(|seen| if known { seen.ok_since } else { seen.ok })
                    .sum(),
                failures: known.then(|| {
                    let mut sum = Failures::default();
                    for failures in lost.iter().filter_map(|lost| lost.get(&at)) {
                        sum.merge(failures);
                    }
                    sum
                }),
                first_token: FirstToken::of(&mut first_token),
            }
        })
        .collect();
    provider.first_token = FirstToken::of(&mut all);
    provider
}
/// The most rate-limited first, then the one that failed most, then the one
/// asked most.
fn order(a: &Provider, b: &Provider) -> std::cmp::Ordering {
    (b.failures.limited, b.failures.total(), b.requests)
        .cmp(&(a.failures.limited, a.failures.total(), a.requests))
        .then_with(|| a.id.cmp(&b.id))
}

/// The curves of every provider in the filter's range and client, whichever
/// provider it names, and of what reached no gateway there. `failures` are
/// the journal's rows that reach into the range, and `failures_since` the
/// time the journal holds them all from, in milliseconds. `hosts` says which
/// host each provider that still exists is on, by its id. Before
/// `detail_since` a day's requests may be kept only as a total.
pub fn read(
    c: &Connection,
    f: &Filter,
    failures_since: i64,
    failures: &[UpstreamFailure],
    hosts: &BTreeMap<String, String>,
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
    let direct = direct(c, f, client, grid)?;
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
                .chain(direct.values().map(|direct| &direct.answered))
                .map(|answered| earliest(answered.buckets.keys().next()))
                .chain(
                    failed
                        .values()
                        .chain(direct.values().map(|direct| &direct.lost))
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
    let stretch = |bucket: i64| (place(bucket) * stretches / count).min(stretches - 1);
    // The journal says what failed in a bucket where the part of it that was
    // asked for reaches past where the journal begins.
    let known = |bucket: i64| grid.end(bucket).min(asked_end.saturating_add(1)) > failures_since;
    // One row of all the records and the journal have of `members`.
    let of = |id: String, members: &[&str]| {
        let answered: Vec<_> = members.iter().filter_map(|id| usage.get(*id)).collect();
        let lost: Vec<_> = members.iter().filter_map(|id| failed.get(*id)).collect();
        series(id, &answered, &lost, stretches, stretch, known)
    };
    let ids: std::collections::BTreeSet<&str> = usage
        .keys()
        .map(String::as_str)
        .chain(failed.keys().copied())
        .collect();
    let mut providers: Vec<Provider> = ids
        .into_iter()
        .map(|id| Provider {
            host: hosts.get(id).cloned(),
            ..of(id.to_owned(), &[id])
        })
        .collect();
    providers.sort_by(order);
    // A provider is here only for having had traffic or failed in the range,
    // so a host comes to a row of its own by two that did.
    let mut shared: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for provider in &providers {
        if let Some(host) = &provider.host {
            shared.entry(host).or_default().push(&provider.id);
        }
    }
    let mut groups: Vec<Provider> = shared
        .into_iter()
        .filter(|(_, members)| members.len() > 1)
        .map(|(host, members)| Provider {
            host: Some(host.to_owned()),
            members: members.iter().map(|id| (*id).to_owned()).collect(),
            ..of(format!("host:{host}"), &members)
        })
        .collect();
    groups.sort_by(order);
    // The records say what failed of these in every bucket, whatever the
    // journal has.
    let mut direct: Vec<Provider> = direct
        .into_iter()
        .map(|(id, direct)| Provider {
            recorded: true,
            ..series(
                id,
                &[&direct.answered],
                &[&direct.lost],
                stretches,
                stretch,
                |_| true,
            )
        })
        .collect();
    direct.sort_by(order);
    let end = grid.end(last);
    Ok(Curves {
        start: first,
        end,
        step,
        // A journal that begins after the range asked for knows nothing of
        // it, though it may begin before the last bucket ends. Said to begin
        // no earlier than that end, it leaves none of what is drawn known.
        failures_since: if failures_since > asked_end {
            failures_since.max(end)
        } else {
            failures_since
        },
        journal_error: None,
        providers,
        groups,
        direct,
    })
}

#[cfg(test)]
#[path = "curves_tests.rs"]
mod tests;
