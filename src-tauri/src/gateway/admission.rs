//! Request slots and bounded FIFO admission. Behavior informed by Sub2API
//! a3eb7ef3 concurrency_service / account scheduler; independently implemented in Rust.
use super::rpm::Ledger;
use super::{
    circuit::Circuit,
    health_settlement::Permits,
    model::{Provider, RetryClass},
    routing::Requirement,
    Route,
};
use parking_lot::Mutex;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{broadcast, Notify},
    time::Instant,
};

#[derive(Clone)]
pub struct Scheduler(Arc<Shared>);
/// Whether a provider can take a request right now, in the order admission
/// checks it. Admission, the failure preview and the failover evidence all
/// read this one rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Gate {
    Ready,
    /// The circuit is unavailable; seconds until it may recover.
    Health(u64),
    Capacity,
    Rpm(Duration),
    /// The provider rate-limited a request and is sent nothing for a while.
    Rest(Duration),
}
struct Shared {
    state: Mutex<State>,
    wake: Notify,
    events: broadcast::Sender<()>,
}
#[derive(Default)]
struct State {
    running: bool,
    epoch: u64,
    next: u64,
    limits: HashMap<String, u32>,
    order: HashMap<String, usize>,
    queued: std::collections::HashSet<String>,
    versions: HashMap<String, String>,
    rpm_limits: HashMap<String, u32>,
    models: HashMap<String, Option<Vec<String>>>,
    intake: HashMap<String, (bool, bool)>,
    active: HashMap<String, usize>,
    rpm: Ledger,
    waiting: VecDeque<(u64, Vec<String>)>,
    capacity_waits: HashMap<u64, Wait>,
    resets: HashMap<String, u64>,
    /// Endless-mode retry timing of each provider.
    paces: HashMap<String, Pace>,
    /// When each endless provider last rate-limited a request.
    limited: HashMap<String, Instant>,
    /// The share every retry gets instead of a random one.
    fixed_share: Option<f64>,
}
/// A request waiting out a delay outside the admission queue.
struct Wait {
    provider: String,
    until: Instant,
    kind: WaitKind,
    /// An endless retry, whose release time follows the provider's pace.
    hit: Option<Hit>,
}
/// The failure an endless retry waits out.
#[derive(Clone, Copy)]
struct Hit {
    at: Instant,
    class: RetryClass,
    /// The retry's own share, in [0, 1), of the stretch a wait may take. It
    /// is drawn once, so that a change of pace moves the retry no further.
    share: f64,
}
impl Wait {
    /// A risk-control or rate-limit retry, whose wait follows the load.
    fn paced(&self) -> bool {
        self.hit.is_some_and(|hit| hit.class != RetryClass::Normal)
    }
}
/// The longest a retry's wait is stretched, as a share of the wait.
const STRETCH: f64 = 0.2;
/// How an endless provider spaces its retries, in seconds for each class.
#[derive(Clone, Copy, PartialEq)]
struct Pace {
    risk: u64,
    limit: u64,
    normal: u64,
    intensity: u8,
    /// Seconds the provider is sent nothing after a rate limit; 0 for none.
    rest: u64,
}
impl Pace {
    fn of(provider: &Provider) -> Self {
        // A day at most, whatever the store holds: the wait is added to a clock.
        let seconds = |saved: u64| saved.clamp(1, 86_400);
        Self {
            risk: seconds(provider.upstream_retry_seconds),
            limit: seconds(provider.limit_retry_seconds()),
            normal: seconds(provider.normal_retry_seconds),
            intensity: provider.retry_intensity(),
            rest: provider.limit_rest_seconds().min(86_400),
        }
    }
    /// How much longer than computed a retry with random `share` waits: up
    /// to a fifth, so that retries do not return on a fixed beat. Never
    /// shorter, and intensity 0 keeps every wait exactly as set.
    fn stretch(self, share: f64) -> f64 {
        if self.intensity == 0 {
            1.0
        } else {
            1.0 + STRETCH * share
        }
    }
    /// When a retry with `share` may go out after the rate limit at `limited`.
    fn rested(self, limited: Instant, share: f64) -> Instant {
        limited + Duration::from_secs(self.rest).mul_f64(self.stretch(share))
    }
    fn base(self, class: RetryClass) -> u64 {
        match class {
            RetryClass::Risk => self.risk,
            RetryClass::Limit => self.limit,
            RetryClass::Normal => self.normal,
        }
    }
    /// The wait of a risk-control or rate-limit retry while `load` requests
    /// press on the provider, as a share of the class's base:
    ///
    /// - two ninths for every request past the second, kept between a fifth
    ///   and five thirds: up to three requests wait about a fifth, to notice
    ///   a recovery soon; the base falls between six and seven; ten or more
    ///   wait five thirds;
    /// - a fifteenth for every request once that is longer, past twenty-five,
    ///   so a crowd's retries never reach the provider closer together than
    ///   a fifteenth of the base.
    ///
    /// The intensity is the exponent in fifths: 0 keeps the base, 5 gives the
    /// shares above and 10 their squares. Never under a second.
    fn paced_delay(self, class: RetryClass, load: usize) -> Duration {
        let load = load.max(1) as f64;
        let reference = (2.0 * (load - 2.0) / 9.0)
            .clamp(0.2, 5.0 / 3.0)
            .max(load / 15.0);
        let factor = reference.powf(f64::from(self.intensity) / 5.0);
        Duration::from_secs_f64(self.base(class) as f64 * factor).max(Duration::from_secs(1))
    }
}
#[derive(Clone)]
pub struct CapacitySource {
    pub provider_id: String,
    pub reset_generation: u64,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum WaitKind {
    Capacity,
    ExistingCapacity,
    Websocket,
    Transient,
}
#[derive(serde::Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CapacityRetry {
    pub provider_id: String,
    pub retry_in: u64,
    pub existing_owner: bool,
}
pub struct Slot {
    scheduler: Scheduler,
    id: String,
}
/// An endless retry's place among its provider's retries.
pub(super) struct Retry {
    place: CapacityWait,
    source: CapacitySource,
    epoch: u64,
}

