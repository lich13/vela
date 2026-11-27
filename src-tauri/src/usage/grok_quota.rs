//! The quota of the account Grok Build is signed in to, asked of Grok's own
//! service with that login as the CLI asks it. The login is sent as it is and
//! never renewed. Nothing here is written to disk or to a log.
use crate::{
    storage::{self, Result},
    usage::model::{failure, limited_until, now},
};
use parking_lot::Mutex;
use serde_json::Value;
use std::{path::Path, sync::LazyLock, time::Duration};

const ORIGIN: &str = "https://cli-chat-proxy.grok.com";
/// The week is counted in credits and the month in money, and each is asked
/// for apart.
const WEEK: &str = "/v1/billing?format=credits";
const MONTH: &str = "/v1/billing";
/// What Grok's `auth.json` keeps its official login under.
const SIGN_IN: &str = "https://accounts.x.ai/sign-in";
/// The CLI the questions are put as. The service turns away a version it
/// takes for too old.
const VERSION: &str = "1.0.92";
const USER_AGENT: &str = "grok-pager/1.0.92 grok-shell/1.0.92 (macos; aarch64)";
const CONNECT_LIMIT: Duration = Duration::from_secs(5);
const REQUEST_LIMIT: Duration = Duration::from_secs(10);
// These are shown in a menu under the client's own name, so they are short.
/// What is said of a login the service no longer takes. What was read with
/// it before says nothing of the account any longer.
pub(crate) const OVER: &str = "登录已过期，请重新登录";
const LIMITED: &str = "查询受限，请稍后重试";

/// What Grok's service says is left to the account Grok Build is signed in to.
#[cfg_attr(test, derive(Debug))]
pub(crate) struct Quota {
    /// The digest of the login that was asked with: another one is another
    /// account, or this one signed in again. It is never shown.
    pub login: String,
    /// The percentage left of the week's credits, where the service said.
    pub week: Option<f64>,
    /// The percentage left of what the plan includes for the month, where
    /// the service said. A plan without a monthly limit has none.
    pub month: Option<f64>,
    /// Why a period is missing, where a question was not answered. Without
    /// it, a period that is missing is one the account does not have.
    pub failed: Option<String>,
}

struct Billing {
    client: reqwest::Client,
    origin: String,
    /// The digest of the login the service asked not to be asked with, and
    /// until when.
    limited: Mutex<Option<(String, i64)>>,
}

static BILLING: LazyLock<Billing> = LazyLock::new(|| Billing::new(ORIGIN.to_owned()));

/// What is left of the week's and the month's quota of the account signed in
/// under `home`. None where Grok holds no official login there. The login is
/// never renewed.
pub(crate) async fn quota(home: &Path) -> Option<Quota> {
    BILLING.quota(home).await
}

impl Billing {
    fn new(origin: String) -> Self {
        Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(CONNECT_LIMIT)
                .timeout(REQUEST_LIMIT)
                .build()
                .expect("Grok 额度连接初始化失败"),
            origin,
            limited: Mutex::default(),
        }
    }

    async fn quota(&self, home: &Path) -> Option<Quota> {
        let token = token(home)?;
        let login = storage::digest(token.as_bytes());
        let limited = self
            .limited
            .lock()
            .as_ref()
            .is_some_and(|(of, until)| *of == login && *until > now());
        if limited {
            return Some(Quota {
                login,
                week: None,
                month: None,
                failed: Some(LIMITED.to_owned()),
            });
        }
        let (credits, money) = tokio::join!(
            self.ask(&token, &login, WEEK),
            self.ask(&token, &login, MONTH)
        );
        // Either answer may carry what the other was asked for.
        let left = |of: fn(&Value) -> Option<f64>, answers: [&Result<Value>; 2]| {
            let mut configs = answers
                .into_iter()
                .flatten()
                .map(|answer| &answer["config"]);
            configs.find_map(of)
        };
        let mut quota = Quota {
            login,
            week: left(week, [&credits, &money]),
            month: left(month, [&money, &credits]),
            failed: None,
        };
        // What one answer gives is told though the other failed. A failure
        // is why a period is missing; where both answered, the account has
        // no such period.
        if quota.week.is_none() || quota.month.is_none() {
            quota.failed = [credits, money]
                .into_iter()
                .find_map(Result::err)
                .map(|error| error.message);
        }
        Some(quota)
    }

    /// What the service answers one of the two questions with.
    async fn ask(&self, token: &str, login: &str, path: &str) -> Result<Value> {
        let response = self
            .client
            .get(format!("{}{path}", self.origin))
            .bearer_auth(token)
            .header("x-xai-token-auth", "xai-grok-cli")
            .header("x-grok-client-version", VERSION)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .send()
            .await
            .map_err(|_| failure("额度服务没有应答"))?;
        match response.status().as_u16() {
            200..=299 => response
                .json::<Value>()
                .await
                .ok()
                .filter(Value::is_object)
                .ok_or_else(|| failure("额度响应无法解析")),
            401 => Err(failure(OVER)),
            403 => Err(failure("查询被拒绝")),
            429 => {
                let until = limited_until(response.headers());
                *self.limited.lock() = Some((login.to_owned(), until));
                Err(failure(LIMITED))
            }
            _ => Err(failure("额度服务暂不可用")),
        }
    }
}

/// The login Grok holds for its official account under `home`, where it
/// holds one.
fn token(home: &Path) -> Option<String> {
    let raw = storage::read_optional(&home.join("auth.json")).ok()??;
    let document: Value = serde_json::from_slice(&raw).ok()?;
    document[SIGN_IN]["key"]
        .as_str()
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
}

/// The percentage left of the week's credits.
fn week(config: &Value) -> Option<f64> {
    let used = number(named(config, "creditUsagePercent", "credit_usage_percent"))?;
    Some((100.0 - used).clamp(0.0, 100.0))
}

/// The percentage left of what the plan includes for the month. A plan
/// without a monthly limit has no such period.
fn month(config: &Value) -> Option<f64> {
    let limit = amount(named(config, "monthlyLimit", "monthly_limit"))?;
    let used = amount(&config["used"])?;
    (limit > 0.0).then(|| (100.0 - used / limit * 100.0).clamp(0.0, 100.0))
}

/// A field the service names in one of two ways.
fn named<'a>(config: &'a Value, camel: &str, snake: &str) -> &'a Value {
    match &config[camel] {
        Value::Null => &config[snake],
        value => value,
    }
}

/// An amount of money: a number, or an object that holds one as `val`.
fn amount(value: &Value) -> Option<f64> {
    match value {
        Value::Object(amount) => number(amount.get("val")?),
        value => number(value),
    }
}

/// A number as the service writes one: as it is, or in a string.
fn number(value: &Value) -> Option<f64> {
    let number = match value {
        Value::String(text) => text.trim().parse().ok()?,
        value => value.as_f64()?,
    };
    Some(number).filter(|number: &f64| number.is_finite())
}

#[cfg(test)]
#[path = "grok_quota_tests.rs"]
mod tests;
