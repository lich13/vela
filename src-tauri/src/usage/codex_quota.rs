//! The quota of a ChatGPT account, asked of ChatGPT with the account's login
//! as Codex asks it. The login is sent as it is and never renewed: ChatGPT
//! takes a refresh token once, so renewing here would end the login Codex or
//! the saved account holds. Nothing here is written to disk or to a log.
use super::Remaining;
use crate::{
    core::{plan_label, QuotaLogin},
    storage::Result,
    usage::model::{failure, limited_until, now},
};
use parking_lot::Mutex;
use reqwest::header::ACCEPT;
use serde_json::Value;
use std::{collections::HashMap, sync::LazyLock, time::Duration};

const ENDPOINT: &str = "https://chatgpt.com/backend-api/wham/usage";
const USER_AGENT: &str =
    "codex-tui/0.149.1 (Mac OS 26.5.2; arm64) iTerm.app/3.6.11 (codex-tui; 0.149.1)";
const CONNECT_LIMIT: Duration = Duration::from_secs(5);
const REQUEST_LIMIT: Duration = Duration::from_secs(10);
/// The two periods an answer may have, and how many seconds each lasts where
/// the answer does not say.
const WINDOWS: [(&str, i64); 2] = [
    ("primary_window", 5 * 3600),
    ("secondary_window", 7 * 86_400),
];
// These are shown in a menu under the account's own name, so they are short.
/// What is said of a login that has ended or that ChatGPT no longer takes.
/// What was read with it before says nothing of the account any longer.
pub(crate) const OVER: &str = "登录已过期，请重新登录";
const LIMITED: &str = "查询受限，请稍后重试";

/// What ChatGPT says is left of an account's quota.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct Quota {
    /// The plan the answer names, as it is shown.
    pub plan: Option<String>,
    /// The periods the answer has, the shorter one first.
    pub periods: Vec<Remaining>,
}

struct ChatGpt {
    client: reqwest::Client,
    endpoint: String,
    /// Until when each account is not asked for.
    limited: Mutex<HashMap<String, i64>>,
}

static CHATGPT: LazyLock<ChatGpt> = LazyLock::new(|| ChatGpt::new(ENDPOINT.to_owned()));

/// The quota of the account `login` belongs to. The login is never renewed.
pub(crate) async fn quota(login: &QuotaLogin) -> Result<Quota> {
    CHATGPT.quota(login).await
}

impl ChatGpt {
    fn new(endpoint: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(CONNECT_LIMIT)
                .timeout(REQUEST_LIMIT)
                .build()
                .expect("Codex 额度连接初始化失败"),
            endpoint,
            limited: Mutex::default(),
        }
    }

    async fn quota(&self, login: &QuotaLogin) -> Result<Quota> {
        // A token past its end would only be refused, and nothing here may
        // renew it.
        let ended = login.expires_at.is_some_and(|ends| {
            i64::try_from(ends).is_ok_and(|ends| ends.saturating_mul(1000) <= now())
        });
        if ended {
            return Err(failure(OVER));
        }
        let account = login.key.as_str();
        let limited = self
            .limited
            .lock()
            .get(account)
            .is_some_and(|until| *until > now());
        if limited {
            return Err(failure(LIMITED));
        }
        let mut request = self
            .client
            .get(&self.endpoint)
            .bearer_auth(&login.access_token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(ACCEPT, "application/json");
        if !login.workspace.is_empty() {
            request = request.header("ChatGPT-Account-Id", &login.workspace);
        }
        let response = request
            .send()
            .await
            .map_err(|_| failure("额度服务没有应答"))?;
        let value = match response.status().as_u16() {
            200..=299 => response
                .json::<Value>()
                .await
                .ok()
                .filter(Value::is_object)
                .ok_or_else(|| failure("额度响应无法解析"))?,
            401 => return Err(failure(OVER)),
            403 => return Err(failure("查询被拒绝")),
            429 => {
                let until = limited_until(response.headers());
                self.limited.lock().insert(account.to_owned(), until);
                return Err(failure(LIMITED));
            }
            _ => return Err(failure("额度服务暂不可用")),
        };
        Ok(Quota {
            plan: plan(&value).or_else(|| login.plan.clone()),
            periods: periods(&value["rate_limit"]),
        })
    }
}

/// The plan the answer names, shown by the rule a login's plan is shown by.
fn plan(value: &Value) -> Option<String> {
    let plan = value["plan_type"]
        .as_str()
        .and_then(crate::events::safe_model)
        .filter(|plan| plan.len() <= 64)?;
    Some(plan_label(&plan).to_owned())
}

/// The periods of an answer's `rate_limit`, the shorter one first. Either may
/// be missing, and a plan may count over other lengths than five hours and a
/// week.
fn periods(limit: &Value) -> Vec<Remaining> {
    let mut found: Vec<(i64, Remaining)> = WINDOWS
        .iter()
        .filter_map(|(name, usual)| {
            let window = &limit[*name];
            let used = window["used_percent"].as_f64()?;
            let seconds = window["limit_window_seconds"]
                .as_i64()
                .filter(|seconds| *seconds > 0)
                .unwrap_or(*usual);
            let period = Remaining {
                label: label(seconds),
                percent: (100.0 - used).clamp(0.0, 100.0),
            };
            Some((seconds, period))
        })
        .collect();
    found.sort_by_key(|(seconds, _)| *seconds);
    found.into_iter().map(|(_, period)| period).collect()
}

/// What a period of `seconds` is called: its days or hours where it is a
/// whole number of them, its minutes otherwise.
fn label(seconds: i64) -> String {
    // Codex counts a period in minutes, rounded up.
    let minutes = seconds.saturating_add(59) / 60;
    if minutes % 1440 == 0 {
        format!("{}d", minutes / 1440)
    } else if minutes % 60 == 0 {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
#[path = "codex_quota_tests.rs"]
mod tests;
