//! What the quick panel shows of one client's gateway, derived from the
//! gateway's own view. It is worked out where the gateway runs, so the panel
//! shows a server's the same way as this machine's.
use crate::{
    events::{ErrorBasis, Reason, RpmWaitReason},
    gateway::{
        self,
        circuit::{Cause, CircuitState, Health},
        freeze,
        quota::{Plan, QuotaView, Subscription, Usage},
        ClientId, ProviderView,
    },
    runtime::Runtime,
};
use chrono::{Local, TimeZone};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreezeStageModel {
    pub name: String,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreezeCardModel {
    pub tone: String,
    pub label: String,
    pub button_label: String,
    pub frozen: bool,
    pub disabled: bool,
    pub stages: Vec<FreezeStageModel>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongWaitModel {
    pub request_id: String,
    /// Seconds since the epoch, for whoever shows it in another time zone.
    pub started_at: u64,
    pub provider_name: String,
    pub model: String,
    pub started_at_text: String,
    pub phase_text: String,
    pub timing_text: String,
    pub cancelling: bool,
    /// A third line while the response is being received: the amount so far,
    /// and its rate or how long it has been silent.
    pub received_text: Option<String>,
    pub rate_text: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaPlanRowModel {
    pub id: usize,
    pub name: String,
    pub reset_text: Option<String>,
    pub reset_title: Option<String>,
    pub remaining_text: String,
    pub used_text: String,
    pub total_text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaSubscriptionDetailModel {
    pub id: usize,
    pub title: String,
    pub status_badge: Option<String>,
    pub expires_text: Option<String>,
    pub unit_badge: Option<String>,
    pub plans: Vec<QuotaPlanRowModel>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaUsageRowModel {
    pub label: String,
    pub requests: String,
    pub tokens: String,
    pub cost: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaDetailModel {
    pub key_only_banner: Option<String>,
    pub headline: String,
    pub source_label: Option<String>,
    pub updated_time: Option<String>,
    pub updated_title: Option<String>,
    pub key_warning: Option<String>,
    pub error_text: Option<String>,
    pub subscriptions: Vec<QuotaSubscriptionDetailModel>,
    pub standalone_unit: Option<String>,
    pub standalone_plans: Vec<QuotaPlanRowModel>,
    pub expiry_text: Option<String>,
    pub usage_rows: Vec<QuotaUsageRowModel>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaCardModel {
    pub tone: String,
    pub subscription_label: Option<String>,
    pub compact_value: String,
    pub headline: String,
    pub refresh_disabled: bool,
    pub loading: bool,
    pub has_error: bool,
    pub refresh_title: String,
    pub detail: QuotaDetailModel,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCardModel {
    pub id: String,
    pub name: String,
    pub priority: Option<String>,
    pub selected: bool,
    pub select_label: String,
    pub handoff_status: Option<String>,
    pub runtime_alert: Option<String>,
    pub fault_event_id: Option<String>,
    pub fault_provider_id: Option<String>,
    pub show_codex_policies: bool,
    pub handoff_after_compaction: bool,
    pub take_new_threads: bool,
    pub concurrency_label: String,
    pub max_concurrency: u32,
    pub rpm_label: String,
    pub max_rpm: u32,
    pub queued: bool,
    pub queue_label: String,
    pub can_move_up: bool,
    pub can_move_down: bool,
    pub quota: QuotaCardModel,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayCardModel {
    pub client_id: ClientId,
    pub revision: String,
    pub running: bool,
    pub status_label: String,
    pub mode: String,
    pub mode_label: String,
    pub power_label: String,
    pub waiting_badge: Option<String>,
    pub long_waits: Vec<LongWaitModel>,
    pub providers: Vec<ProviderCardModel>,
}

pub fn derive_freeze_card(
    view: Option<&freeze::View>,
    error: Option<&str>,
    busy: bool,
) -> FreezeCardModel {
    let err = error.filter(|s| !s.is_empty()).map(str::to_owned);
    let draining = view.is_some_and(|v| v.frozen && v.draining > 0);
    let (tone, label) = if err.is_some() {
        ("red", "状态异常")
    } else if let Some(v) = view {
        if v.partial {
            ("yellow", "部分冻结")
        } else if !v.frozen {
            ("green", "未冻结")
        } else if draining {
            ("yellow", "等待收尾")
        } else {
            ("ice", "可休眠")
        }
    } else {
        ("neutral", "读取状态")
    };
    let frozen = view.is_some_and(|v| v.frozen);
    let button_label = if err.is_some() {
        "重试"
    } else if frozen {
        "解冻"
    } else {
        "全局冻结"
    };
    let stages = view
        .map(|v| {
            [
                ("接收中", v.receiving),
                ("等待发送", v.waiting),
                ("等待响应", v.headers),
                ("传输中", v.streaming),
                ("收尾中", v.finishing),
                ("暂存", v.queued),
            ]
            .into_iter()
            .filter(|(_, count)| *count > 0)
            .map(|(name, count)| FreezeStageModel {
                name: name.into(),
                count,
            })
            .collect()
        })
        .unwrap_or_default();
    FreezeCardModel {
        tone: tone.into(),
        label: label.into(),
        button_label: button_label.into(),
        frozen,
        disabled: (view.is_none() && err.is_none()) || busy || draining,
        stages,
        error: err,
    }
}

pub fn limit_label(cause: Option<&Cause>) -> &'static str {
    let Some(c) = cause else { return "" };
    let reason = c.upstream_reason.as_deref().unwrap_or(c.code.as_str());
    match reason {
        "DAILY_LIMIT_EXCEEDED" | "USER_PLATFORM_DAILY_QUOTA_EXHAUSTED" => return "日额度已用尽",
        "WEEKLY_LIMIT_EXCEEDED" | "USER_PLATFORM_WEEKLY_QUOTA_EXHAUSTED" => return "周额度已用尽",
        "MONTHLY_LIMIT_EXCEEDED" | "USER_PLATFORM_MONTHLY_QUOTA_EXHAUSTED" => {
            return "月额度已用尽"
        }
        "five_hour" => return "5 小时额度已用尽",
        "seven_day" => return "7 天额度已用尽",
        _ => {}
    }
    match c.error_basis {
        Some(ErrorBasis::QuotaExhausted) => "额度已耗尽",
        Some(ErrorBasis::Rpm) => "上游 RPM 限制",
        Some(ErrorBasis::Rps) => "上游 RPS 限制",
        Some(ErrorBasis::Tpm) => "上游 Token 限流",
        Some(ErrorBasis::InputTokens) => "输入 Token 限流",
        Some(ErrorBasis::OutputTokens) => "输出 Token 限流",
        Some(ErrorBasis::Concurrency) => "上游并发限制",
        Some(ErrorBasis::Acceleration) => "流量增长过快",
        Some(ErrorBasis::SpendingRate) => "短时消费速率限制",
        Some(ErrorBasis::UnknownRateLimit) => "未分类限流",
        Some(ErrorBasis::ServiceError) => "上游服务异常",
        Some(ErrorBasis::Billing) => "余额或消费额度不足",
        Some(ErrorBasis::Authentication) => "认证失败",
        Some(ErrorBasis::Permission) => "访问受限",
        Some(ErrorBasis::RequestTooLarge) => "单次请求超限",
        Some(ErrorBasis::InvalidRequest) => "请求或协议不受支持",
        Some(ErrorBasis::SafetyRejection) => "安全策略拒绝",
        Some(ErrorBasis::ModelUnavailable) => "模型不可用",
        Some(ErrorBasis::Capacity) => "临时容量限制",
        Some(ErrorBasis::UsageLimit) => "用量限制，额度状态待核验",
        Some(ErrorBasis::RiskControl) => "风控重试",
        Some(ErrorBasis::LimitRetry) => "限流重试",
        Some(ErrorBasis::Unknown) | None => "",
    }
}

pub fn rpm_wait_label(reason: Option<RpmWaitReason>) -> &'static str {
    match reason {
        Some(RpmWaitReason::SendWindow) => "RPM 分钟额度已满",
        Some(RpmWaitReason::UpstreamBackoff) => "上游 RPM 退避",
        Some(RpmWaitReason::Unknown) | None => "RPM 已满",
    }
}

fn pending_health(h: &Health) -> bool {
    !h.available || h.state != CircuitState::Closed || h.probe_in_flight
}

pub fn provider_fault(p: &ProviderView) -> Option<&Cause> {
    p.health.cause.as_ref().or_else(|| {
        p.model_health
            .iter()
            .find(|m| pending_health(&m.health))
            .and_then(|m| m.health.cause.as_ref())
    })
}

fn cause_text(h: &Health) -> String {
    let Some(c) = h.cause.as_ref() else {
        return String::new();
    };
    let limit = limit_label(Some(c));
    if !limit.is_empty() {
        return limit.into();
    }
    match c.code.as_str() {
        "FIRST_BYTE_TIMEOUT" => return "首字超时".into(),
        "CONNECT_TIMEOUT" => return "连接超时".into(),
        "WS_HANDSHAKE_TIMEOUT" => return "WS 握手超时".into(),
        "TLS_HANDSHAKE_FAILED" | "TLS" => return "TLS 失败".into(),
        "STREAM_TIMEOUT" => return "流超时".into(),
        "STREAM_INTERRUPTED" => return "连接中断".into(),
        "CONNECTION_FAILED" | "NETWORK" => return "连接失败".into(),
        _ => {}
    }
    if c.reason == Reason::Authentication {
        return "认证失败".into();
    }
    if let Some(status) = c.status.filter(|s| *s >= 400) {
        return format!("上游 {status}");
    }
    if let Some(ws) = c.ws_close_code {
        return format!("WS {ws}");
    }
    match c.reason {
        Reason::Capacity => "容量不足".into(),
        Reason::Network => "连接失败".into(),
        Reason::UpstreamService => "上游异常".into(),
        _ => String::new(),
    }
}

fn health_status_for(
    h: &Health,
    rpm_limited: bool,
    rpm_wait: Option<RpmWaitReason>,
    rpm_retry_in: u64,
    max_concurrency: u32,
    active_requests: usize,
) -> String {
    let reason = cause_text(h);
    let seconds = h.retry_in;
    if h.probe_in_flight {
        return if reason.is_empty() {
            "恢复探测中".into()
        } else {
            format!("恢复探测中 · {reason}")
        };
    }
    if h.state == CircuitState::Open {
        let tail = if seconds > 0 {
            format!("等待{seconds}秒")
        } else {
            "待请求探测".into()
        };
        return if reason.is_empty() {
            format!("熔断 · {tail}")
        } else {
            format!("熔断 · {reason} · {tail}")
        };
    }
    if matches!(
        h.cooldown_reason.as_deref(),
        Some("capacity_retry" | "rate_limit" | "hard_rejection")
    ) {
        let head = {
            let l = limit_label(h.cause.as_ref());
            if !l.is_empty() {
                l
            } else if h.cause.as_ref().and_then(|c| c.status) == Some(429)
                || h.cooldown_reason.as_deref() == Some("rate_limit")
            {
                "限流 429"
            } else {
                "容量不足"
            }
        };
        let tail = if seconds > 0 {
            format!("等待{seconds}秒")
        } else {
            "待请求探测".into()
        };
        return format!("{head} · {tail}");
    }
    if matches!(
        h.cooldown_reason.as_deref(),
        Some("single_provider_protected" | "retry_after")
    ) {
        let head = if reason.is_empty() {
            "单供应商保护"
        } else {
            reason.as_str()
        };
        return format!("{head} · 冷却{seconds}秒");
    }
    if h.state == CircuitState::HalfOpen {
        return if reason.is_empty() {
            "待请求探测".into()
        } else {
            format!("待请求探测 · {reason}")
        };
    }
    if rpm_limited {
        return format!(
            "{} · 等待{}秒",
            rpm_wait_label(rpm_wait),
            rpm_retry_in.max(1)
        );
    }
    if max_concurrency > 0 && active_requests >= max_concurrency as usize {
        return format!("并发已满 · {active_requests}/{max_concurrency}");
    }
    String::new()
}

pub fn provider_status(p: &ProviderView) -> String {
    let h = &p.health;
    if h.available && h.state == CircuitState::Closed && h.cooldown_reason.is_none() {
        if let Some(m) = p.model_health.iter().find(|m| pending_health(&m.health)) {
            let label = strip_gpt_prefix(&m.model);
            let inner = health_status_for(&m.health, false, None, 0, 0, 0);
            return format!("{label} · {inner}");
        }
    }
    health_status_for(
        h,
        p.rpm_limited,
        p.rpm_wait_reason,
        p.rpm_retry_in,
        p.max_concurrency,
        p.active_requests,
    )
}

fn strip_gpt_prefix(model: &str) -> &str {
    if let Some(rest) = model.strip_prefix("gpt-") {
        if let Some(pos) = rest.find('-') {
            let ver = &rest[..pos];
            if !ver.is_empty() && ver.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
                return &rest[pos + 1..];
            }
        }
    }
    model
}

pub fn provider_runtime_status(p: &ProviderView, state: &gateway::View) -> String {
    let fault = provider_fault(p);
    let limit = limit_label(fault);
    if let Some(cap) = state
        .capacity_retries
        .iter()
        .find(|w| w.provider_id == p.id && w.existing_owner)
    {
        let head = if limit.is_empty() {
            "临时限流"
        } else {
            limit
        };
        let checking = if p.quota_checking {
            " · 正在核验额度"
        } else {
            ""
        };
        return format!("{head} · 原供应商等待{}秒{checking}", cap.retry_in);
    }
    if p.health.state != CircuitState::Closed
        || p.model_health
            .iter()
            .any(|m| m.health.state != CircuitState::Closed)
    {
        return provider_status(p);
    }
    if let Some(ws) = state
        .websocket_retries
        .iter()
        .find(|w| w.provider_id == p.id)
    {
        return format!("WS 断开 · 重连{}秒", ws.retry_in);
    }
    if let Some(retry) = state
        .transient_retries
        .iter()
        .find(|w| w.provider_id == p.id)
    {
        let head = cause_text(&p.health);
        let head = if head.is_empty() {
            "上游波动"
        } else {
            head.as_str()
        };
        return format!("{head} · 原线路重试{}秒", retry.retry_in);
    }
    provider_status(p)
}

pub fn provider_handoff_status(p: &ProviderView, state: &gateway::View) -> Option<String> {
    let waiting = state
        .handoff_waiting
        .iter()
        .find(|w| w.provider_id == p.id)?;
    let label = match waiting.state.as_str() {
        "awaiting_compaction" => "待压缩后接管",
        "awaiting_request" => "待下一请求接管",
        "capacity_full" => "接管名额已满",
        "claim_in_flight" => "正在尝试接管",
        _ => return None,
    };
    Some(label.into())
}

pub fn wait_phase_label(phase: &str) -> &str {
    match phase {
        "admission" => "等待准入",
        "headers" => "等待首字节",
        "buffering" => "缓冲响应",
        "capacity" => "容量等待",
        "retry" => "等待重试",
        "ws_wait" => "等待重连",
        "compaction" => "压缩中",
        "frozen" => "已暂存",
        "held" => "断网暂存",
        other => other,
    }
}

pub fn derive_long_waits(
    view: &gateway::View,
    extra_elapsed_secs: u64,
    cancelling: &HashSet<String>,
) -> Vec<LongWaitModel> {
    view.pending_requests
        .iter()
        .filter_map(|r| {
            let delta = if r.phase == "frozen" {
                0
            } else {
                extra_elapsed_secs
            };
            let elapsed = r.elapsed_seconds.saturating_add(delta);
            if elapsed < 60 {
                return None;
            }
            let provider_name = r
                .provider_id
                .as_deref()
                .and_then(|pid| view.providers.iter().find(|p| p.id == pid))
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "等待供应商".into());
            let started_at_text = Local
                .timestamp_opt(r.started_at as i64, 0)
                .single()
                .map(|dt| dt.format("%H:%M:%S").to_string())
                .unwrap_or_default();
            // The time of the attempt or wait in progress; the whole request
            // only decides whether it is listed.
            let attempt = r.attempt_seconds.saturating_add(delta);
            let timing_text = match r.remaining_seconds {
                None => format!("本次 {attempt} 秒"),
                Some(rem) => format!("本次 {attempt} 秒 · 剩余 {} 秒", rem.saturating_sub(delta)),
            };
            let received_text = r
                .received_bytes
                .map(|bytes| format!("已收 {}", format_bytes(bytes)));
            let rate_text = r.received_bytes.map(|_| match r.silent_seconds {
                Some(silent) if silent >= 5 => format!("静默 {silent} 秒"),
                _ => format!("{}/s", format_bytes(r.bytes_per_second.unwrap_or(0))),
            });
            Some(LongWaitModel {
                request_id: r.request_id.clone(),
                started_at: r.started_at,
                provider_name,
                model: r.model.clone().unwrap_or_else(|| "未指定模型".into()),
                started_at_text,
                phase_text: wait_phase_label(&r.phase).into(),
                timing_text,
                cancelling: cancelling.contains(&r.request_id),
                received_text,
                rate_text,
            })
        })
        .collect()
}

/// 1024 to a step, one decimal from KB up: `718.6 KB`.
pub(crate) fn format_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = "KB";
    for next in ["MB", "GB"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    format!("{value:.1} {unit}")
}

// MARK: - Quota Formatting, Validation & Scheduler Math

fn add_thousands_commas(int_part: &str) -> String {
    let bytes = int_part.as_bytes();
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 3);
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(b as char);
    }
    out
}

pub fn format_number(n: f64, max_decimals: usize) -> String {
    if !n.is_finite() {
        return "—".into();
    }
    let neg = n < 0.0 && n != 0.0;
    let factor = 10_f64.powi(max_decimals as i32);
    let rounded = (n.abs() * factor).round() / factor;
    if rounded == 0.0 {
        return "0".into();
    }
    let raw = format!("{rounded:.max_decimals$}");
    let (int_part, frac_part) = match raw.split_once('.') {
        Some((i, f)) => (i, f.trim_end_matches('0')),
        None => (raw.as_str(), ""),
    };
    let mut res = String::new();
    if neg {
        res.push('-');
    }
    res.push_str(&add_thousands_commas(int_part));
    if !frac_part.is_empty() {
        res.push('.');
        res.push_str(frac_part);
    }
    res
}

fn round_sig_3(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return 0.0;
    }
    let mag = v.abs().log10().floor() as i32;
    let decimals = 2 - mag;
    if decimals >= 0 {
        let factor = 10_f64.powi(decimals);
        (v * factor).round() / factor
    } else {
        let factor = 10_f64.powi(-decimals);
        (v / factor).round() * factor
    }
}

pub fn compact_number(n: Option<f64>) -> String {
    let Some(n) = n.filter(|v| v.is_finite()) else {
        return "—".into();
    };
    let abs = n.abs();
    if abs < 1000.0 {
        return format_number(n, 3);
    }
    let units = ["", "K", "M", "B"];
    let mut power = ((abs.log10() / 3.0).floor() as usize).min(3);
    let mut value = round_sig_3(n / 1000_f64.powi(power as i32));
    if value.abs() >= 1000.0 && power < 3 {
        power += 1;
        value = round_sig_3(value / 1000.0);
    }
    format!("{}{}", format_number(value, 3), units[power])
}

pub fn is_wallet_placeholder_name(name: Option<&str>) -> bool {
    let Some(trimmed) = name.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    matches!(
        trimmed,
        "钱包余额" | "账户余额" | "余额" | "钱包" | "未识别套餐" | "订阅"
    ) || matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "wallet" | "wallet balance" | "walletbalance" | "account balance" | "balance"
    )
}

fn format_unix_full(secs: i64) -> String {
    Local
        .timestamp_opt(secs, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

fn parse_iso_unix(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.timestamp())
}

fn format_str_full_date(raw: &str) -> String {
    parse_iso_unix(raw)
        .map(format_unix_full)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| raw.to_owned())
}

fn format_str_short_date(raw: &str) -> String {
    parse_iso_unix(raw)
        .and_then(|ts| Local.timestamp_opt(ts, 0).single())
        .map(|dt| dt.format("%Y/%m/%d").to_string())
        .unwrap_or_else(|| raw.to_owned())
}

fn is_quota_expired(quota: &QuotaView, now_secs: u64) -> bool {
    if let Some(unix) = quota.expires_at_unix.filter(|&u| u > 0) {
        return unix <= now_secs as i64;
    }
    if let Some(exp) = quota.expires_at.as_deref() {
        if let Some(ts) = parse_iso_unix(exp) {
            return ts <= now_secs as i64;
        }
    }
    false
}

pub fn quota_tone(quota: Option<&QuotaView>, now_secs: u64) -> &'static str {
    let Some(q) = quota else {
        return "neutral";
    };
    if matches!(q.state.as_str(), "idle" | "loading" | "unsupported") {
        return "neutral";
    }
    if (q.state == "error" && q.plans.is_empty()) || is_quota_expired(q, now_secs) {
        return "critical";
    }
    let Some(primary) = q.plans.first() else {
        return "neutral";
    };
    if primary.unlimited {
        return "good";
    }
    let Some(rem) = primary.remaining else {
        return "neutral";
    };
    if rem <= 0.0 {
        return "critical";
    }
    let total = primary
        .total
        .filter(|&t| t > 0.0)
        .or_else(|| primary.used.map(|u| u + rem).filter(|&s| s > 0.0));
    if let Some(total) = total {
        let ratio = rem / total;
        if ratio <= 0.08 {
            return "critical";
        }
        if ratio <= 0.25 {
            return "warn";
        }
        if ratio >= 0.6 {
            return "good";
        }
    }
    "normal"
}

