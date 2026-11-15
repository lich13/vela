//! Shared dispatch control. Only live requests and in-memory recovery times are kept.
use super::ClientId;
use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::{sync::watch, time::Instant};

const CLIENTS: [ClientId; 4] = [
    ClientId::Codex,
    ClientId::Claude,
    ClientId::Grok,
    ClientId::Dsh,
];
#[derive(Clone, Copy, Debug)]
pub enum Scope {
    Client(ClientId),
    Antigravity,
}
impl Scope {
    fn index(self) -> usize {
        match self {
            Self::Client(ClientId::Codex) => 0,
            Self::Client(ClientId::Claude) => 1,
            Self::Client(ClientId::Grok) => 2,
            Self::Client(ClientId::Dsh) => 3,
            Self::Antigravity => 4,
        }
    }
}
#[derive(Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub frozen: bool,
    pub draining: usize,
    pub receiving: usize,
    pub waiting: usize,
    pub headers: usize,
    pub streaming: usize,
    pub finishing: usize,
    pub queued: usize,
}
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientView {
    pub client_id: ClientId,
    #[serde(flatten)]
    pub status: Status,
    pub resume_at: Option<i64>,
}
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    #[serde(flatten)]
    pub total: Status,
    pub partial: bool,
    pub clients: Vec<ClientView>,
}
impl std::ops::Deref for View {
    type Target = Status;
    fn deref(&self) -> &Status {
        &self.total
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Receiving,
    Waiting,
    Headers,
    Streaming,
    Finishing,
    Done,
}
struct Request {
    scope: usize,
    phase: Phase,
    paused: Duration,
    paused_since: Option<Instant>,
}
impl Request {
    fn paused_for(&self) -> Duration {
        self.paused + self.paused_since.map_or(Duration::ZERO, |at| at.elapsed())
    }
    fn resume_clock(&mut self) {
        self.paused = self.paused_for();
        self.paused_since = None;
    }
}
#[derive(Default)]
struct Frozen {
    frozen: bool,
    resume_at: Option<i64>,
}
#[derive(Default)]
struct State {
    scopes: [Frozen; 5],
    next: u64,
    requests: BTreeMap<u64, Request>,
}
impl State {
    fn statuses(&self) -> [Status; 5] {
        let mut views = std::array::from_fn(|i| Status {
            frozen: self.scopes[i].frozen,
            ..Default::default()
        });
        for request in self.requests.values() {
            let v = &mut views[request.scope];
            match request.phase {
                Phase::Receiving => v.receiving += 1,
                Phase::Waiting if v.frozen => v.queued += 1,
                Phase::Waiting => v.waiting += 1,
                Phase::Headers => v.headers += 1,
                Phase::Streaming => v.streaming += 1,
                Phase::Finishing => v.finishing += 1,
                Phase::Done => {}
            }
        }
        for v in &mut views {
            v.draining = v.receiving + v.headers + v.streaming + v.finishing;
        }
        views
    }
    fn set_scope(&mut self, scope: usize, frozen: bool) -> bool {
        if self.scopes[scope].frozen == frozen {
            return false;
        }
        self.scopes[scope].frozen = frozen;
        if !frozen {
            self.scopes[scope].resume_at = None;
        }
        for r in self.requests.values_mut().filter(|r| r.scope == scope) {
            if frozen && r.phase == Phase::Waiting {
                r.paused_since = Some(Instant::now());
            } else if !frozen {
                r.resume_clock();
            }
        }
        true
    }
}
struct Shared {
    state: Mutex<State>,
    changes: watch::Sender<()>,
    control_changes: watch::Sender<()>,
}
#[derive(Clone)]
pub struct Gate(Arc<Shared>);
impl Default for Gate {
    fn default() -> Self {
        Self(Arc::new(Shared {
            state: Mutex::default(),
            changes: watch::channel(()).0,
            control_changes: watch::channel(()).0,
        }))
    }
}
impl Gate {
    fn view_locked(s: &State) -> View {
        let statuses = s.statuses();
        let mut total = Status {
            frozen: statuses.iter().all(|v| v.frozen),
            ..Default::default()
        };
        for v in &statuses {
            total.draining += v.draining;
            total.receiving += v.receiving;
            total.waiting += v.waiting;
            total.headers += v.headers;
            total.streaming += v.streaming;
            total.finishing += v.finishing;
            total.queued += v.queued;
        }
        View {
            partial: !total.frozen && statuses.iter().any(|v| v.frozen),
            total,
            clients: CLIENTS
                .into_iter()
                .enumerate()
                .map(|(i, client_id)| ClientView {
                    client_id,
                    status: statuses[i].clone(),
                    resume_at: s.scopes[i].resume_at,
                })
                .collect(),
        }
    }
    pub fn view(&self) -> View {
        Self::view_locked(&self.0.state.lock())
    }
    pub fn is_frozen(&self, client: ClientId) -> bool {
        self.0.state.lock().scopes[Scope::Client(client).index()].frozen
    }
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.0.changes.subscribe()
    }
    pub fn subscribe_control(&self) -> watch::Receiver<()> {
        self.0.control_changes.subscribe()
    }
    pub fn set(&self, frozen: bool) -> crate::storage::Result<View> {
        self.set_selected(None, frozen)
    }
    pub fn set_selected(
        &self,
        client: Option<ClientId>,
        frozen: bool,
    ) -> crate::storage::Result<View> {
        let mut s = self.0.state.lock();
        let selected = |i: usize| client.is_none_or(|c| Scope::Client(c).index() == i);
        if !frozen
            && s.statuses()
                .iter()
                .enumerate()
                .any(|(i, v)| selected(i) && v.frozen && v.draining != 0)
        {
            return Err(crate::usage::model::failure("等待收尾完成后才能解冻"));
        }
        let mut changed = false;
        for i in 0..s.scopes.len() {
            if selected(i) {
                changed |= s.set_scope(i, frozen);
            }
        }
        if changed {
            self.0.control_changes.send_replace(());
            self.0.changes.send_replace(());
        }
        Ok(Self::view_locked(&s))
    }
    pub fn schedule(
        &self,
        client: ClientId,
        resume_at: Option<i64>,
        now: i64,
    ) -> crate::storage::Result<View> {
        let mut s = self.0.state.lock();
        let i = Scope::Client(client).index();
        if let Some(at) = resume_at {
            let status = &s.statuses()[i];
            if !status.frozen || status.draining != 0 {
                return Err(crate::usage::model::failure("请在冻结完成后设置恢复时间"));
            }
            if at <= now {
                return Err(crate::usage::model::failure("恢复时间应晚于当前时间"));
            }
        }
        if s.scopes[i].resume_at != resume_at {
            s.scopes[i].resume_at = resume_at;
            self.0.changes.send_replace(());
        }
        Ok(Self::view_locked(&s))
    }
    pub fn next_resume(&self) -> Option<i64> {
        let s = self.0.state.lock();
        if !s.scopes.iter().any(|v| v.resume_at.is_some()) {
            return None;
        }
        let statuses = s.statuses();
        s.scopes
            .iter()
            .enumerate()
            .filter(|(i, _)| statuses[*i].draining == 0)
            .filter_map(|(_, v)| v.resume_at)
            .min()
    }
    pub fn resume_due(&self, now: i64) {
        let mut s = self.0.state.lock();
        if !s
            .scopes
            .iter()
            .any(|v| v.resume_at.is_some_and(|at| at <= now))
        {
            return;
        }
        let statuses = s.statuses();
        let mut changed = false;
        for (i, status) in statuses.iter().enumerate() {
            if status.draining == 0 && s.scopes[i].resume_at.is_some_and(|at| at <= now) {
                changed |= s.set_scope(i, false);
            }
        }
        if changed {
            self.0.control_changes.send_replace(());
            self.0.changes.send_replace(());
        }
    }
    pub fn request(&self, scope: Scope) -> Ticket {
        let mut s = self.0.state.lock();
        s.next += 1;
        let id = s.next;
        s.requests.insert(
            id,
            Request {
                scope: scope.index(),
                phase: Phase::Receiving,
                paused: Duration::ZERO,
                paused_since: None,
            },
        );
        self.0.changes.send_replace(());
        Ticket(Arc::new(Entry {
            gate: self.clone(),
            id,
        }))
    }
}
struct Entry {
    gate: Gate,
    id: u64,
}
impl Drop for Entry {
    fn drop(&mut self) {
        self.gate.0.state.lock().requests.remove(&self.id);
        self.gate.0.changes.send_replace(());
    }
}
#[derive(Clone)]
pub struct Ticket(Arc<Entry>);
pub async fn changed(receiver: &mut Option<watch::Receiver<()>>) {
    if let Some(receiver) = receiver {
        let _ = receiver.changed().await;
    } else {
        std::future::pending::<()>().await;
    }
}
impl Ticket {
    fn phase(&self, phase: Phase) {
        let mut s = self.0.gate.0.state.lock();
        let scopes = s.scopes.each_ref().map(|v| v.frozen);
        if let Some(r) = s.requests.get_mut(&self.0.id) {
            if r.phase == phase || r.phase == Phase::Done {
                return;
            }
            r.resume_clock();
            r.phase = phase;
            if scopes[r.scope] && phase == Phase::Waiting {
                r.paused_since = Some(Instant::now());
            }
            self.0.gate.0.changes.send_replace(());
        }
    }
    pub fn waiting(&self) {
        self.phase(Phase::Waiting);
    }
    pub fn streaming(&self) {
        self.phase(Phase::Streaming);
    }
    pub fn finishing(&self) {
        self.phase(Phase::Finishing);
    }
    pub fn finish(&self) {
        self.phase(Phase::Done);
    }
    pub fn paused(&self) -> bool {
        let s = self.0.gate.0.state.lock();
        s.requests
            .get(&self.0.id)
            .is_some_and(|r| s.scopes[r.scope].frozen && r.phase == Phase::Waiting)
    }
    pub fn paused_for(&self) -> Duration {
        self.0
            .gate
            .0
            .state
            .lock()
            .requests
            .get(&self.0.id)
            .map_or(Duration::ZERO, Request::paused_for)
    }
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.0.gate.subscribe()
    }
    /// Admission and freeze edits share the same lock; only already granted work drains.
    pub fn dispatch(&self) -> bool {
        let mut s = self.0.gate.0.state.lock();
        let Some(scope) = s.requests.get(&self.0.id).map(|r| r.scope) else {
            return false;
        };
        if s.scopes[scope].frozen {
            return false;
        }
        let r = s.requests.get_mut(&self.0.id).unwrap();
        r.resume_clock();
        r.phase = Phase::Headers;
        self.0.gate.0.changes.send_replace(());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn platform_freeze_and_timed_resume_are_independent_and_global_keeps_vertex() {
        let gate = Gate::default();
        let mut tickets = Vec::new();
        for client in CLIENTS {
            let ticket = gate.request(Scope::Client(client));
            ticket.waiting();
            tickets.push(ticket);
        }
        let vertex = gate.request(Scope::Antigravity);
        vertex.waiting();
        gate.set_selected(Some(ClientId::Codex), true).unwrap();
        assert!(gate.view().partial);
        assert!(!gate.view().frozen);
        assert!(!tickets[0].dispatch());
        for ticket in &tickets[1..] {
            assert!(ticket.dispatch());
            ticket.finish();
        }
        assert!(vertex.dispatch());
        vertex.finish();
        gate.schedule(ClientId::Codex, Some(60_000), 0).unwrap();
        gate.set(true).unwrap();
        assert!(gate.view().frozen);
        assert_eq!(gate.next_resume(), Some(60_000));
        assert_eq!(gate.view().clients[0].resume_at, Some(60_000));
        gate.schedule(ClientId::Claude, Some(120_000), 0).unwrap();
        gate.schedule(ClientId::Claude, Some(180_000), 0).unwrap();
        gate.schedule(ClientId::Claude, None, 0).unwrap();
        tokio::time::advance(Duration::from_secs(90)).await;
        gate.resume_due(90_000);
        assert!(!gate.is_frozen(ClientId::Codex));
        assert!(gate.is_frozen(ClientId::Claude));
        assert_eq!(tickets[0].paused_for(), Duration::from_secs(90));
        assert!(tickets[0].dispatch());
        tickets[0].finish();
        let vertex = gate.request(Scope::Antigravity);
        vertex.waiting();
        assert!(!vertex.dispatch());
        gate.schedule(ClientId::Grok, Some(300_000), 90_000)
            .unwrap();
        gate.set(false).unwrap();
        assert_eq!(gate.next_resume(), None);
        assert!(vertex.dispatch());
        assert!(gate
            .view()
            .clients
            .iter()
            .all(|c| !c.status.frozen && c.resume_at.is_none()));
    }
    #[test]
    fn schedule_requires_finished_freeze_and_a_future_time() {
        let gate = Gate::default();
        assert!(gate.schedule(ClientId::Codex, Some(2000), 1000).is_err());
        let active = gate.request(Scope::Client(ClientId::Codex));
        assert!(active.dispatch());
        gate.set_selected(Some(ClientId::Codex), true).unwrap();
        assert!(gate.schedule(ClientId::Codex, Some(2000), 1000).is_err());
        assert!(gate.set_selected(Some(ClientId::Codex), false).is_err());
        active.finish();
        assert!(gate.schedule(ClientId::Codex, Some(1000), 1000).is_err());
        gate.schedule(ClientId::Codex, Some(2000), 1000).unwrap();
        gate.set_selected(Some(ClientId::Codex), false).unwrap();
        assert_eq!(gate.next_resume(), None);
        assert!(!Gate::default().view().frozen);
    }
    #[tokio::test(start_paused = true)]
    async fn drain_blocks_thaw_and_waiting_time_is_excluded() {
        let gate = Gate::default();
        let current = gate.request(Scope::Client(ClientId::Codex));
        current.waiting();
        assert!(current.dispatch());
        current.streaming();
        let queued = gate.request(Scope::Client(ClientId::Codex));
        queued.waiting();
        let view = gate.set(true).unwrap();
        assert_eq!((view.draining, view.streaming, view.queued), (1, 1, 1));
        assert!(!queued.dispatch());
        assert!(gate.set(false).is_err());
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert_eq!(queued.paused_for(), Duration::from_secs(3600));
        assert_eq!(current.paused_for(), Duration::ZERO);
        current.finishing();
        assert_eq!(gate.view().draining, 1);
        current.finish();
        assert_eq!(gate.view().draining, 0);
        gate.set(false).unwrap();
        assert!(queued.dispatch());
        drop(queued);
        assert_eq!(gate.view().draining, 0);
    }
    #[test]
    fn cancelled_waiter_is_removed_and_idle_connections_are_not_requests() {
        let gate = Gate::default();
        gate.set(true).unwrap();
        let request = gate.request(Scope::Client(ClientId::Codex));
        assert_eq!(gate.view().draining, 1);
        request.waiting();
        assert_eq!(gate.view().queued, 1);
        drop(request);
        assert_eq!(gate.view().queued, 0);
        assert_eq!(gate.view().draining, 0);
    }
}

#[cfg(test)]
#[test]
fn control_subscribers_only_wake_on_freeze_and_thaw() {
    let gate = Gate::default();
    let mut control = gate.subscribe_control();
    let mut traffic = gate.subscribe();
    let ticket = gate.request(Scope::Client(ClientId::Codex));
    ticket.waiting();
    ticket.dispatch();
    ticket.streaming();
    ticket.finishing();
    ticket.finish();
    drop(ticket);
    assert!(!control.has_changed().unwrap());
    assert!(traffic.has_changed().unwrap());
    traffic.borrow_and_update();
    gate.set_selected(Some(ClientId::Codex), true).unwrap();
    assert!(control.has_changed().unwrap());
    control.borrow_and_update();
    assert!(traffic.has_changed().unwrap());
    gate.schedule(ClientId::Codex, Some(2000), 1000).unwrap();
    assert!(!control.has_changed().unwrap());
    gate.resume_due(2000);
    assert!(control.has_changed().unwrap());
    control.borrow_and_update();
    gate.set_selected(Some(ClientId::Codex), false).unwrap();
    assert!(!control.has_changed().unwrap());
}