struct RpmReservation {
    scheduler: Scheduler,
    id: String,
    token: u64,
    sent: bool,
}

struct Waiting {
    scheduler: Scheduler,
    ticket: u64,
}
struct CapacityWait {
    scheduler: Scheduler,
    ticket: u64,
}
pub struct Admission {
    pub handoff_wait: Option<crate::usage::model::RoutingDecision>,
    pub route: Route,
    pub permits: Permits,
    pub slot: Slot,
    rpm: Option<RpmReservation>,
    pub reset_generation: u64,
}
#[derive(Debug, PartialEq)]
pub enum Rejected {
    Reconnect,
    Stopped,
    Unavailable,
    Full,
    Timeout,
    Cooling(u64),
    Model,
    Policy,
    RateLimited(u64),
}
pub struct Budget {
    pub traffic: Option<super::freeze::Ticket>,
    pub handoff_owner: Option<String>,
    pub capacity_owner: Option<String>,
    pub reconnect_from: Option<String>,
    remaining: Duration,
    protected_capacity: bool,
    /// The request waits for its providers without a queue deadline.
    endless: bool,
    fixed_provider: bool,
    intake: Option<(super::compaction::Intake, Option<String>)>,
}
impl Admission {
    /// Marks the request as sent; an unsent admission returns its RPM budget on drop.
    pub fn commit_rpm(&mut self) {
        if let Some(rpm) = self.rpm.as_mut() {
            rpm.sent = true;
        }
    }
}
impl Drop for RpmReservation {
    fn drop(&mut self) {
        if self.sent {
            return;
        }
        self.scheduler
            .0
            .state
            .lock()
            .rpm
            .refund(&self.id, self.token);
        self.scheduler.signal();
    }
}
impl Budget {
    /// A thread that follows its owner may only move to a handoff receiver.
    pub fn follow(&mut self, lease: Option<&super::compaction::Lease>) {
        if let Some(lease) = lease.filter(|lease| lease.follows_owner()) {
            self.handoff_owner = lease.handoff.then(|| lease.owner.clone()).flatten();
        }
    }

    pub(super) fn set_intake(&mut self, intake: super::compaction::Intake, owner: Option<String>) {
        self.intake = Some((intake, owner));
    }
    pub(super) fn search_fallback(&mut self) {
        self.fixed_provider = false;
        self.intake = None;
        self.handoff_owner = None;
        self.capacity_owner = None;
        self.reconnect_from = None;
    }
    pub fn protect_capacity(&mut self) {
        self.protected_capacity = true;
    }
    pub(super) fn set_endless(&mut self, endless: bool) {
        self.endless = endless;
    }
    /// Starts the queue wait over; what the request is bound to stays.
    pub(super) fn restart(&mut self, seconds: u64) {
        self.remaining = Duration::from_secs(seconds);
    }
    pub fn pin_provider(&mut self) {
        self.fixed_provider = true;
        self.capacity_owner = None;
        self.reconnect_from = None;
        self.intake = None;
    }
    pub fn new(seconds: u64) -> Self {
        Self {
            traffic: None,
            handoff_owner: None,
            capacity_owner: None,
            reconnect_from: None,
            remaining: Duration::from_secs(seconds),
            protected_capacity: false,
            endless: false,
            fixed_provider: false,
            intake: None,
        }
    }
}
impl Scheduler {
    pub fn new(events: broadcast::Sender<()>) -> Self {
        Self(Arc::new(Shared {
            state: Mutex::new(State {
                // Tests keep exact waits unless they set a share themselves.
                fixed_share: cfg!(test).then_some(0.0),
                ..State::default()
            }),
            wake: Notify::new(),
            events,
        }))
    }
    /// A provider removed from the configuration has no slot to give.
    fn available(s: &State, id: &str) -> bool {
        s.limits.get(id).is_some_and(|limit| {
            *limit == 0 || s.active.get(id).copied().unwrap_or(0) < *limit as usize
        })
    }