fn plan_amount(p: &Plan) -> String {
    if p.unlimited {
        "无限制".into()
    } else if let Some(rem) = p.remaining {
        format!("{} {}", format_number(rem, 3), p.unit)
    } else {
        "未返回剩余额度".into()
    }
}

/// What the capsule shows: whole units from 1 up and two decimals below, cut
/// off rather than rounded so it never overstates the balance.
pub(crate) fn capsule_amount(p: &Plan) -> String {
    if p.unlimited {
        return "无限制".into();
    }
    let Some(remaining) = p.remaining else {
        return "未返回额度".into();
    };
    // Micro-units come first: 0.29 * 100 on its own would be cut to 0.28.
    let micro = (remaining * 1e6).round();
    let number = if micro.abs() < 1e6 {
        format!("{:.2}", (micro / 1e4).trunc() / 100.0)
    } else {
        format_number((micro / 1e6).trunc(), 0)
    };
    if p.unit.is_empty() {
        number
    } else {
        format!("{number} {}", p.unit)
    }
}

fn build_plan_rows(plans: &[Plan]) -> (Option<String>, Vec<QuotaPlanRowModel>) {
    if plans.is_empty() {
        return (None, Vec::new());
    }
    let shared_unit = plans
        .iter()
        .all(|p| p.unit == plans[0].unit && !p.unit.is_empty())
        .then(|| plans[0].unit.clone());
    let fmt_val = |n: Option<f64>, p: &Plan| match n {
        None => "—".into(),
        Some(v) => {
            if shared_unit.is_some() || p.unit.is_empty() {
                format_number(v, 3)
            } else {
                format!("{} {}", format_number(v, 3), p.unit)
            }
        }
    };
    let rows = plans
        .iter()
        .enumerate()
        .map(|(id, p)| QuotaPlanRowModel {
            id,
            name: p.name.clone(),
            reset_text: p
                .reset_at
                .as_deref()
                .map(|r| format!("重置 {}", format_str_short_date(r))),
            reset_title: p.reset_at.as_deref().map(format_str_full_date),
            remaining_text: if p.unlimited {
                "无限制".into()
            } else {
                fmt_val(p.remaining, p)
            },
            used_text: fmt_val(p.used, p),
            total_text: if p.unlimited {
                "∞".into()
            } else {
                fmt_val(p.total, p)
            },
        })
        .collect();
    (shared_unit, rows)
}

