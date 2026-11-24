//! Native macOS SwiftUI + AppKit (`NSPanel`) quick panel state derivation and scheduling.
//! All ViewModel derivation, validation, and quota auto-refresh scheduling live in Rust;
//! `native/quick_panel.swift` is a pure renderer and window host.

use crate::{
    core::ViewState,
    gateway::{self, quota::QuotaView, ClientId, ProviderView},
    instances::{self, InstanceState},
    power, process_control, quick,
    quick_model::*,
    Runtime,
};
use chrono::{Local, TimeZone};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    ffi::{c_char, c_void, CStr, CString},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};
use tauri::Manager;
use tokio::sync::watch;

extern "C" {
    fn vela_macos_effective_dark() -> bool;
    fn vela_quick_init(callback: extern "C" fn(*const c_char));
    fn vela_quick_attach_status_item(raw: *const c_void);
    fn vela_quick_toggle(json: *const c_char) -> bool;
    fn vela_quick_render(json: *const c_char);
    fn vela_quick_hide();
    fn vela_quick_is_visible() -> bool;
}

pub const NOTICE_TTL: Duration = Duration::from_millis(6500);

static VISIBLE: AtomicBool = AtomicBool::new(false);
static PANEL_HOST: OnceLock<Arc<Host>> = OnceLock::new();

pub fn effective_dark() -> bool {
    unsafe { vela_macos_effective_dark() }
}

pub fn is_visible() -> bool {
    VISIBLE.load(Ordering::Acquire)
}

pub fn attach_status_item(raw: *const c_void) {
    unsafe {
        vela_quick_attach_status_item(raw);
    }
}

// MARK: - Serializable ViewModel (Matches quick_panel.swift 1:1)

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientTabModel {
    pub id: ClientId,
    pub label: String,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceTabModel {
    pub id: String,
    pub label: String,
    pub selected: bool,
}

/// A card worked out here, or the one a server answered with.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Shown<T> {
    Here(T),
    There(Value),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlsCardModel {
    pub power_error: Option<String>,
    pub show_clamshell: bool,
    pub clamshell_enabled: bool,
    pub clamshell_disabled: bool,
    pub clamshell_read_error: Option<String>,
    pub quit_busy: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickPanelViewModel {
    pub appearance_mode: String,
    pub pinned: bool,
    pub busy: bool,
    pub freeze: Shown<FreezeCardModel>,
    pub clients: Vec<ClientTabModel>,
    pub selected_client: Option<ClientId>,
    pub client_visible: bool,
    pub error_banner: Option<String>,
    pub notice_banner: Option<String>,
    pub gateway: Option<Shown<GatewayCardModel>>,
    pub controls: ControlsCardModel,
    pub limit_error: Option<String>,
    pub limit_close_seq: u64,
    pub rename_close_seq: u64,
    /// This machine and the servers, once there is a server to switch to.
    pub instances: Vec<InstanceTabModel>,
    pub instance_status: Option<String>,
    pub instance_retry: bool,
    /// A server is shown: what belongs to this machine alone is left out.
    pub remote: bool,
}

// MARK: - Pure Derivation & Validation Logic

pub const ALL_CLIENTS: [ClientId; 5] = [
    ClientId::Codex,
    ClientId::Claude,
    ClientId::Agy,
    ClientId::Grok,
    ClientId::Dsh,
];

pub fn quick_client_label(client: ClientId) -> &'static str {
    match client {
        ClientId::Codex => "Codex",
        ClientId::Claude => "Claude",
        ClientId::Agy => "agy",
        ClientId::Grok => "Grok",
        ClientId::Dsh => "DSH",
    }
}

pub fn validate_provider_name(raw: &str) -> std::result::Result<String, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > 120
        || trimmed.encode_utf16().count() > 120
        || trimmed
            .chars()
            .any(|c| (c as u32) <= 0x1F || (c as u32) == 0x7F)
    {
        return Err("名称应为 1–120 个可见字符");
    }
    Ok(trimmed.to_owned())
}

pub fn validate_limit_input(raw: &str) -> std::result::Result<u32, &'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err("请输入 0–100000 的整数");
    }
    let val = trimmed
        .parse::<u32>()
        .map_err(|_| "请输入 0–100000 的整数")?;
    if val > 100_000 {
        return Err("请输入 0–100000 的整数");
    }
    Ok(val)
}

/// The notice text while it is still within its time to live.
pub fn active_notice_at(notice: Option<(&str, Instant)>, now: Instant) -> Option<String> {
    notice
        .filter(|(_, expires_at)| *expires_at > now)
        .map(|(text, _)| text.to_owned())
}

pub fn reconcile_startup_error(
    local_error: &mut Option<String>,
    local_error_from_startup: &mut bool,
    seeded_startup_message: &mut Option<String>,
    startup_now: Option<&str>,
) {
    if *local_error_from_startup && startup_now.is_none() {
        *local_error = None;
        *local_error_from_startup = false;
    }
    if let Some(msg) = startup_now {
        if seeded_startup_message.as_deref() != Some(msg) {
            *seeded_startup_message = Some(msg.to_owned());
            if local_error.is_none() || *local_error_from_startup {
                *local_error = Some(msg.to_owned());
                *local_error_from_startup = true;
            }
        }
    }
}

pub fn synthesize_quota_error_view(
    provider: &ProviderView,
    cached: Option<&QuotaView>,
    error_message: String,
    refresh_seconds: u64,
    now_secs: u64,
) -> QuotaView {
    let mut base = cached
        .filter(|q| q.version == provider.quota_version)
        .or(provider.quota.as_ref())
        .cloned()
        .unwrap_or_else(|| QuotaView::empty(&provider.id, &provider.quota_version));
    base.state = "error".into();
    base.stale = base.success_at.is_some();
    base.checked_at = Some(now_secs);
    base.error = Some(error_message);
    base.next_refresh_at = (refresh_seconds > 0).then(|| now_secs.saturating_add(refresh_seconds));
    base
}

pub fn should_accept_quota_push(
    providers: Option<&[ProviderView]>,
    existing_cached: Option<&QuotaView>,
    incoming: &QuotaView,
) -> bool {
    if let Some(list) = providers {
        return list
            .iter()
            .any(|p| p.id == incoming.provider_id && p.quota_version == incoming.version);
    }
    existing_cached.is_none_or(|c| c.version == incoming.version)
}

pub fn next_quota_schedule_step(
    providers: &[ProviderView],
    cached: &HashMap<String, QuotaView>,
    in_flight: &HashSet<(String, String)>,
    backoff_until: &HashMap<(String, String), u64>,
    refresh_seconds: u64,
    now_secs: u64,
) -> (Vec<(String, String)>, u64) {
    if providers.is_empty() || refresh_seconds == 0 {
        return (Vec::new(), now_secs.saturating_add(refresh_seconds.max(1)));
    }
    let delay_for = |q: Option<&QuotaView>| -> u64 {
        if q.is_some_and(|v| v.state == "unsupported") {
            refresh_seconds.max(86_400)
        } else {
            refresh_seconds
        }
    };
    let mut due = Vec::new();
    let mut next_at = u64::MAX;
    for p in providers {
        let pv = (p.id.clone(), p.quota_version.clone());
        let wildcard = (p.id.clone(), String::new());
        if in_flight.contains(&pv) || in_flight.contains(&wildcard) {
            continue;
        }
        let q = cached
            .get(&p.id)
            .filter(|q| q.version == p.quota_version)
            .or(p.quota.as_ref());
        let scheduled = q
            .and_then(|v| v.next_refresh_at)
            .or_else(|| {
                q.and_then(|v| v.checked_at)
                    .map(|c| c.saturating_add(delay_for(q)))
            })
            .unwrap_or(0);
        let backoff = backoff_until
            .get(&pv)
            .copied()
            .unwrap_or(0)
            .max(backoff_until.get(&wildcard).copied().unwrap_or(0));
        let at = scheduled
            .max(q.and_then(|v| v.retry_at).unwrap_or(0))
            .max(backoff);
        if at <= now_secs {
            due.push(pv);
        } else {
            next_at = next_at.min(at);
        }
    }
    if next_at == u64::MAX {
        next_at = now_secs.saturating_add(refresh_seconds);
    }
    (due, next_at)
}

/// The row that switches instances: empty until there is a server, then this
/// machine first. The status is only said while it is not simply connected.
pub fn instance_row(
    state: Option<&InstanceState>,
) -> (Vec<InstanceTabModel>, Option<String>, bool) {
    let Some(state) = state.filter(|state| !state.machines.is_empty()) else {
        return (Vec::new(), None, false);
    };
    let tab = |id: &str, label: &str| InstanceTabModel {
        id: id.into(),
        label: label.into(),
        selected: state.active == id,
    };
    let tabs = std::iter::once(tab(instances::LOCAL, "本机"))
        .chain(state.machines.iter().map(|m| tab(&m.id, &m.name)))
        .collect();
    let offline = state.connection.status == instances::Status::Offline;
    let status = match (state.connection.status, &state.connection.error) {
        (instances::Status::Connecting, _) => Some("连接中…".into()),
        (instances::Status::Offline, Some(error)) => Some(format!("离线：{error}")),
        (instances::Status::Offline, None) => Some("离线".into()),
        _ => None,
    };
    (tabs, status, offline)
}

/// The cards of a server's answer with what only this panel knows laid over
/// them: the start of each wait in this machine's time, the waits being
/// interrupted, and a freeze switch that is in use.
pub fn server_cards(
    snapshot: &Value,
    cancelling: &HashSet<String>,
    freeze_busy: bool,
) -> (Value, Option<Value>) {
    let mut freeze = snapshot["freeze"].clone();
    if freeze_busy {
        freeze["disabled"] = Value::Bool(true);
    }
    let mut gateway = snapshot.get("gateway").filter(|g| !g.is_null()).cloned();
    let waits = gateway
        .as_mut()
        .and_then(|gateway| gateway["longWaits"].as_array_mut());
    for wait in waits.into_iter().flatten() {
        let started = wait["startedAt"]
            .as_i64()
            .and_then(|at| Local.timestamp_opt(at, 0).single());
        if let Some(started) = started {
            wait["startedAtText"] = started.format("%H:%M:%S").to_string().into();
        }
        if wait["requestId"]
            .as_str()
            .is_some_and(|id| cancelling.contains(id))
        {
            wait["cancelling"] = Value::Bool(true);
        }
    }
    (freeze, gateway)
}

/// What the panel says once a wait was interrupted.
pub fn interrupted_notice(outcome: &str) -> &'static str {
    match outcome {
        "resent" => "已打断并重发",
        "ended" => "回应已开始交付，已中止，需由客户端重试",
        _ => "等待已结束",
    }
}

