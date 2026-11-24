use crate::errors::code;
use crate::storage::{self, AppError, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub token: String,
    pub queued: bool,
    pub version: String,
    #[serde(default)]
    pub max_concurrency: u32,
    #[serde(default)]
    pub max_rpm: u32,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub handoff_after_compaction: bool,
    #[serde(default)]
    pub take_new_threads: bool,
    #[serde(default)]
    pub end_on_message_stop: bool,
    #[serde(default)]
    pub filter_count_tokens: bool,
    #[serde(default)]
    pub buffer_complete_response: bool,
    #[serde(default)]
    pub endless_mode: bool,
    /// Endless-mode base wait after a risk-control rejection (a status in `risk_codes`).
    #[serde(default = "default_upstream_retry_seconds")]
    pub upstream_retry_seconds: u64,
    /// Endless-mode wait after any other retried failure.
    #[serde(default = "default_normal_retry_seconds")]
    pub normal_retry_seconds: u64,
    /// Upstream statuses endless mode treats as risk control. `None` uses the
    /// client's defaults; an empty list is a deliberate "none".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_codes: Option<Vec<u16>>,
    /// How strongly the risk-control and rate-limit waits follow the
    /// provider's load, 0–10. `None` is the default; 0 keeps them fixed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_intensity: Option<u8>,
    /// Endless-mode wait after a rate limit. `None` follows the risk-control
    /// wait, which covered rate limits before the two were told apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_retry_seconds: Option<u64>,
    /// Seconds an endless provider is sent nothing after it rate-limits a
    /// request. `None` is the default; 0 sends on as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_rest_seconds: Option<u64>,
}
pub(crate) const DEFAULT_UPSTREAM_RETRY_SECONDS: u64 = 33;
pub(crate) const DEFAULT_NORMAL_RETRY_SECONDS: u64 = 3;
pub(crate) const DEFAULT_RETRY_INTENSITY: u8 = 5;
pub(crate) const DEFAULT_LIMIT_REST_SECONDS: u64 = 8;
const MAX_RETRY_INTENSITY: u8 = 10;
const MAX_RISK_CODES: usize = 16;
fn default_upstream_retry_seconds() -> u64 {
    DEFAULT_UPSTREAM_RETRY_SECONDS
}
fn default_normal_retry_seconds() -> u64 {
    DEFAULT_NORMAL_RETRY_SECONDS
}
impl Provider {
    pub(super) fn retry_intensity(&self) -> u8 {
        self.retry_intensity
            .unwrap_or(DEFAULT_RETRY_INTENSITY)
            .min(MAX_RETRY_INTENSITY)
    }
    pub(super) fn limit_retry_seconds(&self) -> u64 {
        self.limit_retry_seconds
            .unwrap_or(self.upstream_retry_seconds)
    }
    pub(super) fn limit_rest_seconds(&self) -> u64 {
        self.limit_rest_seconds
            .unwrap_or(DEFAULT_LIMIT_REST_SECONDS)
    }
}
/// Which of endless mode's three waits a retried failure gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RetryClass {
    /// A status on the provider's risk-control list.
    Risk,
    /// A rate limit: a 429, or a refusal classified as one.
    Limit,
    Normal,
}
/// Sorts a failure endless mode retries. A real error status decides alone:
/// on the risk-control list, a 429, or anything else. A refusal inside a
/// successful response is sorted by the code and type it carries. Content the
/// gateway classified as a rate limit counts as one under any status, and so
/// does a quota that comes back with its day, week or month; an overloaded
/// upstream is not a rate limit.
pub(super) fn retry_class(
    codes: &[u16],
    status: Option<u16>,
    details: &crate::events::Details,
) -> RetryClass {
    use crate::events::ErrorBasis;
    let limited = match details.error_basis {
        Some(ErrorBasis::Capacity) => false,
        Some(ErrorBasis::QuotaExhausted) => true,
        _ => super::failure_policy::temporary(details),
    };
    if let Some(status) = status.filter(|status| *status >= 400) {
        return if codes.contains(&status) {
            RetryClass::Risk
        } else if status == 429 || limited {
            RetryClass::Limit
        } else {
            RetryClass::Normal
        };
    }
    let embedded = details
        .upstream_code
        .as_deref()
        .and_then(|code| code.parse::<u16>().ok());
    if embedded.is_some_and(|code| codes.contains(&code)) {
        RetryClass::Risk
    } else if embedded == Some(429)
        || details.upstream_type.as_deref() == Some("rate_limit_error")
        || limited
    {
        RetryClass::Limit
    } else {
        RetryClass::Normal
    }
}
/// Sorted, deduplicated risk-control statuses.
fn risk_codes(mut codes: Vec<u16>) -> Result<Vec<u16>> {
    codes.sort_unstable();
    codes.dedup();
    if codes.len() > MAX_RISK_CODES || codes.iter().any(|code| !(400..=599).contains(code)) {
        return Err(AppError::new(
            code::SETTINGS,
            "风控代码应为 400–599 的状态码，最多 16 个",
        ));
    }
    Ok(codes)
}
/// The requests a provider serves in place of the rest of the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Delegated {
    /// A client's request to compact its conversation.
    Compaction,
    /// Work a client does beside the conversation: approving a command,
    /// naming a thread, summarising, writing memories.
    Auxiliary,
    /// A search of the web that a client's own tool has the provider run.
    /// Not every provider can.
    WebSearch,
}
/// What one provider takes from the others, and the model and reasoning
/// effort it runs it on. Either one set here replaces whatever the client
/// asked for.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Delegate {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub compaction: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction_effort: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub auxiliary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auxiliary_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auxiliary_effort: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub web_search: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_search_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_search_effort: Option<String>,
}
impl Delegate {
    /// The model and the reasoning effort this provider runs `kind` on, each
    /// when it names one, if it takes that kind at all.
    pub(super) fn takes(&self, kind: Delegated) -> Option<(Option<&str>, Option<&str>)> {
        let (taken, model, effort) = match kind {
            Delegated::Compaction => (
                self.compaction,
                &self.compaction_model,
                &self.compaction_effort,
            ),
            Delegated::Auxiliary => (
                self.auxiliary,
                &self.auxiliary_model,
                &self.auxiliary_effort,
            ),
            Delegated::WebSearch => (
                self.web_search,
                &self.web_search_model,
                &self.web_search_effort,
            ),
        };
        taken.then_some((model.as_deref(), effort.as_deref()))
    }
}
/// The delegation fields of a provider form. An absent field stays as it is;
/// an empty model or effort clears the one that was set.
#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DelegateEdit {
    pub take_compaction: Option<bool>,
    pub compaction_model: Option<String>,
    pub compaction_effort: Option<String>,
    pub take_auxiliary: Option<bool>,
    pub auxiliary_model: Option<String>,
    pub auxiliary_effort: Option<String>,
    pub take_web_search: Option<bool>,
    pub web_search_model: Option<String>,
    pub web_search_effort: Option<String>,
}
impl DelegateEdit {
    /// The form of a new provider that says nothing of what it takes over:
    /// the provider takes every kind `client` has, each on the model and
    /// effort the request names.
    pub(super) fn or_everything(mut self, client: super::ClientId) -> Self {
        use super::ClientId;
        if self.take_compaction.is_some()
            || self.take_auxiliary.is_some()
            || self.take_web_search.is_some()
        {
            return self;
        }
        let both = matches!(client, ClientId::Codex | ClientId::Claude);
        self.take_compaction = both.then_some(true);
        self.take_auxiliary = both.then_some(true);
        self.take_web_search = (both || client == ClientId::Grok).then_some(true);
        self
    }
    /// The form asks this provider to take requests of `kind` over.
    pub(super) fn enables(&self, kind: Delegated) -> bool {
        let taken = match kind {
            Delegated::Compaction => self.take_compaction,
            Delegated::Auxiliary => self.take_auxiliary,
            Delegated::WebSearch => self.take_web_search,
        };
        taken == Some(true)
    }
    fn applied(self, mut delegate: Delegate) -> Result<Delegate> {
        let model = |value: String| {
            let value = value.trim();
            if value.is_empty() {
                Ok(None)
            } else if model_id(value) {
                Ok(Some(value.to_owned()))
            } else {
                Err(AppError::new(
                    code::MODELS,
                    "指定模型应为 1–256 字节的完整模型 ID，不能包含控制字符或通配符",
                ))
            }
        };
        // An effort is the name the client's API gives a level: `low`, `xhigh`.
        let effort = |value: String| {
            let value = value.trim();
            if value.is_empty() {
                Ok(None)
            } else if value.len() <= 32
                && value
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
            {
                Ok(Some(value.to_owned()))
            } else {
                Err(AppError::new(
                    code::SETTINGS,
                    "思考强度应为 1–32 个小写字母、数字、下划线或连字符",
                ))
            }
        };
        if let Some(value) = self.take_compaction {
            delegate.compaction = value;
        }
        if let Some(value) = self.compaction_model {
            delegate.compaction_model = model(value)?;
        }
        if let Some(value) = self.compaction_effort {
            delegate.compaction_effort = effort(value)?;
        }
        if let Some(value) = self.take_auxiliary {
            delegate.auxiliary = value;
        }
        if let Some(value) = self.auxiliary_model {
            delegate.auxiliary_model = model(value)?;
        }
        if let Some(value) = self.auxiliary_effort {
            delegate.auxiliary_effort = effort(value)?;
        }
        if let Some(value) = self.take_web_search {
            delegate.web_search = value;
        }
        if let Some(value) = self.web_search_model {
            delegate.web_search_model = model(value)?;
        }
        if let Some(value) = self.web_search_effort {
            delegate.web_search_effort = effort(value)?;
        }
        Ok(delegate)
    }
}
/// A complete model ID: no wildcard, nothing a header or a log line chokes on.
fn model_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control) && !id.contains('*')
}
const MAX_MODEL_MAPPINGS: usize = 512;
/// The Claude model families a mapping can name as `family:<name>` instead of
/// a complete model ID.
pub const MODEL_FAMILIES: [&str; 4] = ["fable", "opus", "sonnet", "haiku"];
const FAMILY_PREFIX: &str = "family:";
/// The family of a model a Claude client asks for: an alias (`opus`,
/// `opusplan`), or a Claude model ID carrying the family word in either naming
/// (`claude-opus-4-1-20250805`, `claude-3-5-sonnet-20241022`), with or without
/// a vendor prefix or a `[1m]` suffix. Anything else has none.
pub fn model_family(model: &str) -> Option<&'static str> {
    let lower = model.trim().to_ascii_lowercase();
    let id = match lower.find('[') {
        Some(at) if lower.ends_with(']') => &lower[..at],
        _ => lower.as_str(),
    };
    if id == "opusplan" {
        return Some("opus");
    }
    if let Some(family) = MODEL_FAMILIES.into_iter().find(|family| *family == id) {
        return Some(family);
    }
    let at = id.match_indices("claude-").map(|(at, _)| at).find(|at| {
        id[..*at]
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_ascii_alphanumeric())
    })?;
    id[at..]
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find_map(|word| MODEL_FAMILIES.into_iter().find(|family| *family == word))
}
/// The upstream model a mapping sends in place of the requested one: the
/// model's own entry first, then its family's.
pub fn mapped_model<'a>(mapping: &'a BTreeMap<String, String>, model: &str) -> Option<&'a str> {
    if mapping.is_empty() {
        return None;
    }
    mapping
        .get(model)
        .or_else(|| mapping.get(&format!("{FAMILY_PREFIX}{}", model_family(model)?)))
        .map(String::as_str)
}
/// A provider's model mapping with its IDs trimmed: a complete model ID or a
/// `family:<name>` key on the requested side, a complete model ID upstream,
/// each requested model once, none mapped to itself.
pub(super) fn model_map(mapping: BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    let invalid = || {
        AppError::new(
            code::MODELS,
            "模型映射应为 1–256 字节的完整模型 ID，不能包含控制字符或通配符，且请求模型与上游模型不能相同或重复",
        )
    };
    if mapping.len() > MAX_MODEL_MAPPINGS {
        return Err(AppError::new(code::MODELS, "模型映射最多 512 条"));
    }
    let mut result = BTreeMap::new();
    for (from, to) in mapping {
        let (from, to) = (from.trim(), to.trim());
        if !model_id(from)
            || !model_id(to)
            || from
                .strip_prefix(FAMILY_PREFIX)
                .is_some_and(|family| !MODEL_FAMILIES.contains(&family))
            || from == to
            || result.insert(from.to_owned(), to.to_owned()).is_some()
        {
            return Err(invalid());
        }
    }
    Ok(result)
}
fn retry_intensity(value: u8) -> Result<u8> {
    if value > MAX_RETRY_INTENSITY {
        return Err(AppError::new(code::SETTINGS, "动态强度应为 0–10"));
    }
    Ok(value)
}
// Existing defaults: three retries allow four total upstream attempts.
const DEFAULT_MAX_RETRIES: usize = 3;
const DEFAULT_FAILURE_THRESHOLD: u32 = 4;
const DEFAULT_SUCCESS_THRESHOLD: u32 = 2;
const DEFAULT_COOLDOWN_SECONDS: u64 = 60;
// These phase limits have different meanings even where the values match.
const DEFAULT_FIRST_BYTE_SECONDS: u64 = 300;
const DEFAULT_IDLE_SECONDS: u64 = 300;
const DEFAULT_RETRY_BUDGET_SECONDS: u64 = 300;
const DEFAULT_CONNECT_SECONDS: u64 = 300;
pub(super) const USAGE_TAIL_DEADLINE: std::time::Duration = std::time::Duration::from_millis(1500);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub custom_failure_rules: Vec<super::failure_rules::Rule>,
    pub port: u16,
    pub max_retries: usize,
    pub failure_threshold: u32,
    #[serde(default = "default_transient_failure_threshold")]
    pub transient_failure_threshold: u32,
    pub success_threshold: u32,
    pub cooldown_seconds: u64,
    #[serde(default = "default_rate_limit_seconds")]
    pub rate_limit_seconds: u64,
    #[serde(default = "default_request_seconds")]
    pub hard_failure_cooldown_seconds: u64,
    #[serde(default = "default_capacity_retry_seconds")]
    pub capacity_retry_seconds: u64,
    #[serde(default = "default_request_seconds")]
    pub websocket_retry_seconds: u64,
    #[serde(default = "default_endpoint_probe_seconds")]
    pub endpoint_probe_seconds: u64,
    pub first_byte_seconds: u64,
    /// How long a dispatched compaction may stay without output.
    #[serde(default = "default_compaction_seconds")]
    pub compaction_seconds: u64,
    pub idle_seconds: u64,
    #[serde(default = "default_request_seconds")]
    pub retry_budget_seconds: u64,
    pub connect_seconds: u64,
    #[serde(default = "default_wait_seconds")]
    pub queue_seconds: u64,
    #[serde(default = "default_max_waiting")]
    pub max_waiting: usize,
    /// After a failed connection, probe the provider's model list before the
    /// request is sent again, and hold the request while the network is down.
    #[serde(default = "default_network_probe")]
    pub network_probe: bool,
    /// Failed probes in a row that mean the network is down.
    #[serde(default = "default_network_probe_attempts")]
    pub network_probe_attempts: u32,
    /// How long a Claude stream may go without upstream data once the model
    /// has started writing. The other clients keep to `idle_seconds`.
    #[serde(default = "default_stall_seconds")]
    pub stall_seconds: u64,
    /// How long a started response may go without upstream data while its
    /// endpoint is held for a network outage.
    #[serde(default = "default_hold_idle_seconds")]
    pub hold_idle_seconds: u64,
}
fn default_network_probe() -> bool {
    true
}
fn default_network_probe_attempts() -> u32 {
    3
}
fn default_stall_seconds() -> u64 {
    300
}
fn default_hold_idle_seconds() -> u64 {
    60
}
fn default_endpoint_probe_seconds() -> u64 {
    300
}
fn default_request_seconds() -> u64 {
    300
}
fn default_wait_seconds() -> u64 {
    default_request_seconds()
}
fn default_transient_failure_threshold() -> u32 {
    6
}
fn default_rate_limit_seconds() -> u64 {
    5
}
fn default_compaction_seconds() -> u64 {
    400
}
fn default_capacity_retry_seconds() -> u64 {
    60
}
fn default_max_waiting() -> usize {
    100
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            custom_failure_rules: Vec::new(),
            port: super::client::DEFAULT_SHARED_PORT,
            max_retries: DEFAULT_MAX_RETRIES,
            failure_threshold: DEFAULT_FAILURE_THRESHOLD,
            transient_failure_threshold: default_transient_failure_threshold(),
            success_threshold: DEFAULT_SUCCESS_THRESHOLD,
            cooldown_seconds: DEFAULT_COOLDOWN_SECONDS,
            rate_limit_seconds: default_rate_limit_seconds(),
            hard_failure_cooldown_seconds: default_request_seconds(),
            capacity_retry_seconds: default_capacity_retry_seconds(),
            websocket_retry_seconds: default_request_seconds(),
            endpoint_probe_seconds: default_endpoint_probe_seconds(),
            first_byte_seconds: DEFAULT_FIRST_BYTE_SECONDS,
            compaction_seconds: default_compaction_seconds(),
            idle_seconds: DEFAULT_IDLE_SECONDS,
            retry_budget_seconds: DEFAULT_RETRY_BUDGET_SECONDS,
            connect_seconds: DEFAULT_CONNECT_SECONDS,
            queue_seconds: default_wait_seconds(),
            max_waiting: default_max_waiting(),
            network_probe: default_network_probe(),
            network_probe_attempts: default_network_probe_attempts(),
            stall_seconds: default_stall_seconds(),
            hold_idle_seconds: default_hold_idle_seconds(),
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        super::failure_rules::validate(&self.custom_failure_rules)?;
        if self.port == 0
            || self.max_retries > 15
            || self.failure_threshold == 0
            || !(1..=1000).contains(&self.transient_failure_threshold)
            || self.success_threshold == 0
            || self.max_waiting == 0
            || self.max_waiting > 10000
            || !(10..=86400).contains(&self.endpoint_probe_seconds)
            || !(1..=20).contains(&self.network_probe_attempts)
            || [
                self.cooldown_seconds,
                self.rate_limit_seconds,
                self.hard_failure_cooldown_seconds,
                self.capacity_retry_seconds,
                self.websocket_retry_seconds,
                self.first_byte_seconds,
                self.compaction_seconds,
                self.idle_seconds,
                self.retry_budget_seconds,
                self.connect_seconds,
                self.queue_seconds,
                self.stall_seconds,
                self.hold_idle_seconds,
            ]
            .iter()
            .any(|s| *s == 0 || *s > 86400)
        {
            return Err(AppError::new(code::SETTINGS, "端口、阈值和超时参数无效"));
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Store {
    pub schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successful_user_agent: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successful_codex_identity: Option<super::testing::CodexIdentity>,
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub connection: super::codex_api::Connection,
    pub providers: Vec<Provider>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub quota_access_tokens: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provider_endpoints: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub grok_connections: BTreeMap<String, super::grok_config::Connection>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dsh_models: BTreeMap<String, super::dsh::ModelMapping>,
    /// Per provider, the upstream model each requested model is sent as.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_maps: BTreeMap<String, BTreeMap<String, String>>,
    /// Per provider, the kinds of request it takes over from the others.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub delegates: BTreeMap<String, Delegate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dsh_catalog: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grok_resume: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grok_api_model: Option<String>,
    /// Vela publishes the queue's models to the client's own model picker.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub model_catalog: bool,
    /// Codex's picker lists OpenAI's own catalog instead of the queue's models.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub model_catalog_official: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsh: Option<super::dsh::Snapshot>,
    pub settings: Settings,
    pub mode: String,
    pub selected: Option<String>,
    pub local_token: String,
    #[serde(default)]
    pub resume: Option<Resume>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resume {
    pub home: PathBuf,
    pub desired: bool,
    pub pair_hash: String,
}
pub(super) fn new_local_token() -> String {
    format!(
        "gs_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
impl Default for Store {
    fn default() -> Self {
        Self {
            schema: 2,
            successful_user_agent: None,
            successful_codex_identity: None,
            initialized: false,
            connection: Default::default(),
            providers: vec![],
            quota_access_tokens: BTreeMap::new(),
            provider_endpoints: BTreeMap::new(),
            grok_connections: BTreeMap::new(),
            dsh_models: BTreeMap::new(),
            model_maps: BTreeMap::new(),
            delegates: BTreeMap::new(),
            dsh_catalog: vec![],
            grok_resume: None,
            grok_api_model: None,
            model_catalog: false,
            model_catalog_official: false,
            dsh: None,
            settings: Settings::default(),
            mode: "manual".into(),
            selected: None,
            resume: None,
            local_token: new_local_token(),
        }
    }
}
#[derive(Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Edit {
    /// Made from a link opened on the desktop, never from a request.
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    #[serde(skip)]
    ImportLink {
        base_url: String,
        token: String,
        display_name: String,
        protocol: super::provider_protocol::Protocol,
    },
    SaveProvider {
        #[serde(default)]
        end_on_message_stop: Option<bool>,
        #[serde(default)]
        filter_count_tokens: Option<bool>,
        #[serde(default)]
        buffer_complete_response: Option<bool>,
        #[serde(default)]
        endless_mode: Option<bool>,
        #[serde(default)]
        upstream_retry_seconds: Option<u64>,
        #[serde(default)]
        normal_retry_seconds: Option<u64>,
        #[serde(default)]
        risk_codes: Option<Vec<u16>>,
        #[serde(default)]
        retry_intensity: Option<u8>,
        #[serde(default)]
        limit_retry_seconds: Option<u64>,
        #[serde(default)]
        limit_rest_seconds: Option<u64>,
        #[serde(flatten)]
        delegate: DelegateEdit,
        id: Option<String>,
        base_url: String,
        token: String,
        #[serde(default)]
        quota_access_token: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
    SaveGrokProvider {
        #[serde(default)]
        buffer_complete_response: Option<bool>,
        /// Of these a Grok provider may take web searches only.
        #[serde(flatten)]
        delegate: DelegateEdit,
        id: Option<String>,
        base_url: String,
        token: String,
        #[serde(default)]
        quota_access_token: Option<String>,
        #[serde(default)]
        name: Option<String>,
        protocol: super::grok_config::Protocol,
    },
    SaveDshProvider {
        #[serde(default)]
        buffer_complete_response: Option<bool>,
        #[serde(default)]
        endless_mode: Option<bool>,
        #[serde(default)]
        upstream_retry_seconds: Option<u64>,
        #[serde(default)]
        normal_retry_seconds: Option<u64>,
        #[serde(default)]
        risk_codes: Option<Vec<u16>>,
        #[serde(default)]
        retry_intensity: Option<u8>,
        #[serde(default)]
        limit_retry_seconds: Option<u64>,
        #[serde(default)]
        limit_rest_seconds: Option<u64>,
        id: Option<String>,
        base_url: String,
        token: String,
        #[serde(default)]
        quota_access_token: Option<String>,
        #[serde(default)]
        name: Option<String>,
        protocol: super::grok_config::Protocol,
    },
    DuplicateProvider {
        id: String,
    },
    /// Made by this app's Rust from what another instance handed over, never
    /// from a request.
    #[serde(skip)]
    ImportCopy {
        copy: Box<ProviderCopy>,
    },
    DshModelsProvider {
        id: String,
        mapping: super::dsh::ModelMapping,
    },
    ModelMapProvider {
        id: String,
        mapping: BTreeMap<String, String>,
    },
    RenameProvider {
        id: String,
        name: String,
    },
    EndpointsProvider {
        id: String,
        base_urls: Vec<String>,
    },
    SwapEndpoint {
        id: String,
        base_url: String,
    },
    DeleteProvider {
        id: String,
    },
    QueueProvider {
        id: String,
        queued: bool,
    },
    ConcurrencyProvider {
        id: String,
        max_concurrency: u32,
    },
    RpmProvider {
        id: String,
        max_rpm: u32,
    },
    ModelsProvider {
        id: String,
        allowed_models: Option<Vec<String>>,
    },
    PolicyProvider {
        id: String,
        allowed_models: Option<Vec<String>>,
        #[serde(default)]
        handoff_after_compaction: Option<bool>,
        #[serde(default)]
        take_new_threads: Option<bool>,
    },
    TakeoverProvider {
        id: String,
        #[serde(default)]
        handoff_after_compaction: Option<bool>,
        #[serde(default)]
        take_new_threads: Option<bool>,
    },
    Reorder {
        ids: Vec<String>,
    },
    Select {
        id: String,
    },
    Mode {
        mode: String,
    },
    UpsertFailureRule {
        rule: super::failure_rules::Rule,
    },
    DeleteFailureRule {
        id: String,
    },
    Settings {
        settings: Settings,
    },
    Reset {
        id: String,
    },
    Connection {
        mode: String,
    },
    ModelCatalog {
        enabled: bool,
    },
    ModelCatalogSource {
        official: bool,
    },
    Import,
}
pub fn name(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 120 || value.chars().any(char::is_control) {
        Err(AppError::new(code::NAME, "名称应为 1–120 个可见字符"))
    } else {
        Ok(value.to_owned())
    }
}
/// The name of a provider's copy: the original with a suffix, shortened to
/// stay within the name limit.
/// A provider with everything kept beside it, as a copy of it is made here
/// or handed to another instance. It holds the provider's tokens, so it goes
/// from one Rust to the other and is never shown.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCopy {
    provider: Provider,
    #[serde(default)]
    quota_access_token: Option<String>,
    #[serde(default)]
    endpoints: Option<Vec<String>>,
    #[serde(default)]
    grok_connection: Option<super::grok_config::Connection>,
    #[serde(default)]
    dsh_models: Option<super::dsh::ModelMapping>,
    #[serde(default)]
    model_map: Option<BTreeMap<String, String>>,
    #[serde(default)]
    delegate: Option<Delegate>,
}
impl Store {
    /// The provider `id` as it is copied, and where it stands in the list.
    pub(super) fn provider_copy(&self, id: &str) -> Result<(usize, ProviderCopy)> {
        let index = self
            .providers
            .iter()
            .position(|p| p.id == id)
            .ok_or_else(|| AppError::new(code::PROVIDER, "供应商不存在"))?;
        let copy = ProviderCopy {
            provider: self.providers[index].clone(),
            quota_access_token: self.quota_access_tokens.get(id).cloned(),
            endpoints: self.provider_endpoints.get(id).cloned(),
            grok_connection: self.grok_connections.get(id).cloned(),
            dsh_models: self.dsh_models.get(id).cloned(),
            model_map: self.model_maps.get(id).cloned(),
            delegate: self.delegates.get(id).cloned(),
        };
        Ok((index, copy))
    }

    /// Adds a copy at `index` as a provider of its own: a new id and
    /// version, and a name that says it is a copy.
    fn add_copy(&mut self, copy: ProviderCopy, index: usize) {
        let mut provider = copy.provider;
        provider.id = uuid::Uuid::new_v4().to_string();
        provider.version = uuid::Uuid::new_v4().to_string();
        provider.name = copy_name(&provider.name);
        let id = &provider.id;
        if let Some(token) = copy.quota_access_token {
            self.quota_access_tokens.insert(id.clone(), token);
        }
        if let Some(urls) = copy.endpoints {
            self.provider_endpoints.insert(id.clone(), urls);
        }
        if let Some(connection) = copy.grok_connection {
            self.grok_connections.insert(id.clone(), connection);
        }
        if let Some(mapping) = copy.dsh_models {
            self.dsh_models.insert(id.clone(), mapping);
        }
        if let Some(mapping) = copy.model_map {
            self.model_maps.insert(id.clone(), mapping);
        }
        if let Some(delegate) = copy.delegate {
            self.delegates.insert(id.clone(), delegate);
        }
        self.providers.insert(index, provider);
    }
}
fn copy_name(original: &str) -> String {
    const SUFFIX: &str = " (copy)";
    let keep = 120 - SUFFIX.chars().count();
    let base: String = original.chars().take(keep).collect();
    format!("{}{SUFFIX}", base.trim_end())
}
pub(super) fn is_local_ip(ip: std::net::IpAddr) -> bool {
    let ip = match ip {
        std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, std::net::IpAddr::V4),
        _ => ip,
    };
    ip.is_loopback() || ip.is_unspecified()
}
pub fn base_url(value: &str, port: u16) -> Result<url::Url> {
    let mut u = url::Url::parse(value.trim())
        .map_err(|_| AppError::new(code::URL, "请输入完整的 HTTP 或 HTTPS base_url"))?;
    if !["http", "https"].contains(&u.scheme())
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(AppError::new(
            code::URL,
            "base_url 只接受不含认证、查询参数及片段的 HTTP(S) 地址",
        ));
    }
    let host = u.host_str().unwrap_or_default().trim_matches(['[', ']']);
    if (host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(is_local_ip))
        && u.port_or_known_default() == Some(port)
    {
        return Err(AppError::new(code::LOOP, "上游不能指向本网关"));
    }
    normalize_api_path(&mut u);
    Ok(u)
}
pub(super) fn normalize_api_path(url: &mut url::Url) {
    let mut path = url.path().trim_end_matches('/').to_owned();
    for endpoint in [
        "/responses/compact",
        "/chat/completions",
        "/alpha/search",
        "/responses",
        "/messages",
        "/completions",
        "/embeddings",
        "/models",
    ] {
        if let Some(base) = path.strip_suffix(endpoint) {
            path = base.to_owned();
            break;
        }
    }
    let versioned = path.split('/').any(|segment| {
        segment
            .strip_prefix('v')
            .is_some_and(|version| version.as_bytes().first().is_some_and(u8::is_ascii_digit))
    });
    if !versioned {
        path.push_str("/v1");
    }
    url.set_path(&path);
}
fn quota_site(base: &str) -> String {
    base.trim_end_matches('/')
        .trim_end_matches("/v1")
        .to_owned()
}
impl Store {
    pub fn load(path: &Path) -> Result<(Self, String)> {
        let raw = storage::read_optional(path)?;
        let invalid = || AppError::new(code::STORE, "网关存储无法读取，请保留原文件");
        let mut store = match &raw {
            Some(raw) => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(raw).map_err(|_| invalid())?;
                // A provider once kept for web searches alone now takes them
                // over, on the model and effort the request names.
                let searching: Vec<String> = value
                    .get_mut("providers")
                    .and_then(serde_json::Value::as_array_mut)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_object_mut)
                    .filter_map(|provider| {
                        let kept = provider.remove("searchOnly")?.as_bool()?;
                        let id = provider.get("id")?.as_str()?;
                        kept.then(|| id.to_owned())
                    })
                    .collect();
                let mut store: Self = serde_json::from_value(value).map_err(|_| invalid())?;
                for id in searching {
                    store.delegates.entry(id).or_default().web_search = true;
                }
                store
            }
            None => Self::default(),
        };
        if store.schema != 2 {
            return Err(AppError::new(code::STORE, "网关存储版本不受支持"));
        }
        store.settings.validate()?;
        store
            .quota_access_tokens
            .retain(|id, token| !token.is_empty() && store.providers.iter().any(|p| &p.id == id));
        store
            .provider_endpoints
            .retain(|id, _| store.providers.iter().any(|p| &p.id == id));
        store
            .grok_connections
            .retain(|id, _| store.providers.iter().any(|p| &p.id == id));
        store
            .dsh_models
            .retain(|id, _| store.providers.iter().any(|p| &p.id == id));
        store
            .model_maps
            .retain(|id, _| store.providers.iter().any(|p| &p.id == id));
        store
            .delegates
            .retain(|id, _| store.providers.iter().any(|p| &p.id == id));
        for p in &store.providers {
            if let Some(values) = store.provider_endpoints.get_mut(&p.id) {
                *values = endpoint_urls(&p.base_url, values, store.settings.port)?;
            }
        }
        Ok((store, storage::revision(raw.as_deref())))
    }
    pub fn persist(&self, path: &Path, expected: &str) -> Result<String> {
        let raw = serde_json::to_vec_pretty(self)
            .map_err(|_| AppError::new(code::STORE, "无法保存网关设置"))?;
        storage::atomic_write(path, &raw, Some(expected))?;
        Ok(storage::digest(&raw))
    }
    pub fn provider_mut(&mut self, id: &str) -> Result<&mut Provider> {
        self.providers
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| AppError::new(code::PROVIDER, "供应商不存在"))
    }
    /// Records the wire protocol of a Grok or DSH provider that was just
    /// saved; `existing` is `None` for a new provider, which is the last one.
    fn save_connection(
        &mut self,
        existing: Option<String>,
        replacing_token: bool,
        protocol: super::grok_config::Protocol,
    ) -> Result<()> {
        let id = existing
            .or_else(|| self.providers.last().map(|p| p.id.clone()))
            .ok_or_else(|| AppError::new(code::PROVIDER, "供应商不存在"))?;
        let connection = self.grok_connections.entry(id.clone()).or_default();
        let mut connection_changed = false;
        if replacing_token {
            // An explicitly edited credential replaces imported auth
            // headers too; otherwise the old secret wins at dispatch.
            connection_changed |= connection.headers.remove("authorization").is_some();
            connection_changed |= connection.headers.remove("x-api-key").is_some();
        }
        if connection.protocol != protocol {
            connection.protocol = protocol;
            connection_changed = true;
        }
        if connection_changed {
            self.provider_mut(&id)?.version = uuid::Uuid::new_v4().to_string();
        }
        Ok(())
    }
    pub fn edit(&mut self, edit: Edit, running: bool) -> Result<()> {
        if let Edit::SaveGrokProvider {
            protocol: super::provider_protocol::Protocol::GeminiNative,
            ..
        } = &edit
        {
            return Err(AppError::new(
                code::PROVIDER,
                "Grok Build 不支持 Gemini Native",
            ));
        }
        if let Edit::SaveDshProvider { protocol, .. } = &edit {
            if *protocol != super::grok_config::Protocol::ChatCompletions {
                return Err(AppError::new(code::PROVIDER, "DSH 仅支持 Chat Completions"));
            }
        }
        match edit {
            Edit::DshModelsProvider { id, mut mapping } => {
                mapping.validate()?;
                let provider = self.provider_mut(&id)?;
                provider.allowed_models = Some(mapping.allowed());
                provider.version = uuid::Uuid::new_v4().to_string();
                self.dsh_models.insert(id, mapping);
            }
            Edit::ModelMapProvider { id, mapping } => {
                self.provider_mut(&id)?;
                let mapping = model_map(mapping)?;
                // Routing still judges the requested model, so the provider's
                // version and its conversations' affinity stay as they are.
                if mapping.is_empty() {
                    self.model_maps.remove(&id);
                } else {
                    self.model_maps.insert(id, mapping);
                }
            }
            Edit::SaveGrokProvider {
                buffer_complete_response,
                delegate,
                id,
                base_url,
                token,
                quota_access_token,
                name,
                protocol,
            } => {
                let existing = id.clone();
                let replacing_token = !token.is_empty();
                self.edit(
                    Edit::SaveProvider {
                        delegate,
                        end_on_message_stop: None,
                        filter_count_tokens: None,
                        buffer_complete_response,
                        endless_mode: None,
                        upstream_retry_seconds: None,
                        normal_retry_seconds: None,
                        risk_codes: None,
                        retry_intensity: None,
                        limit_retry_seconds: None,
                        limit_rest_seconds: None,
                        id,
                        base_url,
                        token,
                        quota_access_token,
                        name,
                    },
                    running,
                )?;
                self.save_connection(existing, replacing_token, protocol)?;
            }
            Edit::SaveDshProvider {
                buffer_complete_response,
                endless_mode,
                upstream_retry_seconds,
                normal_retry_seconds,
                risk_codes,
                retry_intensity,
                limit_retry_seconds,
                limit_rest_seconds,
                id,
                base_url,
                token,
                quota_access_token,
                name,
                protocol,
            } => {
                let existing = id.clone();
                let replacing_token = !token.is_empty();
                self.edit(
                    Edit::SaveProvider {
                        delegate: Default::default(),
                        end_on_message_stop: None,
                        filter_count_tokens: None,
                        buffer_complete_response,
                        endless_mode,
                        upstream_retry_seconds,
                        normal_retry_seconds,
                        risk_codes,
                        retry_intensity,
                        limit_retry_seconds,
                        limit_rest_seconds,
                        id,
                        base_url,
                        token,
                        quota_access_token,
                        name,
                    },
                    running,
                )?;
                self.save_connection(existing, replacing_token, protocol)?;
            }
            Edit::DuplicateProvider { id } => {
                let (index, copy) = self.provider_copy(&id)?;
                self.add_copy(copy, index + 1);
            }
            Edit::ImportCopy { mut copy } => {
                // The other instance held it to its own port.
                copy.provider.base_url =
                    base_url(&copy.provider.base_url, self.settings.port)?.into();
                self.add_copy(*copy, self.providers.len());
            }
            Edit::ImportLink {
                base_url: raw,
                token,
                display_name,
                protocol,
            } => {
                let url = base_url(&raw, self.settings.port)?;
                if self.providers.iter().any(|p| {
                    p.token == token
                        && self
                            .grok_connections
                            .get(&p.id)
                            .is_none_or(|c| c.protocol == protocol)
                        && base_url(&p.base_url, self.settings.port).is_ok_and(|u| {
                            u.as_str().trim_end_matches('/') == url.as_str().trim_end_matches('/')
                        })
                }) {
                    return Err(AppError::new(code::DUPLICATE, "该供应商已存在"));
                }
                let selected = self.selected.clone();
                self.edit(
                    Edit::SaveProvider {
                        delegate: Default::default(),
                        end_on_message_stop: None,
                        filter_count_tokens: None,
                        buffer_complete_response: None,
                        endless_mode: None,
                        upstream_retry_seconds: None,
                        normal_retry_seconds: None,
                        risk_codes: None,
                        retry_intensity: None,
                        limit_retry_seconds: None,
                        limit_rest_seconds: None,
                        id: None,
                        base_url: raw.clone(),
                        token,
                        quota_access_token: None,
                        name: Some(display_name),
                    },
                    running,
                )?;
                if let Some(p) = self.providers.last_mut() {
                    p.base_url = raw;
                    self.grok_connections.insert(
                        p.id.clone(),
                        super::provider_protocol::Connection {
                            protocol,
                            ..Default::default()
                        },
                    );
                }
                self.selected = selected;
            }
            Edit::SaveProvider {
                end_on_message_stop,
                filter_count_tokens,
                buffer_complete_response,
                endless_mode,
                upstream_retry_seconds,
                normal_retry_seconds,
                risk_codes: codes,
                retry_intensity: intensity,
                limit_retry_seconds,
                limit_rest_seconds,
                delegate,
                id,
                base_url: raw,
                token,
                quota_access_token,
                name: display_name,
            } => {
                let u = base_url(&raw, self.settings.port)?;
                let codes = codes.map(risk_codes).transpose()?;
                let intensity = intensity.map(retry_intensity).transpose()?;
                let delegate = delegate.applied(
                    id.as_ref()
                        .and_then(|id| self.delegates.get(id).cloned())
                        .unwrap_or_default(),
                )?;
                if quota_access_token.as_ref().is_some_and(|value| {
                    value.len() > 16384
                        || value.chars().any(char::is_control)
                        || (!value.is_empty() && value.trim() != value)
                }) {
                    return Err(AppError::new(code::TOKEN, "账号访问令牌格式无效"));
                }
                let existing_id = id.clone();
                if let Some(id) = &id {
                    let old = self.provider_mut(id)?.base_url.clone();
                    if quota_site(&old) != quota_site(u.as_str()) {
                        self.quota_access_tokens.remove(id);
                    }
                }
                if token.contains(['\r', '\n']) {
                    return Err(AppError::new(code::TOKEN, "Token 不得包含换行"));
                }
                let label = if let Some(value) = display_name
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                {
                    name(value)?
                } else {
                    u.host_str().unwrap_or("Provider").to_owned()
                };
                if let Some(id) = id {
                    let p = self.provider_mut(&id)?;
                    let base_url = u.as_str().trim_end_matches('/');
                    let credentials_changed =
                        p.base_url != base_url || (!token.is_empty() && p.token != token);
                    if let Some(value) = end_on_message_stop {
                        p.end_on_message_stop = value;
                    }
                    if let Some(value) = filter_count_tokens {
                        p.filter_count_tokens = value;
                    }
                    if let Some(value) = buffer_complete_response {
                        p.buffer_complete_response = value;
                    }
                    if let Some(value) = endless_mode {
                        p.endless_mode = value;
                    }
                    if let Some(value) = upstream_retry_seconds {
                        p.upstream_retry_seconds = value;
                    }
                    if let Some(value) = normal_retry_seconds {
                        p.normal_retry_seconds = value;
                    }
                    if codes.is_some() {
                        p.risk_codes = codes;
                    }
                    if intensity.is_some() {
                        p.retry_intensity = intensity;
                    }
                    if limit_retry_seconds.is_some() {
                        p.limit_retry_seconds = limit_retry_seconds;
                    }
                    if limit_rest_seconds.is_some() {
                        p.limit_rest_seconds = limit_rest_seconds;
                    }
                    p.base_url = base_url.to_owned();
                    if !token.is_empty() {
                        p.token = token;
                    }
                    if display_name.is_some() {
                        p.name = label;
                    }
                    if credentials_changed {
                        p.version = uuid::Uuid::new_v4().to_string();
                    }
                } else {
                    if token.trim().is_empty() {
                        return Err(AppError::new(code::TOKEN, "请输入供应商 Token"));
                    }
                    let id = uuid::Uuid::new_v4().to_string();
                    self.providers.push(Provider {
                        id: id.clone(),
                        name: label,
                        base_url: u.as_str().trim_end_matches('/').into(),
                        token,
                        queued: true,
                        version: uuid::Uuid::new_v4().to_string(),
                        max_concurrency: 0,
                        max_rpm: 0,
                        allowed_models: None,
                        handoff_after_compaction: false,
                        take_new_threads: false,
                        end_on_message_stop: end_on_message_stop.unwrap_or(false),
                        filter_count_tokens: filter_count_tokens.unwrap_or(false),
                        buffer_complete_response: buffer_complete_response.unwrap_or(false),
                        endless_mode: endless_mode.unwrap_or(false),
                        upstream_retry_seconds: upstream_retry_seconds
                            .unwrap_or(DEFAULT_UPSTREAM_RETRY_SECONDS),
                        normal_retry_seconds: normal_retry_seconds
                            .unwrap_or(DEFAULT_NORMAL_RETRY_SECONDS),
                        risk_codes: codes,
                        retry_intensity: intensity,
                        limit_retry_seconds,
                        limit_rest_seconds,
                    });
                    if self.selected.is_none() {
                        self.selected = Some(id);
                    }
                }
                let id = existing_id.unwrap_or_else(|| self.providers.last().unwrap().id.clone());
                if let Some(value) = quota_access_token {
                    if value.is_empty() {
                        self.quota_access_tokens.remove(&id);
                    } else {
                        self.quota_access_tokens.insert(id.clone(), value);
                    }
                }
                if delegate == Delegate::default() {
                    self.delegates.remove(&id);
                } else {
                    self.delegates.insert(id, delegate);
                }
            }
            Edit::RenameProvider { id, name: value } => {
                self.provider_mut(&id)?.name = name(&value)?
            }
            Edit::EndpointsProvider { id, base_urls } => {
                let primary = self.provider_mut(&id)?.base_url.clone();
                let values = endpoint_urls(&primary, &base_urls, self.settings.port)?;
                if values.is_empty() {
                    self.provider_endpoints.remove(&id);
                } else {
                    self.provider_endpoints.insert(id, values);
                }
            }
            Edit::SwapEndpoint {
                id,
                base_url: selected,
            } => {
                let primary = self.provider_mut(&id)?.base_url.clone();
                let mut values = self
                    .provider_endpoints
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();
                let index = values
                    .iter()
                    .position(|url| url == &selected)
                    .ok_or_else(|| AppError::new(code::ENDPOINT, "辅助端点已变化，请刷新后重试"))?;
                values[index] = primary;
                if quota_site(&values[index]) != quota_site(&selected) {
                    self.quota_access_tokens.remove(&id);
                }
                // Swapping roles does not change credentials or the endpoint set.
                // Keep the provider version so existing conversations retain affinity.
                self.provider_mut(&id)?.base_url = selected;
                self.provider_endpoints.insert(id, values);
            }
            Edit::DeleteProvider { id } => {
                self.quota_access_tokens.remove(&id);
                self.grok_connections.remove(&id);
                self.providers.retain(|p| p.id != id);
                self.dsh_models.remove(&id);
                self.model_maps.remove(&id);
                self.delegates.remove(&id);
                self.provider_endpoints.remove(&id);
                if self.selected.as_deref() == Some(&id) {
                    self.selected = self.providers.first().map(|p| p.id.clone());
                }
            }
            Edit::QueueProvider { id, queued } => self.provider_mut(&id)?.queued = queued,
            Edit::ConcurrencyProvider {
                id,
                max_concurrency,
            } => {
                if max_concurrency > 100000 {
                    return Err(AppError::new(
                        code::CONCURRENCY,
                        "并发上限应为 0–100000，0 表示不限",
                    ));
                }
                self.provider_mut(&id)?.max_concurrency = max_concurrency;
            }
            Edit::RpmProvider { id, max_rpm } => {
                if max_rpm > 100000 {
                    return Err(AppError::new(
                        code::RPM,
                        "RPM 上限应为 0–100000，0 表示不限",
                    ));
                }
                self.provider_mut(&id)?.max_rpm = max_rpm;
            }
            Edit::ModelsProvider { id, allowed_models } => {
                let allowed = allowed_models
                    .map(|models| {
                        let mut result = Vec::new();
                        for model in models {
                            let model = model.trim();
                            if model.is_empty()
                                || model.len() > 256
                                || model.chars().any(char::is_control)
                                || model.contains('*')
                            {
                                return Err(AppError::new(
                                    code::MODELS,
                                    "模型 ID 应为 1–256 字节，不能包含控制字符或通配符",
                                ));
                            }
                            if !result.iter().any(|m| m == model) {
                                result.push(model.to_owned());
                            }
                        }
                        if result.is_empty() || result.len() > 4096 {
                            return Err(AppError::new(code::MODELS, "白名单应包含 1–4096 个模型"));
                        }
                        Ok(result)
                    })
                    .transpose()?;
                self.provider_mut(&id)?.allowed_models = allowed;
            }
            Edit::PolicyProvider {
                id,
                allowed_models,
                handoff_after_compaction,
                take_new_threads,
            } => {
                self.edit(
                    Edit::ModelsProvider {
                        id: id.clone(),
                        allowed_models,
                    },
                    running,
                )?;
                let provider = self.provider_mut(&id)?;
                if let Some(value) = handoff_after_compaction {
                    provider.handoff_after_compaction = value;
                }
                if let Some(value) = take_new_threads {
                    provider.take_new_threads = value;
                }
            }
            Edit::TakeoverProvider {
                id,
                handoff_after_compaction,
                take_new_threads,
            } => {
                if handoff_after_compaction.is_none() && take_new_threads.is_none() {
                    return Err(AppError::new(code::SETTINGS, "请选择要修改的接管开关"));
                }
                let provider = self.provider_mut(&id)?;
                if let Some(value) = handoff_after_compaction {
                    provider.handoff_after_compaction = value;
                }
                if let Some(value) = take_new_threads {
                    provider.take_new_threads = value;
                }
            }
            Edit::Reorder { ids } => {
                let mut sorted = ids.clone();
                sorted.sort();
                sorted.dedup();
                if sorted.len() != self.providers.len()
                    || ids.len() != sorted.len()
                    || sorted
                        .iter()
                        .any(|id| !self.providers.iter().any(|p| &p.id == id))
                {
                    return Err(AppError::new(
                        code::QUEUE,
                        "排序必须包含每个供应商且不能重复",
                    ));
                }
                self.providers
                    .sort_by_key(|p| ids.iter().position(|id| id == &p.id).unwrap_or(usize::MAX));
            }
            Edit::Select { id } => {
                self.provider_mut(&id)?;
                self.selected = Some(id);
                self.mode = "manual".into();
            }
            Edit::Mode { mode } => {
                if !["manual", "auto"].contains(&mode.as_str()) {
                    return Err(AppError::new(code::MODE, "模式无效"));
                }
                self.mode = mode;
            }
            Edit::UpsertFailureRule { rule } => {
                let rules = &mut self.settings.custom_failure_rules;
                if let Some(existing) = rules.iter_mut().find(|r| r.id == rule.id) {
                    *existing = rule;
                } else {
                    rules.push(rule);
                }
                super::failure_rules::validate(rules)?;
            }
            Edit::DeleteFailureRule { id } => {
                self.settings.custom_failure_rules.retain(|r| r.id != id);
            }
            Edit::Settings { settings } => {
                settings.validate()?;
                if running && settings.port != self.settings.port {
                    return Err(AppError::new(code::RUNNING, "修改端口前请先停用网关"));
                }
                for p in &self.providers {
                    base_url(&p.base_url, settings.port)?;
                }
                self.settings = settings;
            }
            Edit::ModelCatalog { enabled } => self.model_catalog = enabled,
            Edit::ModelCatalogSource { official } => self.model_catalog_official = official,
            Edit::Import | Edit::Reset { .. } | Edit::Connection { .. } => {
                unreachable!("handled by gateway")
            }
        }
        for provider in &self.providers {
            if let Some(urls) = self.provider_endpoints.get_mut(&provider.id) {
                *urls = endpoint_urls(&provider.base_url, urls, self.settings.port)?;
            }
        }
        Ok(())
    }
}

pub fn endpoint_urls(primary: &str, values: &[String], port: u16) -> Result<Vec<String>> {
    if values.len() > 8 {
        return Err(AppError::new(code::ENDPOINT, "最多添加 8 个辅助端点"));
    }
    let canonical = |raw: &str| -> Result<String> {
        if raw.len() > 2048 || raw.chars().any(char::is_control) {
            return Err(AppError::new(code::ENDPOINT, "端点地址过长或含有无效字符"));
        }
        Ok(base_url(raw, port)?
            .as_str()
            .trim_end_matches('/')
            .to_owned())
    };
    let main = canonical(primary)?;
    let mut result = Vec::new();
    for value in values {
        let url = canonical(value)?;
        if url != main && !result.contains(&url) {
            result.push(url);
        }
    }
    Ok(result)
}