fn usage_row(label: &str, u: &Usage) -> QuotaUsageRowModel {
    QuotaUsageRowModel {
        label: label.into(),
        requests: compact_number(u.requests),
        tokens: compact_number(u.tokens),
        cost: u.cost.map_or_else(|| "—".into(), |c| format_number(c, 3)),
    }
}

pub fn derive_quota_card(quota: Option<&QuotaView>, now_secs: u64) -> QuotaCardModel {
    let loading = quota.is_some_and(|q| q.state == "loading");
    let key_status = quota.and_then(|q| q.key_status.as_deref());
    let key_only = quota.is_some_and(|q| {
        q.source.as_deref() == Some("newapi")
            && q.plans.first().is_some_and(|p| p.name == "Key 额度")
    });
    let headline = if let Some(first) = quota.and_then(|q| q.plans.first()) {
        if key_only {
            if first.unlimited {
                "Key 配额无限制".into()
            } else {
                format!("Key 剩余 {}", plan_amount(first))
            }
        } else if first.unlimited {
            plan_amount(first)
        } else {
            format!("剩余 {}", plan_amount(first))
        }
    } else if let Some(ks) = key_status {
        ks.to_owned()
    } else {
        match quota.map(|q| q.state.as_str()) {
            Some("unsupported") => "不可查询".into(),
            Some("error") => "查询失败".into(),
            Some("loading") => "查询中…".into(),
            _ => "未查询".into(),
        }
    };
    let compact_value = quota
        .and_then(|q| q.plans.first())
        .map_or_else(|| key_status.unwrap_or("—").to_owned(), capsule_amount);

    let subs: &[Subscription] = quota.map_or(&[], |q| q.subscriptions.as_slice());
    let active_subs: Vec<&Subscription> = subs.iter().filter(|s| s.active == Some(true)).collect();
    let current_sub = if active_subs.len() == 1 {
        Some(active_subs[0])
    } else if subs.len() == 1 {
        Some(&subs[0])
    } else {
        None
    };
    let sub_name = current_sub
        .and_then(|s| s.name.as_deref())
        .filter(|n| !is_wallet_placeholder_name(Some(n)))
        .map(str::to_owned);
    let subscription_label =
        sub_name.or_else(|| (subs.len() > 1).then(|| format!("{} 个套餐", subs.len())));

    let repeated_sub_summary = quota.is_some_and(|q| {
        q.source.as_deref() == Some("sub2api")
            && subs.len() == 1
            && !subs[0].plans.is_empty()
            && q.plans
                .first()
                .is_some_and(|p| p.name == subs[0].name.as_deref().unwrap_or("订阅"))
    });
    let standalone_src: Vec<Plan> = quota
        .map(|q| {
            q.plans
                .iter()
                .enumerate()
                .filter(|(idx, plan)| {
                    if !repeated_sub_summary {
                        return true;
                    }
                    *idx != 0 && !subs[0].plans.iter().any(|sp| sp == *plan)
                })
                .map(|(_, p)| p.clone())
                .collect()
        })
        .unwrap_or_default();
    let (standalone_unit, standalone_plans) = build_plan_rows(&standalone_src);

    let sub_details = subs
        .iter()
        .enumerate()
        .map(|(id, s)| {
            let (unit_badge, plans) = build_plan_rows(&s.plans);
            let status_badge =
                s.status
                    .as_deref()
                    .filter(|st| *st != "active")
                    .map(|st| match st {
                        "expired" => "已到期".into(),
                        "disabled" => "已禁用".into(),
                        "cancelled" | "canceled" => "已取消".into(),
                        other => other.to_owned(),
                    });
            QuotaSubscriptionDetailModel {
                id,
                title: s.name.clone().unwrap_or_else(|| "未识别套餐".into()),
                status_badge,
                expires_text: s
                    .expires_at
                    .as_deref()
                    .map(|e| format!("到期 {}", format_str_short_date(e))),
                unit_badge,
                plans,
            }
        })
        .collect();

    let source_label = match quota.and_then(|q| q.source.as_deref()) {
        Some("sub2api") => Some("Sub2API".into()),
        Some("newapi") => Some("New API".into()),
        _ => None,
    };
    let (updated_time, updated_title) = quota
        .and_then(|q| q.success_at)
        .and_then(|ts| Local.timestamp_opt(ts as i64, 0).single())
        .map(|dt| {
            (
                Some(dt.format("%H:%M").to_string()),
                Some(format!("更新于 {}", dt.format("%Y-%m-%d %H:%M:%S"))),
            )
        })
        .unwrap_or((None, None));
    let error_text = quota.and_then(|q| q.error.as_ref()).map(|err| {
        if let Some(retry) = quota.and_then(|q| q.retry_at) {
            format!("{err} · {} 后重试", format_unix_full(retry as i64))
        } else {
            err.clone()
        }
    });
    let expiry_text = quota
        .and_then(|q| {
            q.expires_at_unix
                .map(format_unix_full)
                .or_else(|| q.expires_at.as_deref().map(format_str_full_date))
        })
        .filter(|s| !s.is_empty())
        .map(|s| format!("Key 到期 {s}"));

    let mut usage_rows = Vec::new();
    if let Some(today) = quota.and_then(|q| q.today.as_ref()) {
        usage_rows.push(usage_row("今日", today));
    }
    if let Some(total) = quota.and_then(|q| q.total_usage.as_ref()) {
        usage_rows.push(usage_row("累计", total));
    }

    let refresh_disabled = loading || quota.and_then(|q| q.retry_at).is_some_and(|r| r > now_secs);
    let has_error = quota.and_then(|q| q.error.as_ref()).is_some();
    let refresh_title = quota
        .and_then(|q| q.error.clone())
        .unwrap_or_else(|| "刷新额度".into());

    QuotaCardModel {
        tone: quota_tone(quota, now_secs).into(),
        subscription_label,
        compact_value,
        headline: headline.clone(),
        refresh_disabled,
        loading,
        has_error,
        refresh_title,
        detail: QuotaDetailModel {
            key_only_banner: key_only.then(|| "账户余额未开放".into()),
            headline,
            source_label,
            updated_time,
            updated_title,
            key_warning: quota
                .filter(|q| !q.plans.is_empty())
                .and_then(|q| q.key_status.clone()),
            error_text,
            subscriptions: sub_details,
            standalone_unit,
            standalone_plans,
            expiry_text,
            usage_rows,
        },
    }
}