// MARK: - Host State, Action Dispatcher & Schedulers

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum QuickAction {
    PanelVisibility {
        visible: bool,
    },
    ToggleFreeze,
    SelectClient {
        #[serde(rename = "clientId")]
        client_id: ClientId,
    },
    TogglePinned,
    ClosePanel,
    OpenMain {
        page: Option<String>,
        #[serde(rename = "providerId")]
        provider_id: Option<String>,
        #[serde(rename = "clientId")]
        client_id: Option<ClientId>,
        #[serde(rename = "eventId")]
        event_id: Option<String>,
        #[serde(rename = "providerAction")]
        provider_action: Option<crate::ProviderAction>,
    },
    ToggleGatewayMode,
    ToggleGatewayPower,
    CancelWait {
        #[serde(rename = "requestId")]
        request_id: String,
    },
    ReorderProviders {
        ids: Vec<String>,
        #[serde(rename = "expectedRevision")]
        expected_revision: Option<String>,
    },
    RenameProvider {
        id: String,
        name: String,
        #[serde(rename = "expectedRevision")]
        expected_revision: Option<String>,
    },
    RefreshQuota {
        id: String,
    },
    SelectProvider {
        id: String,
    },
    ResetProvider {
        id: String,
    },
    ToggleProviderPolicy {
        id: String,
        field: String,
    },
    SaveProviderLimit {
        id: String,
        kind: String,
        value: String,
        #[serde(rename = "expectedRevision")]
        expected_revision: Option<String>,
    },
    ClearLimitError,
    ToggleProviderQueue {
        id: String,
    },
    DismissPowerError,
    ToggleClamshell,
    ForceQuit {
        target: process_control::Target,
    },
    SelectInstance {
        id: String,
    },
}

/// What a server last answered about one of its clients.
struct Answer {
    instance: String,
    client: ClientId,
    snapshot: Value,
}

/// The little of a gateway the actions decide by, from this machine's gateway
/// or from a server's answer.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lens {
    revision: String,
    running: bool,
    mode: String,
    providers: Vec<LensProvider>,
    #[serde(skip)]
    config_revision: Option<String>,
    #[serde(skip)]
    recovery_pending: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LensProvider {
    id: String,
    queued: bool,
    handoff_after_compaction: bool,
    take_new_threads: bool,
}

type PendingSnapshotKey = (ClientId, Vec<(String, u64, String)>);
type QuotaSchedulePlan = (ClientId, Vec<(String, String)>, Duration);

#[derive(Default)]
struct EphemeralState {
    busy: bool,
    freeze_busy: bool,
    freeze_error: Option<String>,
    local_error: Option<String>,
    local_error_from_startup: bool,
    seeded_startup_message: Option<String>,
    notice: Option<(String, Instant, u64)>,
    notice_seq: u64,
    limit_error: Option<String>,
    limit_close_seq: u64,
    rename_close_seq: u64,
    cancelling_waits: HashSet<String>,
    quotas: HashMap<(ClientId, String), QuotaView>,
    in_flight_quotas: HashSet<(ClientId, String, String)>,
    quota_backoff_until: HashMap<(ClientId, String, String), u64>,
    pending_snapshot_key: Option<PendingSnapshotKey>,
    pending_snapshot_at: Option<Instant>,
    power_state: Option<power::State>,
    power_busy: bool,
    power_read_error: Option<String>,
    power_error: Option<String>,
    quit_busy: Option<String>,
    /// The instances as last told. With a server selected the cards come from
    /// `server` and the actions go to it.
    instances: Option<InstanceState>,
    instance_busy: bool,
    server: Option<Answer>,
    server_error: Option<String>,
    /// An answer is on its way, and another is wanted after it.
    server_reading: bool,
    server_stale: bool,
}

struct Host {
    app: tauri::AppHandle,
    state: Mutex<EphemeralState>,
    render_mu: Mutex<()>,
    pending_render: Mutex<Option<CString>>,
    /// What the panel last drew, so an unchanged model is not sent again.
    last_render: Mutex<Option<CString>>,
    wake_tx: watch::Sender<u64>,
    wake_seq: std::sync::atomic::AtomicU64,
    /// Wakes only the power poller: the panel was shown or the switch used.
    power_wake_tx: watch::Sender<u64>,
}

extern "C" fn on_swift_action(raw: *const c_char) {
    if raw.is_null() {
        return;
    }
    let Ok(json) = unsafe { CStr::from_ptr(raw) }.to_str() else {
        return;
    };
    let Ok(action) = serde_json::from_str::<QuickAction>(json) else {
        return;
    };
    let Some(host) = PANEL_HOST.get().cloned() else {
        return;
    };
    host.handle_action(action);
}

impl Host {
    fn bump_wake(&self) {
        let next = self.wake_seq.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.wake_tx.send(next);
    }

    fn bump_power_wake(&self) {
        self.power_wake_tx.send_modify(|seq| *seq += 1);
    }

    /// Settles what a render only reads: a hidden selected client falls back
    /// to a visible one and the gateway learns the client's home directory.
    fn prepare(&self) {
        let (Some(r), Some(panel)) = (
            self.app.try_state::<Arc<Runtime>>(),
            self.app.try_state::<quick::Panel>(),
        ) else {
            return;
        };
        let selected = self.current_selected_client();
        if let Some(cid) = selected {
            if selected != panel.snapshot().selected_client {
                let _ = panel.sync_selected_client(cid);
            }
            if let Ok(home) = r.home(cid) {
                r.gateway(cid).observe_home(&home);
            }
        }
        if self.state.lock().instances.is_none() {
            let told = self
                .app
                .try_state::<Arc<instances::Service>>()
                .and_then(|service| service.state().ok());
            if let Some(state) = told {
                self.state.lock().instances.get_or_insert(state);
            }
        }
        self.read_server();
    }

    /// Whether the selected server can be asked without dialling it again:
    /// an offline one is left to the next action or to the retry button.
    fn server_reachable(&self) -> bool {
        let st = self.state.lock();
        st.instances.as_ref().is_some_and(|state| {
            state.active != instances::LOCAL
                && state.connection.status != instances::Status::Offline
        })
    }

    /// The server the panel shows, if it is not this machine.
    fn server(&self) -> Option<String> {
        let st = self.state.lock();
        let state = st.instances.as_ref()?;
        (state.active != instances::LOCAL).then(|| state.active.clone())
    }

    /// Runs a shared command on `instance`.
    async fn ask(&self, instance: &str, name: &str, args: Value) -> crate::storage::Result<Value> {
        let Some(service) = self
            .app
            .try_state::<Arc<instances::Service>>()
            .map(|service| service.inner().clone())
        else {
            return Err(crate::storage::AppError::new(
                crate::errors::code::STATE,
                "实例尚未就绪",
            ));
        };
        service.call(instance, name, args).await
    }

    /// Asks the selected server for the selected client again. Calls that
    /// arrive while one is under way are answered by one more after it.
    fn read_server(&self) {
        let Some(host) = PANEL_HOST.get().cloned() else {
            return;
        };
        if !host.server_reachable() {
            return;
        }
        {
            let mut st = host.state.lock();
            if st.server_reading {
                st.server_stale = true;
                return;
            }
            st.server_reading = true;
        }
        tauri::async_runtime::spawn(async move {
            loop {
                let asked = host.server().zip(host.current_selected_client());
                let answer = match &asked {
                    Some((instance, client)) => {
                        let cancelling: Vec<String> =
                            host.state.lock().cancelling_waits.iter().cloned().collect();
                        let args = json!({ "clientId": client, "cancelling": cancelling });
                        Some(host.ask(instance, "quick_snapshot", args).await)
                    }
                    None => None,
                };
                let again = {
                    let mut st = host.state.lock();
                    match (asked, answer) {
                        (Some((instance, client)), Some(Ok(snapshot))) => {
                            st.server_error = None;
                            st.server = Some(Answer {
                                instance,
                                client,
                                snapshot,
                            });
                        }
                        (_, Some(Err(error))) => st.server_error = Some(error.message),
                        _ => {}
                    }
                    let again = std::mem::take(&mut st.server_stale);
                    st.server_reading = again;
                    again
                };
                host.render_if_visible();
                if !again {
                    break;
                }
            }
        });
    }

    /// The server's answer about `client`, if it is the selected server's.
    fn answer<T>(&self, client: ClientId, read: impl FnOnce(&Value) -> T) -> Option<T> {
        let st = self.state.lock();
        let active = st.instances.as_ref().map(|state| state.active.as_str());
        st.server
            .as_ref()
            .filter(|answer| Some(answer.instance.as_str()) == active && answer.client == client)
            .map(|answer| read(&answer.snapshot))
    }

    fn lens(&self, client: ClientId) -> Option<Lens> {
        if self.server().is_none() {
            let r = self.app.try_state::<Arc<Runtime>>()?;
            let view = r.gateway(client).try_view().ok()?;
            return Some(Lens {
                revision: view.revision,
                running: view.running,
                mode: view.mode,
                providers: view
                    .providers
                    .into_iter()
                    .map(|p| LensProvider {
                        id: p.id,
                        queued: p.queued,
                        handoff_after_compaction: p.handoff_after_compaction,
                        take_new_threads: p.take_new_threads,
                    })
                    .collect(),
                config_revision: view.config_revision,
                recovery_pending: view.recovery_pending,
            });
        }
        self.answer(client, |snapshot| {
            let mut lens: Lens = serde_json::from_value(snapshot["gateway"].clone()).ok()?;
            lens.config_revision = snapshot["configRevision"].as_str().map(str::to_owned);
            lens.recovery_pending = snapshot["recoveryPending"] == true;
            Some(lens)
        })
        .flatten()
    }

    /// Keeps what the instances service told. Touches nothing but the panel's
    /// own state, so the service may call it while it holds its own.
    fn keep_instances(&self, state: &InstanceState) {
        let mut st = self.state.lock();
        let before = st.instances.as_ref().map(|state| state.active.as_str());
        if before != Some(state.active.as_str()) {
            st.server = None;
            st.server_error = None;
            st.cancelling_waits.clear();
            st.limit_error = None;
        }
        st.instances = Some(state.clone());
    }

    /// Shows the instances as they are now.
    fn show_instances(&self) {
        self.read_server();
        self.bump_wake();
        self.render_if_visible();
    }

    fn wait_interrupted(&self, request_id: &str, outcome: &str) {
        self.state.lock().cancelling_waits.remove(request_id);
        self.set_notice(interrupted_notice(outcome).into());
    }

