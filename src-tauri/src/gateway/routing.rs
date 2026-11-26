//! Model policy is independent of transport versions, health and capacity.
use crate::errors::code;
/// Capture before admission: admitting a first request must not retroactively
/// turn its ordinary capacity fallback into an established-thread wait.
pub(super) fn capacity_owner(
    client: super::ClientId,
    manual: bool,
    lease: Option<&super::compaction::Lease>,
) -> Option<String> {
    if client != super::ClientId::Codex || manual {
        return None;
    }
    lease
        // A legal compaction handoff retains its own receiver/fallback policy;
        // the existing-thread capacity wait applies only to an ordinary bound
        // owner, never to a handoff turn that is already authorized to move.
        .filter(|l| l.follows_owner() && !l.handoff)
        .and_then(|l| l.admission_policy().1)
}
pub(super) fn capacity_cooling(health: &super::circuit::Health) -> bool {
    !health.available
        && health.state == super::circuit::CircuitState::Closed
        && matches!(
            health.cooldown_reason.as_deref(),
            Some("capacity_retry" | "rate_limit")
        )
        && health.cause.as_ref().is_some_and(|cause| {
            matches!(
                cause.reason,
                crate::events::Reason::RateLimit | crate::events::Reason::Capacity
            )
        })
}
/// Give an established conversation two short, cancellable retries on its
/// owner before ordinary failover. These still consume the normal retry budget.
pub fn transient_delay(
    retries: &mut u8,
    retry_after: Option<std::time::Duration>,
) -> Option<std::time::Duration> {
    if *retries >= 2 {
        return None;
    }
    *retries += 1;
    Some(std::time::Duration::from_secs(u64::from(*retries)).max(retry_after.unwrap_or_default()))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Requirement {
    Resource,
    Model(String),
    Unknown,
}
impl Requirement {
    pub fn model(model: Option<&str>) -> Self {
        model
            .filter(|m| !m.is_empty())
            .map(|m| Self::Model(m.into()))
            .unwrap_or(Self::Unknown)
    }
    pub fn allows(&self, allowed: Option<&[String]>) -> bool {
        match (self, allowed) {
            (Self::Resource, _) | (_, None) => true,
            (Self::Model(model), Some(allowed)) => allowed.contains(model),
            (Self::Unknown, Some(_)) => false,
        }
    }
    pub fn code(&self) -> &'static str {
        if matches!(self, Self::Unknown) {
            code::MODEL_UNDETERMINED
        } else {
            code::MODEL_NOT_ALLOWED
        }
    }
    pub fn message(&self) -> &'static str {
        if matches!(self, Self::Unknown) {
            "无法识别请求模型，且没有不限模型的供应商"
        } else {
            "当前候选供应商不允许此模型，请检查模型白名单和上下文归属"
        }
    }
}
/// The path without the API version before it.
fn unversioned(path: &str) -> String {
    let path = super::gemini::path(path);
    path.strip_prefix("/v1/")
        .map(|p| format!("/{p}"))
        .unwrap_or_else(|| path.to_owned())
}
fn under(path: &str, prefix: &str) -> bool {
    path == prefix || path.starts_with(&format!("{prefix}/"))
}
pub fn resource(method: &hyper::Method, path: &str) -> bool {
    if super::gemini::model(path).is_some() {
        return false;
    }
    let path = unversioned(path);
    ["/models", "/files", "/uploads"]
        .iter()
        .any(|prefix| under(&path, prefix))
        || (*method == hyper::Method::GET && path.starts_with("/responses/"))
}
/// A path segment that is what it reads as: nothing an upstream would
/// resolve, merge or decode into another path.
fn plain(segment: &str) -> bool {
    !matches!(segment, "" | "." | "..")
        && segment
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:@".contains(&c))
}
/// What a handed-out key may ask for: a generation in one of the wire
/// protocols whose usage is counted, a compaction, a search, a token count,
/// or the list of models. Whatever else a provider serves is its account's
/// own. The path has to be one of these as it is written: it is passed on as
/// it came, and a provider may well read `/models/../files` as its files.
pub fn counted(method: &hyper::Method, path: &str) -> bool {
    let rest = unversioned(path);
    let rest = rest.strip_suffix('/').unwrap_or(&rest);
    if *method == hyper::Method::GET {
        return rest == "/models"
            || rest
                .strip_prefix("/models/")
                .is_some_and(|id| id.split('/').all(plain));
    }
    *method == hyper::Method::POST
        && (super::gemini::model(path).is_some()
            || matches!(
                rest,
                "/responses"
                    | "/responses/compact"
                    | "/chat/completions"
                    | "/messages"
                    | "/messages/count_tokens"
                    | "/alpha/search"
            ))
}
