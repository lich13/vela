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
struct Shared {
    entries: Mutex<HashMap<String, Entry>>,
    /// Signals every change of an endpoint between down and not down.
    changes: watch::Sender<()>,
}
#[derive(Clone)]
pub(super) struct Links(Arc<Shared>);
impl Default for Links {
    fn default() -> Self {
        Self(Arc::new(Shared {
            entries: Mutex::default(),
            changes: watch::channel(()).0,
        }))
    }
}
impl Links {
    /// Requests are being held on this endpoint because its network is down.
    pub fn down(&self, key: &str) -> bool {
        self.0
            .entries
            .lock()
            .get(key)
            .is_some_and(|entry| *entry.link.borrow() == Link::Down)
    }
    /// Wakes whenever `down` may answer differently. Watchers use this
    /// instead of an endpoint's own link, whose receivers keep its probe alive.
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.0.changes.subscribe()
    }
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.0.entries.lock().is_empty()
    }
    /// Marks an endpoint as held without a probe behind it.
    #[cfg(test)]
    pub fn set_down(&self, key: &str, down: bool) {
        let mut entries = self.0.entries.lock();
        if down {
            entries.insert(
                key.into(),
                Entry {
                    link: Arc::new(watch::channel(Link::Down).0),
                    held: Arc::default(),
                },
            );
        } else {
            entries.remove(key);
        }
        drop(entries);
        self.0.changes.send_replace(());
    }
}
/// What identifies the endpoint a route's requests go to.
pub(super) fn endpoint_key(route: &Route) -> String {
    format!("{}\n{}", route.provider.id, route.provider.base_url)
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
/// endpoint answers; dropping the future ends the wait. With `queued`, the
/// request is the queue's to place elsewhere: it is not held for a provider
/// taken out of the queue, and one taken out while it is held releases it.
pub(super) async fn settle(
    gateway: &Gateway,
    route: &Route,
    settings: &Settings,
    budget: &WaitBudget,
    model: Option<&str>,
    hold: bool,
    queued: bool,
) -> Verdict {
    if !settings.network_probe {
        return Verdict::Reachable;
    }
    // The verdict may take as long as a freeze lasts. Until it comes the
    // request waits rather than drains, so the freeze can still be lifted.
    budget.describe(Some(&route.provider.id), model, "retry");
    let key = endpoint_key(route);
    let (mut link, held) = {
        let mut links = gateway.0.links.0.entries.lock();
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
        // Enabled before the queue is read, so no edit falls between the two.
        let edited = gateway.0.admission.signalled();
        tokio::pin!(edited);
        edited.as_mut().enable();
        let state = *link.borrow_and_update();
        match state {
            Link::Up if pause.is_some() => return Verdict::Restored,
            Link::Up => return Verdict::Reachable,
            Link::Down if !hold => return Verdict::Unreachable,
            Link::Down if queued && !gateway.routing_ids(None).contains(&route.provider.id) => {
                return Verdict::Unreachable
            }
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
        tokio::select! {
            changed = link.changed() => {
                if changed.is_err() {
                    return Verdict::Reachable;
                }
            }
            _ = edited, if queued && pause.is_some() => {}
        }
    }
}

/// Probes until the endpoint answers or nobody waits for it any more. A
/// frozen client sends nothing, the probe included: it resumes with the thaw,
/// and an answer that arrives after the freeze releases nothing.
async fn probe(
    gateway: Gateway,
    route: Route,
    key: String,
    attempts: u32,
    link: Arc<watch::Sender<Link>>,
    held: Arc<AtomicBool>,
) {
    let freeze = gateway.freeze_gate();
    let mut thaw = freeze.subscribe_control();
    let mut failures = 0u32;
    loop {
        thaw.borrow_and_update();
        if freeze.is_frozen(route.client_id) {
            tokio::select! {
                _ = link.closed() => {}
                _ = thaw.changed() => {}
            }
        } else {
            let up = tokio::select! {
                up = reachable(&route) => up,
                _ = link.closed() => false,
            };
            if up {
                let mut links = gateway.0.links.0.entries.lock();
                if !freeze.is_frozen(route.client_id) {
                    link.send_replace(Link::Up);
                    links.remove(&key);
                    drop(links);
                    gateway.0.links.0.changes.send_replace(());
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
            } else {
                failures = failures.saturating_add(1);
                let down = failures >= attempts;
                if down && link.send_replace(Link::Down) != Link::Down {
                    gateway.0.links.0.changes.send_replace(());
                }
                tokio::select! {
                    _ = link.closed() => {}
                    _ = tokio::time::sleep(if down { HELD_GAP } else { CHECK_GAP }) => {}
                }
            }
        }
        // Waiters subscribe under this lock, so none can arrive after the check.
        let mut links = gateway.0.links.0.entries.lock();
        if link.receiver_count() == 0 {
            links.remove(&key);
            drop(links);
            gateway.0.links.0.changes.send_replace(());
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