    fn set_notice(&self, text: String) {
        let seq = {
            let mut st = self.state.lock();
            st.notice_seq += 1;
            let seq = st.notice_seq;
            st.notice = Some((text, Instant::now() + NOTICE_TTL, seq));
            seq
        };
        self.render_if_visible();
        let host = PANEL_HOST.get().cloned();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(NOTICE_TTL).await;
            if let Some(host) = host {
                let expired = {
                    let mut st = host.state.lock();
                    if st.notice.as_ref().is_some_and(|(_, _, s)| *s == seq) {
                        st.notice = None;
                        true
                    } else {
                        false
                    }
                };
                if expired {
                    host.render_if_visible();
                }
            }
        });
    }

    fn set_error(&self, text: String) {
        {
            let mut st = self.state.lock();
            st.local_error = Some(text);
            st.local_error_from_startup = false;
        }
        self.render_if_visible();
    }

    fn build_view_model_json(&self) -> Option<CString> {
        let r = self.app.try_state::<Arc<Runtime>>()?;
        let panel = self.app.try_state::<quick::Panel>()?;
        let prefs = panel.snapshot();
        let visible_clients: Vec<ClientId> = ALL_CLIENTS
            .into_iter()
            .filter(|id| !prefs.hidden_clients.contains(id))
            .collect();
        let selected_client = prefs
            .selected_client
            .filter(|id| visible_clients.contains(id))
            .or_else(|| visible_clients.first().copied());
        let client_visible = selected_client.is_some();

        let accounts: Option<ViewState> = r.state().ok();
        let pref_theme = accounts
            .as_ref()
            .map(|a| a.preferences.theme.as_str())
            .unwrap_or("system");
        let appearance_mode = match pref_theme {
            "dark" | "light" => pref_theme.to_owned(),
            _ => "system".to_owned(),
        };

        let server = self.server();
        let freeze_view = r.gateway.freeze_gate().view();
        let gateway_result = selected_client
            .filter(|_| server.is_none())
            .map(|cid| r.gateway(cid).try_view());

        let now_inst = Instant::now();
        let now_secs = gateway::quota::now();

        let startup_now = r.startup_error.lock().clone();
        let mut st = self.state.lock();
        {
            let st_ref = &mut *st;
            reconcile_startup_error(
                &mut st_ref.local_error,
                &mut st_ref.local_error_from_startup,
                &mut st_ref.seeded_startup_message,
                startup_now.as_ref().map(|e| e.message.as_str()),
            );
        }
        let notice_banner = active_notice_at(
            st.notice.as_ref().map(|(text, at, _)| (text.as_str(), *at)),
            now_inst,
        );
        if notice_banner.is_none() {
            st.notice = None;
        }

        let mut gateway_err: Option<String> = None;
        let gateway_card = match (selected_client, gateway_result) {
            (Some(cid), Some(Ok(gw))) => {
                gateway_err = gw.error.clone();
                let sig: Vec<(String, u64, String)> = gw
                    .pending_requests
                    .iter()
                    .map(|p| (p.request_id.clone(), p.elapsed_seconds, p.phase.clone()))
                    .collect();
                let key = (cid, sig);
                if st.pending_snapshot_key.as_ref() != Some(&key) {
                    st.pending_snapshot_key = Some(key);
                    st.pending_snapshot_at = Some(now_inst);
                }
                let extra_secs = st
                    .pending_snapshot_at
                    .map(|t| now_inst.saturating_duration_since(t).as_secs())
                    .unwrap_or(0);
                let client_quotas: HashMap<String, QuotaView> = st
                    .quotas
                    .iter()
                    .filter(|((c, _), _)| *c == cid)
                    .map(|((_, pid), q)| (pid.clone(), q.clone()))
                    .collect();
                Some(derive_gateway_card(
                    &gw,
                    &client_quotas,
                    now_secs,
                    extra_secs,
                    &st.cancelling_waits,
                ))
            }
            (_, Some(Err(e))) => {
                gateway_err = Some(e.message);
                None
            }
            _ => None,
        };

        let (freeze_card, gateway_card) = match &server {
            None => (
                Shown::Here(derive_freeze_card(
                    Some(&freeze_view),
                    st.freeze_error.as_deref(),
                    st.freeze_busy,
                )),
                gateway_card.map(Shown::Here),
            ),
            Some(instance) => {
                let answer = st.server.as_ref().filter(|a| &a.instance == instance);
                gateway_err = st.server_error.clone().or_else(|| {
                    answer.and_then(|a| a.snapshot["error"].as_str().map(str::to_owned))
                });
                match answer {
                    Some(answer) => {
                        let (freeze, gateway) =
                            server_cards(&answer.snapshot, &st.cancelling_waits, st.freeze_busy);
                        (
                            Shown::There(freeze),
                            gateway
                                .filter(|_| Some(answer.client) == selected_client)
                                .map(Shown::There),
                        )
                    }
                    None => (Shown::Here(derive_freeze_card(None, None, false)), None),
                }
            }
        };
        // What is wrong with this machine's accounts says nothing of a server's.
        let error_banner = st
            .local_error
            .clone()
            .or(gateway_err)
            .or_else(|| accounts.filter(|_| server.is_none()).and_then(|a| a.error));
        let (instances, instance_status, instance_retry) = instance_row(st.instances.as_ref());
        let clients = visible_clients
            .iter()
            .map(|&cid| ClientTabModel {
                id: cid,
                label: quick_client_label(cid).into(),
                selected: Some(cid) == selected_client,
            })
            .collect();
        let controls = ControlsCardModel {
            power_error: st.power_error.clone(),
            show_clamshell: st.power_state.as_ref().is_some_and(|p| p.supported)
                || st.power_read_error.is_some(),
            clamshell_enabled: st.power_state.as_ref().is_some_and(|p| p.enabled),
            clamshell_disabled: st.power_busy
                || st.power_read_error.is_some()
                || st.power_state.is_none(),
            clamshell_read_error: st.power_read_error.clone(),
            quit_busy: st.quit_busy.clone(),
        };
        let vm = QuickPanelViewModel {
            appearance_mode,
            pinned: prefs.pinned,
            busy: st.busy || st.instance_busy,
            freeze: freeze_card,
            clients,
            selected_client,
            client_visible,
            error_banner,
            notice_banner,
            gateway: gateway_card,
            controls,
            limit_error: st.limit_error.clone(),
            limit_close_seq: st.limit_close_seq,
            rename_close_seq: st.rename_close_seq,
            instances,
            instance_status,
            instance_retry,
            remote: server.is_some(),
        };
        drop(st);
        let json = serde_json::to_string(&vm).ok()?;
        CString::new(json).ok()
    }

    fn render_if_visible(&self) {
        if !is_visible() {
            return;
        }
        {
            let _guard = self.render_mu.lock();
            let Some(cstr) = self.build_view_model_json() else {
                return;
            };
            if self.last_render.lock().as_ref() == Some(&cstr) {
                self.pending_render.lock().take();
                return;
            }
            *self.pending_render.lock() = Some(cstr);
        }
        let Some(host) = PANEL_HOST.get().cloned() else {
            return;
        };
        let _ = self.app.run_on_main_thread(move || {
            if is_visible() {
                if let Some(cstr) = host.pending_render.lock().take() {
                    unsafe {
                        vela_quick_render(cstr.as_ptr());
                    }
                    *host.last_render.lock() = Some(cstr);
                }
            } else {
                host.pending_render.lock().take();
            }
        });
    }

    fn current_selected_client(&self) -> Option<ClientId> {
        let panel = self.app.try_state::<quick::Panel>()?;
        let prefs = panel.snapshot();
        let visible_clients: Vec<ClientId> = ALL_CLIENTS
            .into_iter()
            .filter(|id| !prefs.hidden_clients.contains(id))
            .collect();
        prefs
            .selected_client
            .filter(|id| visible_clients.contains(id))
            .or_else(|| visible_clients.first().copied())
    }
}

