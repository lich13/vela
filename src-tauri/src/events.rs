//! Exception journal; complete upstream errors live in separately loaded attachments.
use crate::errors::code;
#[path = "event_details.rs"]
mod details;
use crate::{
    gateway::ClientId,
    storage::{self, AppError, Result},
};
pub(crate) use details::safe_upstream_reason;
pub use details::{
    safe_message, CircuitEvidence, DeliveryEvidence, Details, ErrorBasis, HealthScope, Phase,
    QuotaBasis, QuotaCheck, QuotaSource, QuotaVerdict, RoutingDetails, RoutingOutcome,
    RpmWaitReason,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{hash_map::Entry, HashMap, HashSet, VecDeque},
    io::{BufRead, BufReader},
    path::Path,
    sync::{mpsc, Arc},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{broadcast, oneshot};
const RETENTION: u64 = 7 * 86400;
const MERGE: u64 = 300;
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    ModelUnavailable,
    Authentication,
    UpstreamService,
    Network,
    RateLimit,
    Capacity,
    FailoverExhausted,
    Failover,
    CircuitOpen,
    ConfigConflict,
    StartupRecovery,
    AccountSync,
    ProtocolError,
    ThreadHandoff,
    ThreadRouting,
    RequestInterrupted,
    NetworkHold,
    NetworkRestored,
}
impl Reason {
    pub fn notifiable(self, _action: Action) -> bool {
        matches!(
            self,
            Self::CircuitOpen | Self::ThreadHandoff | Self::NetworkHold | Self::NetworkRestored
        )
    }
    #[cfg(any(feature = "desktop", test))]
    pub fn text(self) -> &'static str {
        match self {
            Self::ModelUnavailable => "指定模型不存在或无权使用",
            Self::Authentication => "上游认证失败",
            Self::UpstreamService => "上游服务异常",
            Self::Network => "上游连接失败",
            Self::RateLimit => "上游限流",
            Self::Capacity => "模型容量不足",
            Self::FailoverExhausted => "请求未完成",
            Self::Failover => "自动换商",
            Self::CircuitOpen => "供应商已熔断",
            Self::ConfigConflict => "配置发生冲突",
            Self::StartupRecovery => "启动恢复失败",
            Self::AccountSync => "账号同步异常",
            Self::ProtocolError => "上游协议返回错误",
            Self::ThreadHandoff => "线程已接管",
            Self::ThreadRouting => "线程路由结果",
            Self::RequestInterrupted => "请求交付提前结束",
            Self::NetworkHold => "网络不通，请求已暂存",
            Self::NetworkRestored => "网络已恢复",
        }
    }
    pub fn code(self) -> ErrorCode {
        match self {
            Self::ModelUnavailable => ErrorCode::ModelUnavailable,
            Self::Authentication => ErrorCode::AuthenticationFailed,
            Self::UpstreamService => ErrorCode::UpstreamServiceError,
            Self::Network => ErrorCode::NetworkError,
            Self::RateLimit => ErrorCode::RateLimited,
            Self::Capacity => ErrorCode::CapacityLimited,
            Self::FailoverExhausted => ErrorCode::FailoverExhausted,
            Self::Failover => ErrorCode::Failover,
            Self::CircuitOpen => ErrorCode::CircuitOpen,
            Self::ConfigConflict => ErrorCode::ConfigConflict,
            Self::StartupRecovery => ErrorCode::StartupRecoveryFailed,
            Self::AccountSync => ErrorCode::AccountSyncError,
            Self::ProtocolError => ErrorCode::ProtocolError,
            Self::ThreadHandoff => ErrorCode::ThreadHandoff,
            Self::ThreadRouting => ErrorCode::ThreadRouting,
            Self::RequestInterrupted => ErrorCode::RequestInterrupted,
            Self::NetworkHold => ErrorCode::NetworkHold,
            Self::NetworkRestored => ErrorCode::NetworkRestored,
        }
    }
}
pub use crate::errors::ErrorCode;
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    TryingNext,
    Returned,
    Waiting,
    Stopped,
    Routed,
    Reconnecting,
    RetryingSame,
    NotRetried,
}
impl Action {
    #[cfg(any(feature = "desktop", test))]
    pub fn text(self) -> &'static str {
        match self {
            Self::TryingNext => "尝试下一供应商",
            Self::Returned => "已反馈客户端",
            Self::Waiting => "等待后重试",
            Self::Stopped => "需要处理",
            Self::Routed => "已切换供应商",
            Self::Reconnecting => "等待后重新连接",
            Self::RetryingSame => "原供应商等待重试",
            Self::NotRetried => "响应已开始，未重试",
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Warning,
    Error,
    Info,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    OrdinaryError,
    CircuitError,
    Status,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Upstream,
    Gateway,
    Antigravity,
    ClaudeCode,
}
// External identities must not become executable gateway clients.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum JournalClient {
    Active(ClientId),
    External(ExternalClient),
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExternalClient {
    Antigravity,
    /// Errors Claude Code wrote to its own session files. The name differs
    /// from the gateway client `claude`, which would otherwise win the
    /// untagged match and make these records look like gateway traffic.
    #[serde(rename = "claude_code")]
    ClaudeCode,
}
impl From<ClientId> for JournalClient {
    fn from(value: ClientId) -> Self {
        Self::Active(value)
    }
}
impl JournalClient {
    pub fn active(self) -> Option<ClientId> {
        match self {
            Self::Active(c) => Some(c),
            Self::External(_) => None,
        }
    }
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Active(client) => client.name(),
            Self::External(ExternalClient::Antigravity) => "Antigravity",
            Self::External(ExternalClient::ClaudeCode) => "Claude Code",
        }
    }
}
/// Query-only classification. Persisted records keep their original schema.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassifiedRecord {
    #[serde(flatten)]
    pub record: Record,
    pub category: Category,
    pub origin: Origin,
}
impl std::ops::Deref for ClassifiedRecord {
    type Target = Record;
    fn deref(&self) -> &Record {
        &self.record
    }
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StatusGroup {
    Success,
    ClientError,
    ServerError,
    NoStatus,
}
impl StatusGroup {
    pub fn matches(self, status: Option<u16>) -> bool {
        match self {
            Self::Success => status.is_some_and(|v| (200..300).contains(&v)),
            Self::ClientError => status.is_some_and(|v| (400..500).contains(&v)),
            Self::ServerError => status.is_some_and(|v| (500..600).contains(&v)),
            Self::NoStatus => status.is_none(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Record {
    #[serde(skip)]
    pub notification_message: Option<String>,
    #[serde(skip)]
    pub notification_provider: Option<String>,
    pub id: String,
    pub first_at: u64,
    pub last_at: u64,
    pub count: u32,
    pub client_id: Option<JournalClient>,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub reason: Reason,
    pub action: Action,
    pub level: Level,
    pub status: Option<u16>,
    pub attempt: Option<u32>,
    pub error_code: ErrorCode,
    pub notified_at: u64,
    pub details: Details,
}
impl Record {
    pub fn configuration_failure(
        client: ClientId,
        error: &AppError,
        startup: bool,
    ) -> Option<Self> {
        let reason = if matches!(
            error.code.as_str(),
            code::CONFLICT | code::RESUME_CONFLICT | code::CLAUDE_PROFILE_CONFLICT
        ) {
            Reason::ConfigConflict
        } else if startup {
            Reason::StartupRecovery
        } else {
            return None;
        };
        let mut record = Self::new(
            Some(client),
            None,
            None,
            reason,
            Action::Stopped,
            None,
            None,
        );
        record.details.local_code = safe_message(&error.code);
        record.details.message = safe_message(&error.message);
        Some(record)
    }
    fn visible(&self) -> bool {
        if self.reason != Reason::ThreadRouting {
            return true;
        }
        self.details.routing.as_ref().is_none_or(|routing| {
            routing.failure.is_some()
                || self.level != Level::Info
                || self.status.is_some_and(|status| status >= 400)
                || self.action == Action::Stopped
                || matches!(
                    routing.evidence.identity,
                    Some(crate::usage::model::ThreadIdentity::Conflict)
                )
                || routing.outcome == RoutingOutcome::SaveFailed
        })
    }
    pub fn origin(&self) -> Origin {
        match self.client_id {
            Some(JournalClient::External(ExternalClient::Antigravity)) => {
                return Origin::Antigravity
            }
            Some(JournalClient::External(ExternalClient::ClaudeCode)) => return Origin::ClaudeCode,
            _ => {}
        }
        let response_error = matches!(
            self.reason,
            Reason::ModelUnavailable
                | Reason::Authentication
                | Reason::UpstreamService
                | Reason::RateLimit
                | Reason::Capacity
                | Reason::ProtocolError
        );
        if response_error
            && !(self.level == Level::Info && self.details.quota_check.is_some())
            && (self.details.raw_error.is_some()
                || self.details.raw.is_some()
                || self.details.upstream_code.is_some()
                || self.details.upstream_type.is_some()
                || self.status.is_some_and(|s| s >= 400)
                || (self.status.is_some() && self.details.error_basis.is_some()))
        {
            Origin::Upstream
        } else {
            Origin::Gateway
        }
    }
    pub fn antigravity_error(
        key: &str,
        timestamp: u64,
        model: Option<&str>,
        raw_error: &str,
    ) -> Self {
        let lower = raw_error.to_ascii_lowercase();
        let (reason, status, code_label) = if lower.contains("stream was interrupted")
            || lower.contains("5m0s")
            || lower.contains("deadline_exceeded")
            || lower.contains("context deadline exceeded")
            || lower.contains("timed out")
        {
            (Reason::RequestInterrupted, None, "STREAM_TIMEOUT")
        } else if lower.contains("unexpected eof") || lower.contains("stream reading error") {
            // The connection closed while the reply was still being read.
            (Reason::RequestInterrupted, None, "STREAM_INTERRUPTED")
        } else if lower.contains("resource_exhausted")
            || lower.contains("quota")
            || lower.contains("rate limit")
            || lower.contains("too many requests")
            || has_status_token(&lower, "429")
        {
            (Reason::RateLimit, Some(429), "RESOURCE_EXHAUSTED")
        } else if lower.contains("unauthenticated")
            || lower.contains("permission_denied")
            || lower.contains("invalid_grant")
            || has_status_token(&lower, "401")
            || has_status_token(&lower, "403")
        {
            (Reason::Authentication, Some(401), "UNAUTHENTICATED")
        } else if lower.contains("model")
            && (lower.contains("not found")
                || lower.contains("unavailable")
                || lower.contains("not supported"))
        {
            (Reason::ModelUnavailable, Some(404), "MODEL_UNAVAILABLE")
        } else {
            (Reason::UpstreamService, Some(502), "ANTIGRAVITY_ERROR")
        };
        Self::external(
            ExternalClient::Antigravity,
            &format!("antigravity-event:{key}"),
            timestamp,
            model,
            reason,
            status,
            Details {
                upstream_code: Some(code_label.into()),
                message: raw_log_message(raw_error),
                ..Default::default()
            },
        )
    }
    /// An error Claude Code recorded in a session file, classified from the
    /// structured fields of that line. `None` for any other line.
    pub fn claude_session_error(line: &serde_json::Value) -> Option<Self> {
        if line["isApiErrorMessage"] != true {
            return None;
        }
        let text = |v: &serde_json::Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        let timestamp = line["timestamp"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .and_then(|at| u64::try_from(at.timestamp()).ok())?;
        let key = format!(
            "claude-code-event:{}:{}",
            line["sessionId"].as_str()?,
            line["uuid"].as_str()?
        );
        let status = line["apiErrorStatus"]
            .as_u64()
            .and_then(|s| u16::try_from(s).ok())
            .filter(|s| (100..600).contains(s));
        let message = line["message"]["content"]
            .as_array()
            .and_then(|blocks| blocks.iter().find_map(|block| block["text"].as_str()))
            .and_then(raw_log_message);
        let mut details = Details {
            upstream_code: text(&line["error"]),
            upstream_type: text(&line["apiError"]),
            upstream_reason: text(&line["streamCutCause"]),
            message,
            ..Default::default()
        };
        let reason = claude_session_reason(status, &mut details);
        Some(Self::external(
            ExternalClient::ClaudeCode,
            &key,
            timestamp,
            None,
            reason,
            status,
            details,
        ))
    }
    /// Session errors stored before they were classified get the same
    /// classification when the journal is read, as do stored 429s that only
    /// said the service was unavailable.
    fn classify_stored(&mut self) {
        let vague = matches!(
            self.details.error_basis,
            None | Some(ErrorBasis::UnknownRateLimit)
        );
        if !vague {
            return;
        }
        if matches!(
            self.client_id,
            Some(JournalClient::External(ExternalClient::ClaudeCode))
        ) {
            self.reason = claude_session_reason(self.status, &mut self.details);
            self.level = external_level(self.reason);
        } else if self.details.error_basis.is_some() {
            crate::gateway::unavailable_429(self.status, &mut self.details);
            if self.details.error_basis != Some(ErrorBasis::Capacity)
                || self.reason != Reason::RateLimit
            {
                return;
            }
            self.reason = Reason::Capacity;
        } else {
            return;
        }
        self.error_code = self.reason.code();
    }
    /// A record read from another application's own files. The id derives
    /// from `key`, so reading the same line again does not add a second one.
    fn external(
        client: ExternalClient,
        key: &str,
        timestamp: u64,
        model: Option<&str>,
        reason: Reason,
        status: Option<u16>,
        details: Details,
    ) -> Self {
        let digest = storage::digest(key.as_bytes());
        let mut bytes = [0u8; 16];
        for (i, slot) in bytes.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&digest[i * 2..i * 2 + 2], 16).unwrap_or(0);
        }
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let t = timestamp.min(now());
        Self {
            notification_message: None,
            notification_provider: None,
            id: uuid::Uuid::from_bytes(bytes).to_string(),
            first_at: t,
            last_at: t,
            count: 1,
            client_id: Some(JournalClient::External(client)),
            provider_id: None,
            model: model.and_then(safe_model),
            reason,
            action: Action::Returned,
            level: external_level(reason),
            status,
            attempt: Some(1),
            error_code: reason.code(),
            notified_at: 0,
            details: details.sanitized(),
        }
    }
    fn merge_seconds(&self) -> u64 {
        if self.reason == Reason::RequestInterrupted {
            60
        } else {
            MERGE
        }
    }
    pub fn new(
        client_id: Option<ClientId>,
        provider_id: Option<&str>,
        model: Option<&str>,
        reason: Reason,
        action: Action,
        status: Option<u16>,
        attempt: Option<u32>,
    ) -> Self {
        let t = now();
        Self {
            notification_message: None,
            notification_provider: None,
            id: uuid::Uuid::new_v4().to_string(),
            first_at: t,
            last_at: t,
            count: 1,
            client_id: client_id.map(Into::into),
            provider_id: provider_id.and_then(safe_id),
            model: model.and_then(safe_model),
            reason,
            action,
            level: if matches!(
                reason,
                Reason::Failover
                    | Reason::ThreadHandoff
                    | Reason::ThreadRouting
                    | Reason::NetworkRestored
            ) {
                Level::Info
            } else if matches!(
                reason,
                Reason::ModelUnavailable
                    | Reason::RateLimit
                    | Reason::Capacity
                    | Reason::NetworkHold
            ) {
                Level::Warning
            } else {
                Level::Error
            },
            status,
            attempt,
            error_code: reason.code(),
            notified_at: 0,
            details: Details::default(),
        }
    }
    #[cfg(any(feature = "desktop", test))]
    pub fn notification(&self) -> String {
        self.notification_at(now())
    }
    #[cfg(any(feature = "desktop", test))]
    fn notification_at(&self, timestamp: u64) -> String {
        if self.reason == Reason::CircuitOpen {
            let provider = self
                .notification_provider
                .as_deref()
                .filter(|s| s.chars().count() <= 120)
                .and_then(safe_message)
                .unwrap_or_else(|| {
                    format!(
                        "供应商 {}",
                        self.provider_id
                            .as_deref()
                            .map(|s| &s[..s.len().min(8)])
                            .unwrap_or("未记录")
                    )
                });
            let scope = match self.details.health_scope {
                Some(HealthScope::Model) => format!(
                    "模型熔断 · {}",
                    self.model.as_deref().unwrap_or("模型未记录")
                ),
                Some(HealthScope::Provider) => "供应商熔断 · 全部模型".into(),
                _ => "熔断 · 影响范围未记录".into(),
            };
            let evidence = self.details.circuit.as_ref();
            let cause = self
                .details
                .upstream_code
                .as_deref()
                .or(self.details.local_code.as_deref());
            let mut direct = Vec::new();
            if let Some(status) = self.status {
                direct.push(format!("HTTP {status}"));
            }
            if let Some(code) = cause {
                direct.push(code.to_owned());
            }
            if let Some(message) = self.details.message.as_deref().and_then(safe_message) {
                direct.push(message.chars().take(120).collect());
            } else if let Some(reason) = evidence.and_then(|c| c.cause_reason) {
                direct.push(reason.text().into());
            }
            if direct.is_empty() {
                direct.push("原因未记录".into());
            }
            let trigger = evidence
                .map(|c| match c.trigger.as_str() {
                    "consecutive_failures" | "transient_failures" => {
                        format!("连续失败 {}/{}", c.failures, c.failure_threshold)
                    }
                    "probe_failed" => "恢复探测失败".into(),
                    _ => "触发条件未记录".into(),
                })
                .unwrap_or_else(|| "触发条件未记录".into());
            let wait = match evidence.and_then(|c| c.retry_at) {
                Some(at) if at > timestamp => {
                    format!("等待 {} 秒后可探测", at - timestamp)
                }
                Some(_) => "待请求探测".into(),
                None => "等待时间未记录".into(),
            };
            return format!(
                "{} · {provider}：{scope}；{}；{trigger}；{wait}",
                self.client_id.map(JournalClient::name).unwrap_or("应用"),
                direct.join(" · ")
            );
        }
        if let Some(message) = &self.notification_message {
            return message.clone();
        }
        format!(
            "{} · {}{}：{}；{}",
            self.client_id.map(JournalClient::name).unwrap_or("应用"),
            self.provider_id
                .as_deref()
                .map(|s| &s[..s.len().min(8)])
                .unwrap_or("系统"),
            self.model
                .as_ref()
                .map(|s| format!(" · {s}"))
                .unwrap_or_default(),
            self.reason.text(),
            self.action.text()
        )
    }
    fn same(&self, other: &Self) -> bool {
        if self.client_id != other.client_id
            || self.provider_id != other.provider_id
            || self.model != other.model
            || self.reason != other.reason
            || self.status != other.status
            || self.action != other.action
        {
            return false;
        }
        let mut details = self.details.clone();
        let mut other_details = other.details.clone();
        details.last_progress_ms = None;
        other_details.last_progress_ms = None;
        details.delivery = None;
        other_details.delivery = None;
        if let Some(raw) = &mut details.raw_error {
            raw.id.clear();
        }
        if let Some(raw) = &mut other_details.raw_error {
            raw.id.clear();
        }
        details == other_details
    }
}
fn safe_id(s: &str) -> Option<String> {
    uuid::Uuid::parse_str(s).ok().map(|v| v.to_string())
}
fn external_level(reason: Reason) -> Level {
    if matches!(
        reason,
        Reason::ModelUnavailable | Reason::RateLimit | Reason::Capacity
    ) {
        Level::Warning
    } else {
        Level::Error
    }
}
/// Classifies a Claude Code session error with the gateway's rules and fills
/// in the basis; errors without one fall back to how the request ended.
fn claude_session_reason(status: Option<u16>, details: &mut Details) -> Reason {
    let (basis, scope, reason) = crate::gateway::external_details(
        status,
        details.upstream_code.as_deref().unwrap_or_default(),
        details.message.as_deref().unwrap_or_default(),
    );
    details.error_basis = basis;
    details.health_scope = scope;
    if let Some(reason) = reason {
        reason
    } else if details.upstream_reason.is_some() {
        Reason::RequestInterrupted
    } else if details.upstream_type.as_deref() == Some("connection_failed") {
        Reason::Network
    } else {
        Reason::UpstreamService
    }
}
fn raw_log_message(value: &str) -> Option<String> {
    let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then(|| text.chars().take(4096).collect())
}
/// Feeds `on_line` the complete lines appended to `path` since the last scan.
/// The first scan of a file starts at most `backlog` bytes before its end.
#[cfg(unix)]
type SourceStamp = (u64, u64);
#[cfg(not(unix))]
type SourceStamp = Option<SystemTime>;

fn source_stamp(meta: &std::fs::Metadata) -> SourceStamp {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    }
    #[cfg(not(unix))]
    {
        meta.created().ok()
    }
}

fn source_identity(path: &Path, meta: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    let id = {
        let (dev, ino) = source_stamp(meta);
        format!("{dev}:{ino}")
    };
    #[cfg(not(unix))]
    let id = format!("{:?}", source_stamp(meta));
    storage::digest(format!("{}:{id}", path.to_string_lossy()).as_bytes())
}

fn scan_new_lines(
    path: &Path,
    meta: &std::fs::Metadata,
    (backlog, line_limit): (u64, usize),
    cursors: &mut FileCursors,
    cleared: Option<&ClearMark>,
    mut on_line: impl FnMut(u64, &[u8]),
) {
    use std::io::{Read, Seek, SeekFrom};
    if cursors.get(path).is_some_and(|c| {
        c.stamp == source_stamp(meta)
            && c.len == meta.len()
            && c.modified == meta.modified().ok()
            && c.offset == meta.len()
    }) {
        return;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    let Ok(meta) = file.metadata() else {
        return;
    };
    let stamp = source_stamp(&meta);
    let identity = source_identity(path, &meta);
    let file_len = meta.len();
    let modified = meta.modified().ok();
    let previous = cursors
        .get(path)
        .filter(|c| c.identity == identity && file_len >= c.offset);
    // On Windows the identity is the time the file was created, which passes
    // to a file that replaces it under the same name; the open file says
    // whether it is still the one that was read.
    #[cfg(windows)]
    let opened = storage::file_id(&file);
    #[cfg(windows)]
    let previous = previous.filter(|c| c.file == opened);
    let mut offset = previous.map_or_else(
        || {
            cleared
                .and_then(|c| c.files.get(&identity))
                .copied()
                .filter(|n| *n <= file_len)
                .unwrap_or(0)
        },
        |c| c.offset,
    );
    let mut discarding = previous.is_some_and(|c| c.discarding);
    let mut positioned = false;
    if previous.is_none() && offset > 0 {
        // A clear can occur while a source is halfway through writing a line.
        let mut last = [0];
        if file.seek(SeekFrom::Start(offset - 1)).is_err() || file.read_exact(&mut last).is_err() {
            return;
        }
        discarding = last[0] != b'\n';
        positioned = true;
    }
    if offset == 0 && file_len > backlog {
        offset = file_len - backlog;
        discarding = true;
    }
    if offset > 0 && !positioned {
        let _ = file.seek(SeekFrom::Start(offset));
    }
    let mut reader = BufReader::new(file.take(file_len - offset));
    let mut line = Vec::new();
    let mut line_offset = offset;
    while let Ok(bytes) = reader.fill_buf() {
        if bytes.is_empty() {
            break;
        }
        let end = memchr::memchr(b'\n', bytes);
        let count = end.map_or(bytes.len(), |i| i + 1);
        if !discarding {
            if line.len() + count <= line_limit {
                line.extend_from_slice(&bytes[..count]);
            } else {
                line.clear();
                discarding = true;
            }
        }
        reader.consume(count);
        offset += count as u64;
        if end.is_some() {
            if !discarding {
                on_line(line_offset, &line);
            }
            discarding = false;
            line.clear();
            line_offset = offset;
        }
    }
    // Short partial lines are reread after completion. Oversized lines keep
    // their discard position so neither allocation nor scanning repeats.
    if !discarding && !line.is_empty() {
        offset = line_offset;
    }
    cursors.insert(
        path.to_path_buf(),
        FileCursor {
            len: file_len,
            modified,
            offset,
            discarding,
            stamp,
            identity,
            #[cfg(windows)]
            file: opened,
        },
    );
}
/// Session files under `dir` modified since `cutoff`, retaining enumeration metadata.
/// Claude Code nests subagent sessions a few directories deep.
fn session_files(dir: &Path, depth: usize, cutoff: u64, out: &mut Vec<SourceFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if depth < 6 {
                session_files(&path, depth + 1, cutoff, out);
            }
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let secs = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            if secs >= cutoff {
                out.push((secs, meta, path));
            }
        }
    }
}
type SourceFile = (u64, std::fs::Metadata, std::path::PathBuf);
fn recent_file(path: std::path::PathBuf, cutoff: u64) -> Option<SourceFile> {
    let meta = std::fs::metadata(&path).ok()?;
    let secs = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    (meta.is_file() && secs >= cutoff).then_some((secs, meta, path))
}
/// The step files and the logs Antigravity wrote since `cutoff`: the app's,
/// and those `agy` keeps in a directory of its own.
fn antigravity_files(home: &Path, cutoff: u64) -> (Vec<SourceFile>, [Vec<SourceFile>; 3]) {
    let mut transcripts = Vec::new();
    for (brain, steps) in [
        (".gemini/antigravity/brain", "transcript.jsonl"),
        (".gemini/antigravity-cli/brain", "transcript_full.jsonl"),
    ] {
        let Ok(entries) = std::fs::read_dir(home.join(brain)) else {
            continue;
        };
        for entry in entries.flatten() {
            let steps = entry.path().join(".system_generated/logs").join(steps);
            transcripts.extend(recent_file(steps, cutoff));
        }
    }
    let mut logs = [Vec::new(), Vec::new(), Vec::new()];
    for (logs, root) in logs.iter_mut().zip([
        home.join("Library/Logs/Antigravity"),
        home.join(".gemini/antigravity/logs"),
        home.join(".gemini/antigravity-cli/log"),
    ]) {
        if let Ok(entries) = std::fs::read_dir(root) {
            logs.extend(
                entries
                    .flatten()
                    .filter_map(|entry| recent_file(entry.path(), cutoff)),
            );
        }
    }
    (transcripts, logs)
}
fn has_status_token(lower: &str, code: &str) -> bool {
    let bytes = lower.as_bytes();
    let pat = code.as_bytes();
    for (idx, _) in lower.match_indices(code) {
        let prev = idx.checked_sub(1).map(|i| bytes[i]);
        let prev2 = idx.checked_sub(2).map(|i| bytes[i]);
        let next = bytes.get(idx + pat.len()).copied();
        let bad_prev = prev.is_some_and(|b| {
            b.is_ascii_alphanumeric()
                || b == b'.'
                || b == b'-'
                || (b == b':' && prev2.is_some_and(|p| p.is_ascii_digit()))
        });
        let bad_next =
            next.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b':' || b == b'-');
        if !bad_prev && !bad_next {
            return true;
        }
    }
    false
}
fn is_actionable_antigravity_log_error(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("[error]")
        || lower.contains("level=error")
        || lower.contains("unauthenticated")
        || lower.contains("resource_exhausted")
        || lower.contains("permission_denied")
        || lower.contains("deadline_exceeded")
        || lower.contains("context deadline exceeded")
        || lower.contains("stream was interrupted")
        || lower.contains("5m0s")
        || lower.contains("quota")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("invalid_grant")
        || has_status_token(&lower, "401")
        || has_status_token(&lower, "403")
        || has_status_token(&lower, "429")
        || has_status_token(&lower, "500")
        || has_status_token(&lower, "502")
        || has_status_token(&lower, "503")
        || has_status_token(&lower, "504")
}
fn parse_antigravity_log_line(line: &str, fallback_secs: u64) -> Option<(u64, &str)> {
    use chrono::{Datelike, TimeZone};
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(after_bracket) = trimmed.strip_prefix('[') {
        if let Some((ts_part, rest)) = after_bracket.split_once(']') {
            if let Some(ndt) = ts_part
                .get(..19)
                .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").ok())
            {
                let ts = chrono::Local
                    .from_local_datetime(&ndt)
                    .single()
                    .and_then(|dt| u64::try_from(dt.timestamp()).ok())
                    .unwrap_or(fallback_secs);
                let rest_trimmed = rest.trim();
                let lower = rest_trimmed.to_ascii_lowercase();
                if lower.contains("[error]") || lower.contains("level=error") {
                    let body = rest_trimmed
                        .strip_prefix("[error]")
                        .or_else(|| rest_trimmed.strip_prefix("[ERROR]"))
                        .unwrap_or(rest_trimmed)
                        .trim();
                    return (!body.is_empty()).then_some((ts, body));
                }
                return None;
            }
        }
    }
    let glog = trimmed
        .strip_prefix("ERROR: logging before google.Init: ")
        .unwrap_or(trimmed);
    if let Some(epoch_part) = glog.strip_prefix("E0000 00:00:") {
        let (sec_str, _) = epoch_part.split_once('.')?;
        let ts = sec_str.parse::<u64>().unwrap_or(fallback_secs);
        let body = glog.split_once(']').map(|(_, b)| b.trim()).unwrap_or(glog);
        return is_actionable_antigravity_log_error(body).then_some((ts, body));
    }
    if glog.starts_with('E')
        && glog.len() > 14
        && glog.as_bytes()[1..5].iter().all(u8::is_ascii_digit)
        && glog.as_bytes()[5] == b' '
    {
        let mm = glog[1..3].parse::<u32>().ok()?;
        let dd = glog[3..5].parse::<u32>().ok()?;
        let hms = glog.get(6..14)?;
        let year = chrono::DateTime::from_timestamp(fallback_secs as i64, 0)
            .map(|dt| dt.with_timezone(&chrono::Local).year())
            .unwrap_or(2026);
        let ts = chrono::NaiveDate::from_ymd_opt(year, mm, dd)
            .and_then(|d| {
                let t = chrono::NaiveTime::parse_from_str(hms, "%H:%M:%S").ok()?;
                chrono::Local.from_local_datetime(&d.and_time(t)).single()
            })
            .and_then(|dt| u64::try_from(dt.timestamp()).ok())
            .unwrap_or(fallback_secs);
        let body = glog.split_once(']').map(|(_, b)| b.trim()).unwrap_or(glog);
        return is_actionable_antigravity_log_error(body).then_some((ts, body));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("[error]") || lower.contains("level=error") {
        let body = trimmed
            .strip_prefix("[error]")
            .or_else(|| trimmed.strip_prefix("[ERROR]"))
            .unwrap_or(trimmed)
            .trim();
        return (!body.is_empty()).then_some((fallback_secs, body));
    }
    None
}
pub fn safe_model(s: &str) -> Option<String> {
    let lower = s.to_ascii_lowercase();
    (!s.is_empty()
        && s.len() <= 120
        && ![
            "sk-", "bearer", "eyj", "http", "token", "secret", "password", "cookie", "users/",
            "home/", ".com", ".net", ".org", ".xyz",
        ]
        .iter()
        .any(|v| lower.contains(v))
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-:/".contains(c))
        && !s.contains("://")
        && !s.starts_with('/')
        && s.parse::<std::net::IpAddr>().is_err()
        && !s
            .as_bytes()
            .windows(32)
            .any(|w| w.iter().all(u8::is_ascii_hexdigit)))
    .then(|| s.to_owned())
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    pub record: Option<Record>,
    pub error: Option<String>,
    /// Whether the desktop app should raise a system notification for it.
    #[cfg_attr(not(feature = "desktop"), allow(dead_code))]
    #[serde(skip)]
    pub notify: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct ClearMark {
    at: u64,
    files: std::collections::BTreeMap<String, u64>,
}
struct JournalSnapshot {
    cleared: Option<Arc<ClearMark>>,
    records: Vec<Arc<Record>>,
}
#[derive(Deserialize)]
struct JournalHeader {
    cleared: ClearMark,
}
/// Lines replaced by a later one of the same row that the file may carry
/// before it is rewritten, or as many as there are rows when that is more.
const SUPERSEDED_LINES: usize = 256;
/// How long a row past retention may wait for the rewrite that removes it
/// while rows are still being added; the idle timer runs as often.
const EXPIRY_BATCH: u64 = 3600;
#[derive(Default)]
struct Journal {
    cleared: Option<Arc<ClearMark>>,
    records: VecDeque<Arc<Record>>,
    /// Lines in the file that a later line with the same id replaces.
    superseded: usize,
    // Runtime health can refer to a just-merged event. Keep bounded aliases for
    // those links; persisted terminal events use the canonical row ID.
    aliases: VecDeque<(String, String)>,
    error: Option<String>,
    dirty: bool,
}
impl Journal {
    fn has_older_than(&self, cutoff: u64) -> bool {
        self.records.iter().any(|r| r.last_at < cutoff)
    }
    fn needs_write(&self, path: &Path) -> bool {
        self.dirty || !path.exists() || self.has_older_than(now().saturating_sub(RETENTION))
    }
    /// Whether changed rows can no longer be added to the end of the file.
    /// Rows past retention leave in batches, so a steady stream of events
    /// does not rewrite the file for each row that ages out.
    fn needs_rewrite(&self, path: &Path) -> bool {
        self.dirty
            || !path.exists()
            || self.superseded > self.records.len().max(SUPERSEDED_LINES)
            || self.has_older_than(now().saturating_sub(RETENTION + EXPIRY_BATCH))
    }
    fn retain_since(&mut self, cutoff: u64) -> bool {
        let before = self.records.len();
        self.records.retain(|r| r.last_at >= cutoff);
        let pruned = self.records.len() != before;
        let retained: HashSet<_> = self.records.iter().map(|r| r.id.as_str()).collect();
        self.aliases
            .retain(|(_, target)| retained.contains(target.as_str()));
        pruned
    }
    fn prepare_rewrite(&mut self, path: &Path, force: bool) -> Option<JournalSnapshot> {
        if !force && !self.needs_write(path) {
            return None;
        }
        self.retain_since(now().saturating_sub(RETENTION));
        self.dirty = false;
        self.superseded = 0;
        Some(JournalSnapshot {
            cleared: self.cleared.clone(),
            records: self.records.iter().cloned().collect(),
        })
    }
    fn finish_write(&mut self, written: Result<()>, pruned: std::io::Result<()>) {
        // A source import can mark the journal dirty while this snapshot is written.
        if written.is_err() {
            self.dirty = true;
            self.error = Some("日志写入失败，请检查文件权限与空间".into());
        } else if pruned.is_err() {
            self.error = Some("日志原文清理失败，请重试".into());
        }
    }
    fn retained_raw_ids(&self) -> HashSet<String> {
        self.records
            .iter()
            .filter_map(|r| r.details.raw_error.as_ref().map(|r| r.id.clone()))
            .collect()
    }
}
enum Job {
    Append(Box<Record>),
    Clear(
        Option<std::path::PathBuf>,
        Option<std::path::PathBuf>,
        oneshot::Sender<Result<()>>,
    ),
    Prune,
    #[cfg(any(feature = "desktop", test))]
    NotificationAccepted(String),
}
struct FileCursor {
    len: u64,
    modified: Option<SystemTime>,
    offset: u64,
    discarding: bool,
    stamp: SourceStamp,
    identity: String,
    #[cfg(windows)]
    file: Option<(u64, u64)>,
}
type FileCursors = std::collections::HashMap<std::path::PathBuf, FileCursor>;
const ANTIGRAVITY_BACKLOG: u64 = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct Service {
    sender: mpsc::SyncSender<Job>,
    journal: Arc<Mutex<Journal>>,
    events: broadcast::Sender<Change>,
    raw_directory: Arc<std::path::PathBuf>,
    file_cursors: Arc<Mutex<FileCursors>>,
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Filter {
    pub origin: Option<Origin>,
    pub from: Option<u64>,
    pub to: Option<u64>,
    pub client_id: Option<JournalClient>,
    pub provider_id: Option<String>,
    pub level: Option<Level>,
    pub category: Option<Category>,
    pub reason: Option<Reason>,
    pub status_group: Option<StatusGroup>,
    pub page: Option<usize>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub items: Vec<ClassifiedRecord>,
    pub total: usize,
    pub page: usize,
    pub error: Option<String>,
}
/// One upstream error status a group of providers has answered with.
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SeenStatus {
    pub status: u16,
    pub count: u32,
    /// The classification of the most recent record that carries one.
    pub basis: Option<ErrorBasis>,
    pub last_at: u64,
}
/// Upstream 4xx and 5xx statuses of the given providers, most frequent first.
fn seen_statuses<'a>(
    records: impl Iterator<Item = &'a Record>,
    client: ClientId,
    providers: &[String],
    cutoff: u64,
) -> Vec<SeenStatus> {
    let mut seen: Vec<(SeenStatus, u64)> = Vec::new();
    for r in records {
        let Some(status) = r.status.filter(|s| (400..600).contains(s)) else {
            continue;
        };
        if r.last_at < cutoff
            || r.client_id != Some(JournalClient::Active(client))
            || r.origin() != Origin::Upstream
            || !r
                .provider_id
                .as_ref()
                .is_some_and(|id| providers.contains(id))
        {
            continue;
        }
        let index = seen
            .iter()
            .position(|(s, _)| s.status == status)
            .unwrap_or_else(|| {
                seen.push((
                    SeenStatus {
                        status,
                        count: 0,
                        basis: None,
                        last_at: 0,
                    },
                    0,
                ));
                seen.len() - 1
            });
        let (entry, basis_at) = &mut seen[index];
        entry.count = entry.count.saturating_add(r.count);
        entry.last_at = entry.last_at.max(r.last_at);
        if r.details.error_basis.is_some() && (entry.basis.is_none() || r.last_at >= *basis_at) {
            entry.basis = r.details.error_basis;
            *basis_at = r.last_at;
        }
    }
    let mut seen: Vec<_> = seen.into_iter().map(|(s, _)| s).collect();
    seen.sort_by_key(|s| (std::cmp::Reverse(s.count), s.status));
    seen
}
impl Journal {
    fn circuit_causes(&self, cutoff: u64) -> HashSet<&str> {
        self.records
            .iter()
            .filter(|r| r.reason == Reason::CircuitOpen && r.last_at >= cutoff)
            .filter_map(|r| r.details.cause_id.as_deref())
            .collect()
    }
}
fn classify(record: &Record, causes: &HashSet<&str>) -> Category {
    if record.reason == Reason::CircuitOpen || causes.contains(record.id.as_str()) {
        Category::CircuitError
    } else if (record.details.quota_check.is_some() && record.level == Level::Info)
        || matches!(
            record.reason,
            Reason::Failover | Reason::ThreadHandoff | Reason::NetworkRestored
        )
        || (matches!(
            record.reason,
            Reason::RequestInterrupted | Reason::ThreadRouting
        ) && record.level == Level::Info)
    {
        Category::Status
    } else {
        Category::OrdinaryError
    }
}
fn load(path: &Path) -> Journal {
    let mut j = Journal::default();
    let file = match std::fs::File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    };
    match file {
        Ok(Some(file)) => {
            let mut reader = BufReader::new(file);
            let mut line = Vec::new();
            let mut first_line = true;
            // A row written again after a change replaces its earlier line
            // where that one stood, so the order is the one kept in memory.
            let mut rows: HashMap<String, usize> = HashMap::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) => break,
                    Ok(_) => (),
                    Err(_) => {
                        j.error = Some("日志读取失败，请检查文件权限与空间".into());
                        break;
                    }
                }
                if line == b"\n" {
                    continue;
                }
                if std::mem::take(&mut first_line) {
                    if let Ok(header) = serde_json::from_slice::<JournalHeader>(&line) {
                        j.cleared = Some(Arc::new(header.cleared));
                        continue;
                    }
                }
                // Only an interrupted write leaves the last line without its end.
                let complete = line.ends_with(b"\n");
                match serde_json::from_slice::<Record>(&line) {
                    Ok(mut r) => {
                        r.classify_stored();
                        // The next write must not continue an unfinished line.
                        j.dirty |= !complete;
                        match rows.entry(r.id.clone()) {
                            Entry::Occupied(row) => {
                                j.records[*row.get()] = Arc::new(r);
                                j.superseded += 1;
                            }
                            Entry::Vacant(row) => {
                                row.insert(j.records.len());
                                j.records.push_back(Arc::new(r));
                            }
                        }
                    }
                    // The row of an interrupted write is lost; the rewrite drops its remains.
                    Err(_) if !complete => j.dirty = true,
                    Err(_) => {
                        j.dirty = true;
                        j.error = Some("日志包含损坏记录，已跳过；其余记录仍可查看".into());
                    }
                }
            }
            j.dirty |= j.retain_since(now().saturating_sub(RETENTION));
        }
        Ok(None) => (),
        Err(_) => j.error = Some("日志读取失败，请检查文件权限与空间".into()),
    }
    j
}
fn append_lines<'a>(path: &Path, rows: impl Iterator<Item = &'a Record>) -> Result<()> {
    use std::io::Write;
    let mut buf = Vec::new();
    for record in rows {
        serde_json::to_writer(&mut buf, record)
            .map_err(|_| AppError::new(code::LOG_WRITE, "日志写入失败，请检查文件权限与空间"))?;
        buf.push(b'\n');
    }
    if buf.is_empty() {
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(storage::io_error)?;
    file.write_all(&buf).map_err(storage::io_error)?;
    Ok(())
}
fn resolve_cause_id<'a, I>(r: &mut Record, aliases: &VecDeque<(String, String)>, records: I)
where
    I: DoubleEndedIterator<Item = &'a Record> + Clone,
{
    if let Some((_, target)) = aliases
        .iter()
        .rev()
        .find(|(id, _)| r.details.cause_id.as_ref() == Some(id))
    {
        r.details.cause_id = Some(target.clone());
    }
    if matches!(r.reason, Reason::CircuitOpen | Reason::FailoverExhausted)
        && r.details
            .cause_id
            .as_ref()
            .is_none_or(|id| !records.clone().any(|old| &old.id == id))
    {
        // A repeated cause may already have been merged into an earlier row.
        r.details.cause_id = records
            .rev()
            .find(|old| {
                !matches!(old.reason, Reason::CircuitOpen | Reason::FailoverExhausted)
                    && (r.reason != Reason::CircuitOpen
                        || old.details.counted_failure == Some(true))
                    && old.client_id == r.client_id
                    && old.provider_id == r.provider_id
                    && old.model == r.model
                    && old.status == r.status
                    && old.details.upstream_code == r.details.upstream_code
                    && old.details.upstream_reason == r.details.upstream_reason
                    && old.details.error_basis == r.details.error_basis
                    && old.details.quota_check == r.details.quota_check
                    && old.details.rpm_wait_reason == r.details.rpm_wait_reason
                    && old.details.local_code == r.details.local_code
                    && old.details.message == r.details.message
                    && old.details.phase == r.details.phase
            })
            .map(|cause| cause.id.clone());
    }
}
fn write_records_stream(path: &Path, snapshot: &JournalSnapshot) -> Result<()> {
    storage::atomic_write_stream(path, |writer| {
        if let Some(cleared) = &snapshot.cleared {
            serde_json::to_writer(
                &mut *writer,
                &serde_json::json!({"cleared": cleared.as_ref()}),
            )?;
            writer.write_all(b"\n")?;
        }
        for record in &snapshot.records {
            serde_json::to_writer(&mut *writer, record.as_ref())?;
            writer.write_all(b"\n")?;
        }
        Ok(())
    })
}
impl Service {
    pub fn new(data: &Path) -> Self {
        let path = data.join("diagnostics.jsonl");
        let raw_directory = Arc::new(data.join("diagnostic-errors"));
        let raw_path = raw_directory.clone();
        let journal = Arc::new(Mutex::new(load(&path)));
        let (events, _) = broadcast::channel(128);
        let (sender, receiver) = mpsc::sync_channel(256);
        let j = journal.clone();
        let events_copy = events.clone();
        let file_cursors = Arc::new(Mutex::new(FileCursors::new()));
        let writer_cursors = file_cursors.clone();
        std::thread::spawn(move || {
            // Retention also runs after restart, before processing queued writes.
            {
                let startup = {
                    let mut initial = j.lock();
                    (initial.error.is_none() && path.exists()).then(|| {
                        let snapshot = initial.prepare_rewrite(&path, false);
                        let retained = initial.retained_raw_ids();
                        (snapshot, retained)
                    })
                };
                if let Some((snapshot, retained)) = startup {
                    let written = match &snapshot {
                        Some(records) => write_records_stream(&path, records),
                        None => Ok(()),
                    };
                    let pruned = if written.is_ok() {
                        prune_raw_ids(&raw_path, &retained)
                    } else {
                        Ok(())
                    };
                    j.lock().finish_write(written, pruned);
                }
            }
            let mut pending = None;
            loop {
                let job = match pending.take() {
                    Some(job) => job,
                    None => match receiver.recv_timeout(std::time::Duration::from_secs(3600)) {
                        Ok(v) => v,
                        Err(mpsc::RecvTimeoutError::Timeout) => Job::Prune,
                        Err(_) => break,
                    },
                };
                if matches!(&job, Job::Prune) {
                    let mut current = j.lock();
                    if !current.needs_write(&path) {
                        if prune_raw(&raw_path, &current).is_err() {
                            current.error = Some("日志原文清理失败，请重试".into());
                        }
                        let _ = events_copy.send(Change {
                            record: None,
                            error: current.error.clone(),
                            notify: false,
                        });
                        continue;
                    }
                }
                if let Job::Clear(home, claude_home, tx) = job {
                    // Scans keep this lock until their discovered rows are added.
                    // Clear therefore cannot race with an in-flight import.
                    let mut cursors = writer_cursors.lock();
                    let mut sources: Vec<_> = cursors.keys().cloned().collect();
                    let cutoff = now().saturating_sub(RETENTION);
                    if let Some(home) = home {
                        let (transcripts, logs) = antigravity_files(&home, cutoff);
                        sources.extend(
                            transcripts
                                .into_iter()
                                .chain(logs.into_iter().flatten())
                                .map(|(_, _, path)| path),
                        );
                    }
                    if let Some(home) = claude_home {
                        let mut sessions = Vec::new();
                        session_files(&home.join("projects"), 0, cutoff, &mut sessions);
                        sources.extend(sessions.into_iter().map(|(_, _, path)| path));
                    }
                    let cleared = ClearMark {
                        at: now(),
                        files: sources
                            .into_iter()
                            .filter_map(|path| {
                                let meta = std::fs::metadata(&path).ok()?;
                                meta.is_file()
                                    .then(|| (source_identity(&path, &meta), meta.len()))
                            })
                            .collect(),
                    };
                    let snapshot = JournalSnapshot {
                        cleared: Some(Arc::new(cleared)),
                        records: Vec::new(),
                    };
                    let result = write_records_stream(&path, &snapshot);
                    let raw_pruned = if result.is_ok() {
                        prune_raw_ids(&raw_path, &HashSet::new())
                    } else {
                        Ok(())
                    };
                    let error = {
                        let mut current = j.lock();
                        if result.is_ok() {
                            current.cleared = snapshot.cleared;
                            cursors.clear();
                            current.records.clear();
                            current.aliases.clear();
                            current.dirty = false;
                            current.superseded = 0;
                            current.error =
                                raw_pruned.err().map(|_| "日志原文清理失败，请重试".into());
                        } else {
                            current.dirty = true;
                            current.error = Some("日志写入失败，请检查文件权限与空间".into());
                        }
                        current.error.clone()
                    };
                    let _ = events_copy.send(Change {
                        record: None,
                        error,
                        notify: false,
                    });
                    let _ = tx.send(result);
                    continue;
                }
                let cleanup = matches!(&job, Job::Prune);
                let prune_only = matches!(&job, Job::Prune);
                let append = matches!(&job, Job::Append(_));
                let mut jobs = vec![job];
                if append {
                    while jobs.len() < 64 {
                        match receiver.try_recv() {
                            Ok(job @ Job::Append(_)) => jobs.push(job),
                            Ok(barrier) => {
                                pending = Some(barrier);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                }
                // Lines of this batch that replace one already in the file.
                let mut superseded = 0;
                let mut changes = Vec::with_capacity(jobs.len());
                for job in jobs {
                    let mut changed = None;
                    let mut notify = false;
                    match job {
                        Job::Append(r) => {
                            let mut r = *r;
                            let raw = r.details.raw.take();
                            if let Some(raw) = &raw {
                                r.details.raw_error = Some(raw.reference(&r.id));
                                // Only the list preview is shortened. Matching and the drawer read the attachment.
                                for value in [
                                    &mut r.details.message,
                                    &mut r.details.upstream_code,
                                    &mut r.details.upstream_reason,
                                    &mut r.details.upstream_type,
                                    &mut r.details.parameter,
                                ]
                                .into_iter()
                                .flatten()
                                {
                                    *value = value.chars().take(512).collect();
                                }
                            }
                            notify = r.reason.notifiable(r.action);
                            let outcome = {
                                let mut journal = j.lock();
                                resolve_cause_id(
                                    &mut r,
                                    &journal.aliases,
                                    journal.records.iter().map(Arc::as_ref),
                                );
                                if let Some(old_arc) =
                                    journal.records.iter_mut().rev().find(|old| {
                                        old.same(&r)
                                            && r.last_at.saturating_sub(old.first_at)
                                                < r.merge_seconds()
                                    })
                                {
                                    superseded += 1;
                                    let old = Arc::make_mut(old_arc);
                                    old.last_at = r.last_at;
                                    old.count = old.count.saturating_add(1);
                                    old.status = r.status;
                                    old.action = r.action;
                                    old.attempt = r.attempt;
                                    if r.reason == Reason::RequestInterrupted {
                                        old.details.delivery = r.details.delivery;
                                        old.details.last_progress_ms = r.details.last_progress_ms;
                                    }
                                    let snapshot = old.clone();
                                    let target = snapshot.id.clone();
                                    journal.aliases.push_back((r.id, target));
                                    while journal.aliases.len() > 4096 {
                                        journal.aliases.pop_front();
                                    }
                                    Ok(snapshot)
                                } else if raw.is_none() {
                                    let snapshot = r.clone();
                                    journal.records.push_back(Arc::new(r));
                                    Ok(snapshot)
                                } else {
                                    Err(r)
                                }
                            };
                            match outcome {
                                Ok(snapshot) => {
                                    changed = Some(snapshot);
                                }
                                Err(mut r) => {
                                    let raw_failed = raw
                                        .as_ref()
                                        .is_some_and(|raw| raw.save(&raw_path, &r.id).is_err());
                                    if raw_failed {
                                        if let Some(reference) = &mut r.details.raw_error {
                                            reference.incomplete = Some("错误原文保存失败".into());
                                        }
                                    }
                                    let mut journal = j.lock();
                                    if raw_failed {
                                        journal.error =
                                            Some("错误原文保存失败，请检查文件权限与空间".into());
                                    }
                                    changed = Some(r.clone());
                                    journal.records.push_back(Arc::new(r));
                                }
                            }
                        }
                        #[cfg(any(feature = "desktop", test))]
                        Job::NotificationAccepted(id) => {
                            let mut journal = j.lock();
                            if let Some(r_arc) = journal.records.iter_mut().find(|r| r.id == id) {
                                let r = Arc::make_mut(r_arc);
                                r.notified_at = now();
                                changed = Some(r.clone());
                                superseded += 1;
                            }
                        }
                        Job::Clear(..) | Job::Prune => (),
                    }
                    changes.push((changed, notify));
                }
                let (snapshot, retained_raw_ids) = {
                    let mut journal = j.lock();
                    let before_retention = journal.records.len();
                    journal.superseded += superseded;
                    let snapshot = if prune_only {
                        journal.prepare_rewrite(&path, false)
                    } else if journal.needs_rewrite(&path) {
                        journal.prepare_rewrite(&path, true)
                    } else {
                        None
                    };
                    let retained_raw = (cleanup || journal.records.len() != before_retention)
                        .then(|| journal.retained_raw_ids());
                    (snapshot, retained_raw)
                };
                // A merged or acknowledged row is written again at the end;
                // reading keeps the last line of each id.
                let result = match &snapshot {
                    Some(records) => write_records_stream(&path, records),
                    None if prune_only => Ok(()),
                    None => append_lines(
                        &path,
                        changes.iter().filter_map(|(record, _)| record.as_ref()),
                    ),
                };
                let raw_pruned = if result.is_ok() {
                    if let Some(retained) = &retained_raw_ids {
                        prune_raw_ids(&raw_path, retained)
                    } else {
                        Ok(())
                    }
                } else {
                    Ok(())
                };
                let error = {
                    let mut journal = j.lock();
                    journal.finish_write(result, raw_pruned);
                    journal.error.clone()
                };
                for (record, notify) in changes {
                    let _ = events_copy.send(Change {
                        record,
                        error: error.clone(),
                        notify,
                    });
                }
            }
        });
        Self {
            sender,
            journal,
            events,
            raw_directory,
            file_cursors,
        }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Change> {
        self.events.subscribe()
    }
    pub fn emit(&self, r: Record) {
        self.emit_checked(r);
    }
    /// Queue acceptance, not a durability acknowledgment. Gateway diagnostics
    /// use this receipt only to avoid a second description of the same fault.
    pub(crate) fn emit_checked(&self, mut r: Record) -> bool {
        if !r.visible() {
            return false;
        }
        r.details = r.details.sanitized();
        if self.sender.try_send(Job::Append(Box::new(r))).is_err() {
            let mut j = self.journal.lock();
            j.error = Some("日志队列已满，部分事件未记录".into());
            let _ = self.events.send(Change {
                record: None,
                error: j.error.clone(),
                notify: false,
            });
            return false;
        }
        true
    }
    #[cfg(feature = "desktop")]
    pub fn notification_accepted(&self, id: String) {
        // Failure to persist acknowledgment never blocks gateway delivery.
        let _ = self.sender.try_send(Job::NotificationAccepted(id));
    }
    pub fn seen_statuses(&self, client: ClientId, providers: &[String]) -> Vec<SeenStatus> {
        let j = self.journal.lock();
        seen_statuses(
            j.records.iter().map(|r| &**r),
            client,
            providers,
            now().saturating_sub(RETENTION),
        )
    }
    pub fn query(&self, f: Filter) -> Page {
        let j = self.journal.lock();
        let cutoff = now().saturating_sub(RETENTION);
        let causes = j.circuit_causes(cutoff);
        let mut rows: Vec<_> = j
            .records
            .iter()
            .filter(|r| {
                r.visible()
                    && f.origin.is_none_or(|v| r.origin() == v)
                    && r.last_at >= cutoff
                    && f.from.is_none_or(|t| r.last_at >= t)
                    && f.to.is_none_or(|t| r.first_at <= t)
                    && f.client_id.is_none_or(|v| r.client_id == Some(v))
                    && f.provider_id
                        .as_ref()
                        .is_none_or(|v| r.provider_id.as_ref() == Some(v))
                    && f.level.is_none_or(|v| r.level == v)
                    && f.category.is_none_or(|v| classify(r, &causes) == v)
                    && f.reason.is_none_or(|v| r.reason == v)
                    && f.status_group.is_none_or(|v| v.matches(r.status))
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.last_at));
        let total = rows.len();
        let page = f.page.unwrap_or(1).max(1).min(total.div_ceil(50).max(1));
        Page {
            items: rows
                .into_iter()
                .skip((page - 1) * 50)
                .take(50)
                .map(|r| ClassifiedRecord {
                    origin: r.origin(),
                    record: (**r).clone(),
                    category: classify(r, &causes),
                })
                .collect(),
            total,
            page,
            error: j.error.clone(),
        }
    }
    pub fn sync_antigravity_logs(&self, home: &Path) {
        let cutoff = now().saturating_sub(RETENTION);
        let mut cursors = self.file_cursors.lock();
        let mut discovered = Vec::new();
        let cleared = self.journal.lock().cleared.clone();
        let cutoff = cutoff.max(cleared.as_ref().map_or(0, |c| c.at));
        let (transcripts, logs) = antigravity_files(home, cutoff);
        for (file_secs, meta, path) in transcripts {
            let conv = path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .unwrap_or_default()
                .to_string_lossy();
            scan_new_lines(
                &path,
                &meta,
                (ANTIGRAVITY_BACKLOG, 512 * 1024),
                &mut cursors,
                cleared.as_deref(),
                |line_offset, line| {
                    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) {
                        let step_type = v["type"].as_str().unwrap_or("");
                        let raw_err = v["errorMessage"]
                            .as_str()
                            .or_else(|| v["error"]["message"].as_str())
                            .or_else(|| v["error"].as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty());
                        let is_err =
                            !matches!(step_type, "GENERIC" | "TOOL_RESPONSE" | "USER_INPUT")
                                && (step_type.eq_ignore_ascii_case("ERROR_MESSAGE")
                                    || raw_err.is_some());
                        if is_err {
                            let step_key = v["step_index"]
                                .as_u64()
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| {
                                    format!("off:{line_offset}:{}", &storage::digest(line)[..12])
                                });
                            let ts = v["created_at"]
                                .as_str()
                                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                                .and_then(|dt| u64::try_from(dt.timestamp()).ok())
                                .unwrap_or(file_secs);
                            if ts >= cutoff {
                                let raw = raw_err.unwrap_or("Antigravity step error");
                                let model =
                                    v["model"].as_str().or_else(|| v["response_model"].as_str());
                                let r = Record::antigravity_error(
                                    &format!("transcript:{conv}:{step_key}"),
                                    ts,
                                    model,
                                    raw,
                                );
                                discovered.push(r);
                            }
                        }
                    }
                },
            );
        }
        for (file_secs, meta, path) in logs.into_iter().flatten() {
            let fname = path.file_name().unwrap_or_default().to_string_lossy();
            scan_new_lines(
                &path,
                &meta,
                (ANTIGRAVITY_BACKLOG, 64 * 1024),
                &mut cursors,
                cleared.as_deref(),
                |line_offset, line| {
                    let line_str = String::from_utf8_lossy(line);
                    if let Some((line_ts, body)) = parse_antigravity_log_line(&line_str, file_secs)
                    {
                        if line_ts >= cutoff {
                            let key = format!(
                                "logfile:{fname}:{line_offset}:{}",
                                &storage::digest(body.as_bytes())[..16]
                            );
                            let r = Record::antigravity_error(&key, line_ts, None, body);
                            discovered.push(r);
                        }
                    }
                },
            );
        }
        self.add_external(discovered);
        drop(cursors);
    }
    /// Reads the API errors Claude Code wrote to its session files in the
    /// last week. Only lines marked as an API error are parsed.
    pub fn sync_claude_logs(&self, claude_home: &Path) {
        const MARK: &[u8] = br#""isApiErrorMessage":true"#;
        let cutoff = now().saturating_sub(RETENTION);
        let mut cursors = self.file_cursors.lock();
        let cleared = self.journal.lock().cleared.clone();
        let cutoff = cutoff.max(cleared.as_ref().map_or(0, |c| c.at));
        let mut sessions = Vec::new();
        session_files(&claude_home.join("projects"), 0, cutoff, &mut sessions);
        let mut discovered = Vec::new();
        for (_, meta, path) in sessions {
            // The whole week is wanted and a session file can be large, so
            // the first scan reads all of it; later scans read what was added.
            scan_new_lines(
                &path,
                &meta,
                (u64::MAX, 64 * 1024),
                &mut cursors,
                cleared.as_deref(),
                |_, line| {
                    if memchr::memmem::find(line, MARK).is_none() {
                        return;
                    }
                    let record = serde_json::from_slice(line)
                        .ok()
                        .and_then(|line| Record::claude_session_error(&line));
                    discovered.extend(record.filter(|r| r.last_at >= cutoff));
                },
            );
        }
        self.add_external(discovered);
        drop(cursors);
    }
    fn add_external(&self, discovered: Vec<Record>) {
        if discovered.is_empty() {
            return;
        }
        let mut guard = self.journal.lock();
        let mut known: HashSet<String> = guard.records.iter().map(|r| r.id.clone()).collect();
        let before = guard.records.len();
        guard.records.extend(
            discovered
                .into_iter()
                .filter(|r| known.insert(r.id.clone()))
                .map(Arc::new),
        );
        if guard.records.len() != before {
            guard.dirty = true;
            let _ = self.sender.try_send(Job::Prune);
        }
    }
    pub fn detail(&self, id: &str) -> Option<ClassifiedRecord> {
        let journal = self.journal.lock();
        let cutoff = now().saturating_sub(RETENTION);
        let causes = journal.circuit_causes(cutoff);
        let target = journal
            .aliases
            .iter()
            .rev()
            .find(|(alias, _)| alias == id)
            .map(|(_, target)| target.as_str())
            .unwrap_or(id);
        journal
            .records
            .iter()
            .find(|r| r.visible() && r.id == target && r.last_at >= cutoff)
            .map(|r| ClassifiedRecord {
                origin: r.origin(),
                record: (**r).clone(),
                category: classify(r, &causes),
            })
    }
    pub fn raw(&self, id: &str) -> Result<Option<crate::event_raw::View>> {
        let reference = self
            .detail(id)
            .and_then(|record| record.record.details.raw_error);
        reference
            .map(|reference| crate::event_raw::read(&self.raw_directory, &reference.id))
            .transpose()
    }
    pub async fn clear(
        &self,
        home: Option<std::path::PathBuf>,
        claude_home: Option<std::path::PathBuf>,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .try_send(Job::Clear(home, claude_home, tx))
            .map_err(|_| AppError::new(code::LOG_BUSY, "日志繁忙，请重试"))?;
        rx.await
            .map_err(|_| AppError::new(code::LOG_WRITE, "日志清空失败"))?
    }
}

fn prune_raw(directory: &Path, journal: &Journal) -> std::io::Result<()> {
    prune_raw_ids(directory, &journal.retained_raw_ids())
}

fn prune_raw_ids(directory: &Path, retained: &HashSet<String>) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".error")) else {
            continue;
        };
        if uuid::Uuid::parse_str(id).is_ok()
            && !retained.contains(id)
            && entry.file_type()?.is_file()
        {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "events_categories_tests.rs"]
mod categories_tests;

#[cfg(test)]
mod diagnostic_record_tests {
    use super::*;
    #[test]
    fn external_journal_identity_is_readable_but_not_executable() {
        let client: JournalClient = serde_json::from_str(r#""antigravity""#).unwrap();
        assert_eq!(client.active(), None);
        assert_eq!(serde_json::to_string(&client).unwrap(), r#""antigravity""#);
        assert!(serde_json::from_str::<ClientId>(r#""antigravity""#).is_err());
        // The gateway's Antigravity client is a different identity: the app's
        // own log lines never read as traffic of the gateway.
        let gateway: JournalClient = serde_json::from_str(r#""agy""#).unwrap();
        assert_eq!(gateway.active(), Some(ClientId::Agy));
        assert_eq!(serde_json::to_string(&gateway).unwrap(), r#""agy""#);
    }
    #[test]
    fn claude_code_session_identity_never_reads_as_the_gateway_client() {
        let external = JournalClient::External(ExternalClient::ClaudeCode);
        let wire = serde_json::to_string(&external).unwrap();
        assert_eq!(wire, r#""claude_code""#);
        let read: JournalClient = serde_json::from_str(&wire).unwrap();
        assert_eq!(read, external);
        assert_eq!(read.active(), None);
        // The gateway client keeps its own name and stays executable.
        let gateway: JournalClient = serde_json::from_str(r#""claude""#).unwrap();
        assert_eq!(gateway.active(), Some(ClientId::Claude));
        assert!(serde_json::from_str::<ClientId>(&wire).is_err());
    }
    #[test]
    fn claude_session_errors_are_classified_from_structured_fields() {
        let line = |extra: serde_json::Value| {
            let mut line = serde_json::json!({
                "type": "assistant",
                "isApiErrorMessage": true,
                "uuid": "00000000-0000-4000-8000-000000000001",
                "sessionId": "00000000-0000-4000-8000-000000000002",
                "timestamp": "2026-01-01T00:00:00Z",
                "cwd": "/fixture/private/path",
                "message": {"model": "<synthetic>", "content": [{"type": "text", "text": "API Error: fixture"}]},
            });
            line.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            line
        };
        for (extra, reason, status) in [
            (
                serde_json::json!({"error": "rate_limit", "apiErrorStatus": 429}),
                Reason::RateLimit,
                Some(429),
            ),
            (
                serde_json::json!({"error": "authentication_failed", "apiErrorStatus": 401}),
                Reason::Authentication,
                Some(401),
            ),
            (
                serde_json::json!({"error": "server_error", "apiError": "connection_failed"}),
                Reason::Network,
                None,
            ),
            (
                serde_json::json!({"error": "server_error", "apiError": "connection_failed", "streamCutCause": "stream_truncated"}),
                Reason::RequestInterrupted,
                None,
            ),
            (
                serde_json::json!({"error": "server_error", "apiErrorStatus": 529}),
                Reason::Capacity,
                Some(529),
            ),
            (
                serde_json::json!({"error": "billing_error", "apiErrorStatus": 403}),
                Reason::UpstreamService,
                Some(403),
            ),
            (
                serde_json::json!({"error": "server_error", "apiErrorStatus": 500}),
                Reason::UpstreamService,
                Some(500),
            ),
        ] {
            let record = Record::claude_session_error(&line(extra.clone())).unwrap();
            assert_eq!((record.reason, record.status), (reason, status), "{extra}");
            assert_eq!(record.origin(), Origin::ClaudeCode);
            assert_eq!(record.action, Action::Returned);
            assert_eq!(
                record.details.message.as_deref(),
                Some("API Error: fixture")
            );
            assert_eq!(
                record.details.upstream_code.as_deref(),
                extra["error"].as_str()
            );
            // Only the listed fields are kept; the working directory is not.
            assert!(!serde_json::to_string(&record)
                .unwrap()
                .contains("fixture/private"));
            // The same line always yields the same record.
            let again = Record::claude_session_error(&line(extra)).unwrap();
            assert_eq!(again.id, record.id);
        }
        let basis = |extra: serde_json::Value, text: &str| {
            let mut line = line(extra);
            line["message"]["content"][0]["text"] = text.into();
            let record = Record::claude_session_error(&line).unwrap();
            (record.reason, record.details.error_basis)
        };
        let limited = serde_json::json!({"error": "rate_limit", "apiErrorStatus": 429});
        assert_eq!(
            basis(
                limited.clone(),
                "API Error: Request rejected (429) · Service Unavailable"
            ),
            (Reason::Capacity, Some(ErrorBasis::Capacity))
        );
        assert_eq!(
            basis(
                limited.clone(),
                r#"API Error: 429 {"type":"error","error":{"type":"rate_limit_error","message":"This request would exceed your organization's rate limit of 50 requests per minute"}}"#
            ),
            (Reason::RateLimit, Some(ErrorBasis::Rpm))
        );
        assert_eq!(
            basis(limited, "API Error: fixture"),
            (Reason::RateLimit, Some(ErrorBasis::UnknownRateLimit))
        );
        assert_eq!(
            basis(
                serde_json::json!({"error": "billing_error", "apiErrorStatus": 403}),
                "API Error: fixture"
            ),
            (Reason::UpstreamService, Some(ErrorBasis::Billing))
        );
        assert_eq!(
            basis(
                serde_json::json!({"error": "invalid_request", "apiErrorStatus": 400}),
                r#"API Error: 400 {"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#
            ),
            (Reason::ProtocolError, Some(ErrorBasis::RequestTooLarge))
        );
        // A record stored before classification gains it when read back.
        let mut stored = Record::claude_session_error(&{
            let mut line = line(serde_json::json!({"error": "rate_limit", "apiErrorStatus": 429}));
            line["message"]["content"][0]["text"] =
                "API Error: Request rejected (429) · Service Unavailable".into();
            line
        })
        .unwrap();
        for old in [None, Some(ErrorBasis::UnknownRateLimit)] {
            stored.details.error_basis = old;
            stored.reason = Reason::RateLimit;
            stored.classify_stored();
            assert_eq!(
                (stored.reason, stored.details.error_basis),
                (Reason::Capacity, Some(ErrorBasis::Capacity))
            );
        }
        let mut ordinary = line(serde_json::json!({}));
        ordinary["isApiErrorMessage"] = false.into();
        assert!(Record::claude_session_error(&ordinary).is_none());
    }
    #[test]
    fn configuration_diagnostics_preserve_actual_codes_and_safe_causes() {
        for code in [
            code::CONFLICT,
            code::RESUME_CONFLICT,
            code::CLAUDE_PROFILE_CONFLICT,
        ] {
            let error = AppError::new(
                code,
                "受管字段被外部修改 settings.json https://example.invalid/v1 \"model\"",
            );
            let record = Record::configuration_failure(ClientId::Dsh, &error, true).unwrap();
            assert_eq!(record.reason, Reason::ConfigConflict);
            assert_eq!(record.details.local_code.as_deref(), Some(code));
            let message = record.details.message.unwrap();
            assert_eq!(
                message,
                "受管字段被外部修改 settings.json https://example.invalid/v1 \"model\""
            );
        }
        for code in [code::RECOVERY, "GUARDED", "GATEWAY_ACTIVE", "IO"] {
            let error = AppError::new(code, "恢复未完成");
            assert!(Record::configuration_failure(ClientId::Dsh, &error, false).is_none());
            let record = Record::configuration_failure(ClientId::Dsh, &error, true).unwrap();
            assert_eq!(record.reason, Reason::StartupRecovery);
            assert_eq!(record.details.local_code.as_deref(), Some(code));
            assert_eq!(record.details.message.as_deref(), Some("恢复未完成"));
        }
    }
    #[test]
    fn seen_statuses_sum_upstream_errors_of_the_given_providers() {
        let record = |provider: &str, client, status, count, last_at, basis| {
            let mut r = Record::new(
                Some(client),
                Some(provider),
                Some("fixture-model"),
                Reason::RateLimit,
                Action::TryingNext,
                Some(status),
                Some(1),
            );
            r.count = count;
            r.last_at = last_at;
            r.details.error_basis = basis;
            r
        };
        let records = [
            record(
                "00000000-0000-4000-8000-000000000001",
                ClientId::Claude,
                429,
                2,
                100,
                Some(ErrorBasis::Rpm),
            ),
            record(
                "00000000-0000-4000-8000-000000000002",
                ClientId::Claude,
                429,
                3,
                200,
                Some(ErrorBasis::Tpm),
            ),
            record(
                "00000000-0000-4000-8000-000000000002",
                ClientId::Claude,
                429,
                1,
                300,
                None,
            ),
            record(
                "00000000-0000-4000-8000-000000000001",
                ClientId::Claude,
                503,
                9,
                150,
                None,
            ),
            // Another provider, another client, a success and an expired record.
            record(
                "00000000-0000-4000-8000-000000000003",
                ClientId::Claude,
                500,
                7,
                100,
                None,
            ),
            record(
                "00000000-0000-4000-8000-000000000001",
                ClientId::Codex,
                502,
                7,
                100,
                None,
            ),
            record(
                "00000000-0000-4000-8000-000000000001",
                ClientId::Claude,
                200,
                7,
                100,
                None,
            ),
            record(
                "00000000-0000-4000-8000-000000000001",
                ClientId::Claude,
                529,
                7,
                10,
                None,
            ),
        ];
        let providers = [
            "00000000-0000-4000-8000-000000000001".to_owned(),
            "00000000-0000-4000-8000-000000000002".to_owned(),
        ];
        assert_eq!(
            seen_statuses(records.iter(), ClientId::Claude, &providers, 50),
            [
                SeenStatus {
                    status: 503,
                    count: 9,
                    basis: None,
                    last_at: 150
                },
                SeenStatus {
                    status: 429,
                    count: 6,
                    basis: Some(ErrorBasis::Tpm),
                    last_at: 300
                },
            ]
        );
    }
    #[test]
    fn distinct_error_codes_statuses_and_actions_do_not_merge() {
        let mut a = Record::new(
            None,
            None,
            Some("fixture-model"),
            Reason::ModelUnavailable,
            Action::TryingNext,
            Some(404),
            Some(1),
        );
        a.details.upstream_code = Some("model_not_found".into());
        a.details.counted_failure = Some(false);
        let mut b = a.clone();
        assert!(a.same(&b));
        b.status = Some(403);
        assert!(!a.same(&b));
        b = a.clone();
        b.details.upstream_code = Some("unsupported_model".into());
        assert!(!a.same(&b));
        b = a.clone();
        b.action = Action::Returned;
        assert!(!a.same(&b));
        b = a.clone();
        b.details.ws_close_code = Some(1013);
        assert!(!a.same(&b));
    }
}
#[cfg(test)]
#[path = "events_durability_tests.rs"]
mod durability_tests;

#[cfg(test)]
mod routing_visibility_tests {
    use super::*;
    use crate::usage::model::{RoutingDecision, RoutingEvidence};

    fn persist(j: &mut Journal, path: &Path, changed: bool) -> Result<()> {
        if let Some(snapshot) = j.prepare_rewrite(path, changed) {
            let written = write_records_stream(path, &snapshot);
            j.finish_write(written.clone(), Ok(()));
            written?;
        }
        Ok(())
    }

    #[test]
    fn loaded_retention_remains_dirty_until_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.jsonl");
        let keep = Record::new(
            None,
            None,
            None,
            Reason::AccountSync,
            Action::Stopped,
            None,
            None,
        );
        let mut expired = keep.clone();
        expired.id = uuid::Uuid::new_v4().to_string();
        expired.last_at = now() - RETENTION - 1;
        let lines = format!(
            "{}\n{}\n",
            serde_json::to_string(&keep).unwrap(),
            serde_json::to_string(&expired).unwrap()
        );
        std::fs::write(&path, lines).unwrap();
        let mut journal = load(&path);
        assert!(journal.error.is_none());
        assert_eq!(journal.records.len(), 1);
        assert!(journal.needs_write(&path));
        persist(&mut journal, &path, false).unwrap();
        assert!(!journal.needs_write(&path));
        assert!(!load(&path).dirty);
        std::fs::write(&path, "invalid fixture\n").unwrap();
        let mut damaged = load(&path);
        assert!(damaged.error.is_some());
        assert!(damaged.needs_write(&path));
        persist(&mut damaged, &path, false).unwrap();
        assert!(damaged.error.is_some());
        assert!(!damaged.needs_write(&path));
        assert!(std::fs::read(&path).unwrap().is_empty());
    }

    #[test]
    fn unchanged_retention_keeps_file_and_expired_records_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.jsonl");
        let mut journal = Journal::default();
        journal.records.push_back(Arc::new(Record::new(
            None,
            None,
            None,
            Reason::AccountSync,
            Action::Stopped,
            None,
            None,
        )));
        persist(&mut journal, &path, true).unwrap();
        let old_time = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old_time)
            .unwrap();
        persist(&mut journal, &path, false).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            old_time
        );
        Arc::make_mut(&mut journal.records[0]).last_at = now() - RETENTION - 1;
        persist(&mut journal, &path, false).unwrap();
        assert!(journal.records.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), b"");
    }

    #[test]
    fn routine_routing_is_hidden_on_emit_and_query_but_faults_remain() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::new(dir.path());
        let mut lines = Vec::new();
        for outcome in [
            RoutingOutcome::OwnerPreserved,
            RoutingOutcome::WindowConsumed,
            RoutingOutcome::BoundaryRegistered,
            RoutingOutcome::Failed,
            RoutingOutcome::OwnershipSaved,
        ] {
            let mut record = Record::new(
                None,
                None,
                None,
                Reason::ThreadRouting,
                Action::Returned,
                Some(200),
                None,
            );
            record.details.routing = Some(RoutingDetails {
                decision: RoutingDecision::BoundaryConsumed,
                evidence: RoutingEvidence::default(),
                outcome,
                attempt_id: None,
                failure: None,
            });
            assert!(!service.emit_checked(record.clone()));
            record.details.routing.as_mut().unwrap().failure =
                Some(crate::gateway::failover::Failure::Transport);
            assert!(record.visible());
            lines.push(serde_json::to_string(&record).unwrap());
        }
        assert_eq!(service.query(Filter::default()).total, 0);
        let path = dir.path().join("faults.jsonl");
        std::fs::write(&path, lines.join("\n")).unwrap();
        let loaded = load(&path);
        assert_eq!(loaded.records.len(), 5);
        assert!(loaded.records.iter().all(|r| r
            .details
            .routing
            .as_ref()
            .unwrap()
            .failure
            .is_some()));
        let service = Service::new(dir.path());
        *service.journal.lock() = loaded;
        assert_eq!(service.query(Filter::default()).total, 5);
        // Query/count also defend against routine records still held in memory.
        let mut routine = (*service.journal.lock().records[0]).clone();
        routine.id = uuid::Uuid::new_v4().to_string();
        routine.details.routing.as_mut().unwrap().failure = None;
        let id = routine.id.clone();
        service.journal.lock().records.push_back(Arc::new(routine));
        assert_eq!(
            service
                .query(Filter {
                    reason: Some(Reason::ThreadRouting),
                    ..Default::default()
                })
                .total,
            5
        );
        assert!(service.detail(&id).is_none());
    }

    #[tokio::test]
    async fn incremental_append_adds_new_lines_and_a_merge_adds_the_changed_row_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.jsonl");
        let service = Service::new(dir.path());
        let mut rx = service.subscribe();

        let first = Record::new(
            Some(ClientId::Codex),
            None,
            Some("fixture-model-a"),
            Reason::RateLimit,
            Action::TryingNext,
            Some(429),
            Some(1),
        );
        service.emit(first);
        rx.recv().await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);

        service.journal.lock().error = Some("fixture attachment warning".into());
        // Keep a handle to the old file: append updates it, atomic replacement does not.
        let mut original = std::fs::File::open(&path).unwrap();

        let second = Record::new(
            Some(ClientId::Codex),
            None,
            Some("fixture-model-b"),
            Reason::UpstreamService,
            Action::Returned,
            Some(502),
            Some(1),
        );
        service.emit(second.clone());
        rx.recv().await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        let mut appended = String::new();
        std::io::Read::read_to_string(&mut original, &mut appended).unwrap();
        assert_eq!(appended.lines().count(), 2);
        drop(original);
        assert!(service.query(Filter::default()).error.is_some());

        let mut repeated = second;
        repeated.id = uuid::Uuid::new_v4().to_string();
        service.emit(repeated);
        let merged = rx.recv().await.unwrap().record.unwrap();
        assert_eq!(merged.count, 2);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 3);
        let reloaded = load(&path);
        assert!(reloaded.error.is_none());
        assert!(!reloaded.dirty);
        assert_eq!(reloaded.superseded, 1);
        assert_eq!(reloaded.records.len(), 2);
        assert_eq!(reloaded.records[1].id, merged.id);
        assert_eq!(reloaded.records[1].count, 2);
    }

    #[tokio::test]
    async fn cleared_claude_files_are_skipped_until_they_change() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("claude");
        let sessions = home.join("projects/fixture");
        std::fs::create_dir_all(&sessions).unwrap();
        let old = sessions.join("old.jsonl");
        let active = sessions.join("active.jsonl");
        for path in [&old, &active] {
            std::fs::write(path, b"{}\n").unwrap();
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(now() - 60))
                .unwrap();
        }
        let service = Service::new(&dir.path().join("data"));
        service.clear(None, Some(home.clone())).await.unwrap();
        service.sync_claude_logs(&home);
        assert!(service.file_cursors.lock().is_empty());

        let entry = serde_json::json!({
            "uuid":"fixture-after-clear", "sessionId":"fixture-session",
            "timestamp":chrono::Utc::now().to_rfc3339(), "isApiErrorMessage":true,
            "message":{"content":[{"type":"text","text":"API Error: 503 unavailable"}]}
        });
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&active)
                .unwrap(),
            "{entry}"
        )
        .unwrap();
        service.sync_claude_logs(&home);
        assert_eq!(service.file_cursors.lock().len(), 1);
        assert_eq!(service.query(Filter::default()).total, 1);
    }

    #[tokio::test]
    async fn clear_write_failure_preserves_in_memory_records_and_merged_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.jsonl");
        let service = Service::new(dir.path());
        let mut rx = service.subscribe();

        let base = Record::new(
            Some(ClientId::Codex),
            None,
            Some("fixture-model"),
            Reason::UpstreamService,
            Action::Returned,
            Some(502),
            Some(1),
        );
        let canonical_id = base.id.clone();
        service.emit(base.clone());
        rx.recv().await.unwrap();

        let mut dup = base;
        let alias_id = uuid::Uuid::new_v4().to_string();
        dup.id = alias_id.clone();
        service.emit(dup);
        rx.recv().await.unwrap();
        assert_eq!(service.detail(&alias_id).unwrap().record.id, canonical_id);

        // Replace diagnostics.jsonl with a directory so atomic rename fails during Clear.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("blocker"), b"x").unwrap();

        assert!(service.clear(None, None).await.is_err());
        assert_eq!(service.query(Filter::default()).total, 1);
        assert_eq!(service.detail(&alias_id).unwrap().record.id, canonical_id);

        // Restore path and confirm normal clear empties records and aliases.
        std::fs::remove_dir_all(&path).unwrap();
        service.clear(None, None).await.unwrap();
        assert_eq!(service.query(Filter::default()).total, 0);
        assert!(service.detail(&alias_id).is_none());
    }

    #[test]
    fn agy_errors_are_read_from_its_own_steps_and_logs_as_antigravity() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::new(dir.path());
        let cli = dir.path().join(".gemini/antigravity-cli");
        let steps = cli.join("brain/session-1/.system_generated/logs");
        std::fs::create_dir_all(&steps).unwrap();
        // A tool that failed is the session's own business; the model call
        // that failed and the step agy wrote about it are not.
        let lines = [
            r#"{"step_index":3,"type":"GENERIC","status":"ERROR","source":"MODEL","error":"file does not exist"}"#,
            r#"{"step_index":4,"type":"PLANNER_RESPONSE","status":"ERROR","source":"MODEL","model":"fixture-model","error":"stream reading error: unexpected EOF"}"#,
            r#"{"step_index":5,"type":"ERROR_MESSAGE","status":"DONE","source":"SYSTEM","error":"API error (attempt 1): RESOURCE_EXHAUSTED"}"#,
            r#"{"step_index":6,"type":"PLANNER_RESPONSE","status":"DONE","source":"MODEL","model":"fixture-model"}"#,
        ];
        std::fs::write(steps.join("transcript_full.jsonl"), lines.join("\n") + "\n").unwrap();
        std::fs::create_dir_all(cli.join("log")).unwrap();
        std::fs::write(
            cli.join("log/cli-fixture_000000.log"),
            "I1125 07:41:06.000000      1 server.go:10] Started\n\
             E1125 07:41:06.000000      1 errorreport.go:282] error getting token source: not signed in\n\
             E1125 07:41:07.000000      1 errorreport.go:282] transport error (ResourceExhausted): RESOURCE_EXHAUSTED\n",
        )
        .unwrap();

        service.sync_antigravity_logs(dir.path());
        let page = service.query(Filter::default());
        let mut found: Vec<_> = page
            .items
            .iter()
            .map(|r| {
                assert_eq!(r.client_id.unwrap().name(), "Antigravity");
                (r.details.upstream_code.clone().unwrap(), r.model.clone())
            })
            .collect();
        found.sort();
        let model = Some("fixture-model".to_string());
        assert_eq!(
            found,
            [
                ("RESOURCE_EXHAUSTED".to_string(), None),
                ("RESOURCE_EXHAUSTED".to_string(), None),
                ("STREAM_INTERRUPTED".to_string(), model),
            ]
        );
        // Read once: the same files give nothing more.
        service.sync_antigravity_logs(dir.path());
        assert_eq!(service.query(Filter::default()).total, 3);
    }

    #[test]
    fn antigravity_log_scan_preserves_unfinished_half_line_and_seeks_tail_over_4mb() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let service = Service::new(dir.path());
        let log_dir = dir
            .path()
            .join(".gemini/antigravity/brain/conv-1/.system_generated/logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let transcript = log_dir.join("transcript.jsonl");

        // 1) Half-written line should not advance offset or lose the event once completed.
        let part1 = br#"{"step_index":1,"type":"ERROR_MESSAGE","errorMessage":"429 Too Many "#;
        let part2 = b"Requests from upstream\"}\n";
        std::fs::write(&transcript, part1).unwrap();
        service.sync_antigravity_logs(dir.path());
        assert_eq!(service.query(Filter::default()).total, 0);

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        f.write_all(part2).unwrap();
        drop(f);
        service.sync_antigravity_logs(dir.path());
        assert_eq!(service.query(Filter::default()).total, 1);

        // 2) Initial scan of >4MB file seeks to tail (file_len - 4MB) and skips early errors.
        let log_dir2 = dir.path().join(".gemini/antigravity/logs");
        std::fs::create_dir_all(&log_dir2).unwrap();
        let big_log = log_dir2.join("language_server.log");
        let mut big = std::fs::File::create(&big_log).unwrap();
        writeln!(big, "[ERROR] 401 Unauthorized early error").unwrap();
        let filler = vec![b'x'; 1024];
        for _ in 0..(4 * 1024 + 64) {
            big.write_all(&filler).unwrap();
            big.write_all(b"\n").unwrap();
        }
        writeln!(big, "[ERROR] 503 Service Unavailable tail error").unwrap();
        drop(big);

        service.sync_antigravity_logs(dir.path());
        let page = service.query(Filter::default());
        assert_eq!(page.total, 2);
        assert!(page
            .items
            .iter()
            .any(|r| r.reason == Reason::UpstreamService));
        assert!(!page
            .items
            .iter()
            .any(|r| r.reason == Reason::Authentication));
    }

    #[test]
    fn load_journal_retains_antigravity_log_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diagnostics.jsonl");
        let log_record = Record::antigravity_error(
            "transcript:conv-1:1",
            now(),
            Some("gemini-2.5-pro"),
            "429 Too Many Requests for \"gemini-2.5-pro\" at https://example.invalid/v1/models",
        );
        assert!(log_record.details.upstream_type.is_none());
        assert_eq!(
            log_record.details.message.as_deref(),
            Some(
                "429 Too Many Requests for \"gemini-2.5-pro\" at https://example.invalid/v1/models"
            )
        );
        let timeout_record = Record::antigravity_error(
            "transcript:conv-1:3",
            now(),
            Some("gemini-2.5-pro"),
            "Error: stream was interrupted after 5m0s",
        );
        assert_eq!(
            timeout_record.details.upstream_code.as_deref(),
            Some("STREAM_TIMEOUT")
        );
        let interrupted_record = Record::antigravity_error(
            "transcript:conv-1:5",
            now(),
            Some("gemini-2.5-pro"),
            "stream reading error: unexpected EOF",
        );
        assert_eq!(interrupted_record.reason, Reason::RequestInterrupted);
        assert_eq!(
            interrupted_record.details.upstream_code.as_deref(),
            Some("STREAM_INTERRUPTED")
        );
        let content = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&log_record).unwrap(),
            serde_json::to_string(&timeout_record).unwrap(),
            serde_json::to_string(&interrupted_record).unwrap()
        );
        std::fs::write(&path, content).unwrap();

        let loaded = load(&path);
        assert!(!loaded.dirty);
        assert_eq!(loaded.records.len(), 3);
        assert_eq!(loaded.records[0].id, log_record.id);
        assert_eq!(loaded.records[1].id, timeout_record.id);
        assert_eq!(loaded.records[2].id, interrupted_record.id);
        assert!(loaded.records[0].details.upstream_type.is_none());
    }
}

