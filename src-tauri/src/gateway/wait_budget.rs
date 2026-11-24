//! Queue/backoff time shares one budget; upstream attempts have phase timeouts.
use super::connectivity::Links;
use parking_lot::Mutex;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{broadcast, watch},
    time::Instant,
};

/// How long an upstream may stay silent before a wait gives up.
#[derive(Clone, Copy)]
pub enum Silence {
    /// Until the caller's deadline.
    Ordinary,
    /// Compaction, once dispatched: this long from dispatch while it has
    /// produced no output, without limit afterwards.
    Compaction(Option<Duration>),
    /// A response that has started: until the caller's deadline, and no
    /// longer than the outage limit while its endpoint is held.
    Started,
    /// A stream the model is writing: as `Started`, and no longer than this.
    Flowing(Duration),
}
/// Why a wait for the upstream gave up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quiet {
    /// The caller's deadline passed.
    Deadline,
    /// A stream the model was writing went silent.
    Stalled,
    /// The response went silent while its endpoint's network was down.
    Held,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequest {
    pub request_id: String,
    pub provider_id: Option<String>,
    pub model: Option<String>,
    pub started_at: u64,
    pub elapsed_seconds: u64,
    /// Live wait time since the provider, model or phase was last set.
    pub attempt_seconds: u64,
    pub remaining_seconds: Option<u64>,
    pub phase: String,
    /// Upstream bytes of the attempt in progress; absent while nothing is
    /// being received.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub received_bytes: Option<u64>,
    /// Averaged over the last five seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_per_second: Option<u64>,
    /// Since the last upstream byte.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub silent_seconds: Option<u64>,
}
const RATE_WINDOW: Duration = Duration::from_secs(5);
/// Upstream data received by the attempt in progress.
struct Transfer {
    received: u64,
    began: Instant,
    last: Instant,
    /// Bytes per recent slice of time, oldest first.
    recent: VecDeque<(Instant, u64)>,
}
impl Transfer {
    fn add(&mut self, bytes: u64, now: Instant) {
        self.received = self.received.saturating_add(bytes);
        self.last = now;
        while self
            .recent
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > RATE_WINDOW)
        {
            self.recent.pop_front();
        }
        match self.recent.back_mut() {
            Some((at, slice)) if now.duration_since(*at) < Duration::from_millis(250) => {
                *slice = slice.saturating_add(bytes)
            }
            _ => self.recent.push_back((now, bytes)),
        }
    }
    fn rate(&self, now: Instant) -> u64 {
        let bytes: u64 = self
            .recent
            .iter()
            .filter(|(at, _)| now.duration_since(*at) <= RATE_WINDOW)
            .map(|(_, bytes)| bytes)
            .sum();
        // A transfer younger than the window is averaged over its own age.
        let span = now
            .duration_since(self.began)
            .clamp(Duration::from_secs(1), RATE_WINDOW);
        (bytes as f64 / span.as_secs_f64()).round() as u64
    }
}
/// The endpoint an attempt's response comes from, watched for an outage.
#[derive(Clone)]
struct Outage {
    links: Links,
    key: String,
    limit: Duration,
}
struct Pending {
    view: PendingRequest,
    transfer: Option<Transfer>,
    started: Instant,
    deadline: Instant,
    compaction_execution: bool,
    compaction_since: Instant,
    endless: bool,
    executing: bool,
    execution_elapsed: Duration,
    traffic: Option<super::freeze::Ticket>,
    paused_before: Duration,
    /// When the attempt or wait being shown began, and `paused_for` then.
    attempt_since: Instant,
    attempt_paused: Duration,
}
impl Pending {
    fn paused_for(&self) -> Duration {
        self.traffic.as_ref().map_or(Duration::ZERO, |t| {
            t.paused_for().saturating_sub(self.paused_before)
        })
    }
    fn paused(&self) -> bool {
        self.traffic.as_ref().is_some_and(|t| t.paused())
    }
    fn expires_at(&self) -> Instant {
        self.deadline + self.paused_for() + self.execution_elapsed
    }
}
#[derive(Default)]
struct State {
    active: Option<Pending>,
    cancelled: Option<Instant>,
    /// The response is with the client: a cancel can no longer send the
    /// request again, only end it.
    delivering: bool,
    /// Kept here rather than with `active`, which the first output removes
    /// while the response is still being relayed.
    outage: Option<Outage>,
}
struct Shared {
    state: Mutex<State>,
    changes: watch::Sender<()>,
    registry: Option<Registry>,
}
#[derive(Clone)]
pub(super) struct Registry(Arc<RegistryInner>);
struct RegistryInner {
    requests: Mutex<HashMap<String, Weak<Shared>>>,
    events: broadcast::Sender<()>,
}
impl Registry {
    pub fn new(events: broadcast::Sender<()>) -> Self {
        Self(Arc::new(RegistryInner {
            requests: Mutex::new(HashMap::new()),
            events,
        }))
    }
    fn remove(&self, id: &str) {
        self.0.requests.lock().remove(id);
        let _ = self.0.events.send(());
    }
    pub fn view(&self) -> Vec<PendingRequest> {
        let entries: Vec<_> = self
            .0
            .requests
            .lock()
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        let mut result: Vec<_> = entries
            .iter()
            .filter_map(|shared| {
                let state = shared.state.lock();
                let active = state.active.as_ref()?;
                let mut view = active.view.clone();
                view.elapsed_seconds = active
                    .started
                    .elapsed()
                    .saturating_sub(active.paused_for())
                    .as_secs();
                view.attempt_seconds = active
                    .attempt_since
                    .elapsed()
                    .saturating_sub(active.paused_for().saturating_sub(active.attempt_paused))
                    .as_secs();
                if let Some(transfer) = &active.transfer {
                    let now = Instant::now();
                    view.received_bytes = Some(transfer.received);
                    view.bytes_per_second = Some(transfer.rate(now));
                    view.silent_seconds = Some(now.duration_since(transfer.last).as_secs());
                }
                if active.paused() {
                    view.phase = "frozen".into();
                }
                view.remaining_seconds = (!active.compaction_execution
                    && !active.endless
                    && !active.executing
                    && !active.paused())
                .then(|| {
                    active
                        .expires_at()
                        .saturating_duration_since(Instant::now())
                        .as_millis()
                        .div_ceil(1000) as u64
                });
                Some(view)
            })
            .collect();
        result.sort_by(|a, b| {
            a.started_at
                .cmp(&b.started_at)
                .then(a.request_id.cmp(&b.request_id))
        });
        result
    }
    #[cfg(test)]
    pub fn cancel(&self, id: &str) -> bool {
        self.interrupt(id).is_some()
    }
    /// Cancels the wait `id` names. `Some(true)` when the request is sent
    /// again, `Some(false)` when its response was already with the client
    /// and is ended instead, `None` when there was nothing left to cancel.
    pub fn interrupt(&self, id: &str) -> Option<bool> {
        self.cancel_shared(id).map(|delivering| !delivering)
    }
    fn cancel_shared(&self, id: &str) -> Option<bool> {
        let shared = self.0.requests.lock().get(id).and_then(Weak::upgrade)?;
        let mut state = shared.state.lock();
        if !state.active.as_ref().is_some_and(|p| {
            p.view.request_id == id
                && (p.compaction_execution
                    || p.endless
                    || p.executing
                    || p.paused()
                    || p.expires_at() > Instant::now())
        }) {
            return None;
        }
        state.cancelled = state.active.take().map(|p| p.deadline);
        let delivering = state.delivering;
        drop(state);
        self.remove(id);
        shared.changes.send_replace(());
        Some(delivering)
    }
}
impl Drop for Shared {
    fn drop(&mut self) {
        if let (Some(registry), Some(active)) = (&self.registry, &self.state.get_mut().active) {
            registry.remove(&active.view.request_id);
        }
    }
}
#[derive(Clone)]
pub(super) struct WaitBudget {
    shared: Arc<Shared>,
    pub seconds: u64,
}
pub(super) struct Execution {
    budget: WaitBudget,
    deadline: Option<Instant>,
    started: Instant,
}
impl Drop for Execution {
    fn drop(&mut self) {
        let mut state = self.budget.shared.state.lock();
        if let Some(active) = state
            .active
            .as_mut()
            .filter(|p| Some(p.deadline) == self.deadline)
        {
            active.executing = false;
            active.execution_elapsed += self.started.elapsed();
        }
        drop(state);
        self.budget.shared.changes.send_replace(());
    }
}
impl WaitBudget {
    pub fn execution(&self) -> Execution {
        let mut state = self.shared.state.lock();
        let deadline = state.active.as_mut().map(|active| {
            active.executing = true;
            active.deadline
        });
        drop(state);
        self.shared.changes.send_replace(());
        Execution {
            budget: self.clone(),
            deadline,
            started: Instant::now(),
        }
    }
    pub fn tracked(seconds: u64, registry: &Registry) -> Self {
        Self::with_registry(seconds, Some(registry.clone()))
    }
    fn with_registry(seconds: u64, registry: Option<Registry>) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                changes: watch::channel(()).0,
                registry,
            }),
            seconds,
        }
    }
    pub fn begin(&self) {
        self.begin_at(Instant::now());
    }
    pub fn begin_at(&self, started: Instant) {
        let id = uuid::Uuid::new_v4().to_string();
        let mut state = self.shared.state.lock();
        if let (Some(registry), Some(old)) = (&self.shared.registry, state.active.take()) {
            registry.remove(&old.view.request_id);
        }
        state.cancelled = None;
        state.active = Some(Pending {
            view: PendingRequest {
                request_id: id.clone(),
                provider_id: None,
                model: None,
                started_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .saturating_sub(started.elapsed().as_secs()),
                elapsed_seconds: 0,
                attempt_seconds: 0,
                remaining_seconds: Some(self.seconds),
                phase: "admission".into(),
                received_bytes: None,
                bytes_per_second: None,
                silent_seconds: None,
            },
            transfer: None,
            started,
            deadline: started + Duration::from_secs(self.seconds),
            compaction_execution: false,
            compaction_since: started,
            endless: false,
            executing: false,
            execution_elapsed: Duration::ZERO,
            traffic: None,
            paused_before: Duration::ZERO,
            attempt_since: started,
            attempt_paused: Duration::ZERO,
        });
        if let Some(registry) = &self.shared.registry {
            registry
                .0
                .requests
                .lock()
                .insert(id, Arc::downgrade(&self.shared));
            let _ = registry.0.events.send(());
        }
        drop(state);
        self.shared.changes.send_replace(());
    }
    pub fn set_endless(&self, endless: bool) {
        if let Some(active) = &mut self.shared.state.lock().active {
            active.endless = endless;
        }
        self.shared.changes.send_replace(());
    }
    pub fn attach_traffic(&self, traffic: &super::freeze::Ticket) {
        if let Some(active) = &mut self.shared.state.lock().active {
            active.paused_before = traffic.paused_for();
            // `paused_for` counts from here on.
            active.attempt_paused = Duration::ZERO;
            active.traffic = Some(traffic.clone());
        }
        self.shared.changes.send_replace(());
    }
    pub fn traffic(&self) -> Option<super::freeze::Ticket> {
        self.shared
            .state
            .lock()
            .active
            .as_ref()
            .and_then(|p| p.traffic.clone())
    }
    pub fn describe(&self, provider: Option<&str>, model: Option<&str>, phase: &str) {
        let mut state = self.shared.state.lock();
        if let Some(active) = &mut state.active {
            if matches!(
                phase,
                "admission" | "capacity" | "retry" | "ws_wait" | "held"
            ) {
                if let Some(traffic) = &active.traffic {
                    traffic.waiting();
                }
            }
            if let Some(provider) = provider {
                active.view.provider_id = Some(provider.into());
            }
            if let Some(model) = model {
                active.view.model = crate::events::safe_message(model);
            }
            active.view.phase = phase.into();
            active.attempt_since = Instant::now();
            active.attempt_paused = active.paused_for();
            active.compaction_execution = false;
            // Each attempt and each wait counts its own transfer.
            active.transfer = None;
        }
        drop(state);
        self.shared.changes.send_replace(());
        if let Some(registry) = &self.shared.registry {
            let _ = registry.0.events.send(());
        }
    }
    /// Counts upstream bytes for the request `deadline` identifies. The list
    /// is read on a timer, so nobody is woken for it.
    pub fn progress(&self, deadline: Option<Instant>, bytes: usize) {
        let mut state = self.shared.state.lock();
        if let Some(active) = state
            .active
            .as_mut()
            .filter(|p| Some(p.deadline) == deadline)
        {
            let now = Instant::now();
            active
                .transfer
                .get_or_insert_with(|| Transfer {
                    received: 0,
                    began: now,
                    last: now,
                    recent: VecDeque::new(),
                })
                .add(bytes as u64, now);
        }
    }
    /// Sets the endpoint whose outage shortens this request's response
    /// waits, or none while outages are not probed.
    pub fn watch_endpoint(&self, endpoint: Option<(Links, String, Duration)>) {
        self.shared.state.lock().outage =
            endpoint.map(|(links, key, limit)| Outage { links, key, limit });
    }
    /// Marks the response as handed to the client.
    pub fn delivering(&self) {
        self.shared.state.lock().delivering = true;
    }
    pub fn buffering(&self) {
        let mut state = self.shared.state.lock();
        let updated = state
            .active
            .as_mut()
            .filter(|active| !active.compaction_execution && active.view.phase != "buffering")
            .map(|active| active.view.phase = "buffering".into())
            .is_some();
        drop(state);
        if updated {
            self.shared.changes.send_replace(());
            if let Some(registry) = &self.shared.registry {
                let _ = registry.0.events.send(());
            }
        }
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.shared.state.lock().active.as_ref().map(|p| p.deadline)
    }
    pub fn compaction_execution(&self, deadline: Option<Instant>, executing: bool) {
        let mut state = self.shared.state.lock();
        if let Some(active) = state
            .active
            .as_mut()
            .filter(|p| Some(p.deadline) == deadline)
        {
            if executing && !active.compaction_execution {
                active.compaction_since = Instant::now();
            }
            active.compaction_execution = executing;
            if executing {
                active.view.phase = "compaction".into();
            }
        }
        drop(state);
        self.shared.changes.send_replace(());
        if let Some(registry) = &self.shared.registry {
            let _ = registry.0.events.send(());
        }
    }
    pub fn executing_compaction(&self) -> bool {
        self.shared
            .state
            .lock()
            .active
            .as_ref()
            .is_some_and(|p| p.compaction_execution)
    }
    fn compaction_since(&self) -> Option<Instant> {
        self.shared
            .state
            .lock()
            .active
            .as_ref()
            .filter(|p| p.compaction_execution)
            .map(|p| p.compaction_since)
    }
    pub async fn response<T>(
        &self,
        deadline: Instant,
        silence: Silence,
        future: impl std::future::Future<Output = T>,
    ) -> Result<T, Quiet> {
        let limit = match silence {
            Silence::Ordinary => {
                return tokio::time::timeout_at(deadline, future)
                    .await
                    .map_err(|_| Quiet::Deadline)
            }
            Silence::Started => return self.watched(deadline, None, future).await,
            Silence::Flowing(stall) => return self.watched(deadline, Some(stall), future).await,
            Silence::Compaction(limit) => limit,
        };
        let mut changes = self.shared.changes.subscribe();
        tokio::pin!(future);
        loop {
            changes.borrow_and_update();
            if let Some(since) = self.compaction_since() {
                return match limit {
                    Some(limit) => tokio::time::timeout_at(since + limit, future)
                        .await
                        .map_err(|_| Quiet::Deadline),
                    None => Ok(future.await),
                };
            }
            tokio::select! {
                biased;
                result = &mut future => return Ok(result),
                _ = changes.changed() => {},
                _ = tokio::time::sleep_until(deadline) => return Err(Quiet::Deadline),
            }
        }
    }
    /// Waits for a started response. While the endpoint is held for an
    /// outage the wait ends `limit` after it began; once the hold is over,
    /// `limit` after that, since a connection silent through an outage has
    /// most likely died with it.
    async fn watched<T>(
        &self,
        deadline: Instant,
        stall: Option<Duration>,
        future: impl std::future::Future<Output = T>,
    ) -> Result<T, Quiet> {
        let began = Instant::now();
        let (limit, cause) = match stall {
            Some(stall) if began + stall < deadline => (began + stall, Quiet::Stalled),
            _ => (deadline, Quiet::Deadline),
        };
        let Some(outage) = self.shared.state.lock().outage.clone() else {
            return tokio::time::timeout_at(limit, future)
                .await
                .map_err(|_| cause);
        };
        let mut changes = outage.links.subscribe();
        let mut watching = true;
        let mut held = false;
        let mut outage_limit = None;
        tokio::pin!(future);
        loop {
            changes.borrow_and_update();
            if outage.links.down(&outage.key) {
                held = true;
                outage_limit = Some(began + outage.limit);
            } else if held {
                held = false;
                outage_limit = Some(Instant::now() + outage.limit);
            }
            let (limit, cause) = match outage_limit {
                Some(outage_limit) if outage_limit < limit => (outage_limit, Quiet::Held),
                _ => (limit, cause),
            };
            tokio::select! {
                biased;
                result = &mut future => return Ok(result),
                changed = changes.changed(), if watching => watching = changed.is_ok(),
                _ = tokio::time::sleep_until(limit) => return Err(cause),
            }
        }
    }
    pub fn complete(&self, deadline: Option<Instant>) {
        let mut state = self.shared.state.lock();
        if state
            .active
            .as_ref()
            .is_some_and(|p| Some(p.deadline) == deadline)
        {
            if let (Some(registry), Some(active)) = (&self.shared.registry, state.active.take()) {
                registry.remove(&active.view.request_id);
            }
        }
        drop(state);
        self.shared.changes.send_replace(());
    }
    pub fn is_cancelled(&self, deadline: Option<Instant>) -> bool {
        let state = self.shared.state.lock();
        state.cancelled.is_some() && state.cancelled == deadline
    }
    pub fn cancel_requested(&self) -> bool {
        self.shared.state.lock().cancelled.is_some()
    }
    pub fn acknowledge_cancel(&self) {
        self.shared.state.lock().cancelled = None;
    }
    pub fn expired(&self, deadline: Option<Instant>) -> bool {
        self.shared.state.lock().active.as_ref().is_some_and(|p| {
            !p.compaction_execution
                && !p.endless
                && !p.executing
                && Some(p.deadline) == deadline
                && !p.paused()
                && Instant::now() >= p.expires_at()
        })
    }
    pub async fn cancelled(&self) {
        let mut changes = self.shared.changes.subscribe();
        loop {
            changes.borrow_and_update();
            if self.shared.state.lock().cancelled.is_some() {
                return;
            }
            if changes.changed().await.is_err() {
                return;
            }
        }
    }
    pub async fn wait(&self) {
        let mut changes = self.shared.changes.subscribe();
        loop {
            changes.borrow_and_update();
            let mut traffic_changes = self.traffic().map(|t| t.subscribe());
            let deadline = self
                .shared
                .state
                .lock()
                .active
                .as_ref()
                .filter(|p| !p.compaction_execution && !p.endless && !p.executing && !p.paused())
                .map(|p| (p.deadline, p.expires_at()));
            if let Some((identity, deadline)) = deadline {
                tokio::select! {
                    biased;
                    _ = changes.changed() => {},
                    _ = super::freeze::changed(&mut traffic_changes) => {},
                    _ = tokio::time::sleep_until(deadline) => { if self.expired(Some(identity)) { return; } }
                }
            } else {
                tokio::select! {
                    _ = changes.changed() => {},
                    _ = super::freeze::changed(&mut traffic_changes) => {},
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::protocol::Protocol;
    use serde_json::json;
    impl WaitBudget {
        pub fn new(seconds: u64) -> Self {
            Self::with_registry(seconds, None)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn upstream_execution_preserves_remaining_wait_budget_and_cancellation() {
        let registry = Registry::new(broadcast::channel(16).0);
        let budget = WaitBudget::tracked(10, &registry);
        budget.begin();
        let identity = budget.deadline();
        tokio::time::advance(Duration::from_secs(3)).await;
        let mut protocol = Protocol::new(true);
        protocol.attach_wait_budget(&budget);
        tokio::time::advance(Duration::from_secs(600)).await;
        assert!(!budget.expired(identity));
        assert_eq!(registry.view()[0].remaining_seconds, None);
        protocol.finish(Some(503), crate::errors::code::HTTP);
        assert_eq!(registry.view()[0].remaining_seconds, Some(7));
        tokio::time::advance(Duration::from_secs(6)).await;
        assert!(!budget.expired(identity));
        budget.wait().await;
        assert!(budget.expired(identity));
        budget.complete(identity);

        budget.begin();
        let mut protocol = Protocol::new(true);
        protocol.attach_wait_budget(&budget);
        tokio::time::advance(Duration::from_secs(600)).await;
        assert!(registry.cancel(&registry.view()[0].request_id));
        budget.cancelled().await;
        protocol.finish(None, crate::errors::code::CANCELLED);
        assert!(registry.view().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn freeze_preserves_deadline_identity_and_only_live_wait_time_counts() {
        let registry = Registry::new(broadcast::channel(16).0);
        let budget = WaitBudget::tracked(10, &registry);
        let gate = crate::gateway::freeze::Gate::default();
        let traffic = gate.request(crate::gateway::freeze::Scope::Client(
            crate::gateway::ClientId::Codex,
        ));
        traffic.waiting();
        budget.begin();
        budget.attach_traffic(&traffic);
        let identity = budget.deadline();
        tokio::time::advance(Duration::from_secs(3)).await;
        gate.set(true).unwrap();
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert!(!budget.expired(identity));
        assert!(tokio::time::timeout(Duration::from_secs(1), budget.wait())
            .await
            .is_err());
        let row = registry.view().pop().unwrap();
        assert_eq!(row.phase, "frozen");
        assert_eq!(row.elapsed_seconds, 3);
        assert_eq!(row.remaining_seconds, None);
        gate.set(false).unwrap();
        assert_eq!(budget.deadline(), identity);
        assert_eq!(registry.view()[0].remaining_seconds, Some(7));
        tokio::time::advance(Duration::from_secs(6)).await;
        assert!(!budget.expired(identity));
        budget.wait().await;
        assert!(budget.expired(identity));
        budget.complete(identity);

        budget.begin();
        budget.attach_traffic(&traffic);
        gate.set(true).unwrap();
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert!(registry.cancel(&registry.view()[0].request_id));
        budget.cancelled().await;
        assert!(registry.view().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn attempt_time_restarts_with_each_description_and_skips_frozen_time() {
        let registry = Registry::new(broadcast::channel(16).0);
        let budget = WaitBudget::tracked(600, &registry);
        let gate = crate::gateway::freeze::Gate::default();
        let traffic = gate.request(crate::gateway::freeze::Scope::Client(
            crate::gateway::ClientId::Codex,
        ));
        traffic.waiting();
        budget.begin();
        budget.attach_traffic(&traffic);
        let identity = budget.deadline();
        let times = || {
            let row = registry.view().pop().unwrap();
            (row.elapsed_seconds, row.attempt_seconds)
        };
        tokio::time::advance(Duration::from_secs(40)).await;
        assert_eq!(times(), (40, 40));

        budget.describe(Some("p1"), None, "headers");
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(times(), (45, 5));
        // Buffering and a compaction belong to the attempt they follow.
        budget.buffering();
        tokio::time::advance(Duration::from_secs(5)).await;
        budget.compaction_execution(identity, true);
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(times(), (55, 15));

        // Frozen time counts for neither, including what was frozen before
        // the attempt began.
        gate.set(true).unwrap();
        tokio::time::advance(Duration::from_secs(100)).await;
        gate.set(false).unwrap();
        budget.describe(None, None, "retry");
        tokio::time::advance(Duration::from_secs(2)).await;
        gate.set(true).unwrap();
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert_eq!(times(), (57, 2));
        gate.set(false).unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(times(), (58, 3));
        assert_eq!(
            serde_json::to_value(registry.view().pop().unwrap()).unwrap()["attemptSeconds"],
            3
        );
    }

    #[tokio::test(start_paused = true)]
    async fn compaction_can_be_silent_beyond_300_seconds_and_cancelled_without_resetting_budget() {
        let registry = Registry::new(broadcast::channel(16).0);
        let budget = WaitBudget::tracked(300, &registry);
        budget.begin();
        let identity = budget.deadline();
        let mut protocol = Protocol::new(true);
        protocol.attach_wait_budget(&budget);
        protocol.set_operation(crate::usage::model::Operation::Compaction);
        protocol.begin_compaction_execution();
        tokio::time::advance(Duration::from_secs(601)).await;
        assert!(!budget.expired(identity));
        assert!(tokio::time::timeout(Duration::from_secs(1), budget.wait())
            .await
            .is_err());
        protocol.value(&json!({"type":"response.output_text.delta","delta":"fixture"}));
        let row = registry.view().pop().unwrap();
        assert_eq!(row.remaining_seconds, None);
        assert_eq!(row.phase, "compaction");
        assert!(registry.cancel(&row.request_id));
        budget.cancelled().await;
        protocol.finish(None, crate::errors::code::CANCELLED);
        assert!(budget.is_cancelled(identity));
        budget.complete(identity);
        assert!(registry.view().is_empty());

        budget.begin();
        let identity = budget.deadline();
        budget.compaction_execution(identity, true);
        tokio::time::advance(Duration::from_secs(301)).await;
        budget.describe(None, None, "retry");
        assert_eq!(budget.deadline(), identity);
        assert!(budget.expired(identity));
        budget.wait().await;
    }

    #[tokio::test(start_paused = true)]
    async fn response_deadline_switches_only_after_dispatch_and_old_tail_cannot_end_new_compaction()
    {
        let budget = WaitBudget::new(300);
        budget.begin();
        let identity = budget.deadline();
        let response = budget
            .response(
                Instant::now() + Duration::from_secs(1),
                Silence::Compaction(None),
                async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    budget.compaction_execution(identity, true);
                    tokio::time::sleep(Duration::from_secs(601)).await;
                    42
                },
            )
            .await;
        assert_eq!(response.unwrap(), 42);
        budget.complete(identity);
        budget.begin();
        let current = budget.deadline();
        budget.compaction_execution(current, true);
        budget.compaction_execution(identity, false);
        budget.complete(identity);
        assert!(budget.executing_compaction());
        budget.complete(current);
        budget.begin();
        let mut ordinary = Protocol::new(true);
        ordinary.attach_wait_budget(&budget);
        let hints = crate::gateway::replay::RequestHints::from_value(json!({
            "input":[{"type":"compaction","encrypted_content":"fixture"},{"role":"user","content":"fixture"}]
        }), false).unwrap();
        ordinary.routing_evidence(Some(&hints));
        ordinary.begin_compaction_execution();
        assert!(!budget.executing_compaction());
        assert!(budget
            .response(
                Instant::now() + Duration::from_secs(1),
                Silence::Ordinary,
                std::future::pending::<()>()
            )
            .await
            .is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn dispatched_compaction_waits_its_own_limit_for_the_first_output() {
        let budget = WaitBudget::new(300);
        budget.begin();
        let identity = budget.deadline();
        let started = Instant::now();
        let limit = Silence::Compaction(Some(Duration::from_secs(400)));
        let response = budget
            .response(Instant::now() + Duration::from_secs(60), limit, async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                budget.compaction_execution(identity, true);
                std::future::pending::<()>().await
            })
            .await;
        assert!(response.is_err());
        // Counted from the dispatch, not from the ordinary deadline.
        assert_eq!(started.elapsed(), Duration::from_secs(410));
    }

    #[tokio::test(start_paused = true)]
    async fn a_flowing_stream_stalls_at_its_own_limit_or_the_callers_deadline() {
        let budget = WaitBudget::new(300);
        budget.begin();
        let stall = Silence::Flowing(Duration::from_secs(120));
        let started = Instant::now();
        let quiet = budget
            .response(
                started + Duration::from_secs(300),
                stall,
                std::future::pending::<()>(),
            )
            .await;
        assert_eq!(quiet, Err(Quiet::Stalled));
        assert_eq!(started.elapsed(), Duration::from_secs(120));

        let started = Instant::now();
        let quiet = budget
            .response(
                started + Duration::from_secs(30),
                stall,
                std::future::pending::<()>(),
            )
            .await;
        assert_eq!(quiet, Err(Quiet::Deadline));
        assert_eq!(started.elapsed(), Duration::from_secs(30));

        let data = budget
            .response(started + Duration::from_secs(300), stall, async {
                tokio::time::sleep(Duration::from_secs(119)).await;
                7
            })
            .await;
        assert_eq!(data, Ok(7));
    }

    #[tokio::test(start_paused = true)]
    async fn a_started_response_is_cut_short_while_its_endpoint_is_held() {
        let budget = WaitBudget::new(300);
        budget.begin();
        let links = Links::default();
        let limit = Duration::from_secs(60);
        let far = Duration::from_secs(400);
        // Without an endpoint to watch only the caller's deadline counts.
        let started = Instant::now();
        let quiet = budget
            .response(
                started + far,
                Silence::Started,
                std::future::pending::<()>(),
            )
            .await;
        assert_eq!(quiet, Err(Quiet::Deadline));
        assert_eq!(started.elapsed(), far);

        budget.watch_endpoint(Some((links.clone(), "fixture".into(), limit)));
        let after = |links: &Links, seconds, down| {
            let links = links.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(seconds)).await;
                links.set_down("fixture", down);
            })
        };
        let wait = |silence| {
            let budget = &budget;
            async move {
                let started = Instant::now();
                let quiet = budget
                    .response(started + far, silence, std::future::pending::<()>())
                    .await;
                (quiet, started.elapsed())
            }
        };

        // Held 25 seconds into the silence: over 60 seconds after it began.
        after(&links, 25, true);
        assert_eq!(wait(Silence::Started).await, (Err(Quiet::Held), limit));
        // Still held when the next wait begins.
        assert_eq!(wait(Silence::Started).await, (Err(Quiet::Held), limit));
        // The earlier of the two limits ends a flowing stream.
        assert_eq!(
            wait(Silence::Flowing(Duration::from_secs(120))).await,
            (Err(Quiet::Held), limit)
        );
        assert_eq!(
            wait(Silence::Flowing(Duration::from_secs(45))).await,
            (Err(Quiet::Stalled), Duration::from_secs(45))
        );
        // The hold ends 20 seconds in: the wait gets the limit again from there.
        after(&links, 20, false);
        assert_eq!(
            wait(Silence::Started).await,
            (Err(Quiet::Held), Duration::from_secs(80))
        );
        // Already silent for longer than the limit when the hold begins.
        after(&links, 90, true);
        assert_eq!(
            wait(Silence::Started).await,
            (Err(Quiet::Held), Duration::from_secs(90))
        );
        // The wait for a response head does not watch the endpoint.
        assert_eq!(wait(Silence::Ordinary).await, (Err(Quiet::Deadline), far));
        // Data that arrives in time is returned, held or not.
        let data = budget
            .response(Instant::now() + far, Silence::Started, async {
                tokio::time::sleep(Duration::from_secs(59)).await;
                7
            })
            .await;
        assert_eq!(data, Ok(7));

        budget.watch_endpoint(None);
        assert_eq!(wait(Silence::Started).await, (Err(Quiet::Deadline), far));
    }

    #[tokio::test(start_paused = true)]
    async fn a_receiving_request_reports_its_transfer_until_its_phase_changes() {
        let registry = Registry::new(broadcast::channel(16).0);
        let budget = WaitBudget::tracked(300, &registry);
        budget.begin();
        let identity = budget.deadline();
        budget.describe(Some("fixture"), None, "headers");
        let row = || {
            let row = registry.view().pop().unwrap();
            (row.received_bytes, row.bytes_per_second, row.silent_seconds)
        };
        assert_eq!(row(), (None, None, None));

        budget.progress(identity, 4096);
        tokio::time::advance(Duration::from_secs(1)).await;
        budget.progress(identity, 4096);
        assert_eq!(row(), (Some(8192), Some(8192), Some(0)));
        // Averaged over five seconds once the transfer is that old.
        tokio::time::advance(Duration::from_secs(4)).await;
        assert_eq!(row(), (Some(8192), Some(1638), Some(4)));
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(row(), (Some(8192), Some(0), Some(6)));
        // The tail of an earlier round is not this request's transfer.
        budget.progress(identity.map(|at| at + Duration::from_secs(1)), 1);
        assert_eq!(row().0, Some(8192));
        budget.progress(identity, 1024);
        assert_eq!(row(), (Some(9216), Some(205), Some(0)));

        let json = serde_json::to_value(registry.view().pop().unwrap()).unwrap();
        assert_eq!(
            (
                &json["receivedBytes"],
                &json["bytesPerSecond"],
                &json["silentSeconds"]
            ),
            (&json!(9216), &json!(205), &json!(0))
        );
        // A retry, a hold or the next attempt starts from nothing.
        budget.describe(Some("fixture"), None, "retry");
        assert_eq!(row(), (None, None, None));
        let json = serde_json::to_value(registry.view().pop().unwrap()).unwrap();
        assert!(json.get("receivedBytes").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeats_do_not_extend_shared_budget_and_effective_output_removes_deadline() {
        let budget = WaitBudget::new(300);
        budget.begin();
        let deadline = budget.deadline();
        let mut first = Protocol::new(true);
        first.attach_wait_budget(&budget);
        first.response(200, true, "identity");
        tokio::time::advance(Duration::from_secs(150)).await;
        first.feed(b": heartbeat\n\n");
        assert_eq!(budget.deadline(), deadline);
        first.finish(Some(503), crate::errors::code::HTTP);
        let mut retry = Protocol::new(true);
        retry.attach_wait_budget(&budget);
        tokio::time::advance(Duration::from_secs(149)).await;
        retry.value(
            &json!({"type":"response.created", "response":{"id":"fixture","status":"in_progress"}}),
        );
        assert_eq!(budget.deadline(), deadline);
        retry.value(&json!({"type":"response.output_text.delta", "delta":"fixture"}));
        assert!(budget.deadline().is_none());
        tokio::time::advance(Duration::from_secs(601)).await;
        assert!(!budget.expired(deadline));
        retry.value(
            &json!({"type":"response.completed", "response":{"id":"fixture","status":"completed"}}),
        );
        assert!(retry.succeeded());
    }

    #[tokio::test(start_paused = true)]
    async fn new_websocket_turn_gets_its_own_budget_and_old_tail_cannot_complete_it() {
        let budget = WaitBudget::new(300);
        budget.begin();
        let old = budget.deadline();
        budget.complete(old);
        tokio::time::advance(Duration::from_secs(700)).await;
        budget.begin();
        let current = budget.deadline();
        budget.complete(old);
        assert_eq!(budget.deadline(), current);
        tokio::time::advance(Duration::from_secs(300)).await;
        assert!(budget.expired(current));
        budget.wait().await;
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn cancel_is_per_request_and_old_ids_cannot_cancel_a_new_round() {
        let registry = Registry::new(broadcast::channel(16).0);
        let first = WaitBudget::tracked(300, &registry);
        let second = WaitBudget::tracked(300, &registry);
        first.begin();
        second.begin();
        first.describe(Some("fixture-one"), Some("fixture-model"), "headers");
        tokio::time::advance(Duration::from_secs(61)).await;
        let row = registry
            .view()
            .into_iter()
            .find(|p| p.provider_id.as_deref() == Some("fixture-one"))
            .unwrap();
        assert_eq!(
            (row.elapsed_seconds, row.remaining_seconds),
            (61, Some(239))
        );
        let deadline = first.deadline();
        assert!(registry.cancel(&row.request_id));
        first.cancelled().await;
        assert!(first.is_cancelled(deadline));
        assert!(second.deadline().is_some());
        assert_eq!(registry.view().len(), 1);
        first.acknowledge_cancel();
        first.begin();
        assert!(!registry.cancel(&row.request_id));
        assert_eq!(registry.view().len(), 2);
        drop(first);
        drop(second);
        assert!(registry.view().is_empty());
    }
    #[tokio::test(start_paused = true)]
    async fn output_and_expiry_reject_cancel_and_registries_are_isolated() {
        let a = Registry::new(broadcast::channel(16).0);
        let b = Registry::new(broadcast::channel(16).0);
        let budget = WaitBudget::tracked(300, &a);
        budget.begin();
        let id = a.view()[0].request_id.clone();
        assert!(!b.cancel(&id));
        budget.complete(budget.deadline());
        assert!(!a.cancel(&id));
        budget.begin();
        let id = a.view()[0].request_id.clone();
        tokio::time::advance(Duration::from_secs(300)).await;
        assert!(!a.cancel(&id));
    }
    #[test]
    fn a_cancel_tells_a_resend_from_an_ended_delivery() {
        let registry = Registry::new(broadcast::channel(16).0);
        let waiting = WaitBudget::tracked(300, &registry);
        waiting.begin();
        let id = registry.view()[0].request_id.clone();
        assert_eq!(registry.interrupt(&id), Some(true));
        assert_eq!(registry.interrupt(&id), None);
        let delivering = WaitBudget::tracked(300, &registry);
        delivering.begin();
        delivering.delivering();
        let id = registry.view()[0].request_id.clone();
        assert_eq!(registry.interrupt(&id), Some(false));
    }
    #[test]
    fn output_and_cancel_have_one_atomic_winner() {
        for _ in 0..24 {
            let registry = Registry::new(broadcast::channel(16).0);
            let budget = WaitBudget::tracked(300, &registry);
            budget.begin();
            let id = registry.view()[0].request_id.clone();
            let deadline = budget.deadline();
            std::thread::scope(|scope| {
                let task = scope.spawn(|| registry.cancel(&id));
                budget.complete(deadline);
                assert_eq!(task.join().unwrap(), budget.is_cancelled(deadline));
            });
            assert!(registry.view().is_empty());
        }
    }
    #[tokio::test]
    async fn cancelled_output_race_keeps_protocol_cancelled() {
        let registry = Registry::new(broadcast::channel(16).0);
        let budget = WaitBudget::tracked(300, &registry);
        budget.begin();
        let mut protocol = crate::gateway::protocol::Protocol::new(true);
        protocol.attach_wait_budget(&budget);
        let id = registry.view()[0].request_id.clone();
        assert!(registry.cancel(&id));
        protocol.value(
            &serde_json::json!({"type":"response.completed","response":{"id":"fixture","status":"completed"}}),
        );
        protocol.finish(Some(200), crate::errors::code::OK);
        assert_eq!(
            protocol.terminal(),
            Some(crate::gateway::protocol::Terminal::Cancelled)
        );
        assert!(!protocol.succeeded());
        assert!(!protocol.transport_failure());
        assert!(registry.view().is_empty());
    }
}