impl Host {
    fn handle_action(self: &Arc<Self>, action: QuickAction) {
        match action {
            QuickAction::PanelVisibility { visible } => {
                VISIBLE.store(visible, Ordering::Release);
                if let Some(r) = self.app.try_state::<Arc<Runtime>>() {
                    r.mark_window_visible("quick", visible);
                }
                self.bump_wake();
            }
            QuickAction::ClosePanel => {
                hide(&self.app);
            }
            QuickAction::TogglePinned => {
                if let Some(panel) = self.app.try_state::<quick::Panel>() {
                    let current = panel.snapshot().pinned;
                    if let Err(e) = panel.update(&self.app, Some(!current), None, None) {
                        self.set_error(e.message);
                    }
                }
            }
            QuickAction::SelectClient { client_id } => {
                if let Some(panel) = self.app.try_state::<quick::Panel>() {
                    self.state.lock().limit_error = None;
                    if let Err(e) = panel.update(&self.app, None, None, Some(client_id)) {
                        self.set_error(e.message);
                        self.bump_wake();
                    }
                    self.prepare();
                }
            }
            QuickAction::OpenMain {
                page,
                provider_id,
                client_id,
                event_id,
                provider_action,
            } => {
                let app = self.app.clone();
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = crate::open_main(
                        app,
                        page,
                        provider_id,
                        client_id,
                        event_id,
                        provider_action,
                    )
                    .await
                    {
                        host.set_error(e.message);
                    }
                });
            }
            QuickAction::ToggleFreeze => {
                let server = self.server();
                // Whether it is frozen, and whether the switch is locked.
                let (frozen, locked) = match &server {
                    None => {
                        let Some(r) = self.app.try_state::<Arc<Runtime>>() else {
                            return;
                        };
                        let view = r.gateway.freeze_gate().view();
                        (view.frozen, view.frozen && view.draining > 0)
                    }
                    Some(instance) => {
                        let st = self.state.lock();
                        let Some(answer) = st.server.as_ref().filter(|a| &a.instance == instance)
                        else {
                            return;
                        };
                        let card = &answer.snapshot["freeze"];
                        (card["frozen"] == true, card["disabled"] == true)
                    }
                };
                if locked {
                    return;
                }
                {
                    let mut st = self.state.lock();
                    if st.freeze_busy {
                        return;
                    }
                    if st.freeze_error.is_some() {
                        st.freeze_error = None;
                        drop(st);
                        self.render_if_visible();
                        return;
                    }
                    st.freeze_busy = true;
                }
                self.render_if_visible();
                match server {
                    None => {
                        let res = self
                            .app
                            .try_state::<Arc<Runtime>>()
                            .map(|r| r.gateway.freeze_gate().set(!frozen));
                        let mut st = self.state.lock();
                        st.freeze_busy = false;
                        st.freeze_error = res.and_then(|res| res.err()).map(|e| e.message);
                        drop(st);
                        self.bump_wake();
                        self.render_if_visible();
                    }
                    Some(instance) => {
                        let host = self.clone();
                        tauri::async_runtime::spawn(async move {
                            let args = json!({ "frozen": !frozen });
                            let res = host.ask(&instance, "set_gateway_frozen", args).await;
                            host.state.lock().freeze_busy = false;
                            // The card is the server's; its failure is told above it.
                            if let Err(e) = res {
                                host.set_error(e.message);
                            }
                            host.read_server();
                            host.render_if_visible();
                        });
                    }
                }
            }
            QuickAction::ToggleGatewayMode => {
                let Some(cid) = self.current_selected_client() else {
                    return;
                };
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    let Some(view) = host.lens(cid) else {
                        return;
                    };
                    let next_mode = if view.mode == "auto" {
                        "manual"
                    } else {
                        "auto"
                    };
                    host.commit_gateway_edit(
                        cid,
                        gateway::Edit::Mode {
                            mode: next_mode.into(),
                        },
                        Some(view.revision),
                        true,
                    )
                    .await;
                });
            }
            QuickAction::ToggleGatewayPower => {
                let Some(cid) = self.current_selected_client() else {
                    return;
                };
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    let Some(view) = host.lens(cid) else {
                        return;
                    };
                    let server = host.server();
                    let r = host
                        .app
                        .try_state::<Arc<Runtime>>()
                        .map(|r| r.inner().clone());
                    {
                        let mut st = host.state.lock();
                        if st.busy {
                            return;
                        }
                        st.busy = true;
                        st.local_error = None;
                        st.local_error_from_startup = false;
                    }
                    host.render_if_visible();
                    let config = view.config_revision.as_deref();
                    let stop = view.running || view.recovery_pending;
                    let outcome = match (&server, &r) {
                        (Some(instance), _) => if stop {
                            let args = json!({ "clientId": cid, "expectedConfigRevision": config });
                            host.ask(instance, "stop_gateway", args).await
                        } else {
                            let args = json!({
                                "clientId": cid,
                                "expectedRevision": view.revision,
                                "expectedConfigRevision": config,
                            });
                            host.ask(instance, "start_gateway", args).await
                        }
                        .map(|_| ()),
                        (None, Some(r)) => if stop {
                            crate::rpc::handlers::stop_gateway_for(r, cid, config).await
                        } else {
                            crate::rpc::handlers::start_gateway_for(r, cid, &view.revision, config)
                                .await
                        }
                        .map(|_| ()),
                        (None, None) => Ok(()),
                    };
                    host.state.lock().busy = false;
                    match outcome {
                        // A server says so itself, as it does to every window.
                        Ok(()) if server.is_some() => host.read_server(),
                        Ok(()) => {
                            host.set_notice(format!("配置已切换，请重新打开 {}", cid.name()));
                        }
                        Err(e) => host.set_error(e.message),
                    }
                    host.bump_wake();
                    host.render_if_visible();
                });
            }
            QuickAction::CancelWait { request_id } => {
                let Some(cid) = self.current_selected_client() else {
                    return;
                };
                self.state
                    .lock()
                    .cancelling_waits
                    .insert(request_id.clone());
                self.render_if_visible();
                let Some(instance) = self.server() else {
                    if let Some(r) = self.app.try_state::<Arc<Runtime>>() {
                        let outcome = r.gateway(cid).interrupt_wait(&request_id);
                        self.wait_interrupted(&request_id, outcome);
                    }
                    return;
                };
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    let args = json!({ "clientId": cid, "requestId": request_id });
                    match host.ask(&instance, "cancel_gateway_wait", args).await {
                        Ok(outcome) => {
                            host.wait_interrupted(&request_id, outcome.as_str().unwrap_or(""));
                        }
                        Err(e) => {
                            host.state.lock().cancelling_waits.remove(&request_id);
                            host.set_error(e.message);
                        }
                    }
                    host.read_server();
                });
            }
            QuickAction::ReorderProviders {
                ids,
                expected_revision,
            } => {
                let Some(cid) = self.current_selected_client() else {
                    return;
                };
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    host.commit_gateway_edit(
                        cid,
                        gateway::Edit::Reorder { ids },
                        expected_revision,
                        true,
                    )
                    .await;
                });
            }
            QuickAction::RenameProvider {
                id,
                name,
                expected_revision,
            } => {
                let clean = match validate_provider_name(&name) {
                    Ok(v) => v,
                    Err(msg) => {
                        self.set_error(msg.into());
                        return;
                    }
                };
                let Some(cid) = self.current_selected_client() else {
                    return;
                };
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    if host
                        .commit_gateway_edit(
                            cid,
                            gateway::Edit::RenameProvider { id, name: clean },
                            expected_revision,
                            true,
                        )
                        .await
                    {
                        host.state.lock().rename_close_seq += 1;
                        host.render_if_visible();
                    }
                });
            }
            QuickAction::RefreshQuota { id } => {
                let Some(cid) = self.current_selected_client() else {
                    return;
                };
                let Some(instance) = self.server() else {
                    self.spawn_quota_query(cid, id, None, true);
                    return;
                };
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    let args = json!({ "clientId": cid, "providerId": id, "force": true });
                    if let Err(e) = host.ask(&instance, "query_provider_quota", args).await {
                        host.set_error(e.message);
                    }
                    host.read_server();
                });
            }
            QuickAction::SelectProvider { id } => {
                let Some(cid) = self.current_selected_client() else {
                    return;
                };
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    let running_before = host.lens(cid).is_some_and(|view| view.running);
                    // A server says what the choice did itself, as it does to every window.
                    if host
                        .commit_gateway_edit(cid, gateway::Edit::Select { id }, None, true)
                        .await
                        && host.server().is_none()
                    {
                        host.set_notice(if running_before {
                            "供应商已切换，新请求已生效".into()
                        } else {
                            format!("配置已切换，请重新打开 {}", cid.name())
                        });
                    }
                });
            }
            QuickAction::ResetProvider { id } => {
                let Some(cid) = self.current_selected_client() else {
                    return;
                };
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    if host
                        .commit_gateway_edit(cid, gateway::Edit::Reset { id }, None, true)
                        .await
                    {
                        host.set_notice("已重置熔断".into());
                    }
                });
            }
            QuickAction::ToggleProviderPolicy { id, field } => {
                let Some(cid) = self.current_selected_client() else {
                    return;
                };
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    let Some(view) = host.lens(cid) else {
                        return;
                    };
                    let Some(p) = view.providers.iter().find(|p| p.id == id) else {
                        return;
                    };
                    let edit = match field.as_str() {
                        "handoffAfterCompaction" => gateway::Edit::TakeoverProvider {
                            id,
                            handoff_after_compaction: Some(!p.handoff_after_compaction),
                            take_new_threads: None,
                        },
                        "takeNewThreads" => gateway::Edit::TakeoverProvider {
                            id,
                            handoff_after_compaction: None,
                            take_new_threads: Some(!p.take_new_threads),
                        },
                        _ => return,
                    };
                    host.commit_gateway_edit(cid, edit, Some(view.revision), true)
                        .await;
                });
            }
            QuickAction::ClearLimitError => {
                self.state.lock().limit_error = None;
                self.render_if_visible();
            }
            QuickAction::SaveProviderLimit {
                id,
                kind,
                value,
                expected_revision,
            } => {
                let parsed = match validate_limit_input(&value) {
                    Ok(v) => v,
                    Err(msg) => {
                        self.state.lock().limit_error = Some(msg.into());
                        self.render_if_visible();
                        return;
                    }
                };
                let Some(cid) = self.current_selected_client() else {
                    return;
                };
                let edit = if kind == "rpm" {
                    gateway::Edit::RpmProvider {
                        id,
                        max_rpm: parsed,
                    }
                } else {
                    gateway::Edit::ConcurrencyProvider {
                        id,
                        max_concurrency: parsed,
                    }
                };
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    host.state.lock().limit_error = None;
                    if host
                        .commit_gateway_edit(cid, edit, expected_revision, false)
                        .await
                    {
                        let mut st = host.state.lock();
                        st.limit_error = None;
                        st.limit_close_seq += 1;
                        drop(st);
                        host.render_if_visible();
                    }
                });
            }
            QuickAction::ToggleProviderQueue { id } => {
                let Some(cid) = self.current_selected_client() else {
                    return;
                };
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    let Some(view) = host.lens(cid) else {
                        return;
                    };
                    let Some(p) = view.providers.iter().find(|p| p.id == id) else {
                        return;
                    };
                    host.commit_gateway_edit(
                        cid,
                        gateway::Edit::QueueProvider {
                            id,
                            queued: !p.queued,
                        },
                        Some(view.revision),
                        true,
                    )
                    .await;
                });
            }
            QuickAction::DismissPowerError => {
                self.state.lock().power_error = None;
                self.render_if_visible();
            }
            QuickAction::ToggleClamshell => {
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    let Some(r) = host
                        .app
                        .try_state::<Arc<Runtime>>()
                        .map(|s| s.inner().clone())
                    else {
                        return;
                    };
                    let target = {
                        let mut st = host.state.lock();
                        if st.power_busy {
                            return;
                        }
                        let Some(cur) = st.power_state.clone() else {
                            return;
                        };
                        st.power_busy = true;
                        st.power_error = None;
                        (!cur.enabled, cur.revision)
                    };
                    host.render_if_visible();
                    // The shared path publishes the new state or the error to
                    // this panel and the main window alike.
                    let res =
                        crate::commands::set_clamshell(&host.app, &r, target.0, target.1).await;
                    host.state.lock().power_busy = false;
                    match res {
                        Ok(next) => host.set_notice(if next.enabled {
                            "已开启合盖不休眠".into()
                        } else {
                            "已恢复电池休眠设置".into()
                        }),
                        Err(_) => host.render_if_visible(),
                    }
                    host.bump_power_wake();
                });
            }
            QuickAction::SelectInstance { id } => {
                let Some(service) = self
                    .app
                    .try_state::<Arc<instances::Service>>()
                    .map(|service| service.inner().clone())
                else {
                    return;
                };
                let revision = {
                    let mut st = self.state.lock();
                    let Some(revision) = st.instances.as_ref().map(|s| s.revision.clone()) else {
                        return;
                    };
                    if st.instance_busy {
                        return;
                    }
                    st.instance_busy = true;
                    st.local_error = None;
                    st.local_error_from_startup = false;
                    revision
                };
                self.render_if_visible();
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    // The same instance again is how an offline one is retried.
                    let selected = service.select(&revision, &id).await;
                    host.state.lock().instance_busy = false;
                    // On a failure too: the list may be what was stale.
                    if let Ok(state) = service.state() {
                        host.keep_instances(&state);
                    }
                    if let Err(e) = selected {
                        host.set_error(e.message);
                    }
                    host.show_instances();
                });
            }
            QuickAction::ForceQuit { target } => {
                let label = match target {
                    process_control::Target::Codex => "codex",
                    process_control::Target::Claude => "claude",
                    // The panel has no control for it.
                    process_control::Target::Agy => return,
                };
                {
                    let mut st = self.state.lock();
                    if st.quit_busy.is_some() {
                        return;
                    }
                    st.quit_busy = Some(label.into());
                }
                self.render_if_visible();
                let host = self.clone();
                tauri::async_runtime::spawn(async move {
                    let Some(r) = host
                        .app
                        .try_state::<Arc<Runtime>>()
                        .map(|s| s.inner().clone())
                    else {
                        host.state.lock().quit_busy = None;
                        return;
                    };
                    let res = crate::commands::force_quit(&r, target).await;
                    host.state.lock().quit_busy = None;
                    match res {
                        Ok(out) => {
                            let failed = if out.failed > 0 {
                                format!("，{} 个未能退出", out.failed)
                            } else {
                                String::new()
                            };
                            let term = if out.terminal_cleanup_failed > 0 {
                                "；部分终端未能安全恢复，请重新打开对应终端"
                            } else {
                                ""
                            };
                            host.set_notice(format!(
                                "已退出 {} 个进程{failed}{term}",
                                out.terminated
                            ));
                        }
                        Err(e) => host.set_error(e.message),
                    }
                    host.render_if_visible();
                });
            }
        }
    }
}