#[cfg(test)]
mod external_scan_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn oversized_partial_lines_are_skipped_until_newline_then_normal_lines_resume() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let limit = 64 * 1024;
        std::fs::write(&path, vec![b'x'; limit + 1]).unwrap();
        let mut cursors = FileCursors::new();
        let mut lines = Vec::new();
        let mut scan = |cursors: &mut FileCursors| {
            let meta = std::fs::metadata(&path).unwrap();
            scan_new_lines(
                &path,
                &meta,
                (u64::MAX, limit),
                cursors,
                None,
                |offset, line| lines.push((offset, line.to_vec())),
            );
        };
        scan(&mut cursors);
        assert!(cursors[&path].discarding);
        assert_eq!(cursors[&path].offset, (limit + 1) as u64);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"still oversized").unwrap();
        scan(&mut cursors);
        assert_eq!(
            cursors[&path].offset,
            std::fs::metadata(&path).unwrap().len()
        );
        file.write_all(b"\nnormal\npartial").unwrap();
        scan(&mut cursors);
        assert!(!cursors[&path].discarding);
        file.write_all(b" completed\n").unwrap();
        scan(&mut cursors);
        assert_eq!(
            lines
                .iter()
                .map(|(_, line)| line.as_slice())
                .collect::<Vec<_>>(),
            [b"normal\n".as_slice(), b"partial completed\n"]
        );
        // Truncation and file rotation restart the cursor.
        std::fs::write(&path, b"short\n").unwrap();
        scan_new_lines(
            &path,
            &std::fs::metadata(&path).unwrap(),
            (u64::MAX, limit),
            &mut cursors,
            None,
            |_, line| assert_eq!(line, b"short\n"),
        );
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"other\n").unwrap();
        // A time of its own: unopened, Windows tells a file of the same
        // length from the one it replaced only by when it was written.
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&replacement)
            .unwrap()
            .set_modified(written + std::time::Duration::from_secs(5))
            .unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let mut rotated = Vec::new();
        scan_new_lines(
            &path,
            &std::fs::metadata(&path).unwrap(),
            (u64::MAX, limit),
            &mut cursors,
            None,
            |_, line| rotated.extend_from_slice(line),
        );
        assert_eq!(rotated, b"other\n");
    }
}