pub fn derive_gateway_card(
    view: &gateway::View,
    cached_quotas: &HashMap<String, QuotaView>,
    now_secs: u64,
    extra_elapsed_secs: u64,
    cancelling_waits: &HashSet<String>,
) -> GatewayCardModel {
    let total_providers = view.providers.len();
    let mut priority_index = 0usize;
    let providers = view
        .providers
        .iter()
        .enumerate()
        .map(|(idx, p)| {
            let priority = if p.queued {
                priority_index += 1;
                Some(format!("P{priority_index}"))
            } else {
                None
            };
            let selected = view.mode == "manual" && view.selected.as_deref() == Some(p.id.as_str());
            let runtime_status = provider_runtime_status(p, view);
            let fault = provider_fault(p);
            let quota_ref = cached_quotas
                .get(&p.id)
                .filter(|q| q.version == p.quota_version)
                .or(p.quota.as_ref());
            let conc_cap = if p.max_concurrency == 0 {
                "∞".into()
            } else {
                p.max_concurrency.to_string()
            };
            let rpm_cap = if p.max_rpm == 0 {
                "∞".into()
            } else {
                p.max_rpm.to_string()
            };
            ProviderCardModel {
                id: p.id.clone(),
                name: p.name.clone(),
                priority,
                selected,
                select_label: if selected { "已选择" } else { "选择" }.into(),
                handoff_status: provider_handoff_status(p, view),
                runtime_alert: (!runtime_status.is_empty()).then_some(runtime_status),
                fault_event_id: fault.map(|f| f.event_id.clone()),
                fault_provider_id: fault.map(|_| p.id.clone()),
                show_codex_policies: view.client_id == ClientId::Codex,
                handoff_after_compaction: p.handoff_after_compaction,
                take_new_threads: p.take_new_threads,
                concurrency_label: format!("并发 {}/{conc_cap}", p.active_requests),
                max_concurrency: p.max_concurrency,
                rpm_label: format!("RPM {}/{rpm_cap}", p.rpm_used),
                max_rpm: p.max_rpm,
                queued: p.queued,
                queue_label: if p.queued {
                    "已入队"
                } else {
                    "加入队列"
                }
                .into(),
                can_move_up: idx > 0,
                can_move_down: idx + 1 < total_providers,
                quota: derive_quota_card(quota_ref, now_secs),
            }
        })
        .collect();

    GatewayCardModel {
        client_id: view.client_id,
        revision: view.revision.clone(),
        running: view.running,
        status_label: if view.running {
            "运行中"
        } else {
            "已关闭"
        }
        .into(),
        mode: view.mode.clone(),
        mode_label: if view.mode == "auto" {
            "自动"
        } else {
            "手动"
        }
        .into(),
        power_label: if view.running {
            "停用网关"
        } else {
            "启用网关"
        }
        .into(),
        waiting_badge: (view.waiting_requests > 0)
            .then(|| format!("等待 {}", view.waiting_requests)),
        long_waits: derive_long_waits(view, extra_elapsed_secs, cancelling_waits),
        providers,
    }
}