impl Host {
    async fn commit_gateway_edit(
        self: &Arc<Self>,
        client_id: ClientId,
        edit: gateway::Edit,
        expected_revision: Option<String>,
        surface_in_banner: bool,
    ) -> bool {
        let Some(r) = self
            .app
            .try_state::<Arc<Runtime>>()
            .map(|s| s.inner().clone())
        else {
            return false;
        };
        let server = self.server();
        // What the server last answered is what the edit is checked against.
        let seen = server.as_ref().map(|_| self.lens(client_id));
        if matches!(seen, Some(None)) {
            return false;
        }
        {
            let mut st = self.state.lock();
            if st.busy {
                return false;
            }
            st.busy = true;
            if surface_in_banner {
                st.local_error = None;
                st.local_error_from_startup = false;
            }
        }
        self.render_if_visible();

        let outcome = if let (Some(instance), Some(Some(current))) = (&server, seen) {
            let config = (matches!(&edit, gateway::Edit::Select { .. }) && !current.running)
                .then_some(current.config_revision)
                .flatten();
            let args = json!({
                "clientId": client_id,
                "edit": edit,
                "expectedRevision": expected_revision.unwrap_or(current.revision),
                "expectedConfigRevision": config,
            });
            self.ask(instance, "update_gateway", args).await.map(|_| ())
        } else {
            let _guard = r.official_tx.lock().await;
            match (r.home(client_id), r.gateway(client_id).try_view()) {
                (Ok(home), Ok(current)) => {
                    let rev = expected_revision.unwrap_or_else(|| current.revision.clone());
                    let cfg_rev =
                        if matches!(&edit, gateway::Edit::Select { .. }) && !current.running {
                            current.config_revision.as_deref()
                        } else {
                            None
                        };
                    r.gateway(client_id)
                        .edit_checked(edit, &rev, &home, cfg_rev)
                        .map(|_| ())
                        .map_err(|e| r.config_failure(client_id, e))
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            }
        };

        self.state.lock().busy = false;
        match outcome {
            Ok(()) if server.is_some() => {
                self.read_server();
                self.render_if_visible();
                true
            }
            Ok(()) => {
                let _ = r.refresh();
                self.bump_wake();
                self.render_if_visible();
                true
            }
            Err(e) => {
                let mut st = self.state.lock();
                if surface_in_banner {
                    st.local_error = Some(e.message);
                    st.local_error_from_startup = false;
                } else {
                    st.limit_error = Some(e.message);
                }
                drop(st);
                self.bump_wake();
                self.render_if_visible();
                false
            }
        }
    }

    fn spawn_quota_query(
        self: &Arc<Self>,
        client_id: ClientId,
        provider_id: String,
        expected_version: Option<String>,
        force: bool,
    ) {
        if self.server().is_some() {
            return;
        }
        let resolved_version = expected_version.unwrap_or_else(|| {
            self.app
                .try_state::<Arc<Runtime>>()
                .and_then(|r| r.gateway(client_id).try_view().ok())
                .and_then(|v| v.providers.into_iter().find(|p| p.id == provider_id))
                .map(|p| p.quota_version)
                .unwrap_or_default()
        });
        let flight_key = (client_id, provider_id.clone(), resolved_version.clone());
        {
            let mut st = self.state.lock();
            if !st.in_flight_quotas.insert(flight_key.clone()) {
                return;
            }
        }
        let cache_key = (client_id, provider_id.clone());
        let host = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut force_banner_err: Option<String> = None;
            if let Some(r) = host
                .app
                .try_state::<Arc<Runtime>>()
                .map(|s| s.inner().clone())
            {
                let refresh_seconds = r.core.lock().preferences().quota_refresh_seconds;
                match r.gateway(client_id).query_quota(&provider_id, force).await {
                    Ok(view) => {
                        let cur_providers =
                            r.gateway(client_id).try_view().ok().map(|v| v.providers);
                        let now_secs = gateway::quota::now();
                        let unscheduled = view.checked_at.is_none()
                            && view.next_refresh_at.is_none()
                            && view.retry_at.is_none();
                        let mut st = host.state.lock();
                        if should_accept_quota_push(
                            cur_providers.as_deref(),
                            st.quotas.get(&cache_key),
                            &view,
                        ) {
                            let ver_key = (client_id, provider_id.clone(), view.version.clone());
                            let wild_key = (client_id, provider_id.clone(), String::new());
                            if unscheduled {
                                st.quota_backoff_until
                                    .insert(ver_key, now_secs.saturating_add(1));
                            } else {
                                st.quota_backoff_until.remove(&ver_key);
                                st.quota_backoff_until.remove(&wild_key);
                            }
                            st.quotas.insert(cache_key.clone(), view);
                        }
                    }
                    Err(e) => {
                        let now_secs = gateway::quota::now();
                        if e.code == crate::errors::code::STALE {
                            host.state
                                .lock()
                                .quota_backoff_until
                                .insert(flight_key.clone(), now_secs.saturating_add(1));
                        } else {
                            if force {
                                force_banner_err = Some(e.message.clone());
                            }
                            let cur_provider = r.gateway(client_id).try_view().ok().and_then(|v| {
                                v.providers.into_iter().find(|p| p.id == provider_id)
                            });
                            let mut st = host.state.lock();
                            match cur_provider {
                                Some(p)
                                    if resolved_version.is_empty()
                                        || p.quota_version == resolved_version =>
                                {
                                    let err_view = synthesize_quota_error_view(
                                        &p,
                                        st.quotas.get(&cache_key),
                                        e.message,
                                        refresh_seconds,
                                        now_secs,
                                    );
                                    let ver_key =
                                        (client_id, provider_id.clone(), p.quota_version.clone());
                                    st.quotas.insert(cache_key.clone(), err_view);
                                    if refresh_seconds == 0 {
                                        st.quota_backoff_until
                                            .insert(ver_key, now_secs.saturating_add(5));
                                    } else {
                                        st.quota_backoff_until.remove(&ver_key);
                                    }
                                }
                                Some(_) => {}
                                None => {
                                    st.quota_backoff_until.insert(
                                        flight_key.clone(),
                                        now_secs.saturating_add(refresh_seconds.max(5)),
                                    );
                                }
                            }
                        }
                    }
                }
            }
            {
                let mut st = host.state.lock();
                st.in_flight_quotas.remove(&flight_key);
                if let Some(msg) = force_banner_err {
                    st.local_error = Some(msg);
                    st.local_error_from_startup = false;
                }
            }
            host.bump_wake();
            host.render_if_visible();
        });
    }

    fn spawn_quota_scheduler(self: &Arc<Self>) {
        let host = self.clone();
        let mut wake_rx = self.wake_tx.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                if !is_visible() {
                    if wake_rx.changed().await.is_err() {
                        break;
                    }
                    continue;
                }
                let plan = (|| -> Option<QuotaSchedulePlan> {
                    if host.server().is_some() {
                        return None;
                    }
                    let r = host.app.try_state::<Arc<Runtime>>()?;
                    if r.quitting.load(Ordering::Relaxed) {
                        return None;
                    }
                    let cid = host.current_selected_client()?;
                    let freeze = r.gateway.freeze_gate().view();
                    if freeze
                        .clients
                        .iter()
                        .any(|c| c.client_id == cid && c.status.frozen)
                    {
                        return None;
                    }
                    let refresh_seconds = r.core.lock().preferences().quota_refresh_seconds;
                    if refresh_seconds == 0 {
                        return None;
                    }
                    let view = r.gateway(cid).try_view().ok()?;
                    if view.providers.is_empty() {
                        return None;
                    }
                    let now_secs = gateway::quota::now();
                    let (client_quotas, client_in_flight, client_backoff) = {
                        let st = host.state.lock();
                        let quotas: HashMap<String, QuotaView> = st
                            .quotas
                            .iter()
                            .filter(|((c, _), _)| *c == cid)
                            .map(|((_, pid), q)| (pid.clone(), q.clone()))
                            .collect();
                        let in_flight: HashSet<(String, String)> = st
                            .in_flight_quotas
                            .iter()
                            .filter(|(c, _, _)| *c == cid)
                            .map(|(_, pid, ver)| (pid.clone(), ver.clone()))
                            .collect();
                        let backoff: HashMap<(String, String), u64> = st
                            .quota_backoff_until
                            .iter()
                            .filter(|((c, _, _), _)| *c == cid)
                            .map(|((_, pid, ver), &until)| ((pid.clone(), ver.clone()), until))
                            .collect();
                        (quotas, in_flight, backoff)
                    };
                    let (due, next_at) = next_quota_schedule_step(
                        &view.providers,
                        &client_quotas,
                        &client_in_flight,
                        &client_backoff,
                        refresh_seconds,
                        now_secs,
                    );
                    let wait_secs = next_at.saturating_sub(now_secs).clamp(1, 86_400);
                    Some((cid, due, Duration::from_secs(wait_secs)))
                })();

                match plan {
                    None => {
                        if wake_rx.changed().await.is_err() {
                            break;
                        }
                    }
                    Some((cid, due, sleep_for)) => {
                        for (pid, ver) in &due {
                            host.spawn_quota_query(cid, pid.clone(), Some(ver.clone()), false);
                        }
                        let delay = if due.is_empty() {
                            sleep_for
                        } else {
                            Duration::from_secs(1)
                        };
                        tokio::select! {
                            res = wake_rx.changed() => if res.is_err() { break; },
                            _ = tokio::time::sleep(delay) => {}
                        }
                    }
                }
            }
        });
    }

    fn spawn_power_poller(self: &Arc<Self>) {
        let host = self.clone();
        let mut wake_rx = self.power_wake_tx.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                if !is_visible() {
                    if wake_rx.changed().await.is_err() {
                        break;
                    }
                    continue;
                }
                if !host.state.lock().power_busy {
                    if let Some(r) = host
                        .app
                        .try_state::<Arc<Runtime>>()
                        .map(|s| s.inner().clone())
                    {
                        if let Ok(res) =
                            tauri::async_runtime::spawn_blocking(move || r.shell.power.state())
                                .await
                        {
                            let mut st = host.state.lock();
                            if !st.power_busy {
                                match res {
                                    Ok(s) => {
                                        st.power_state = Some(s);
                                        st.power_read_error = None;
                                    }
                                    Err(e) => {
                                        st.power_read_error = Some(e.message);
                                    }
                                }
                            }
                            drop(st);
                            host.render_if_visible();
                        }
                    }
                }
                tokio::select! {
                    res = wake_rx.changed() => if res.is_err() { break; },
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                }
            }
        });
    }

    fn spawn_wait_ticker(self: &Arc<Self>) {
        let host = self.clone();
        let mut wake_rx = self.wake_tx.subscribe();
        tauri::async_runtime::spawn(async move {
            loop {
                if is_visible() && host.server().is_some() {
                    tokio::select! {
                        res = wake_rx.changed() => if res.is_err() { break; },
                        _ = tokio::time::sleep(Duration::from_secs(3)) => host.read_server(),
                    }
                    continue;
                }
                let has_active_wait = is_visible()
                    && host
                        .current_selected_client()
                        .and_then(|cid| {
                            let r = host.app.try_state::<Arc<Runtime>>()?;
                            r.gateway(cid).try_view().ok()
                        })
                        .is_some_and(|v| v.pending_requests.iter().any(|p| p.phase != "frozen"));
                // The refresh button unlocks when a retry time passes.
                let retry_due = is_visible() && {
                    let now = gateway::quota::now();
                    let selected = host.current_selected_client();
                    host.state.lock().quotas.iter().any(|((cid, _), q)| {
                        Some(*cid) == selected && q.retry_at.is_some_and(|at| at + 1 >= now)
                    })
                };
                if !has_active_wait && !retry_due {
                    if wake_rx.changed().await.is_err() {
                        break;
                    }
                    continue;
                }
                tokio::select! {
                    res = wake_rx.changed() => if res.is_err() { break; },
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {
                        host.render_if_visible();
                    }
                }
            }
        });
    }
}

