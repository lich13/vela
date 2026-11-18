//! After a failed connection, asks the provider for its model list before the
//! request is sent again. Any HTTP answer proves the path is open. Requests
//! that failed on the same endpoint share one probe.
use super::{model::Settings, replay, wait_budget::WaitBudget, ClientId, Gateway, Route};
use crate::events::{Action, Reason, Record};
use hyper::{header, Request};
use parking_lot::Mutex;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::watch;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Between the probes that decide whether the network is down.
const CHECK_GAP: Duration = Duration::from_millis(if cfg!(test) { 100 } else { 2000 });
/// Between the probes that wait for the network to come back.
const HELD_GAP: Duration = Duration::from_millis(if cfg!(test) { 100 } else { 5000 });

#[derive(Clone, Copy, PartialEq, Eq)]
enum Link {
    Checking,
    Down,
    Up,
}
struct Entry {
    link: Arc<watch::Sender<Link>>,
    /// A request is being held on this endpoint, so its return is announced.
    held: Arc<AtomicBool>,
}
#[derive(Clone, Default)]
pub(super) struct Links(Arc<Mutex<HashMap<String, Entry>>>);
#[cfg(test)]
impl Links {
    pub fn is_empty(&self) -> bool {
        self.0.lock().is_empty()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// The provider answered, or probing is off: retry as before.
    Reachable,
    /// The network is down and this request has somewhere else to go.
    Unreachable,
    /// The request was held until the provider answered again.
    Restored,
}

/// The connection failures a probe can explain; a gateway loop is not one.
pub(super) fn connection_failure(local_code: Option<&str>) -> bool {
    use crate::errors::code;
    matches!(
        local_code,
        Some(
            code::CONNECTION_FAILED
                | code::CONNECT_TIMEOUT
                | code::TLS_HANDSHAKE_FAILED
                | code::WS_HANDSHAKE_TIMEOUT
        )
    )
}

/// Probes the endpoint `route` just failed to reach. With `hold`, a request
/// whose network is down waits here, outside its retry budget, until the
/// endpoint answers; dropping the future ends the wait.
pub(super) async fn settle(
    gateway: &Gateway,
    route: &Route,
    settings: &Settings,
    budget: &WaitBudget,
    model: Option<&str>,
    hold: bool,
) -> Verdict {
    if !settings.network_probe {
        return Verdict::Reachable;
    }
    let key = format!("{}\n{}", route.provider.id, route.provider.base_url);
    let (mut link, held) = {
        let mut links = gateway.0.links.0.lock();
        let entry = links.entry(key.clone()).or_insert_with(|| {
            let entry = Entry {
                link: Arc::new(watch::channel(Link::Checking).0),
                held: Arc::default(),
            };
            tokio::spawn(probe(
                gateway.clone(),
                route.clone(),
                key,
                settings.network_probe_attempts,
                entry.link.clone(),
                entry.held.clone(),
            ));
            entry
        });
        (entry.link.subscribe(), entry.held.clone())
    };
    let mut pause = None;
    loop {
        let state = *link.borrow_and_update();
        match state {
            Link::Up if pause.is_some() => return Verdict::Restored,
            Link::Up => return Verdict::Reachable,
            Link::Down if !hold => return Verdict::Unreachable,
            Link::Down if pause.is_none() => {
                budget.describe(Some(&route.provider.id), model, "held");
                pause = Some(budget.execution());
                if !held.swap(true, Ordering::SeqCst) {
                    report(
                        gateway,
                        route,
                        Reason::NetworkHold,
                        Action::Waiting,
                        "网络不通，请求已暂存，恢复后自动重发",
                    );
                }
            }
            _ => {}
        }
        if link.changed().await.is_err() {
            return Verdict::Reachable;
        }
    }
}

/// Probes until the endpoint answers or nobody waits for it any more.
async fn probe(
    gateway: Gateway,
    route: Route,
    key: String,
    attempts: u32,
    link: Arc<watch::Sender<Link>>,
    held: Arc<AtomicBool>,
) {
    let mut failures = 0u32;
    loop {
        let up = tokio::select! {
            up = reachable(&route) => up,
            _ = link.closed() => false,
        };
        if up {
            let mut links = gateway.0.links.0.lock();
            link.send_replace(Link::Up);
            links.remove(&key);
            drop(links);
            if held.load(Ordering::SeqCst) {
                report(
                    &gateway,
                    &route,
                    Reason::NetworkRestored,
                    Action::RetryingSame,
                    "网络已恢复，已重发暂存请求",
                );
            }
            return;
        }
        failures = failures.saturating_add(1);
        let down = failures >= attempts;
        if down {
            link.send_replace(Link::Down);
        }
        tokio::select! {
            _ = link.closed() => {}
            _ = tokio::time::sleep(if down { HELD_GAP } else { CHECK_GAP }) => {}
        }
        // Waiters subscribe under this lock, so none can arrive after the check.
        let mut links = gateway.0.links.0.lock();
        if link.receiver_count() == 0 {
            links.remove(&key);
            return;
        }
    }
}

/// Whether the provider's model list answers at all, whatever the status.
async fn reachable(route: &Route) -> bool {
    let Ok(target) = route.target(&"/v1/models".parse().expect("static model path")) else {
        return true;
    };
    let mut builder = Request::builder()
        .method("GET")
        .uri(target)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", route.provider.token),
        )
        .header(header::ACCEPT, "application/json")
        .header(header::ACCEPT_ENCODING, "identity");
    if route.client_id == ClientId::Claude
        || route
            .grok
            .as_ref()
            .is_some_and(|c| c.protocol == super::grok_config::Protocol::Messages)
    {
        builder = builder.header("anthropic-version", "2023-06-01");
    }
    let Ok(mut request) = builder.body(replay::empty()) else {
        return true;
    };
    if let Some(connection) = &route.grok {
        if connection
            .authenticate(request.headers_mut(), &route.provider.token)
            .is_err()
        {
            return true;
        }
    }
    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, route.client.request(request)).await,
        Ok(Ok(_))
    )
}

fn report(gateway: &Gateway, route: &Route, reason: Reason, action: Action, text: &str) {
    let mut record = Record::new(
        Some(route.client_id),
        Some(&route.provider.id),
        None,
        reason,
        action,
        None,
        None,
    );
    let provider = crate::events::safe_message(&route.provider.name).unwrap_or_else(|| {
        format!(
            "供应商 {}",
            &route.provider.id[..route.provider.id.len().min(8)]
        )
    });
    record.notification_message = Some(format!(
        "{} · {provider}：{text}",
        record
            .client_id
            .map(crate::events::JournalClient::name)
            .unwrap_or("应用")
    ));
    if let Some(events) = gateway.0.diagnostics.lock().as_ref() {
        events.emit(record);
    }
}
