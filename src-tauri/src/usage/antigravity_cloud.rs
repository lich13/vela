//! Account quota asked of Antigravity's own service with the account's login,
//! as the app and `agy` ask it. No process is started, so the answer takes
//! under a second once an account's project is known. A login is sent as it
//! is and never renewed: that takes the OAuth client of the Antigravity app,
//! which Vela does not carry. An access token that has ended is of use again
//! once Antigravity has renewed it, which it does for the account it is
//! signed in to. Nothing here is written to disk or to a log.
use super::{periods, Periods};
use crate::{
    antigravity::auth::QuotaLogin,
    storage::AppError,
    usage::model::{failure, limited_until, now},
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
    sync::LazyLock,
    time::Duration,
};

/// Asked in this order until the Gemini quota has been reported; what a later
/// one reports that the earlier ones did not is added.
const HOSTS: [&str; 3] = [
    "https://daily-cloudcode-pa.googleapis.com",
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];
const USER_AGENT: &str = "antigravity/hub/2.35.2 (aidev_client; os_type=darwin; arch=arm64)";
const CONNECT_LIMIT: Duration = Duration::from_secs(5);
const REQUEST_LIMIT: Duration = Duration::from_secs(10);
/// Everything asked for one account: its project and the quota.
const ACCOUNT_LIMIT: Duration = Duration::from_secs(20);
/// An access token this close to its end is not sent.
const MARGIN: i64 = 60_000;

/// Why an account's quota was not read.
#[cfg_attr(test, derive(Debug))]
pub(super) enum Failure {
    /// The access token at hand has ended, or the service refused it. Only
    /// Antigravity renews a login, so the service was told nothing it takes.
    Expired,
    /// The service asked not to be asked for a while.
    Limited,
    /// Nothing came back: it was not reached, or did not answer in time.
    Unreachable,
    Other(AppError),
}
impl Failure {
    /// Whether the service spoke for the account itself, so that the app and
    /// `agy`, which ask the same service, would fare no better.
    pub(super) fn stands(&self) -> bool {
        matches!(self, Self::Limited)
    }
    /// Whether there was no token the service takes, so that it said nothing
    /// of the quota or of itself.
    pub(super) fn expired(&self) -> bool {
        matches!(self, Self::Expired)
    }
    /// Why the account was not read, as its owner is told. Antigravity is
    /// signed in to the `current` account alone, so the login of another is
    /// renewed only once it is switched to.
    pub(super) fn error(self, current: bool) -> AppError {
        match self {
            Self::Expired if current => failure("访问令牌已到期，由 Antigravity 续期后更新"),
            Self::Expired => failure("访问令牌已到期，切换到此账号后更新"),
            Self::Limited => failure("Antigravity 额度查询受限，请稍后重试"),
            Self::Unreachable => failure("Antigravity 额度服务没有应答，请检查网络后重试"),
            Self::Other(error) => error,
        }
    }
}
type Answer<T> = std::result::Result<T, Failure>;

/// What is kept of an account for as long as Vela runs.
#[derive(Clone, Default)]
struct Kept {
    /// The project its quota is asked under.
    project: Option<String>,
    /// Until when the service is not asked.
    limited_until: i64,
}

struct Cloud {
    client: reqwest::Client,
    hosts: Vec<String>,
    kept: Mutex<HashMap<String, Kept>>,
}

static CLOUD: LazyLock<Cloud> = LazyLock::new(|| Cloud::new(HOSTS.map(str::to_owned).to_vec()));

/// The quota periods of the account `login` belongs to.
pub(super) async fn quota(login: &QuotaLogin) -> Answer<Periods> {
    CLOUD.quota(login).await
}