pub fn connect(app: &tauri::AppHandle) {
    let (wake_tx, _) = watch::channel(0u64);
    let (power_wake_tx, _) = watch::channel(0u64);
    let host = Arc::new(Host {
        app: app.clone(),
        state: Mutex::new(EphemeralState::default()),
        render_mu: Mutex::new(()),
        pending_render: Mutex::new(None),
        last_render: Mutex::new(None),
        wake_tx,
        wake_seq: std::sync::atomic::AtomicU64::new(0),
        power_wake_tx,
    });
    if PANEL_HOST.set(host.clone()).is_ok() {
        let _ = app.run_on_main_thread(|| unsafe {
            vela_quick_init(on_swift_action);
        });
        host.spawn_quota_scheduler();
        host.spawn_power_poller();
        host.spawn_wait_ticker();
    }
}

pub fn toggle(app: &tauri::AppHandle) {
    let Some(host) = PANEL_HOST.get().cloned() else {
        return;
    };
    let handle = app.clone();
    let do_toggle = move || {
        if unsafe { vela_quick_is_visible() } {
            host.pending_render.lock().take();
            host.last_render.lock().take();
            let now_visible = unsafe { vela_quick_toggle(std::ptr::null()) };
            VISIBLE.store(now_visible, Ordering::Release);
            if let Some(r) = handle.try_state::<Arc<Runtime>>() {
                r.mark_window_visible("quick", now_visible);
            }
            host.bump_wake();
            return;
        }
        host.prepare();
        let cstr = {
            let _guard = host.render_mu.lock();
            host.pending_render.lock().take();
            let built = host.build_view_model_json();
            VISIBLE.store(true, Ordering::Release);
            built
        };
        let ptr = cstr.as_ref().map_or(std::ptr::null(), |s| s.as_ptr());
        let now_visible = unsafe { vela_quick_toggle(ptr) };
        VISIBLE.store(now_visible, Ordering::Release);
        *host.last_render.lock() = cstr.filter(|_| now_visible);
        if let Some(r) = handle.try_state::<Arc<Runtime>>() {
            r.mark_window_visible("quick", now_visible);
        }
        host.bump_wake();
        host.bump_power_wake();
    };
    if objc2::MainThreadMarker::new().is_some() {
        do_toggle();
    } else {
        let _ = app.run_on_main_thread(do_toggle);
    }
}

pub fn hide(app: &tauri::AppHandle) {
    let handle = app.clone();
    let do_hide = move || {
        let _guard = PANEL_HOST.get().map(|h| h.render_mu.lock());
        VISIBLE.store(false, Ordering::Release);
        if let Some(host) = PANEL_HOST.get() {
            host.pending_render.lock().take();
            host.last_render.lock().take();
        }
        drop(_guard);
        unsafe {
            vela_quick_hide();
        }
        if let Some(r) = handle.try_state::<Arc<Runtime>>() {
            r.mark_window_visible("quick", false);
        }
        if let Some(host) = PANEL_HOST.get() {
            host.bump_wake();
        }
    };
    if objc2::MainThreadMarker::new().is_some() {
        do_hide();
    } else {
        let _ = app.run_on_main_thread(do_hide);
    }
}

pub fn notify_state_changed(_app: &tauri::AppHandle) {
    if let Some(host) = PANEL_HOST.get() {
        if is_visible() {
            host.prepare();
        }
        host.bump_wake();
        host.render_if_visible();
    }
}

/// The instances service says what it shows now. It still holds its state, so
/// only what the panel keeps is touched here.
pub fn instances_changed(state: &InstanceState) {
    let Some(host) = PANEL_HOST.get().cloned() else {
        return;
    };
    host.keep_instances(state);
    tauri::async_runtime::spawn(async move { host.show_instances() });
}

/// A server told of a change. What the panel shows of it is read again; what
/// it says to every window is said here too.
pub fn server_event(instance: &str, event: &str, payload: &Value) {
    let Some(host) = PANEL_HOST.get().cloned() else {
        return;
    };
    if !is_visible() || host.server().as_deref() != Some(instance) {
        return;
    }
    match (event, payload.as_str()) {
        ("switch-notice", Some(message)) => host.set_notice(message.into()),
        ("gateway-state" | "gateway-freeze" | "switch-state" | "provider-quota", _) => {
            host.read_server();
        }
        _ => {}
    }
}

pub fn on_freeze_changed(_app: &tauri::AppHandle) {
    if let Some(host) = PANEL_HOST.get() {
        host.state.lock().freeze_error = None;
        host.bump_wake();
        host.render_if_visible();
    }
}

pub fn push_quota(app: &tauri::AppHandle, client_id: ClientId, quota: QuotaView) {
    if let Some(host) = PANEL_HOST.get() {
        let cur_providers = app
            .try_state::<Arc<Runtime>>()
            .and_then(|r| r.gateway(client_id).try_view().ok())
            .map(|v| v.providers);
        let key = (client_id, quota.provider_id.clone());
        let clear_backoff = quota.state != "loading"
            && (quota.checked_at.is_some()
                || quota.next_refresh_at.is_some()
                || quota.retry_at.is_some());
        let accepted = {
            let mut st = host.state.lock();
            if should_accept_quota_push(cur_providers.as_deref(), st.quotas.get(&key), &quota) {
                if clear_backoff {
                    st.quota_backoff_until.remove(&(
                        client_id,
                        quota.provider_id.clone(),
                        quota.version.clone(),
                    ));
                    st.quota_backoff_until.remove(&(
                        client_id,
                        quota.provider_id.clone(),
                        String::new(),
                    ));
                }
                st.quotas.insert(key, quota);
                true
            } else {
                false
            }
        };
        if accepted {
            host.bump_wake();
            host.render_if_visible();
        }
    }
}

pub fn push_notice(_app: &tauri::AppHandle, message: String) {
    if let Some(host) = PANEL_HOST.get() {
        host.set_notice(message);
    }
}

pub fn push_error(_app: &tauri::AppHandle, message: String) {
    if let Some(host) = PANEL_HOST.get() {
        host.set_error(message);
    }
}

pub fn push_power_state(_app: &tauri::AppHandle, state: power::State) {
    if let Some(host) = PANEL_HOST.get() {
        let mut st = host.state.lock();
        st.power_state = Some(state);
        st.power_read_error = None;
        st.power_error = None;
        drop(st);
        host.render_if_visible();
    }
}

