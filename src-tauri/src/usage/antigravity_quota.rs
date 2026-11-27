//! Observed account quota boundaries. Never extrapolate a new window.
use super::{time, AntigravityQuotaTrack, AntigravityTrackWindow, Api, WindowAcc};
use crate::{
    antigravity::auth,
    storage::{AppError, Result},
    usage::model::failure,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

#[path = "antigravity_quota_cli.rs"]
mod cli;
#[path = "antigravity_cloud.rs"]
mod cloud;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", from = "Saved")]
pub struct Period {
    pub start: i64,
    pub end: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_percent: Option<f64>,
    /// Set where the account's quota was reported with no period to show;
    /// `start` and `end` say nothing then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub without: Option<Without>,
}
/// A period as it was written. An untouched quota comes with the Unix epoch
/// for its reset, and used to be saved as a period ending there.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Saved {
    start: i64,
    end: i64,
    #[serde(default)]
    remaining_percent: Option<f64>,
    #[serde(default)]
    without: Option<Without>,
}
impl From<Saved> for Period {
    fn from(saved: Saved) -> Self {
        if saved.without.is_none() && saved.end <= 0 {
            return Self {
                remaining_percent: saved.remaining_percent,
                without: Some(Without::NoReset),
                ..Self::default()
            };
        }
        Self {
            start: saved.start,
            end: saved.end,
            remaining_percent: saved.remaining_percent,
            without: saved.without,
        }
    }
}
/// Why a reported quota has no period.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Without {
    /// The account does not have this quota.
    Disabled,
    /// It came without the time it resets.
    NoReset,
}
impl Period {
    pub(in crate::usage) fn current(&self, now: i64) -> bool {
        self.without.is_some() || (self.start <= now && now < self.end)
    }
    fn contains(&self, at: i64) -> bool {
        self.start <= at && at < self.end
    }
}
pub type Periods = BTreeMap<String, Period>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsage {
    pub id: String,
    pub name: String,
    pub tracks: Vec<AntigravityQuotaTrack>,
    /// Always zero: every account is shown by its current quota periods.
    pub total: AntigravityTrackWindow,
}
fn parse_remaining_percent(bucket: &Value) -> Option<f64> {
    if let Some(fraction) = bucket["remainingFraction"]
        .as_f64()
        .or_else(|| bucket["remaining_fraction"].as_f64())
        .filter(|v| v.is_finite())
    {
        return Some(((fraction.clamp(0.0, 1.0) * 100.0) * 10.0).round() / 10.0);
    }
    bucket["remainingPercent"]
        .as_f64()
        .or_else(|| bucket["remaining_percent"].as_f64())
        .filter(|v| v.is_finite())
        .map(|pct| (pct.clamp(0.0, 100.0) * 10.0).round() / 10.0)
}
pub fn parse(value: &Value) -> Periods {
    periods(&value["response"]["groups"])
}
/// The periods of the quota groups, as the app's local service and
/// `agy /usage` both report them.
fn periods(groups: &Value) -> Periods {
    let mut periods = Periods::new();
    for b in groups
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["buckets"].as_array().into_iter().flatten())
    {
        let Some(id) = b["bucketId"].as_str().or_else(|| b["id"].as_str()) else {
            continue;
        };
        let duration = match id {
            "gemini-5h" | "3p-5h" => 5 * 3_600_000,
            "gemini-weekly" | "3p-weekly" => 7 * 86_400_000,
            _ => continue,
        };
        // The Unix epoch stands for no reset time: the quota is untouched.
        let end = time(&b["resetTime"])
            .or_else(|| time(&b["reset_time"]))
            .filter(|end| *end > 0);
        let disabled = b["disabled"] == true;
        let (Some(end), false) = (end, disabled) else {
            // A period reported for the same quota elsewhere stands.
            periods.entry(id.to_owned()).or_insert(Period {
                remaining_percent: parse_remaining_percent(b).filter(|_| !disabled),
                without: Some(if disabled {
                    Without::Disabled
                } else {
                    Without::NoReset
                }),
                ..Period::default()
            });
            continue;
        };
        let remaining_percent = parse_remaining_percent(b);
        let next = Period {
            start: end.saturating_sub(duration),
            end,
            remaining_percent,
            without: None,
        };
        match periods.get_mut(id) {
            Some(existing) if existing.without.is_some() => *existing = next,
            Some(existing) => {
                existing.start = next.start;
                existing.end = next.end;
                existing.remaining_percent = match (existing.remaining_percent, remaining_percent) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (Some(a), None) => Some(a),
                    (None, b) => b,
                };
            }
            None => {
                periods.insert(id.to_owned(), next);
            }
        }
    }
    periods
}
impl Api {
    pub(super) async fn quota(
        &self,
        data: &Path,
        home: &Path,
    ) -> Result<Option<(String, Periods)>> {
        let Some((id, email)) = auth::current_oauth_identity(data, home)? else {
            return Ok(None);
        };
        Ok(self.quota_for(&email).await?.map(|periods| (id, periods)))
    }
    async fn quota_for(&self, email: &str) -> Result<Option<Periods>> {
        // One instance that cannot answer must not hide the quota of the
        // instance signed in to the selected account.
        let mut last_error = None;
        for endpoint in &self.endpoints {
            let user = match self.call_on(endpoint, "GetUserStatus", &json!({})).await {
                Ok(user) => user,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            if !user["userStatus"]["email"]
                .as_str()
                .is_some_and(|e| e.eq_ignore_ascii_case(email))
            {
                continue;
            }
            match self
                .call_on(endpoint, "RetrieveUserQuotaSummary", &json!({}))
                .await
            {
                Ok(value) => return Ok(Some(parse(&value))),
                Err(error) => last_error = Some(error),
            }
        }
        last_error.map_or(Ok(None), Err)
    }
}
/// What asking one saved account for its quota came to.
pub(in crate::usage) struct Asked {
    pub id: String,
    /// The account Antigravity is signed in to.
    pub current: bool,
    pub answer: std::result::Result<Periods, AppError>,
}
/// The quota of every saved account, each asked of Antigravity's service with
/// its own login and handed to `settle` as soon as it has answered, so that
/// one that is slow keeps no other waiting. The signed-in account is read
/// from the running app or through `agy` where the service gave no answer.
pub(in crate::usage) async fn observe_all<T>(
    data: &Path,
    settle: impl Fn(Asked) -> T,
) -> Result<Vec<T>> {
    let from = data.to_path_buf();
    let logins = tokio::task::spawn_blocking(move || auth::quota_logins(&from))
        .await
        .map_err(|_| failure("Antigravity 额度查询中断"))??;
    Ok(ask_all(&logins, cloud::quota, || observe(data), settle).await)
}
async fn ask_all<'a, C, CF, O, OF, T>(
    logins: &'a [auth::QuotaLogin],
    cloud: C,
    local: O,
    settle: impl Fn(Asked) -> T,
) -> Vec<T>
where
    C: Fn(&'a auth::QuotaLogin) -> CF,
    CF: std::future::Future<Output = std::result::Result<Periods, cloud::Failure>>,
    O: Fn() -> OF,
    OF: std::future::Future<Output = Result<Option<(String, Periods)>>>,
{
    let (cloud, local, settle) = (&cloud, &local, &settle);
    futures_util::future::join_all(logins.iter().map(|login| async move {
        let answer = match cloud(login).await {
            Ok(periods) => Ok(periods),
            // The app and `agy` ask the same service, so they are left out
            // where it wants to be left alone.
            Err(failure) if login.current && !failure.stands() => match local().await {
                // They answer for whoever is signed in by now.
                Ok(Some((found, periods))) if found == login.id => Ok(periods),
                // The service was not asked, so why they were of no help
                // is all there is to say.
                Err(error) if failure.expired() => Err(error),
                // Otherwise why the service gave nothing says more.
                Ok(_) | Err(_) => Err(failure.error(true)),
            },
            Err(failure) => Err(failure.error(login.current)),
        };
        settle(Asked {
            id: login.id.clone(),
            current: login.current,
            answer,
        })
    }))
    .await
}
/// The quota of the signed-in account: from the running app, or from the
/// `agy` CLI when the app has no answer.
pub async fn observe(data: &Path) -> Result<Option<(String, Periods)>> {
    let home = crate::antigravity::home()?;
    // A signed-out `agy` opens the browser by itself, so nothing is asked
    // without a login.
    let Some((id, email)) = auth::current_oauth_identity(data, &home)? else {
        return Ok(None);
    };
    let from_app = match Api::connect(&home).await {
        Ok(Some(api)) => api.quota_for(&email).await,
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    };
    if let Ok(Some(periods)) = from_app {
        return Ok(Some((id, periods)));
    }
    let periods = cli::quota(&id).await?;
    Ok(Some((id, periods)))
}
pub(in crate::usage) struct AccountAccumulator {
    account: Account,
    periods: Periods,
    windows: BTreeMap<&'static str, WindowAcc>,
    now: i64,
}
impl AccountAccumulator {
    pub(in crate::usage) fn new(account: Account, mut periods: Periods, now: i64) -> Self {
        periods.retain(|_, p| p.current(now));
        Self {
            account,
            periods,
            windows: BTreeMap::new(),
            now,
        }
    }
    pub(in crate::usage) fn earliest(&self) -> Option<i64> {
        self.periods
            .values()
            .filter(|p| p.without.is_none())
            .map(|p| p.start)
            .min()
    }
    pub(super) fn ingest(&mut self, at: i64, track: Option<&str>, amounts: &WindowAcc) {
        let add = |w: &mut WindowAcc| {
            w.requests += amounts.requests;
            w.unpriced_requests += amounts.unpriced_requests;
            w.tokens = w.tokens.saturating_add(amounts.tokens);
            w.reasoning_tokens = w.reasoning_tokens.saturating_add(amounts.reasoning_tokens);
            w.cost += amounts.cost;
        };
        if self.periods.is_empty() {
            return;
        }
        let keys = match track {
            Some("gemini") => ["gemini-5h", "gemini-weekly"],
            Some("claude_gpt") => ["3p-5h", "3p-weekly"],
            _ => return,
        };
        for key in keys {
            if self.periods.get(key).is_some_and(|p| p.contains(at)) {
                add(self.windows.entry(key).or_default());
            }
        }
    }
    pub(super) fn finish(mut self) -> AccountUsage {
        let mut tracks = Vec::new();
        for (id, label, burst, weekly) in [
            ("gemini", "Gemini", "gemini-5h", "gemini-weekly"),
            ("claude_gpt", "Claude / GPT", "3p-5h", "3p-weekly"),
        ] {
            let mut window = |key: &str| {
                let spent = self.windows.remove(key).unwrap_or_default();
                let reported = self.periods.remove(key);
                let without = reported.as_ref().and_then(|p| p.without);
                let remaining_percent = reported.as_ref().and_then(|p| p.remaining_percent);
                let period = reported.filter(|p| p.without.is_none());
                let hourly = cost_per_hour(spent.cost, period.as_ref(), self.now);
                let mut w = spent.into_view();
                w.cost_per_hour = hourly;
                w.remaining_percent = remaining_percent;
                w.without = without;
                w.period = period;
                w
            };
            tracks.push(AntigravityQuotaTrack {
                id: id.into(),
                label: label.into(),
                burst_5h: window(burst),
                weekly_7d: window(weekly),
            });
        }
        AccountUsage {
            id: self.account.id,
            name: self.account.name,
            tracks,
            total: WindowAcc::default().into_view(),
        }
    }
}
/// Spend of the period so far for each hour since it began. The first minute
/// counts as a whole one, so a period that has just begun shows no runaway
/// rate.
fn cost_per_hour(cost: rust_decimal::Decimal, period: Option<&Period>, now: i64) -> String {
    use rust_decimal::{Decimal, RoundingStrategy};
    let Some(period) = period else {
        return "0".into();
    };
    let elapsed = now
        .saturating_sub(period.start)
        .min(period.end.saturating_sub(period.start))
        .max(60_000);
    let hours = Decimal::from(elapsed) / Decimal::from(3_600_000);
    super::fmt_usd((cost / hours).round_dp_with_strategy(4, RoundingStrategy::MidpointAwayFromZero))
}

#[cfg(test)]
#[path = "antigravity_quota_tests.rs"]
mod tests;