impl Cloud {
    fn new(hosts: Vec<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(CONNECT_LIMIT)
                .timeout(REQUEST_LIMIT)
                .build()
                .expect("Antigravity 额度连接初始化失败"),
            hosts,
            kept: Mutex::default(),
        }
    }

    async fn quota(&self, login: &QuotaLogin) -> Answer<Periods> {
        if self.kept(&login.id).limited_until > now() {
            return Err(Failure::Limited);
        }
        // A token that does not say when it ends is tried as it is.
        let fresh = |ends: &i64| ends.saturating_sub(MARGIN) > now();
        let token = login
            .access_token
            .as_deref()
            .filter(|_| login.expires_at.as_ref().is_none_or(fresh))
            .ok_or(Failure::Expired)?;
        tokio::time::timeout(ACCOUNT_LIMIT, self.summary(&login.id, token))
            .await
            .unwrap_or(Err(Failure::Unreachable))
    }

    fn kept(&self, id: &str) -> Kept {
        self.kept.lock().get(id).cloned().unwrap_or_default()
    }
    fn keep(&self, id: &str, change: impl FnOnce(&mut Kept)) {
        change(self.kept.lock().entry(id.to_owned()).or_default());
    }

    /// The project the account's quota is asked under. An account whose
    /// project cannot be learned is asked without one.
    async fn project(&self, id: &str, token: &str) -> Answer<Option<String>> {
        if let Some(project) = self.kept(id).project {
            return Ok(Some(project));
        }
        let Some(host) = self.hosts.first() else {
            return Ok(None);
        };
        let value = match self
            .post(
                id,
                &format!("{host}/v1internal:loadCodeAssist"),
                token,
                &json!({"metadata": {"ideType": "ANTIGRAVITY"}}),
            )
            .await
        {
            Ok(value) => value,
            Err(Failure::Other(_)) => return Ok(None),
            Err(failure) => return Err(failure),
        };
        let named = |value: &Value| {
            value
                .as_str()
                .or_else(|| value["id"].as_str())
                .filter(|project| !project.is_empty())
                .map(str::to_owned)
        };
        let project = ["cloudaicompanionProject", "projectId", "project"]
            .into_iter()
            .find_map(|key| named(&value[key]));
        if let Some(project) = &project {
            self.keep(id, |kept| kept.project = Some(project.clone()));
        }
        Ok(project)
    }

    async fn summary(&self, id: &str, token: &str) -> Answer<Periods> {
        let body = match self.project(id, token).await? {
            Some(project) => json!({ "project": project }),
            None => json!({}),
        };
        let mut groups = Vec::new();
        let mut seen = BTreeSet::new();
        let mut failed = None;
        for (order, host) in self.hosts.iter().enumerate() {
            let url = format!("{host}/v1internal:retrieveUserQuotaSummary");
            let value = match self.post(id, &url, token, &body).await {
                Ok(value) => value,
                // The hosts are one service: what it says of the token or of
                // being left alone holds at each of them. They are reached
                // the same way too, so they are not waited for in turn where
                // the first gave no sign.
                Err(failure @ (Failure::Expired | Failure::Limited)) => return Err(failure),
                Err(Failure::Unreachable) if order == 0 => return Err(Failure::Unreachable),
                Err(failure) => {
                    failed.get_or_insert(failure);
                    continue;
                }
            };
            let earlier = seen.clone();
            for group in value["groups"].as_array().into_iter().flatten() {
                let buckets: Vec<&Value> = group["buckets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|bucket| {
                        let id = bucket["bucketId"]
                            .as_str()
                            .or_else(|| bucket["id"].as_str());
                        id.is_some_and(|id| {
                            seen.insert(id.to_owned());
                            !earlier.contains(id)
                        })
                    })
                    .collect();
                groups.push(json!({ "buckets": buckets }));
            }
            if seen.iter().any(|id| id.starts_with("gemini-")) {
                break;
            }
        }
        let periods = periods(&Value::Array(groups));
        // An answer naming none of the quotas says nothing about them, and
        // must not replace the periods saved earlier.
        if periods.is_empty() {
            // The account may have moved to another project since.
            self.keep(id, |kept| kept.project = None);
            return Err(failed.unwrap_or_else(|| other("Antigravity 未返回账号额度")));
        }
        Ok(periods)
    }

    async fn post(&self, id: &str, url: &str, token: &str, body: &Value) -> Answer<Value> {
        let response = self
            .client
            .post(url)
            .bearer_auth(token)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .json(body)
            .send()
            .await
            .map_err(|_| Failure::Unreachable)?;
        match response.status().as_u16() {
            200..=299 => response.json().await.map_err(|_| unreadable()),
            // The token may have ended early or been withdrawn.
            401 => Err(Failure::Expired),
            403 => Err(other("Antigravity 账号没有额度读取权限")),
            429 => {
                let until = limited_until(response.headers());
                self.keep(id, |kept| kept.limited_until = until);
                Err(Failure::Limited)
            }
            _ => Err(unavailable()),
        }
    }
}

fn other(message: &str) -> Failure {
    Failure::Other(failure(message))
}
fn unavailable() -> Failure {
    other("Antigravity 额度服务暂不可用，请重试")
}
fn unreadable() -> Failure {
    other("Antigravity 额度响应无法解析")
}

#[cfg(test)]
#[path = "antigravity_cloud_tests.rs"]
mod tests;