pub fn push_power_error(_app: &tauri::AppHandle, error: Option<String>) {
    if let Some(host) = PANEL_HOST.get() {
        host.state.lock().power_error = error;
        host.render_if_visible();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        events::Reason,
        gateway::{
            circuit::{Cause, CircuitState, Health},
            freeze,
            model_health::ModelHealth,
            quota::{Plan, Subscription, Usage},
            wait_budget::PendingRequest,
            CapacityRetry, HandoffWaiting,
        },
    };

    fn sample_health() -> Health {
        Health {
            state: CircuitState::Closed,
            failures: 0,
            retry_in: 0,
            cooldown_reason: None,
            probe_in_flight: false,
            available: true,
            revision: 1,
            cause: None,
        }
    }

    fn empty_quota(id: &str, version: &str) -> QuotaView {
        QuotaView {
            provider_id: id.into(),
            version: version.into(),
            state: "idle".into(),
            source: None,
            checked_at: None,
            success_at: None,
            retry_at: None,
            next_refresh_at: None,
            stale: false,
            error: None,
            key_status: None,
            plans: Vec::new(),
            subscriptions: Vec::new(),
            expires_at: None,
            expires_at_unix: None,
            today: None,
            total_usage: None,
        }
    }

    fn sample_provider(id: &str, name: &str) -> ProviderView {
        let dir = tempfile::tempdir().unwrap();
        let gw = gateway::Gateway::new(dir.path().to_path_buf()).unwrap();
        let home = dir.path().join("codex");
        std::fs::create_dir_all(&home).unwrap();
        gw.edit(
            gateway::Edit::SaveProvider {
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
                base_url: "https://api.example.invalid/v1".into(),
                token: "sk-test-token".into(),
                quota_access_token: None,
                name: Some(name.into()),
            },
            &gw.try_view().unwrap().revision,
            &home,
        )
        .unwrap();
        let mut p = gw.try_view().unwrap().providers.remove(0);
        p.id = id.into();
        p
    }

    #[test]
    fn freeze_card_derives_tones_labels_and_nonzero_stages() {
        let mut v = freeze::View {
            total: freeze::Status {
                frozen: false,
                draining: 0,
                receiving: 0,
                waiting: 2,
                headers: 0,
                streaming: 1,
                finishing: 0,
                queued: 3,
            },
            partial: false,
            clients: vec![],
        };
        let unfrozen = derive_freeze_card(Some(&v), None, false);
        assert_eq!(unfrozen.tone, "green");
        assert_eq!(unfrozen.label, "未冻结");
        assert_eq!(unfrozen.button_label, "全局冻结");
        assert_eq!(
            unfrozen
                .stages
                .iter()
                .map(|s| (s.name.as_str(), s.count))
                .collect::<Vec<_>>(),
            vec![("等待发送", 2), ("传输中", 1), ("暂存", 3)]
        );

        v.total.frozen = true;
        v.total.draining = 1;
        let draining = derive_freeze_card(Some(&v), None, false);
        assert_eq!(draining.tone, "yellow");
        assert_eq!(draining.label, "等待收尾");
        assert!(draining.disabled);

        v.total.draining = 0;
        let ready = derive_freeze_card(Some(&v), None, false);
        assert_eq!(ready.tone, "ice");
        assert_eq!(ready.label, "可休眠");
        assert_eq!(ready.button_label, "解冻");

        let fault = derive_freeze_card(Some(&v), Some("状态读取失败"), false);
        assert_eq!(fault.tone, "red");
        assert_eq!(fault.label, "状态异常");
        assert_eq!(fault.button_label, "重试");
    }

    #[test]
    fn provider_status_handoff_and_long_waits_match_frontend_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let gw = gateway::Gateway::new(dir.path().to_path_buf()).unwrap();
        let mut view = gw.try_view().unwrap();
        let mut p = sample_provider("p1", "Primary");
        p.queued = true;
        p.health.cause = Some(Cause {
            event_id: "00000000-0000-4000-8000-000000000001".into(),
            code: "STREAM_TIMEOUT".into(),
            upstream_reason: None,
            error_basis: None,
            reason: Reason::UpstreamService,
            status: Some(502),
            ws_close_code: None,
        });
        view.providers = vec![p];
        view.transient_retries = vec![CapacityRetry {
            provider_id: "p1".into(),
            retry_in: 5,
            existing_owner: false,
        }];
        view.handoff_waiting = vec![HandoffWaiting {
            provider_id: "p1".into(),
            state: "awaiting_compaction".into(),
        }];
        view.pending_requests = vec![
            PendingRequest {
                request_id: "r-short".into(),
                provider_id: Some("p1".into()),
                model: Some("claude-sonnet-4-5".into()),
                started_at: 1_700_000_000,
                elapsed_seconds: 58,
                attempt_seconds: 12,
                remaining_seconds: None,
                phase: "buffering".into(),
                received_bytes: Some(735_846),
                bytes_per_second: Some(1_638),
                silent_seconds: Some(4),
            },
            PendingRequest {
                request_id: "r-frozen".into(),
                provider_id: Some("p1".into()),
                model: None,
                started_at: 1_700_000_000,
                elapsed_seconds: 59,
                attempt_seconds: 59,
                remaining_seconds: Some(120),
                phase: "frozen".into(),
                received_bytes: None,
                bytes_per_second: None,
                silent_seconds: None,
            },
            PendingRequest {
                request_id: "r-silent".into(),
                provider_id: Some("p1".into()),
                model: None,
                started_at: 1_700_000_000,
                elapsed_seconds: 90,
                attempt_seconds: 90,
                remaining_seconds: None,
                phase: "buffering".into(),
                received_bytes: Some(512),
                bytes_per_second: Some(0),
                silent_seconds: Some(45),
            },
            PendingRequest {
                request_id: "r-retry".into(),
                provider_id: Some("p1".into()),
                model: None,
                started_at: 1_700_000_000,
                elapsed_seconds: 90,
                attempt_seconds: 20,
                remaining_seconds: Some(30),
                phase: "retry".into(),
                received_bytes: None,
                bytes_per_second: None,
                silent_seconds: None,
            },
        ];

        let card = derive_gateway_card(&view, &HashMap::new(), 1_700_000_100, 3, &HashSet::new());
        assert_eq!(
            card.providers[0].handoff_status.as_deref(),
            Some("待压缩后接管")
        );
        assert_eq!(
            card.providers[0].runtime_alert.as_deref(),
            Some("流超时 · 原线路重试5秒")
        );
        assert_eq!(
            card.providers[0].fault_event_id.as_deref(),
            Some("00000000-0000-4000-8000-000000000001")
        );
        assert_eq!(card.providers[0].priority.as_deref(), Some("P1"));

        // r-short reaches 58 + 3 = 61s (>= 60s) with buffering -> "缓冲响应";
        // r-frozen does not advance elapsed while frozen (stays 59s < 60s).
        // The text counts the attempt in progress, not the whole request.
        let waits = &card.long_waits;
        assert_eq!(
            waits
                .iter()
                .map(|w| w.request_id.as_str())
                .collect::<Vec<_>>(),
            ["r-short", "r-silent", "r-retry"]
        );
        assert_eq!(waits[0].phase_text, "缓冲响应");
        assert_eq!(waits[0].timing_text, "本次 15 秒");
        assert_eq!(waits[2].timing_text, "本次 23 秒 · 剩余 27 秒");

        // A transfer adds what it has received and its rate, or its silence
        // from five seconds on; a request that is not receiving adds nothing.
        assert_eq!(waits[0].received_text.as_deref(), Some("已收 718.6 KB"));
        assert_eq!(waits[0].rate_text.as_deref(), Some("1.6 KB/s"));
        assert_eq!(waits[1].received_text.as_deref(), Some("已收 512 B"));
        assert_eq!(waits[1].rate_text.as_deref(), Some("静默 45 秒"));
        assert_eq!(waits[2].received_text, None);
        assert_eq!(waits[2].rate_text, None);
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");

        // Model-scoped open circuit surfaces stripped gpt-* model label.
        let mut p2 = sample_provider("p2", "Scoped");
        let mut mh = sample_health();
        mh.state = CircuitState::Open;
        mh.available = false;
        mh.retry_in = 12;
        p2.model_health = vec![ModelHealth {
            model: "gpt-5.4-codex".into(),
            health: mh,
        }];
        assert_eq!(provider_status(&p2), "codex · 熔断 · 等待12秒");
    }

    #[test]
    fn quota_card_formats_abbreviations_filters_wallet_placeholders_and_dedups_sub2api() {
        assert!(is_wallet_placeholder_name(Some("钱包余额")));
        assert!(is_wallet_placeholder_name(Some("Wallet Balance")));
        assert!(!is_wallet_placeholder_name(Some("Pro Team")));

        let amount = |remaining: f64, unit: &str| {
            capsule_amount(&Plan {
                remaining: Some(remaining),
                unit: unit.into(),
                ..Plan::default()
            })
        };
        // Two decimals below one; cut off, never rounded up, and not by
        // float error either; no trailing space without a unit.
        assert_eq!(amount(0.917, "USD"), "0.91 USD");
        assert_eq!(amount(1.999, "USD"), "1 USD");
        assert_eq!(amount(0.29, "USD"), "0.29 USD");
        assert_eq!(amount(87_495.917, ""), "87,495");

        let plan = Plan {
            name: "Pro Team".into(),
            remaining: Some(87_495.917),
            used: Some(12_500.0),
            total: Some(100_000.0),
            unit: "USD".into(),
            unlimited: false,
            reset_at: None,
        };
        let q = QuotaView {
            provider_id: "p1".into(),
            version: "v1".into(),
            state: "ok".into(),
            source: Some("sub2api".into()),
            checked_at: Some(1000),
            success_at: Some(1000),
            retry_at: Some(1050),
            next_refresh_at: Some(1060),
            stale: false,
            error: None,
            key_status: None,
            plans: vec![plan.clone()],
            subscriptions: vec![Subscription {
                name: Some("Pro Team".into()),
                status: Some("active".into()),
                expires_at: None,
                active: Some(true),
                plans: vec![plan],
            }],
            expires_at: None,
            expires_at_unix: None,
            today: Some(Usage {
                requests: Some(1200.0),
                tokens: Some(87_500.0),
                cost: Some(3.25),
            }),
            total_usage: None,
        };
        let card = derive_quota_card(Some(&q), 1020);
        assert_eq!(card.tone, "good");
        assert_eq!(card.subscription_label.as_deref(), Some("Pro Team"));
        assert_eq!(card.compact_value, "87,495 USD");
        assert_eq!(card.headline, "剩余 87,495.917 USD");
        assert!(
            card.refresh_disabled,
            "retryAt > now disables manual refresh"
        );
        assert!(
            card.detail.standalone_plans.is_empty(),
            "Sub2API repeated single-subscription summary must be deduplicated"
        );
        assert_eq!(card.detail.usage_rows.len(), 1);
        assert_eq!(card.detail.usage_rows[0].tokens, "87.5K");
    }

    #[test]
    fn quota_tone_tiers_and_gateway_power_switch_follow_their_thresholds() {
        let quota = |remaining: f64, total: f64| QuotaView {
            provider_id: "p1".into(),
            version: "v1".into(),
            state: "ok".into(),
            source: None,
            checked_at: Some(1000),
            success_at: Some(1000),
            retry_at: None,
            next_refresh_at: None,
            stale: false,
            error: None,
            key_status: None,
            plans: vec![Plan {
                name: "Fixture".into(),
                remaining: Some(remaining),
                used: None,
                total: Some(total),
                unit: "USD".into(),
                unlimited: false,
                reset_at: None,
            }],
            subscriptions: vec![],
            expires_at: None,
            expires_at_unix: None,
            today: None,
            total_usage: None,
        };
        for (remaining, tone) in [
            (60.0, "good"),
            (59.0, "normal"),
            (26.0, "normal"),
            (25.0, "warn"),
            (9.0, "warn"),
            (8.0, "critical"),
            (0.0, "critical"),
        ] {
            assert_eq!(
                quota_tone(Some(&quota(remaining, 100.0)), 1000),
                tone,
                "{remaining}"
            );
        }
        // An expired subscription is critical whatever is left.
        let mut expired = quota(90.0, 100.0);
        expired.expires_at_unix = Some(1000);
        assert_eq!(quota_tone(Some(&expired), 999), "good");
        assert_eq!(quota_tone(Some(&expired), 1000), "critical");
        let mut failed = quota(90.0, 100.0);
        failed.state = "error".into();
        assert_eq!(quota_tone(Some(&failed), 1000), "good");
        failed.plans.clear();
        assert_eq!(quota_tone(Some(&failed), 1000), "critical");
        assert_eq!(quota_tone(None, 1000), "neutral");

        // A gateway with no provider still has its switch labelled either way.
        let dir = tempfile::tempdir().unwrap();
        let gw = gateway::Gateway::new(dir.path().to_path_buf()).unwrap();
        let mut view = gw.try_view().unwrap();
        let card = |view: &gateway::View| {
            derive_gateway_card(view, &HashMap::new(), 1000, 0, &HashSet::new())
        };
        view.running = false;
        view.recovery_pending = false;
        view.providers.clear();
        assert_eq!(card(&view).power_label, "启用网关");
        view.running = true;
        let running = card(&view);
        assert_eq!(
            (running.status_label.as_str(), running.power_label.as_str()),
            ("运行中", "停用网关")
        );
    }

    /// The Swift panel decodes exactly what Rust serializes: every model
    /// struct has the same property names on both sides.
    #[test]
    fn swift_models_declare_the_same_properties_as_the_rust_view_model() {
        fn fields(source: &str, open: &str, keyword: &[&str]) -> Vec<String> {
            let start = source
                .find(open)
                .unwrap_or_else(|| panic!("missing {open}"));
            let body = &source[start..];
            let body = &body[..body.find("\n}").expect("struct end")];
            body.lines()
                .skip(1)
                .filter_map(|line| {
                    let line = line.strip_prefix("    ")?;
                    let rest = keyword.iter().find_map(|k| line.strip_prefix(k))?;
                    let (name, kind) = rest.split_once(':')?;
                    // A Swift computed property opens a block; it is not decoded.
                    (!kind.contains('{') && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
                        .then(|| name.to_owned())
                })
                .collect()
        }
        let camel = |name: &str| {
            let mut out = String::new();
            let mut upper = false;
            for c in name.chars() {
                if c == '_' {
                    upper = true;
                } else if std::mem::take(&mut upper) {
                    out.extend(c.to_uppercase());
                } else {
                    out.push(c);
                }
            }
            out
        };
        // The cards are in the model, what the panel adds around them is here.
        let rust = concat!(
            include_str!("quick_model.rs"),
            include_str!("quick_macos.rs")
        );
        let swift = include_str!("../native/quick_panel.swift");
        let names = [
            "FreezeStageModel",
            "FreezeCardModel",
            "ClientTabModel",
            "InstanceTabModel",
            "LongWaitModel",
            "QuotaPlanRowModel",
            "QuotaSubscriptionDetailModel",
            "QuotaUsageRowModel",
            "QuotaDetailModel",
            "QuotaCardModel",
            "ProviderCardModel",
            "GatewayCardModel",
            "ControlsCardModel",
            "QuickPanelViewModel",
        ];
        for name in names {
            let mut ours: Vec<_> = fields(rust, &format!("pub struct {name} {{"), &["pub "])
                .iter()
                .map(|f| camel(f))
                .collect();
            let mut theirs = fields(swift, &format!("struct {name}: "), &["let ", "var "]);
            assert!(!ours.is_empty(), "{name}");
            ours.sort();
            theirs.sort();
            assert_eq!(ours, theirs, "{name}");
        }
        // Every Codable model in the Swift file is covered above.
        let declared = swift
            .lines()
            .filter(|l| l.starts_with("struct ") && l.contains("Codable"))
            .count();
        assert_eq!(declared, names.len());
    }

    #[test]
    fn the_instance_row_appears_with_a_server_and_says_when_it_is_not_connected() {
        let state = |active: &str, status, error: Option<&str>| InstanceState {
            revision: "r".into(),
            active: active.into(),
            machines: vec![instances::Machine {
                id: "m1".into(),
                name: "fixture".into(),
                kind: Default::default(),
                ssh_host: "example.invalid".into(),
                fallback_host: None,
                use_sudo: true,
            }],
            connection: instances::Connection {
                status,
                version: None,
                error: error.map(str::to_owned),
                unsupported: Vec::new(),
            },
        };
        assert_eq!(instance_row(None), (Vec::new(), None, false));
        let alone = InstanceState {
            machines: Vec::new(),
            ..state(instances::LOCAL, instances::Status::Local, None)
        };
        assert_eq!(instance_row(Some(&alone)), (Vec::new(), None, false));

        let (tabs, status, retry) = instance_row(Some(&state(
            "m1",
            instances::Status::Offline,
            Some("fixture failure"),
        )));
        let shown: Vec<_> = tabs
            .iter()
            .map(|tab| (tab.id.as_str(), tab.label.as_str(), tab.selected))
            .collect();
        assert_eq!(
            shown,
            [(instances::LOCAL, "本机", false), ("m1", "fixture", true)]
        );
        assert_eq!(status.as_deref(), Some("离线：fixture failure"));
        assert!(retry);

        let connecting = state("m1", instances::Status::Connecting, None);
        assert_eq!(
            instance_row(Some(&connecting)).1.as_deref(),
            Some("连接中…")
        );
        let connected = state("m1", instances::Status::Connected, None);
        assert_eq!(instance_row(Some(&connected)).1, None);
    }

    #[test]
    fn a_servers_cards_are_shown_with_what_only_this_panel_knows() {
        let snapshot = json!({
            "freeze": { "frozen": false, "disabled": false },
            "gateway": {
                "revision": "r1",
                "longWaits": [
                    { "requestId": "a", "startedAt": 0, "startedAtText": "server", "cancelling": false },
                    { "requestId": "b", "startedAt": 0, "startedAtText": "server", "cancelling": false },
                ],
            },
        });
        let cancelling = HashSet::from(["b".to_owned()]);
        let (freeze, gateway) = server_cards(&snapshot, &cancelling, true);
        assert_eq!(freeze["disabled"], true);
        let waits = &gateway.expect("gateway")["longWaits"];
        // The start is told in this machine's time, not the server's.
        let here = Local.timestamp_opt(0, 0).single().expect("epoch");
        assert_eq!(
            waits[0]["startedAtText"],
            here.format("%H:%M:%S").to_string()
        );
        assert_eq!(waits[0]["cancelling"], false);
        assert_eq!(waits[1]["cancelling"], true);

        let (freeze, gateway) = server_cards(
            &json!({ "freeze": {}, "gateway": null }),
            &cancelling,
            false,
        );
        assert!(freeze.get("disabled").is_none());
        assert!(gateway.is_none());
    }

    #[test]
    fn validators_and_notice_ttl_enforce_exact_boundaries() {
        assert_eq!(validate_provider_name("  OpenAI  ").unwrap(), "OpenAI");
        assert!(validate_provider_name("   ").is_err());
        assert!(validate_provider_name(&"a".repeat(121)).is_err());
        assert!(validate_provider_name("bad\nname").is_err());

        assert_eq!(validate_limit_input("0").unwrap(), 0);
        assert_eq!(validate_limit_input("100000").unwrap(), 100_000);
        assert!(validate_limit_input("100001").is_err());
        assert!(validate_limit_input("-1").is_err());
        assert!(validate_limit_input("12.5").is_err());

        let t0 = Instant::now();
        let notice = Some(("已重置熔断", t0 + NOTICE_TTL));
        assert_eq!(
            active_notice_at(notice, t0 + Duration::from_millis(6499)).as_deref(),
            Some("已重置熔断")
        );
        assert!(active_notice_at(notice, t0 + NOTICE_TTL).is_none());
        assert!(active_notice_at(None, t0).is_none());
    }

    #[test]
    fn quota_scheduler_respects_unsupported_86400_floor_next_refresh_and_retry_at() {
        let mut p_due = sample_provider("due", "Due");
        p_due.quota_version = "v1".into();
        let mut p_unsup = sample_provider("unsup", "Unsupported");
        p_unsup.quota_version = "v1".into();
        let mut p_retry = sample_provider("retry", "Retry");
        p_retry.quota_version = "v1".into();

        let mut cached = HashMap::new();
        cached.insert(
            "unsup".into(),
            QuotaView {
                state: "unsupported".into(),
                checked_at: Some(1_000),
                ..empty_quota("unsup", "v1")
            },
        );
        cached.insert(
            "retry".into(),
            QuotaView {
                state: "error".into(),
                checked_at: Some(1_000),
                next_refresh_at: Some(1_060),
                retry_at: Some(1_200),
                ..empty_quota("retry", "v1")
            },
        );

        let providers = vec![p_due.clone(), p_unsup, p_retry];
        let (due, next_at) = next_quota_schedule_step(
            &providers,
            &cached,
            &HashSet::new(),
            &HashMap::new(),
            60,
            1_100,
        );
        assert_eq!(due, vec![("due".to_owned(), "v1".to_owned())]);
        // retry_at=1200 is earlier than unsupported's 1000 + 86400 = 87400
        assert_eq!(next_at, 1_200);

        // In-flight provider is not re-dispatched while its same-version query is running,
        // whereas editing the provider to v2 dispatches v2 immediately without waiting for v1.
        let in_flight = HashSet::from([("due".to_owned(), "v1".to_owned())]);
        let (due_inflight, next_inflight) =
            next_quota_schedule_step(&providers, &cached, &in_flight, &HashMap::new(), 60, 1_100);
        assert!(due_inflight.is_empty());
        assert_eq!(next_inflight, 1_200);

        let mut p_due_v2 = p_due.clone();
        p_due_v2.quota_version = "v2".into();
        let (due_v2, _) = next_quota_schedule_step(
            &[p_due_v2.clone()],
            &cached,
            &in_flight,
            &HashMap::from([(("due".to_owned(), "v1".to_owned()), 1_500_u64)]),
            60,
            1_100,
        );
        assert_eq!(
            due_v2,
            vec![("due".to_owned(), "v2".to_owned())],
            "new quota_version must not be blocked by stale v1 flight or v1 backoff"
        );
        assert!(!should_accept_quota_push(
            Some(&[p_due_v2.clone()]),
            None,
            &empty_quota("due", "v1"),
        ));
        assert!(should_accept_quota_push(
            Some(&[p_due_v2]),
            Some(&empty_quota("due", "v1")),
            &empty_quota("due", "v2"),
        ));
        assert!(!should_accept_quota_push(
            None,
            Some(&empty_quota("due", "v2")),
            &empty_quota("due", "v1"),
        ));

        // Failed query synthesizes error view with next_refresh_at so scheduler never spins at 0ms.
        let mut prior_ok = empty_quota("due", "v1");
        prior_ok.state = "ok".into();
        prior_ok.success_at = Some(950);
        let err_view =
            synthesize_quota_error_view(&p_due, Some(&prior_ok), "上游连接超时".into(), 60, 1_100);
        assert_eq!(err_view.state, "error");
        assert!(err_view.stale);
        assert_eq!(err_view.error.as_deref(), Some("上游连接超时"));
        assert_eq!(err_view.next_refresh_at, Some(1_160));
        cached.insert("due".into(), err_view);
        let (due_after_err, next_after_err) = next_quota_schedule_step(
            &providers,
            &cached,
            &HashSet::new(),
            &HashMap::new(),
            60,
            1_100,
        );
        assert!(
            due_after_err.is_empty(),
            "error view must back off until next_refresh_at"
        );
        assert_eq!(next_after_err, 1_160);

        // Transient STALE / BUSY backoff floor also suppresses immediate re-query when cached is empty.
        cached.remove("due");
        let backoff = HashMap::from([(("due".to_owned(), "v1".to_owned()), 1_105_u64)]);
        let (due_backoff, next_backoff) =
            next_quota_schedule_step(&providers, &cached, &HashSet::new(), &backoff, 60, 1_100);
        assert!(due_backoff.is_empty());
        assert_eq!(next_backoff, 1_105);

        cached.insert(
            "due".into(),
            QuotaView {
                state: "ok".into(),
                checked_at: Some(1_200),
                next_refresh_at: Some(90_000),
                ..empty_quota("due", "v1")
            },
        );
        let (due2, next_at2) = next_quota_schedule_step(
            &providers,
            &cached,
            &HashSet::new(),
            &HashMap::new(),
            60,
            1_200,
        );
        assert_eq!(due2, vec![("retry".to_owned(), "v1".to_owned())]);
        // unsupported checked_at=1000 + max(60, 86400) = 87_400
        assert_eq!(next_at2, 87_400);
    }

    #[test]
    fn reconcile_startup_error_tracks_async_arrival_and_main_consumption() {
        let mut local_error: Option<String> = None;
        let mut from_startup = false;
        let mut seeded: Option<String> = None;

        // 1. Opening quick before async gateway resume finishes does not lock out later arrival.
        reconcile_startup_error(&mut local_error, &mut from_startup, &mut seeded, None);
        assert!(local_error.is_none());
        assert!(!from_startup);
        assert!(seeded.is_none());

        // 2. Async resume error arrives and seeds the banner.
        reconcile_startup_error(
            &mut local_error,
            &mut from_startup,
            &mut seeded,
            Some("Codex：端口占用"),
        );
        assert_eq!(local_error.as_deref(), Some("Codex：端口占用"));
        assert!(from_startup);

        // 3. Subsequent startup overwrite (e.g. second client resume failure) updates startup banner.
        reconcile_startup_error(
            &mut local_error,
            &mut from_startup,
            &mut seeded,
            Some("Claude：配置损坏"),
        );
        assert_eq!(local_error.as_deref(), Some("Claude：配置损坏"));
        assert!(from_startup);

        // 4. If user triggers an action inside quick that clears local_error while main is still unopened,
        // the same startup error is not re-seeded on subsequent quick renders.
        local_error = None;
        from_startup = false;
        reconcile_startup_error(
            &mut local_error,
            &mut from_startup,
            &mut seeded,
            Some("Claude：配置损坏"),
        );
        assert!(local_error.is_none());
        assert!(!from_startup);

        // 5. If startup banner was active and "main" consumes startup_error, quick clears it immediately.
        let mut local2: Option<String> = None;
        let mut from_startup2 = false;
        let mut seeded2: Option<String> = None;
        reconcile_startup_error(
            &mut local2,
            &mut from_startup2,
            &mut seeded2,
            Some("Claude：配置损坏"),
        );
        assert_eq!(local2.as_deref(), Some("Claude：配置损坏"));
        reconcile_startup_error(&mut local2, &mut from_startup2, &mut seeded2, None);
        assert!(local2.is_none());
        assert!(!from_startup2);

        // 6. Non-startup runtime error is preserved when startup_error is None.
        let mut runtime_err = Some("切换失败".to_owned());
        let mut runtime_from_startup = false;
        reconcile_startup_error(
            &mut runtime_err,
            &mut runtime_from_startup,
            &mut seeded2,
            None,
        );
        assert_eq!(runtime_err.as_deref(), Some("切换失败"));
    }
}