    pub fn configure(&self, providers: &[Provider], running: bool) {
        let mut s = self.0.state.lock();
        let changed: Vec<_> =
            s.versions
                .iter()
                .filter(|(id, version)| {
                    providers.iter().find(|p| &p.id == *id).is_none_or(|p| {
                        p.version != **version || p.queued != s.queued.contains(*id)
                    })
                })
                .map(|(id, _)| id.clone())
                .collect();
        for id in changed {
            *s.resets.entry(id).or_default() += 1;
        }
        s.versions = providers
            .iter()
            .map(|p| (p.id.clone(), p.version.clone()))
            .collect();
        s.rpm
            .retain_providers(|id| providers.iter().any(|p| p.id == id));
        if s.running != running {
            s.epoch += 1;
            s.limited.clear();
        }
        s.running = running;
        s.limits = providers
            .iter()
            .map(|p| (p.id.clone(), p.max_concurrency))
            .collect();
        s.rpm_limits = providers
            .iter()
            .map(|p| (p.id.clone(), p.max_rpm))
            .collect();
        s.models = providers
            .iter()
            .map(|p| (p.id.clone(), p.allowed_models.clone()))
            .collect();
        s.intake = providers
            .iter()
            .map(|p| {
                (
                    p.id.clone(),
                    (p.handoff_after_compaction, p.take_new_threads),
                )
            })
            .collect();
        s.order = providers
            .iter()
            .enumerate()
            .map(|(i, p)| (p.id.clone(), i))
            .collect();
        s.queued = providers
            .iter()
            .filter(|p| p.queued)
            .map(|p| p.id.clone())
            .collect();
        let paces: HashMap<_, _> = providers
            .iter()
            .map(|p| (p.id.clone(), Pace::of(p)))
            .collect();
        let repaced: Vec<_> = paces
            .iter()
            .filter(|(id, pace)| s.paces.get(*id).is_some_and(|old| old != *pace))
            .map(|(id, _)| id.clone())
            .collect();
        s.limited.retain(|id, _| paces.contains_key(id));
        s.paces = paces;
        let now = Instant::now();
        for id in repaced {
            Self::repace(&mut s, &id, now, true);
        }
        drop(s);
        self.signal();
    }
    /// When the provider's rest after its last rate limit is over.
    fn rested(s: &State, id: &str) -> Option<Instant> {
        Some(*s.limited.get(id)? + Duration::from_secs(s.paces.get(id)?.rest))
    }
    /// How much longer the provider is sent nothing.
    fn resting(s: &State, id: &str, now: Instant) -> Option<Duration> {
        Self::rested(s, id)
            .filter(|rested| *rested > now)
            .map(|rested| rested - now)
    }
    /// Requests pressing on the provider: the ones it is serving and the ones
    /// waiting out an endless retry. A provider that rejects more often has
    /// more of them waiting, so the count follows how often it pushes back.
    fn load(s: &State, id: &str) -> usize {
        let waiting = s
            .capacity_waits
            .values()
            .filter(|wait| wait.provider == id && wait.hit.is_some())
            .count();
        s.active.get(id).copied().unwrap_or(0) + waiting
    }
    /// When the retry of `hit` goes out.
    ///
    /// A risk-control or rate-limit retry waits its paced delay and then
    /// takes the first opening that is a share of that delay away from every
    /// such retry the provider has `scheduled` (in ascending order): a crowd
    /// does not return at once, and a short interval is not held behind a
    /// long one. Any other retry waits the normal interval.
    ///
    /// None goes out before the provider has rested from the rate limit at
    /// `limited`. The retry's own stretch lengthens its wait and its rest
    /// alike; intensity 0 keeps the plain intervals.
    fn release(
        pace: Pace,
        hit: Hit,
        load: usize,
        limited: Option<Instant>,
        now: Instant,
        scheduled: &[Instant],
    ) -> Instant {
        let stretch = pace.stretch(hit.share);
        let rested = limited.map(|limited| pace.rested(limited, hit.share));
        let rested = |release: Instant| rested.map_or(release, |rested| release.max(rested));
        if hit.class == RetryClass::Normal {
            return rested(hit.at + Duration::from_secs(pace.normal).mul_f64(stretch));
        }
        let delay = pace.paced_delay(hit.class, load).mul_f64(stretch);
        // A retry overdue under an edited pace still takes its turn.
        let mut release = rested((hit.at + delay).max(now));
        if pace.intensity == 0 {
            return release;
        }
        let gap = delay.div_f64(load.max(1) as f64);
        for other in scheduled {
            if *other >= release + gap {
                break;
            }
            if *other + gap > release {
                release = *other + gap;
            }
        }
        release
    }
    /// Recomputes when the endless retries waiting on `id` go out, in the
    /// order they were hit: `all` of them after a change of pace, or else
    /// only the ones due before the provider has rested from a rate limit.
    /// The others keep their time, and the moved ones their distance to it.
    fn repace(s: &mut State, id: &str, now: Instant, all: bool) {
        let Some(pace) = s.paces.get(id).copied() else {
            return;
        };
        let load = Self::load(s, id);
        let limited = s.limited.get(id).copied();
        let mut waits: Vec<_> = s
            .capacity_waits
            .iter_mut()
            .filter_map(|(ticket, wait)| {
                let hit = wait.hit.filter(|_| wait.provider == id)?;
                Some((hit, *ticket, wait))
            })
            .collect();
        waits.sort_by_key(|(hit, ticket, _)| (hit.at, *ticket));
        let moved = |hit: Hit, until: Instant| {
            all || limited.is_some_and(|limited| until < pace.rested(limited, hit.share))
        };
        let mut scheduled: Vec<_> = waits
            .iter()
            .filter(|(hit, _, wait)| wait.paced() && !moved(*hit, wait.until))
            .map(|(_, _, wait)| wait.until)
            .collect();
        scheduled.sort_unstable();
        for (hit, _, wait) in waits {
            if !moved(hit, wait.until) {
                continue;
            }
            wait.until = Self::release(pace, hit, load, limited, now, &scheduled);
            if wait.paced() {
                let place = scheduled.partition_point(|other| *other <= wait.until);
                scheduled.insert(place, wait.until);
            }
        }
    }
    /// Registers an endless retry on `source`'s provider and returns its place
    /// with the wait before it goes out. The caller still holds its slot, so
    /// the request itself counts towards the load. A retry the provider
    /// `limited` starts its rest over, for every retry already waiting too.
    pub(super) fn retry(
        &self,
        source: &CapacitySource,
        class: RetryClass,
        limited: bool,
    ) -> (Retry, Duration) {
        let mut s = self.0.state.lock();
        let now = Instant::now();
        let id = &source.provider_id;
        let pace = s.paces.get(id).copied().unwrap_or(Pace {
            risk: super::model::DEFAULT_UPSTREAM_RETRY_SECONDS,
            limit: super::model::DEFAULT_UPSTREAM_RETRY_SECONDS,
            normal: super::model::DEFAULT_NORMAL_RETRY_SECONDS,
            intensity: 0,
            rest: 0,
        });
        if limited && pace.rest > 0 {
            s.limited.insert(id.clone(), now);
            Self::repace(&mut s, id, now, false);
        }
        let hit = Hit {
            at: now,
            class,
            share: s.fixed_share.unwrap_or_else(random_share),
        };
        let load = Self::load(&s, id);
        let mut scheduled: Vec<_> = s
            .capacity_waits
            .values()
            .filter(|wait| wait.provider == *id && wait.paced())
            .map(|wait| wait.until)
            .collect();
        scheduled.sort_unstable();
        let until = Self::release(pace, hit, load, s.limited.get(id).copied(), now, &scheduled);
        s.next += 1;
        let ticket = s.next;
        s.capacity_waits.insert(
            ticket,
            Wait {
                provider: id.clone(),
                until,
                kind: WaitKind::Transient,
                hit: Some(hit),
            },
        );
        let epoch = s.epoch;
        drop(s);
        self.signal();
        (
            Retry {
                place: CapacityWait {
                    scheduler: self.clone(),
                    ticket,
                },
                source: source.clone(),
                epoch,
            },
            until.saturating_duration_since(now),
        )
    }
    /// Sends counted in the current window and seconds until the next one fits.
    pub fn rpm_status(&self, id: &str, limit: u32) -> (usize, u64, bool) {
        let mut s = self.0.state.lock();
        let used = if limit == 0 { 0 } else { s.rpm.used(id) };
        let gate = s.rpm.gate(id, limit);
        (used, gate.map_or(0, seconds), gate.is_some())
    }
    pub fn counts(&self) -> (HashMap<String, usize>, usize) {
        let s = self.0.state.lock();
        (s.active.clone(), s.waiting.len() + s.capacity_waits.len())
    }
    fn gate(s: &mut State, route: &Route, requirement: &Requirement) -> Gate {
        let id = &route.provider.id;
        let health = route.health_for(requirement);
        if !health.available {
            return Gate::Health(health.retry_in);
        }
        if !Self::available(s, id) {
            return Gate::Capacity;
        }
        let limit = s.rpm_limits.get(id).copied().unwrap_or_default();
        let rest = Self::resting(s, id, Instant::now());
        match s.rpm.gate(id, limit) {
            Some(delay) if rest.is_none_or(|rest| rest <= delay) => Gate::Rpm(delay),
            _ => rest.map_or(Gate::Ready, Gate::Rest),
        }
    }
    /// The gates of `routes` under one lock. It reserves nothing: a later
    /// admission checks again before it takes a slot.
    pub(super) fn peek(&self, routes: &[&Route], requirement: &Requirement) -> Vec<Gate> {
        let mut s = self.0.state.lock();
        routes
            .iter()
            .map(|route| Self::gate(&mut s, route, requirement))
            .collect()
    }
    pub fn capacity_retries(&self) -> Vec<CapacityRetry> {
        let mut retries = self.retries(WaitKind::Capacity);
        retries.extend(self.retries(WaitKind::ExistingCapacity));
        retries
    }
    pub fn websocket_retries(&self) -> Vec<CapacityRetry> {
        self.retries(WaitKind::Websocket)
    }
    pub fn transient_retries(&self) -> Vec<CapacityRetry> {
        self.retries(WaitKind::Transient)
    }
    fn retries(&self, kind: WaitKind) -> Vec<CapacityRetry> {
        let s = self.0.state.lock();
        let mut result: Vec<_> = s
            .capacity_waits
            .values()
            .filter(|wait| wait.kind == kind)
            .map(|wait| CapacityRetry {
                provider_id: wait.provider.clone(),
                existing_owner: kind == WaitKind::ExistingCapacity,
                retry_in: wait
                    .until
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .div_ceil(1000) as u64,
            })
            .collect();
        result.sort_by(|a, b| {
            a.provider_id
                .cmp(&b.provider_id)
                .then(a.retry_in.cmp(&b.retry_in))
        });
        result
    }
    pub async fn wait_capacity(
        &self,
        sources: &[CapacitySource],
        delay: Duration,
        max_waiting: usize,
    ) -> Result<(), Rejected> {
        self.wait_kind(sources, delay, max_waiting, WaitKind::Capacity)
            .await
    }
    pub fn capacity_source(&self, id: &str) -> CapacitySource {
        CapacitySource {
            provider_id: id.into(),
            reset_generation: self.0.state.lock().resets.get(id).copied().unwrap_or(0),
        }
    }
    pub async fn wait_owner_capacity(
        &self,
        sources: &[CapacitySource],
        delay: Duration,
        max_waiting: usize,
    ) -> Result<(), Rejected> {
        self.wait_kind(sources, delay, max_waiting, WaitKind::ExistingCapacity)
            .await
    }
    pub async fn wait_transient(
        &self,
        source: CapacitySource,
        delay: Duration,
        max_waiting: usize,
    ) -> Result<(), Rejected> {
        self.wait_kind(&[source], delay, max_waiting, WaitKind::Transient)
            .await
    }
    pub async fn wait_websocket(
        &self,
        id: &str,
        delay: Duration,
        max_waiting: usize,
    ) -> Result<(), Rejected> {
        let generation = self.0.state.lock().resets.get(id).copied().unwrap_or(0);
        self.wait_kind(
            &[CapacitySource {
                provider_id: id.into(),
                reset_generation: generation,
            }],
            delay,
            max_waiting,
            WaitKind::Websocket,
        )
        .await
    }
    async fn wait_kind(
        &self,
        sources: &[CapacitySource],
        delay: Duration,
        max_waiting: usize,
        kind: WaitKind,
    ) -> Result<(), Rejected> {
        let until = Instant::now() + delay;
        let (ticket, epoch) = {
            let mut s = self.0.state.lock();
            if !s.running {
                return Err(Rejected::Stopped);
            }
            if Self::was_reset(&s, sources) {
                return Ok(());
            }
            if s.waiting.len() + s.capacity_waits.len() >= max_waiting {
                return Err(Rejected::Full);
            }
            s.next += 1;
            let ticket = s.next;
            s.capacity_waits.insert(
                ticket,
                Wait {
                    provider: sources
                        .last()
                        .map(|s| s.provider_id.clone())
                        .unwrap_or_default(),
                    until,
                    kind,
                    hit: None,
                },
            );
            (ticket, s.epoch)
        };
        let _wait = CapacityWait {
            scheduler: self.clone(),
            ticket,
        };
        self.signal();
        loop {
            let notified = self.0.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let s = self.0.state.lock();
                if !s.running || s.epoch != epoch {
                    return Err(Rejected::Stopped);
                }
                if Self::was_reset(&s, sources) {
                    return Ok(());
                }
            }
            tokio::select! {
                _ = notified => {},
                _ = tokio::time::sleep_until(until) => return Ok(()),
            }
        }
    }
    pub(super) fn capacity_current(&self, sources: &[CapacitySource]) -> bool {
        let s = self.0.state.lock();
        s.running && !Self::was_reset(&s, sources)
    }
    fn was_reset(state: &State, sources: &[CapacitySource]) -> bool {
        sources.iter().any(|source| {
            state.resets.get(&source.provider_id).copied().unwrap_or(0) != source.reset_generation
        })
    }
    pub fn reset_provider(&self, id: &str, circuits: &[Circuit]) {
        // Same lock order as admission: slot state, then circuit. Capture the
        // reset generation with each reservation so a reset during an upstream
        // attempt cannot be lost before that attempt registers its wait.
        let mut state = self.0.state.lock();
        for circuit in circuits {
            circuit.reset();
        }
        state.limited.remove(id);
        *state.resets.entry(id.to_owned()).or_default() += 1;
        drop(state);
        self.signal();
    }
    pub fn signal(&self) {
        self.0.wake.notify_waiters();
        let _ = self.0.events.send(());
    }
    pub async fn acquire_for_immediate(
        &self,
        routes: &[Route],
        manual: bool,
        max_waiting: usize,
        budget: &mut Budget,
        requirement: &Requirement,
        immediate: bool,
    ) -> Result<Admission, Rejected> {
        let started = Instant::now();
        if let Some(traffic) = &budget.traffic {
            traffic.waiting();
        }
        let paused_before = budget
            .traffic
            .as_ref()
            .map_or(Duration::ZERO, |t| t.paused_for());
        let mut paused = Duration::ZERO;
        let mut freeze_changes = budget.traffic.as_ref().map(|t| t.subscribe());
        let epoch = self.0.state.lock().epoch;
        let mut waiting: Option<Waiting> = None;
        let mut deadline = tokio::time::Instant::now() + budget.remaining;
        let mut cooling;
        let mut rate_retry;
        let result = 'admission: loop {
            if let Some(traffic) = &budget.traffic {
                let next = traffic.paused_for().saturating_sub(paused_before);
                deadline += next.saturating_sub(paused);
                paused = next;
            }
            let next_wake;
            let notified = self.0.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if budget.traffic.as_ref().is_some_and(|t| t.paused()) {
                {
                    let mut s = self.0.state.lock();
                    if !s.running || s.epoch != epoch {
                        break Err(Rejected::Stopped);
                    }
                    if waiting.is_none() {
                        if s.waiting.len() + s.capacity_waits.len() >= max_waiting {
                            break Err(Rejected::Full);
                        }
                        s.next += 1;
                        let ticket = s.next;
                        s.waiting.push_back((
                            ticket,
                            routes.iter().map(|r| r.provider.id.clone()).collect(),
                        ));
                        waiting = Some(Waiting {
                            scheduler: self.clone(),
                            ticket,
                        });
                        let _ = self.0.events.send(());
                    }
                }
                tokio::select! {
                    _ = notified => {},
                    _ = super::freeze::changed(&mut freeze_changes) => {},
                }
                continue;
            }
            {
                let mut s = self.0.state.lock();
                if !s.running || s.epoch != epoch {
                    break Err(Rejected::Stopped);
                }
                let matching: Vec<_> = routes
                    .iter()
                    .filter(|r| {
                        s.limits.contains_key(&r.provider.id)
                            && s.versions.get(&r.provider.id) == Some(&r.provider.version)
                    })
                    .filter(|r| {
                        budget.fixed_provider || manual || s.queued.contains(&r.provider.id)
                    })
                    .collect();
                if matching.is_empty() {
                    break Err(Rejected::Unavailable);
                }
                let matching: Vec<_> = matching
                    .into_iter()
                    .filter(|r| {
                        requirement.allows(s.models.get(&r.provider.id).and_then(|m| m.as_deref()))
                    })
                    .collect();
                if matching.is_empty() {
                    break Err(Rejected::Model);
                }
                let mut eligible = matching;
                if let Some((intake, owner)) = &budget.intake {
                    eligible.retain(|r| {
                        owner.as_deref() == Some(&r.provider.id)
                            || s.intake.get(&r.provider.id).is_some_and(|(compact, new)| {
                                super::compaction::accepts_flags(*compact, *new, *intake)
                            })
                    });
                    if eligible.is_empty() {
                        break Err(Rejected::Policy);
                    }
                }
                if let Some(owner) = budget
                    .handoff_owner
                    .as_ref()
                    .filter(|_| !budget.fixed_provider)
                {
                    eligible.retain(|r| {
                        r.provider.id == *owner
                            || (s.order.get(&r.provider.id) < s.order.get(owner)
                                && s.intake
                                    .get(&r.provider.id)
                                    .is_some_and(|(compact, _)| *compact))
                    });
                    if eligible.is_empty() {
                        break Err(Rejected::Policy);
                    }
                }
                if (budget.protected_capacity || budget.handoff_owner.is_some())
                    && !budget.fixed_provider
                    && !manual
                {
                    eligible.sort_by_key(|r| {
                        let preferred =
                            budget.intake.as_ref().is_some_and(|(intake, _)| {
                                *intake == super::compaction::Intake::NewThread
                            }) && s.intake.get(&r.provider.id).is_some_and(|(_, new)| *new);
                        (
                            !preferred,
                            s.order.get(&r.provider.id).copied().unwrap_or(usize::MAX),
                        )
                    });
                }
                if let Some(owner) = &budget.capacity_owner {
                    eligible.retain(|r| {
                        r.provider.id == *owner
                            || s.order
                                .get(&r.provider.id)
                                .zip(s.order.get(owner))
                                .is_some_and(|(candidate, owner)| candidate > owner)
                    });
                    let Some(route) = eligible.iter().find(|r| r.provider.id == *owner) else {
                        break Err(Rejected::Unavailable);
                    };
                    let rpm_limit = s.rpm_limits.get(owner).copied().unwrap_or_default();
                    let rpm_blocked = s.rpm.gate(owner, rpm_limit).is_some()
                        || Self::resting(&s, owner, Instant::now()).is_some();
                    if !route.health_for(requirement).available
                        || rpm_blocked
                        || Self::available(&s, owner)
                    {
                        eligible.retain(|r| r.provider.id == *owner);
                    }
                }
                let mut any_ready = false;
                let now = Instant::now();
                let rested = eligible
                    .iter()
                    .filter_map(|r| Self::rested(&s, &r.provider.id))
                    .filter(|rested| *rested > now)
                    .min();
                next_wake = eligible
                    .iter()
                    .filter_map(|route| {
                        let health = route.health_for(requirement);
                        (health.retry_in > 0).then(|| now + Duration::from_secs(health.retry_in))
                    })
                    .chain(
                        eligible
                            .iter()
                            .filter_map(|r| s.rpm.next_change(&r.provider.id)),
                    )
                    .chain(rested)
                    .min();
                let mut retry_in = u64::MAX;
                rate_retry = None;
                let ticket = waiting.as_ref().map(|w| w.ticket);
                if let Some(ticket) = ticket {
                    if let Some(entry) = s.waiting.iter_mut().find(|(id, _)| *id == ticket) {
                        entry.1 = eligible
                            .iter()
                            .filter(|r| r.health_for(requirement).available)
                            .map(|r| r.provider.id.clone())
                            .collect();
                    }
                }
                let mut accepted = None;
                let mut capacity_blocked = false;
                let mut intake_skip = None;
                let first_turn = budget
                    .intake
                    .as_ref()
                    .is_some_and(|(intake, _)| *intake == super::compaction::Intake::NewThread);
                for route in &eligible {
                    let id = &route.provider.id;
                    let gate = Self::gate(&mut s, route, requirement);
                    if let Gate::Health(recovers_in) = gate {
                        if first_turn && route.provider.take_new_threads {
                            intake_skip = Some(if route.provider_circuit.health().available {
                                crate::usage::model::RoutingDecision::NewThreadModelCooling
                            } else {
                                crate::usage::model::RoutingDecision::NewThreadProviderUnavailable
                            });
                        }
                        retry_in = retry_in.min(recovers_in.max(1));
                        continue;
                    }
                    any_ready = true;
                    if gate == Gate::Capacity {
                        if first_turn && route.provider.take_new_threads {
                            intake_skip =
                                Some(crate::usage::model::RoutingDecision::NewThreadCapacityFull);
                        }
                        capacity_blocked |= budget
                            .handoff_owner
                            .as_ref()
                            .is_some_and(|owner| owner != id);
                        continue;
                    }
                    // A waiter pinned to another provider never blocks this provider.
                    if s.waiting
                        .iter()
                        .take_while(|(id, _)| Some(*id) != ticket)
                        .any(|(_, ids)| ids.contains(id))
                    {
                        // A FIFO follower also waits for this provider's RPM
                        // budget. It must not time out before the head waiter
                        // gets a chance to send when the minute rolls forward.
                        if let Gate::Rpm(delay) | Gate::Rest(delay) = gate {
                            rate_retry = Some(rate_retry.unwrap_or(0).max(seconds(delay)));
                        }
                        if first_turn && route.provider.take_new_threads {
                            intake_skip =
                                Some(crate::usage::model::RoutingDecision::NewThreadQueueWaiting);
                        }
                        continue;
                    }
                    if let Gate::Rpm(delay) | Gate::Rest(delay) = gate {
                        rate_retry = Some(seconds(delay));
                        break;
                    }
                    if budget
                        .reconnect_from
                        .as_ref()
                        .is_some_and(|current| current != id)
                    {
                        // Re-enter connection selection before reserving a
                        // slot or RPM; the next admission rechecks all gates.
                        break 'admission Err(Rejected::Reconnect);
                    }
                    if let Some(permits) = Permits::acquire_for(route, requirement) {
                        if budget.traffic.as_ref().is_some_and(|t| !t.dispatch()) {
                            drop(permits);
                            continue 'admission;
                        }
                        let limited = s.rpm_limits.get(id).is_some_and(|limit| *limit > 0);
                        let rpm = limited.then(|| RpmReservation {
                            scheduler: self.clone(),
                            id: id.clone(),
                            token: s.rpm.charge(id),
                            sent: false,
                        });
                        *s.active.entry(id.clone()).or_default() += 1;
                        accepted = Some(Admission {
                            handoff_wait: (capacity_blocked
                                && budget.handoff_owner.as_deref() == Some(id))
                            .then_some(crate::usage::model::RoutingDecision::ReceiverCapacityFull)
                            .or(intake_skip),
                            route: (**route).clone(),
                            permits,
                            rpm,
                            reset_generation: s.resets.get(id).copied().unwrap_or(0),
                            slot: Slot {
                                scheduler: self.clone(),
                                id: id.clone(),
                            },
                        });
                        break;
                    }
                }
                cooling = (!any_ready).then_some(if retry_in == u64::MAX { 1 } else { retry_in });
                if let Some(admission) = accepted {
                    break Ok(admission);
                }
                if let Some(retry) = rate_retry {
                    if immediate {
                        break Err(Rejected::RateLimited(retry));
                    }
                }
                if immediate {
                    break Err(cooling.map_or(Rejected::Timeout, Rejected::Cooling));
                }
                if waiting.is_none() {
                    if s.waiting.len() + s.capacity_waits.len() >= max_waiting {
                        break Err(Rejected::Full);
                    }
                    s.next += 1;
                    let ticket = s.next;
                    s.waiting.push_back((
                        ticket,
                        eligible
                            .iter()
                            .filter(|r| r.health_for(requirement).available)
                            .map(|r| r.provider.id.clone())
                            .collect(),
                    ));
                    waiting = Some(Waiting {
                        scheduler: self.clone(),
                        ticket,
                    });
                    let _ = self.0.events.send(());
                }
            }
            tokio::select! {
                biased;
                _ = super::freeze::changed(&mut freeze_changes) => {},
                _ = notified => {},
                // RPM waits share the request's waiting budget with every other wait.
                _ = tokio::time::sleep_until(deadline), if !budget.protected_capacity && !budget.endless => break Err(match (rate_retry, cooling) {
                    (Some(retry), _) => Rejected::RateLimited(retry),
                    (None, Some(retry)) => Rejected::Cooling(retry),
                    (None, None) => Rejected::Timeout,
                }),
                _ = async {
                    if let Some(at) = next_wake { tokio::time::sleep_until(at).await; }
                    else { std::future::pending::<()>().await; }
                } => {},
            }
        };
        if waiting.is_some() {
            budget.remaining = budget
                .remaining
                .saturating_sub(started.elapsed().saturating_sub(paused));
        }
        drop(waiting);
        self.signal();
        result
    }
}
impl Retry {
    /// Sends the retry at once.
    pub(super) fn release(&self) {
        let scheduler = &self.place.scheduler;
        if let Some(wait) = scheduler
            .0
            .state
            .lock()
            .capacity_waits
            .get_mut(&self.place.ticket)
        {
            // A later change of pace or a rest must not push it back.
            wait.hit = None;
            wait.until = Instant::now();
        }
        scheduler.signal();
    }
    /// Waits until the retry may go out. The release time is read again on
    /// every wake, so an edited interval applies to a retry already waiting.
    pub(super) async fn wait(self) -> Result<(), Rejected> {
        let shared = &self.place.scheduler.0;
        loop {
            let notified = shared.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let until = {
                let s = shared.state.lock();
                if !s.running || s.epoch != self.epoch {
                    return Err(Rejected::Stopped);
                }
                if Scheduler::was_reset(&s, std::slice::from_ref(&self.source)) {
                    return Ok(());
                }
                match s.capacity_waits.get(&self.place.ticket) {
                    Some(wait) if wait.until > Instant::now() => wait.until,
                    _ => return Ok(()),
                }
            };
            tokio::select! {
                _ = notified => {},
                _ = tokio::time::sleep_until(until) => {},
            }
        }
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        let mut s = self.scheduler.0.state.lock();
        if let Some(n) = s.active.get_mut(&self.id) {
            *n = n.saturating_sub(1);
        }
        drop(s);
        self.scheduler.signal();
    }
}
impl Drop for Waiting {
    fn drop(&mut self) {
        self.scheduler
            .0
            .state
            .lock()
            .waiting
            .retain(|(id, _)| *id != self.ticket);
        self.scheduler.signal();
    }
}
impl Drop for CapacityWait {
    fn drop(&mut self) {
        self.scheduler
            .0
            .state
            .lock()
            .capacity_waits
            .remove(&self.ticket);
        self.scheduler.signal();
    }
}