/// Everything the panel shows of one client, in one answer.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub freeze: FreezeCardModel,
    /// Absent when the gateway's state cannot be read; `error` says why.
    pub gateway: Option<GatewayCardModel>,
    pub error: Option<String>,
    /// What turning the gateway on or off is checked against.
    pub config_revision: Option<String>,
    pub recovery_pending: bool,
}

/// The panel's view of `client` as of now. `cancelling` are the waits the
/// panel is interrupting, which it shows as such until they are gone.
pub fn snapshot(rt: &Runtime, client: ClientId, cancelling: &HashSet<String>) -> Snapshot {
    if let Ok(home) = rt.home(client) {
        rt.gateway(client).observe_home(&home);
    }
    let freeze = derive_freeze_card(Some(&rt.gateway.freeze_gate().view()), None, false);
    let accounts = || rt.state().ok().and_then(|state| state.error);
    match rt.gateway(client).try_view() {
        Ok(view) => Snapshot {
            freeze,
            gateway: Some(derive_gateway_card(
                &view,
                &HashMap::new(),
                gateway::quota::now(),
                0,
                cancelling,
            )),
            error: view.error.clone().or_else(accounts),
            config_revision: view.config_revision,
            recovery_pending: view.recovery_pending,
        },
        Err(error) => Snapshot {
            freeze,
            gateway: None,
            error: Some(error.message),
            config_revision: None,
            recovery_pending: false,
        },
    }
}