/// Whole seconds a caller should wait, never less than one.
fn seconds(delay: Duration) -> u64 {
    delay.as_millis().div_ceil(1000).max(1) as u64
}
/// A fraction in [0, 1) from the 48 random bits a version 4 UUID leads with.
fn random_share() -> f64 {
    (uuid::Uuid::new_v4().as_u128() >> 80) as f64 / (1u64 << 48) as f64
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;

#[cfg(test)]
mod test_support {
    use super::*;
    impl Scheduler {
        /// The share every retry registered from now on takes of its stretch;
        /// `None` draws one at random, as outside the tests.
        pub fn set_share(&self, share: Option<f64>) {
            self.0.state.lock().fixed_share = share;
        }
        pub async fn acquire(
            &self,
            routes: &[Route],
            manual: bool,
            max_waiting: usize,
            budget: &mut Budget,
        ) -> Result<Admission, Rejected> {
            self.acquire_for(routes, manual, max_waiting, budget, &Requirement::Resource)
                .await
        }
        pub async fn acquire_for(
            &self,
            routes: &[Route],
            manual: bool,
            max_waiting: usize,
            budget: &mut Budget,
            requirement: &Requirement,
        ) -> Result<Admission, Rejected> {
            self.acquire_for_immediate(routes, manual, max_waiting, budget, requirement, false)
                .await
        }
    }
}
