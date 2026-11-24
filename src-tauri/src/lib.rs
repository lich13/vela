// Until the command table arrives, the build without the desktop shell
// compiles the services but not the commands that call them.
#![cfg_attr(not(feature = "desktop"), allow(dead_code, unused_imports))]
#[cfg(all(feature = "desktop", not(any(target_os = "macos", windows))))]
compile_error!("The Vela desktop app supports only macOS and Windows");

mod antigravity;
#[cfg(feature = "desktop")]
use crate::errors::code;
mod auth_cache;
mod claude_profile;
mod cleanup_spool;
#[cfg(feature = "desktop")]
mod commands;
mod configuration;
mod core;
mod download;
mod errors;
mod event_raw;
mod events;
mod exchange_template;
#[cfg(test)]
mod exchange_template_tests;
mod gateway;
#[cfg(feature = "desktop")]
mod links;
mod login;
mod migrations;
mod models_dev;
#[cfg(feature = "desktop")]
mod notifications;
mod official;
#[cfg(feature = "desktop")]
mod power;
#[cfg(feature = "desktop")]
#[cfg(target_os = "macos")]
mod power_macos;
// Ending a client's processes is written for macOS and Windows only.
#[cfg(any(target_os = "macos", windows))]
mod process_control;
mod provider_testing;
#[cfg(feature = "desktop")]
mod quick;
#[cfg(feature = "desktop")]
#[cfg(target_os = "macos")]
mod quick_macos;
mod runtime;
#[cfg(feature = "desktop")]
mod startup;
#[cfg(feature = "desktop")]
#[cfg(target_os = "macos")]
mod startup_macos;
mod storage;
#[cfg(feature = "desktop")]
#[cfg(target_os = "macos")]
mod tray_macos;
#[cfg(feature = "desktop")]
mod update;
mod usage;
#[cfg(feature = "desktop")]
use core::{Preferences, ViewState};
#[cfg(feature = "desktop")]
use parking_lot::Mutex;
pub(crate) use runtime::Runtime;
#[cfg(feature = "desktop")]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use storage::{AppError, Result};
#[cfg(feature = "desktop")]
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};

/// What only the desktop app keeps beside the services: its windows, the
/// sign-ins it hosts and the system features it drives.
#[cfg(feature = "desktop")]
pub(crate) struct Shell {
    imports: Mutex<links::Imports>,
    login: Mutex<login::Session>,
    claude_login: Mutex<claude_profile::login::Session>,
    startup: startup::Service,
    force_quitting: AtomicBool,
    power: power::Service,
    window_visible: [AtomicBool; 2],
    pending_main: PendingMainEvents,
    main_window: Mutex<MainWindowState>,
    main_create_lock: tokio::sync::Mutex<()>,
    main_destroyed: tokio::sync::Notify,
    /// Close requests sent to the frontend and the latest one it acknowledged.
    main_close_seq: std::sync::atomic::AtomicU64,
    main_close_ack: std::sync::atomic::AtomicU64,
}
#[cfg(feature = "desktop")]
impl Shell {
    pub(crate) fn new() -> Self {
        Self {
            imports: Mutex::new(links::Imports::default()),
            login: Mutex::new(Default::default()),
            claude_login: Mutex::new(Default::default()),
            startup: startup::Service::new(),
            force_quitting: AtomicBool::new(false),
            power: power::Service::new(),
            window_visible: [AtomicBool::new(false), AtomicBool::new(false)],
            pending_main: PendingMainEvents::default(),
            main_window: Mutex::new(MainWindowState::default()),
            main_create_lock: tokio::sync::Mutex::new(()),
            main_destroyed: tokio::sync::Notify::new(),
            main_close_seq: Default::default(),
            main_close_ack: Default::default(),
        }
    }
}

#[cfg(feature = "desktop")]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum MainWindowPhase {
    #[default]
    Absent,
    Creating,
    Ready,
    Destroying,
}

#[cfg(feature = "desktop")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MainTarget {
    Page(String),
    ProviderSettings {
        id: String,
        client_id: gateway::ClientId,
        action: ProviderAction,
    },
    EventLog {
        id: String,
        client_id: gateway::ClientId,
        provider_id: Option<String>,
    },
}

#[cfg(feature = "desktop")]
#[derive(Default)]
pub(crate) struct MainWindowState {
    pub(crate) phase: MainWindowPhase,
    pub(crate) generation: u64,
    pub(crate) want_show: bool,
    pub(crate) pending_target: Option<MainTarget>,
    /// The window was shown before its frontend reported ready.
    pub(crate) shown_early: bool,
}

#[cfg(feature = "desktop")]
impl MainWindowState {
    pub(crate) fn request_open(&mut self, target: Option<MainTarget>) -> MainWindowPhase {
        self.want_show = true;
        if let Some(t) = target {
            self.pending_target = Some(t);
        }
        self.phase
    }

    pub(crate) fn begin_creating(&mut self) -> Option<u64> {
        if !self.want_show || self.phase != MainWindowPhase::Absent {
            return None;
        }
        self.phase = MainWindowPhase::Creating;
        self.shown_early = false;
        self.generation += 1;
        Some(self.generation)
    }

    pub(crate) fn finish_ready(&mut self) -> Option<(bool, Option<MainTarget>)> {
        if !matches!(
            self.phase,
            MainWindowPhase::Creating | MainWindowPhase::Ready
        ) {
            return None;
        }
        self.phase = MainWindowPhase::Ready;
        Some((self.want_show, self.pending_target.take()))
    }

    pub(crate) fn begin_destroying(&mut self) -> bool {
        self.want_show = false;
        self.pending_target = None;
        if matches!(
            self.phase,
            MainWindowPhase::Absent | MainWindowPhase::Destroying
        ) {
            return false;
        }
        self.phase = MainWindowPhase::Destroying;
        true
    }

    /// Settles a destroy. A `Destroyed` event that arrives after the state
    /// already moved on belongs to the previous window and changes nothing.
    pub(crate) fn on_destroyed(&mut self) -> Option<bool> {
        if self.phase != MainWindowPhase::Destroying {
            return None;
        }
        self.phase = MainWindowPhase::Absent;
        Some(self.want_show)
    }
}

#[cfg(feature = "desktop")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MainSignal {
    ProviderTests,
    PricingState,
    UsageState,
}
#[cfg(feature = "desktop")]
impl MainSignal {
    pub(crate) const fn event_name(self) -> &'static str {
        match self {
            Self::ProviderTests => "provider-tests",
            Self::PricingState => "pricing-state",
            Self::UsageState => "usage-state",
        }
    }
}

#[cfg(feature = "desktop")]
pub(crate) struct DrainedMainEvents {
    app_event: Option<events::Change>,
    notification_state: Option<notifications::State>,
    provider_tests: bool,
    pricing_state: bool,
    usage_state: bool,
    config_states: Vec<serde_json::Value>,
    antigravity_auth: Option<antigravity::auth::AntigravityAuthState>,
    switch_error: Option<AppError>,
}

#[cfg(feature = "desktop")]
#[derive(Default)]
pub(crate) struct PendingMainEvents {
    app_event: Mutex<Option<events::Change>>,
    notification_state: Mutex<Option<notifications::State>>,
    config_states: Mutex<std::collections::BTreeMap<String, serde_json::Value>>,
    antigravity_auth: Mutex<Option<antigravity::auth::AntigravityAuthState>>,
    switch_error: Mutex<Option<AppError>>,
    provider_tests: AtomicBool,
    pricing_state: AtomicBool,
    usage_state: AtomicBool,
}
#[cfg(feature = "desktop")]
impl PendingMainEvents {
    fn record_switch_error(&self, error: AppError) {
        *self.switch_error.lock() = Some(error);
    }
    fn record_antigravity_auth(&self, state: &antigravity::auth::AntigravityAuthState) {
        let mut slot = self.antigravity_auth.lock();
        let mut next = state.clone();
        next.oauth_added |= slot.as_ref().is_some_and(|s| s.oauth_added);
        *slot = Some(next);
    }
    fn record_signal(&self, signal: MainSignal) {
        match signal {
            MainSignal::ProviderTests => self.provider_tests.store(true, Ordering::Relaxed),
            MainSignal::PricingState => self.pricing_state.store(true, Ordering::Relaxed),
            MainSignal::UsageState => self.usage_state.store(true, Ordering::Relaxed),
        }
    }
    fn record_app_event(&self, change: &events::Change) {
        {
            let mut slot = self.app_event.lock();
            let previous_record = slot.as_ref().and_then(|old| old.record.clone());
            *slot = Some(events::Change {
                record: change.record.clone().or(previous_record),
                error: change.error.clone(),
                notify: false,
            });
        }
    }
    fn record_notification_state(&self, state: notifications::State) {
        {
            let mut slot = self.notification_state.lock();
            *slot = Some(state);
        }
    }
    fn record_config_state(&self, key: &str, payload: serde_json::Value) {
        {
            let mut map = self.config_states.lock();
            map.insert(key.to_owned(), payload);
        }
    }
    fn drain(&self) -> DrainedMainEvents {
        let app_event = self.app_event.lock().take();
        let notification_state = self.notification_state.lock().take();
        let switch_error = self.switch_error.lock().take();
        let provider_tests = self.provider_tests.swap(false, Ordering::Relaxed);
        let pricing_state = self.pricing_state.swap(false, Ordering::Relaxed);
        let usage_state = self.usage_state.swap(false, Ordering::Relaxed);
        let config_states = std::mem::take(&mut *self.config_states.lock())
            .into_values()
            .collect();
        DrainedMainEvents {
            antigravity_auth: self.antigravity_auth.lock().take(),
            app_event,
            notification_state,
            provider_tests,
            pricing_state,
            usage_state,
            config_states,
            switch_error,
        }
    }
    fn drain_for_window(&self, label: &str) -> Option<DrainedMainEvents> {
        (label == "main").then(|| self.drain())
    }
}
#[cfg(feature = "desktop")]
impl Runtime {
    pub(crate) fn mark_window_visible(&self, label: &str, visible: bool) -> bool {
        let slot = match label {
            "main" => &self.shell.window_visible[0],
            "quick" => &self.shell.window_visible[1],
            _ => return visible,
        };
        let was_visible = slot.swap(visible, Ordering::Relaxed);
        if visible && !was_visible {
            self.poll_wake.notify_one();
        }
        was_visible
    }
}

/// The desktop end of the runtime's sink: visible windows, the tray and the
/// native panel.
#[cfg(feature = "desktop")]
struct Host {
    app: tauri::AppHandle,
    /// Whether the lid setting was already closed for the current freeze.
    power_attempted: Mutex<bool>,
    #[cfg(windows)]
    tray_dark: Mutex<Option<bool>>,
}
#[cfg(feature = "desktop")]
impl runtime::Sink for Host {
    fn emit(&self, event: runtime::Event<'_>) {
        use runtime::Event;
        let app = &self.app;
        let name = event.name();
        match &event {
            Event::State(state) => {
                if let Some(tray) = app.tray_by_id("switch") {
                    let _ = tray.set_tooltip(Some(tray_tooltip_text(state)));
                }
                for window in visible_windows(app) {
                    let _ = window.emit(name, state);
                }
                #[cfg(target_os = "macos")]
                quick_macos::notify_state_changed(app);
            }
            Event::Error(error) => emit_switch_error(app, (*error).clone()),
            Event::StartupError(error) => {
                #[cfg(target_os = "macos")]
                quick_macos::notify_state_changed(app);
                let _ = app.emit(name, error);
            }
            Event::Gateway(view) => {
                for window in visible_windows(app) {
                    let _ = window.emit(name, view);
                }
                #[cfg(target_os = "macos")]
                if quick_macos::is_visible() {
                    quick_macos::notify_state_changed(app);
                }
            }
            Event::Freeze(view) => {
                for window in visible_windows(app) {
                    let _ = window.emit(name, view);
                }
                #[cfg(target_os = "macos")]
                quick_macos::on_freeze_changed(app);
                if power::close_after_freeze(
                    &mut self.power_attempted.lock(),
                    view.frozen,
                    view.draining,
                ) {
                    let r = app.state::<Arc<Runtime>>().inner().clone();
                    let gate = r.gateway.freeze_gate();
                    let app_handle = app.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        match r.shell.power.disable_if_enabled(|| {
                            let v = gate.view();
                            v.frozen && v.draining == 0
                        }) {
                            Ok(Some(state)) => {
                                #[cfg(target_os = "macos")]
                                {
                                    quick_macos::push_power_state(&app_handle, state.clone());
                                    quick_macos::push_power_error(&app_handle, None);
                                }
                                let _ = app_handle.emit("clamshell-state", state);
                                let _ =
                                    app_handle.emit("clamshell-error", Option::<AppError>::None);
                            }
                            Ok(None) => {}
                            Err(err) => {
                                #[cfg(target_os = "macos")]
                                quick_macos::push_power_error(
                                    &app_handle,
                                    Some(err.message.clone()),
                                );
                                let _ = app_handle.emit("clamshell-error", Some(err));
                            }
                        }
                    });
                }
            }
            #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
            Event::Quota { client_id, quota } => {
                for window in visible_windows(app) {
                    let _ = window.emit(name, &event);
                }
                #[cfg(target_os = "macos")]
                quick_macos::push_quota(app, *client_id, (*quota).clone());
            }
            Event::Config { key, state } => emit_main_config_state(app, key, (*state).clone()),
            Event::AntigravityAuth(state) => emit_antigravity_auth_state(app, state),
        }
    }
    fn attentive(&self) -> bool {
        has_visible_ui(&self.app)
    }
    fn busy(&self) -> bool {
        self.app.try_state::<Arc<Runtime>>().is_some_and(|r| {
            login_active(&r.shell.login.lock().state)
                || matches!(
                    r.shell.claude_login.lock().state.phase.as_str(),
                    "waiting" | "cancelling"
                )
        })
    }
    fn tick(&self) {
        #[cfg(windows)]
        {
            let dark = tray_is_dark();
            let mut last = self.tray_dark.lock();
            if *last != Some(dark) {
                if let Some(tray) = self.app.tray_by_id("switch") {
                    let bytes = if dark {
                        include_bytes!("../icons/tray-white.png").as_slice()
                    } else {
                        include_bytes!("../icons/tray-black.png").as_slice()
                    };
                    if let Ok(image) = tauri::image::Image::from_bytes(bytes) {
                        if tray.set_icon(Some(image)).is_ok() {
                            *last = Some(dark);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(feature = "desktop")]
#[cfg(target_os = "macos")]
fn application_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    // The native predefined Quit calls NSApplication.terminate directly.
    // Use our command so configuration restoration can finish (or report a conflict).
    let menu = Menu::default(app)?;
    menu.remove_at(0)?;
    let application = tauri::menu::Submenu::with_items(
        app,
        "Vela",
        true,
        &[
            &PredefinedMenuItem::about(app, Some("关于 Vela"), None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "app-quit", "退出 Vela", true, Some("CmdOrCtrl+Q"))?,
        ],
    )?;
    menu.insert(&application, 0)?;
    Ok(menu)
}
#[cfg(feature = "desktop")]
fn login_active(s: &login::LoginState) -> bool {
    ["starting", "waiting", "cancelling"].contains(&s.phase.as_str())
}
#[cfg(feature = "desktop")]
pub(crate) fn visible_windows(app: &tauri::AppHandle) -> Vec<tauri::WebviewWindow> {
    let main_ready = app
        .try_state::<Arc<Runtime>>()
        .is_none_or(|r| r.shell.main_window.lock().phase == MainWindowPhase::Ready);
    let quick_ready = app.try_state::<quick::Panel>().is_none_or(|p| p.is_ready());
    ["main", "quick"]
        .into_iter()
        .filter(|&name| (name == "main" && main_ready) || (name == "quick" && quick_ready))
        .filter_map(|name| app.get_webview_window(name))
        .filter(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
        .collect()
}
#[cfg(feature = "desktop")]
pub(crate) fn has_visible_ui(app: &tauri::AppHandle) -> bool {
    #[cfg(target_os = "macos")]
    if quick_macos::is_visible() {
        return true;
    }
    !visible_windows(app).is_empty()
}
#[cfg(feature = "desktop")]
pub(crate) fn main_visible_window(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    if app
        .try_state::<Arc<Runtime>>()
        .is_some_and(|r| r.shell.main_window.lock().phase != MainWindowPhase::Ready)
    {
        return None;
    }
    app.get_webview_window("main")
        .filter(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
}
#[cfg(feature = "desktop")]
pub(crate) fn emit_switch_notice(app: &tauri::AppHandle, message: impl Into<String>) {
    let message = message.into();
    #[cfg(target_os = "macos")]
    quick_macos::push_notice(app, message.clone());
    let _ = app.emit("switch-notice", message);
}
#[cfg(feature = "desktop")]
pub(crate) fn emit_switch_error(app: &tauri::AppHandle, error: AppError) {
    #[cfg(target_os = "macos")]
    quick_macos::push_error(app, error.message.clone());
    if let Some(r) = app.try_state::<Arc<Runtime>>() {
        if r.shell.main_window.lock().phase != MainWindowPhase::Ready {
            r.shell.pending_main.record_switch_error(error.clone());
        }
    }
    let _ = app.emit("switch-error", error);
}
#[cfg(feature = "desktop")]
pub(crate) fn emit_if_main_visible<S: serde::Serialize + Clone>(
    app: &tauri::AppHandle,
    signal: MainSignal,
    payload: S,
) {
    if let Some(window) = main_visible_window(app) {
        let _ = window.emit(signal.event_name(), payload);
    } else if let Some(r) = app.try_state::<Arc<Runtime>>() {
        r.shell.pending_main.record_signal(signal);
    }
}
#[cfg(feature = "desktop")]
pub(crate) fn emit_main_app_event(app: &tauri::AppHandle, change: &events::Change) {
    if let Some(window) = main_visible_window(app) {
        let _ = window.emit("app-event", change);
    } else if let Some(r) = app.try_state::<Arc<Runtime>>() {
        r.shell.pending_main.record_app_event(change);
    }
}
#[cfg(feature = "desktop")]
pub(crate) fn emit_main_notification_state(app: &tauri::AppHandle, state: notifications::State) {
    if let Some(window) = main_visible_window(app) {
        let _ = window.emit("notification-state", state);
    } else if let Some(r) = app.try_state::<Arc<Runtime>>() {
        r.shell.pending_main.record_notification_state(state);
    }
}
#[cfg(feature = "desktop")]
pub(crate) fn emit_main_config_state(
    app: &tauri::AppHandle,
    client_key: &str,
    payload: serde_json::Value,
) {
    if let Some(window) = main_visible_window(app) {
        let _ = window.emit("config-state", payload);
    } else if let Some(r) = app.try_state::<Arc<Runtime>>() {
        r.shell
            .pending_main
            .record_config_state(client_key, payload);
    }
}
#[cfg(feature = "desktop")]
pub(crate) fn emit_antigravity_auth_state(
    app: &tauri::AppHandle,
    state: &antigravity::auth::AntigravityAuthState,
) {
    if let Some(window) = main_visible_window(app) {
        let _ = window.emit("antigravity-auth-state", state);
        if state.oauth_added {
            let _ = window.emit("switch-notice", "新账号已自动加入切换列表");
        }
    } else if let Some(r) = app.try_state::<Arc<Runtime>>() {
        r.shell.pending_main.record_antigravity_auth(state);
    }
}
#[cfg(feature = "desktop")]
pub(crate) fn sync_window_on_show(app: &tauri::AppHandle, label: &str) {
    let Some(window) = app.get_webview_window(label) else {
        return;
    };
    let Some(r) = app.try_state::<Arc<Runtime>>() else {
        return;
    };
    if label == "main" && r.shell.main_window.lock().phase != MainWindowPhase::Ready {
        return;
    }
    if label == "quick"
        && app
            .try_state::<quick::Panel>()
            .is_some_and(|p| !p.is_ready())
    {
        return;
    }
    let was_visible = r.mark_window_visible(label, true);
    if let Some(drained) = r.shell.pending_main.drain_for_window(label) {
        if let Some(err) = drained.switch_error {
            let _ = window.emit("switch-error", err);
        }
        if let Some(state) = drained.antigravity_auth {
            let _ = window.emit("antigravity-auth-state", &state);
            if state.oauth_added {
                let _ = window.emit("switch-notice", "新账号已自动加入切换列表");
            }
        }
        if let Some(change) = drained.app_event {
            let _ = window.emit("app-event", &change);
        }
        if let Some(state) = drained.notification_state {
            let _ = window.emit("notification-state", state);
        }
        if drained.provider_tests {
            let _ = window.emit(MainSignal::ProviderTests.event_name(), ());
        }
        if drained.pricing_state && was_visible {
            let _ = window.emit(MainSignal::PricingState.event_name(), ());
        }
        if drained.usage_state && was_visible {
            let _ = window.emit(MainSignal::UsageState.event_name(), ());
        }
        for payload in drained.config_states {
            let _ = window.emit("config-state", payload);
        }
    }
    if was_visible {
        return;
    }
    if let Ok(state) = state_for(&r) {
        let _ = window.emit("switch-state", &state);
    }
    for g in [&r.gateway, &r.claude, &r.agy, &r.grok, &r.dsh] {
        if let Ok(view) = g.try_view() {
            let _ = window.emit("gateway-state", &view);
            let client_id = g.client_id();
            for quota in view.providers.iter().filter_map(|p| p.quota.as_ref()) {
                let _ = window.emit(
                    "provider-quota",
                    serde_json::json!({"clientId": client_id, "quota": quota}),
                );
            }
        }
    }
    if label == "main" {
        let _ = window.emit("usage-state", ());
        let _ = window.emit("pricing-state", ());
    }
}
#[cfg(feature = "desktop")]
pub(crate) const fn resolve_window_background_color(
    pref_theme: &str,
    system_dark: bool,
) -> tauri::window::Color {
    let is_dark = match pref_theme.as_bytes() {
        b"dark" => true,
        b"light" => false,
        _ => system_dark,
    };
    if is_dark {
        tauri::window::Color(0x07, 0x0B, 0x14, 0xFF)
    } else {
        tauri::window::Color(0xF3, 0xF6, 0xFC, 0xFF)
    }
}
#[cfg(feature = "desktop")]
#[cfg(windows)]
fn apps_use_dark_theme() -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
        .and_then(|key| key.get_value::<u32, _>("AppsUseLightTheme"))
        .unwrap_or(1)
        == 0
}
#[cfg(feature = "desktop")]
fn system_apps_dark() -> bool {
    #[cfg(target_os = "macos")]
    {
        quick_macos::effective_dark()
    }
    #[cfg(windows)]
    {
        apps_use_dark_theme()
    }
}
#[cfg(feature = "desktop")]
pub(crate) fn window_background_color_for_theme(pref_theme: &str) -> tauri::window::Color {
    resolve_window_background_color(pref_theme, system_apps_dark())
}
#[cfg(feature = "desktop")]
pub(crate) fn window_background_color(app: &tauri::AppHandle) -> tauri::window::Color {
    let pref = app
        .try_state::<Arc<Runtime>>()
        .map(|r| r.core.lock().preferences().theme)
        .unwrap_or_else(|| "system".into());
    window_background_color_for_theme(&pref)
}
#[cfg(feature = "desktop")]
fn emit_main_target(window: &tauri::WebviewWindow, target: MainTarget) {
    match target {
        MainTarget::Page(page) => {
            let _ = window.emit("navigate", page);
        }
        MainTarget::ProviderSettings {
            id,
            client_id,
            action,
        } => {
            let _ = window.emit(
                "provider-settings",
                serde_json::json!({"id": id, "clientId": client_id, "action": action}),
            );
        }
        MainTarget::EventLog {
            id,
            client_id,
            provider_id,
        } => {
            let _ = window.emit(
                "event-log",
                serde_json::json!({"id": id, "clientId": client_id, "providerId": provider_id}),
            );
        }
    }
}
#[cfg(feature = "desktop")]
fn present_main_window(
    app: &tauri::AppHandle,
    window: &tauri::WebviewWindow,
    target: Option<MainTarget>,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Regular)
        .map_err(|_| AppError::new(code::WINDOW, "无法显示主窗口"))?;
    let _ = window.unminimize();
    window
        .show()
        .map_err(|_| AppError::new(code::WINDOW, "无法显示主窗口"))?;
    let _ = window.set_focus();
    let _ = window.emit("app-visibility", true);
    sync_window_on_show(app, "main");
    if let Some(target) = target {
        emit_main_target(window, target);
    }
    Ok(())
}
#[cfg(feature = "desktop")]
async fn ensure_main_window(app: &tauri::AppHandle, r: &Arc<Runtime>) -> Result<()> {
    let _guard = r.shell.main_create_lock.lock().await;
    let mut absent_polls = 0u32;
    let gen = loop {
        let notified = r.shell.main_destroyed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        {
            let mut st = r.shell.main_window.lock();
            if !st.want_show || r.quitting.load(Ordering::Relaxed) {
                return Ok(());
            }
            match st.phase {
                MainWindowPhase::Destroying => {
                    // The Destroyed event normally settles this; only a window
                    // that stays gone for a second without one is settled here.
                    if app.get_webview_window("main").is_none() {
                        absent_polls += 1;
                        if absent_polls >= 20 {
                            st.on_destroyed();
                            continue;
                        }
                    } else {
                        absent_polls = 0;
                    }
                }
                MainWindowPhase::Creating => {
                    let shown_early = st.shown_early;
                    drop(st);
                    if shown_early {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.unminimize();
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    return Ok(());
                }
                MainWindowPhase::Ready => {
                    if let Some(w) = app.get_webview_window("main") {
                        let target = st.pending_target.take();
                        drop(st);
                        return present_main_window(app, &w, target);
                    }
                    st.phase = MainWindowPhase::Absent;
                    continue;
                }
                MainWindowPhase::Absent => {
                    let Some(gen) = st.begin_creating() else {
                        return Ok(());
                    };
                    break gen;
                }
            }
        }
        let _ = tokio::time::timeout(std::time::Duration::from_millis(50), notified).await;
    };

    let bg = window_background_color(app);
    let build_app = app.clone();
    let built = tauri::async_runtime::spawn_blocking(move || {
        tauri::WebviewWindowBuilder::new(
            &build_app,
            "main",
            tauri::WebviewUrl::App("index.html".into()),
        )
        .title("Vela")
        .inner_size(960., 680.)
        .min_inner_size(760., 540.)
        .center()
        .visible(false)
        .background_color(bg)
        .build()
    })
    .await;

    match built {
        Ok(Ok(_)) => {
            let fallback_app = app.clone();
            let fallback_r = r.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                let still_creating = {
                    let mut st = fallback_r.shell.main_window.lock();
                    let creating = st.phase == MainWindowPhase::Creating
                        && st.generation == gen
                        && st.want_show;
                    st.shown_early |= creating;
                    creating
                };
                if still_creating {
                    if let Some(w) = fallback_app.get_webview_window("main") {
                        #[cfg(target_os = "macos")]
                        let _ =
                            fallback_app.set_activation_policy(tauri::ActivationPolicy::Regular);
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                }
            });
            Ok(())
        }
        _ => {
            r.shell.main_window.lock().phase = MainWindowPhase::Absent;
            Err(AppError::new(code::WINDOW, "无法创建主窗口"))
        }
    }
}
#[cfg(feature = "desktop")]
fn destroy_main_window(app: &tauri::AppHandle) -> Result<()> {
    let Some(r) = app.try_state::<Arc<Runtime>>() else {
        return Ok(());
    };
    let previous = {
        let mut st = r.shell.main_window.lock();
        let previous = st.phase;
        if !st.begin_destroying() {
            return Ok(());
        }
        previous
    };
    if let Some(w) = app.get_webview_window("main") {
        if w.destroy().is_err() {
            r.shell.main_window.lock().phase = previous;
            return Err(AppError::new(code::WINDOW, "无法关闭主窗口"));
        }
        r.mark_window_visible("main", false);
        r.grok.cancel_grok_import(None);
        r.core.lock().exchange_clear();
    } else {
        on_main_window_destroyed(app);
    }
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    Ok(())
}
#[cfg(feature = "desktop")]
fn on_main_window_destroyed(app: &tauri::AppHandle) {
    let Some(r) = app.try_state::<Arc<Runtime>>().map(|s| s.inner().clone()) else {
        return;
    };
    let Some(want_show) = r.shell.main_window.lock().on_destroyed() else {
        return;
    };
    r.mark_window_visible("main", false);
    r.grok.cancel_grok_import(None);
    r.core.lock().exchange_clear();
    let reopen = want_show && !r.quitting.load(Ordering::Relaxed);
    r.shell.main_destroyed.notify_waiters();
    if reopen {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = ensure_main_window(&handle, &r).await {
                emit_switch_error(&handle, e);
            }
        });
    } else {
        #[cfg(target_os = "macos")]
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    }
}
#[cfg(feature = "desktop")]
pub(crate) fn show(app: &tauri::AppHandle, page: Option<&str>) -> Result<()> {
    notifications::on_manual_open(app);
    let Some(r) = app.try_state::<Arc<Runtime>>().map(|s| s.inner().clone()) else {
        return Ok(());
    };
    let phase = r
        .shell
        .main_window
        .lock()
        .request_open(page.map(|p| MainTarget::Page(p.to_owned())));
    if phase == MainWindowPhase::Ready {
        if let Some(w) = app.get_webview_window("main") {
            let target = r.shell.main_window.lock().pending_target.take();
            return present_main_window(app, &w, target);
        }
        r.shell.main_window.lock().phase = MainWindowPhase::Absent;
    }
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = ensure_main_window(&handle, &r).await {
            emit_switch_error(&handle, e);
        }
    });
    Ok(())
}
#[cfg(feature = "desktop")]
fn tray_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    for (id, label) in [
        ("open", "打开 Vela"),
        ("config", "编辑配置"),
        ("gateway", "打开网关"),
        ("settings", "设置"),
        ("check_update", "检查更新"),
        ("github", "GitHub"),
        ("quit", "退出 Vela"),
    ] {
        if id == "quit" {
            menu.append(&PredefinedMenuItem::separator(app)?)?;
        }
        menu.append(&MenuItem::with_id(app, id, label, true, None::<&str>)?)?;
    }
    Ok(menu)
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn check_for_updates() -> Result<update::Info> {
    update::latest().await
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn open_github() -> Result<()> {
    update::open_repository()
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn open_update_release() -> Result<()> {
    update::open_releases()
}
#[cfg(feature = "desktop")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ProviderAction {
    #[default]
    Settings,
    Testing,
    Edit,
}
#[cfg(feature = "desktop")]
fn valid_main_page(page: &str) -> bool {
    [
        "accounts", "config", "gateway", "freeze", "logs", "usage", "settings",
    ]
    .contains(&page)
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn open_main(
    app: tauri::AppHandle,
    page: Option<String>,
    provider_id: Option<String>,
    client_id: Option<gateway::ClientId>,
    event_id: Option<String>,
    provider_action: Option<ProviderAction>,
) -> Result<()> {
    if page.as_deref().is_some_and(|p| !valid_main_page(p)) {
        return Err(AppError::new(code::WINDOW, "无效页面"));
    }
    let target = if let Some(id) = event_id {
        uuid::Uuid::parse_str(&id).map_err(|_| AppError::new(code::EVENT, "日志标识无效"))?;
        Some(MainTarget::EventLog {
            id,
            client_id: client_id.unwrap_or_default(),
            provider_id,
        })
    } else if let Some(id) = provider_id {
        Some(MainTarget::ProviderSettings {
            id,
            client_id: client_id.unwrap_or_default(),
            action: provider_action.unwrap_or_default(),
        })
    } else {
        page.map(MainTarget::Page)
    };
    quick::hide_quick(app.clone())?;
    notifications::on_manual_open(&app);
    let Some(r) = app.try_state::<Arc<Runtime>>().map(|s| s.inner().clone()) else {
        return Ok(());
    };
    let phase = r.shell.main_window.lock().request_open(target);
    if phase == MainWindowPhase::Ready {
        if let Some(w) = app.get_webview_window("main") {
            let pending = r.shell.main_window.lock().pending_target.take();
            return present_main_window(&app, &w, pending);
        }
        r.shell.main_window.lock().phase = MainWindowPhase::Absent;
    }
    ensure_main_window(&app, &r).await
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn close_main(app: tauri::AppHandle, window: tauri::WebviewWindow) -> Result<()> {
    if window.label() != "main" {
        return Ok(());
    }
    destroy_main_window(&app)
}
/// The frontend received a close request and now owns the decision.
#[cfg(feature = "desktop")]
#[tauri::command]
fn ack_main_close(window: tauri::WebviewWindow, r: tauri::State<'_, Arc<Runtime>>) {
    if window.label() == "main" {
        r.shell.main_close_ack.store(
            r.shell.main_close_seq.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }
}
#[cfg(feature = "desktop")]
const MAIN_CLOSE_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[cfg(feature = "desktop")]
fn tray_tooltip_text(state: &ViewState) -> String {
    let current = state
        .accounts
        .iter()
        .find(|a| a.current)
        .map(|a| a.name.as_str())
        .unwrap_or("未保存账号");
    format!("Vela · {current}")
}

#[cfg(feature = "desktop")]
fn publish(app: &tauri::AppHandle, state: ViewState) {
    if let Some(r) = app.try_state::<Arc<Runtime>>() {
        r.publish(state);
    }
}
#[cfg(feature = "desktop")]
fn refresh(_: &tauri::AppHandle, r: &Runtime) -> Result<ViewState> {
    r.refresh()
}
#[cfg(feature = "desktop")]
pub(crate) use runtime::coalesce_gateway_events;
#[cfg(feature = "desktop")]
fn publish_gateway(app: &tauri::AppHandle, gateway: &gateway::Gateway) {
    if let Some(r) = app.try_state::<Arc<Runtime>>() {
        r.publish_gateway(gateway);
    }
}
#[cfg(feature = "desktop")]
fn state_for(r: &Runtime) -> Result<ViewState> {
    r.state()
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn list_provider_models(
    client_id: gateway::ClientId,
    r: tauri::State<'_, Arc<Runtime>>,
    provider_id: String,
    force: bool,
) -> Result<gateway::catalog::View> {
    r.gateway(client_id).list_models(&provider_id, force).await
}
/// The upstream error statuses met by the providers that share a host with
/// the given provider, or with the address of one that is not saved yet.
#[cfg(feature = "desktop")]
#[tauri::command]
fn get_provider_seen_statuses(
    client_id: gateway::ClientId,
    r: tauri::State<'_, Arc<Runtime>>,
    provider_id: Option<String>,
    base_url: Option<String>,
) -> Vec<events::SeenStatus> {
    let providers = r
        .gateway(client_id)
        .same_host_providers(provider_id.as_deref(), base_url.as_deref());
    r.diagnostics.seen_statuses(client_id, &providers)
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn get_state(r: tauri::State<'_, Arc<Runtime>>) -> Result<ViewState> {
    state_for(r.inner())
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn switch_account(
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
    id: String,
    expected_revision: String,
) -> Result<ViewState> {
    let _connection_guard = r.official_tx.lock().await;
    let mut core = r.core.lock();
    let official_view = core.official_view();
    if official::blocks_gateway(&r.data) && official_view.account_id.as_deref() != Some(id.as_str())
    {
        return Err(AppError::new(
            code::OFFICIAL_MODE,
            "请先关闭官方账号连接，再切换其他账号",
        ));
    }
    let result = core.switch_account(&id, &expected_revision);
    drop(core);
    match result {
        Ok(s) => {
            publish(&app, s.clone());
            Ok(s)
        }
        Err(e) => {
            let _ = refresh(&app, &r);
            Err(e)
        }
    }
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn use_official_account(
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
    account_id: String,
    expected_auth_revision: String,
    expected_config_revision: String,
) -> Result<ViewState> {
    let _official_guard = r.official_tx.lock().await;
    let state = get_state(r.clone())?;
    let account = state
        .accounts
        .iter()
        .find(|a| a.id == account_id)
        .ok_or_else(|| AppError::new(code::ACCOUNT, "账号不存在"))?;
    if account.kind != "chatgpt" {
        return Err(AppError::new(
            code::OFFICIAL_MODE,
            "只有 ChatGPT 账号可以使用官方连接",
        ));
    }
    if state.official_mode.enabled {
        if state.official_mode.account_id.as_deref() == Some(account_id.as_str()) {
            return Ok(state);
        }
        return Err(AppError::new(
            code::OFFICIAL_MODE,
            "已有官方账号连接，请先关闭",
        ));
    }
    if r.gateway.try_view()?.recovery_pending && !r.gateway.try_view()?.running {
        return Err(AppError::new(
            code::RECOVERY,
            "请先处理 Codex 网关的恢复事务",
        ));
    }
    let gateway_was_running = r.gateway.try_view()?.running;
    if gateway_was_running {
        r.gateway.stop_checked(None).await?;
    }
    let home = r.home(gateway::ClientId::Codex)?;
    let config_revision = r.gateway.read_config(&home)?.revision;
    if !gateway_was_running
        && config_revision != expected_config_revision
        && !expected_config_revision.is_empty()
    {
        return Err(AppError::new(
            code::CONFLICT,
            "Codex 配置已变化，请刷新后重试",
        ));
    }
    let auth_revision = expected_auth_revision.clone();
    official::enable_with_switch(&r.data, &home, &account_id, &config_revision, || {
        r.core
            .lock()
            .switch_account(&account_id, &auth_revision)
            .map(|_| ())
    })?;
    let next = refresh(&app, &r)?;
    emit_switch_notice(&app, "官方账号已启用，请重新打开 Codex");
    Ok(next)
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn disable_official_account(
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<ViewState> {
    let _official_guard = r.official_tx.lock().await;
    let home = r.home(gateway::ClientId::Codex)?;
    official::disable(&r.data, &home)?;
    let next = refresh(&app, &r)?;
    emit_switch_notice(&app, "官方连接已关闭，配置已恢复");
    Ok(next)
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn import_current(app: tauri::AppHandle, r: tauri::State<'_, Arc<Runtime>>) -> Result<ViewState> {
    let mut c = r.core.lock();
    let path = c.home().join("auth.json");
    c.import_file(&path, None)?;
    drop(c);
    refresh(&app, &r)
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn rename_account(
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
    id: String,
    name: String,
) -> Result<ViewState> {
    r.core.lock().rename(&id, &name)?;
    refresh(&app, &r)
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn delete_account(
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
    id: String,
) -> Result<ViewState> {
    {
        let session = r.shell.login.lock();
        if login_active(&session.state)
            && session.state.target_account_id.as_deref() == Some(id.as_str())
        {
            return Err(AppError::new(
                code::LOGIN_BUSY,
                "该账号正在刷新登录，请先取消或等待完成",
            ));
        }
    }
    r.core.lock().delete(&id)?;
    refresh(&app, &r)
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn read_config(
    client_id: gateway::ClientId,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<configuration::Document> {
    r.gateway(client_id).read_config(&r.home(client_id)?)
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn read_previous_config(
    client_id: gateway::ClientId,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<String> {
    r.gateway(client_id).previous_config()
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn validate_config(client_id: gateway::ClientId, text: String) -> Result<()> {
    configuration::validate(client_id, &text)
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn save_config(
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
    text: String,
    expected_revision: String,
    client_id: gateway::ClientId,
) -> Result<configuration::Document> {
    let _official_guard = r.official_tx.lock().await;
    let home = r.home(client_id)?;
    if client_id == gateway::ClientId::Codex {
        official::guard_save(&r.data, &home, &text)?;
    } else if client_id == gateway::ClientId::Claude {
        claude_profile::guard_save(&r.data, &home, &claude_profile::user_home()?, &text)?;
    }
    let doc = r
        .gateway(client_id)
        .save_config(&home, &text, &expected_revision)
        .map_err(|e| r.config_failure(client_id, e))?;
    let _ = app.emit(
        "config-state",
        serde_json::json!({"clientId":client_id,"revision":doc.revision,"guarded":doc.guarded}),
    );
    refresh(&app, &r)?;
    Ok(doc)
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn set_quota_refresh(
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
    seconds: u64,
    expected_seconds: u64,
) -> Result<ViewState> {
    let state = r.core.lock().set_quota_refresh(seconds, expected_seconds)?;
    r.gateway.set_quota_interval(seconds);
    r.claude.set_quota_interval(seconds);
    r.agy.set_quota_interval(seconds);
    r.grok.set_quota_interval(seconds);
    r.dsh.set_quota_interval(seconds);
    publish(&app, state.clone());
    Ok(state)
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn set_preferences(
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
    mut preferences: Preferences,
) -> Result<ViewState> {
    let _official_guard = r.official_tx.lock().await;
    if matches!(
        r.shell.claude_login.lock().state.phase.as_str(),
        "waiting" | "cancelling"
    ) && r.core.lock().preferences().claude_home != preferences.claude_home
    {
        return Err(AppError::new(
            code::LOGIN_BUSY,
            "请先完成或取消 Claude 登录",
        ));
    }
    if login_active(&r.shell.login.lock().state) {
        return Err(AppError::new(code::LOGIN_BUSY, "请先完成或取消登录"));
    }
    if (r.gateway.guarded_home() || official::blocks_gateway(&r.data))
        && r.core.lock().preferences().codex_home != preferences.codex_home
    {
        return Err(AppError::new(
            code::GATEWAY_ACTIVE,
            "更换 Codex 目录前请先停用网关并恢复配置",
        ));
    }
    if (r.claude.guarded_home() || claude_profile::blocks(&r.data))
        && r.core.lock().preferences().claude_home != preferences.claude_home
    {
        return Err(AppError::new(
            code::GATEWAY,
            "请先停止 Claude Code 网关并处理恢复事务",
        ));
    }
    if r.dsh.guarded_home() && r.core.lock().preferences().dsh_home != preferences.dsh_home {
        return Err(AppError::new(
            code::GATEWAY,
            "请先停止 DSH 网关并处理恢复事务",
        ));
    }
    if r.grok.guarded_home() && r.core.lock().preferences().grok_home != preferences.grok_home {
        return Err(AppError::new(
            code::GATEWAY,
            "请先停止 Grok Build 网关并处理恢复事务",
        ));
    }
    let s = {
        let mut core = r.core.lock();
        preferences.quota_refresh_seconds = core.preferences().quota_refresh_seconds;
        core.set_preferences(preferences)?
    };
    r.gateway
        .set_quota_interval(s.preferences.quota_refresh_seconds);
    r.claude
        .set_quota_interval(s.preferences.quota_refresh_seconds);
    r.agy
        .set_quota_interval(s.preferences.quota_refresh_seconds);
    r.grok
        .set_quota_interval(s.preferences.quota_refresh_seconds);
    r.dsh
        .set_quota_interval(s.preferences.quota_refresh_seconds);
    if s.preferences.system_notifications {
        notifications::on_manual_open(&app);
    }
    r.gateway.observe_home(&r.home(gateway::ClientId::Codex)?);
    r.claude.observe_home(&r.home(gateway::ClientId::Claude)?);
    r.grok.observe_home(&r.home(gateway::ClientId::Grok)?);
    r.dsh.observe_home(&r.home(gateway::ClientId::Dsh)?);
    let bg = window_background_color_for_theme(&s.preferences.theme);
    for label in ["main", "quick"] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.set_background_color(Some(bg));
        }
    }
    publish(&app, s.clone());
    Ok(s)
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn get_startup(r: tauri::State<'_, Arc<Runtime>>) -> Result<startup::View> {
    let r = r.inner().clone();
    tauri::async_runtime::spawn_blocking(move || r.shell.startup.view())
        .await
        .map_err(|_| AppError::new(code::STARTUP, "读取启动设置失败"))?
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn set_startup(r: tauri::State<'_, Arc<Runtime>>, enabled: bool) -> Result<startup::View> {
    let r = r.inner().clone();
    tauri::async_runtime::spawn_blocking(move || r.shell.startup.update(enabled))
        .await
        .map_err(|_| AppError::new(code::STARTUP, "更新启动设置失败"))?
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn get_login(r: tauri::State<'_, Arc<Runtime>>) -> Result<login::LoginState> {
    Ok(r.shell.login.lock().state.clone())
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn start_login(
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
    mode: String,
    target_account_id: Option<String>,
    expected_credential_revision: Option<String>,
) -> Result<login::LoginState> {
    if !["browser", "device"].contains(&mode.as_str()) {
        return Err(AppError::new(code::LOGIN_MODE, "登录方式无效"));
    }
    let prefs = r.core.lock().preferences();
    let cli = login::resolve_cli(&prefs.cli_path)?;
    let target = match target_account_id.as_deref() {
        Some(id) if mode == "device" => Some(r.core.lock().login_target(
            id,
            expected_credential_revision.as_deref().unwrap_or_default(),
        )?),
        Some(_) => {
            return Err(AppError::new(
                code::LOGIN_MODE,
                "指定账号刷新请使用设备码登录",
            ))
        }
        None => None,
    };
    let mut session = r.shell.login.lock();
    if login_active(&session.state) {
        return Err(AppError::new(code::LOGIN_BUSY, "已有登录正在进行"));
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    session.cancel = Some(tx);
    session.state = login::LoginState {
        phase: "starting".into(),
        mode: mode.clone(),
        message: "正在启动官方登录…".into(),
        target_account_id: target_account_id.clone(),
        ..Default::default()
    };
    let initial = session.state.clone();
    drop(session);
    r.poll_wake.notify_one();
    let runtime = r.inner().clone();
    tauri::async_runtime::spawn(async move {
        let (sender, mut events) = tokio::sync::mpsc::channel(8);
        let task = login::run(&cli, &mode, rx, sender);
        tokio::pin!(task);
        let result = loop {
            tokio::select! {
                           Some(mut state) = events.recv() => {
                               {
            let mut s = runtime.shell.login.lock();
                                   // Buffered CLI prompts must not revive a cancelled session.
                                   if s.cancel.is_some() {
                                       state.target_account_id = target_account_id.clone();
                                       s.state = state.clone();
                                       let _ = app.emit("login-state", state);
                                   }
                               }
                           },
                           result = &mut task => break result,
                       }
        };
        let _connection_guard = runtime.official_tx.lock().await;
        let mut final_state = match result {
            Ok(Some(raw)) => match (|| -> Result<bool> {
                let s = runtime.shell.login.lock();
                if s.cancel.is_none() {
                    return Err(AppError::new(code::LOGIN_CANCELLED, "登录已取消"));
                }
                let mut c = runtime.core.lock();
                match target.as_ref() {
                    Some(target) => c.complete_target_login(&raw, target),
                    None => c.complete_login(&raw),
                }
            })() {
                Ok(updated) => login::LoginState {
                    phase: "success".into(),
                    mode: mode.clone(),
                    message: if target.is_some() {
                        "指定账号凭据已更新"
                    } else if updated {
                        "当前账号凭据已更新"
                    } else {
                        "账号已添加，选择后即可切换"
                    }
                    .into(),
                    ..Default::default()
                },
                Err(e) => login::LoginState {
                    phase: "error".into(),
                    mode: mode.clone(),
                    message: e.message,
                    ..Default::default()
                },
            },
            Ok(None) => login::LoginState {
                phase: "cancelled".into(),
                mode: mode.clone(),
                message: "登录已取消".into(),
                ..Default::default()
            },
            Err(e) => login::LoginState {
                phase: "error".into(),
                mode: mode.clone(),
                message: e.message,
                ..Default::default()
            },
        };
        final_state.target_account_id = target_account_id;
        {
            let mut s = runtime.shell.login.lock();
            s.state = final_state.clone();
            s.cancel = None;
        }
        let _ = app.emit("login-state", final_state);
        let _ = refresh(&app, &runtime);
        if runtime.quitting.load(Ordering::Relaxed) {
            app.exit(0);
        }
    });
    Ok(initial)
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn cancel_login(r: tauri::State<'_, Arc<Runtime>>) -> Result<()> {
    let mut s = r.shell.login.lock();
    if let Some(tx) = s.cancel.take() {
        let _ = tx.send(());
        s.state.phase = "cancelling".into();
    }
    Ok(())
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn open_login_url(r: tauri::State<'_, Arc<Runtime>>) -> Result<()> {
    let s = r.shell.login.lock();
    let url = s
        .state
        .auth_url
        .as_ref()
        .or(s.state.url.as_ref())
        .ok_or_else(|| AppError::new(code::LOGIN_URL, "登录链接尚未生成"))?;
    open::that(url).map_err(|_| AppError::new(code::OPEN, "无法打开浏览器"))
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn copy_login_value(
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
    kind: login::CopyKind,
) -> Result<()> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    r.shell
        .login
        .lock()
        .copy_value(kind, |value| app.clipboard().write_text(value))
}
#[cfg(feature = "desktop")]
fn quit(app: &tauri::AppHandle, r: &Arc<Runtime>) {
    if r.quit_pending.swap(true, Ordering::Relaxed) {
        return;
    }
    let runtime = r.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = runtime.stop_gateways().await {
            runtime.quit_pending.store(false, Ordering::Relaxed);
            emit_switch_error(&app, e);
            let _ = show(&app, Some("gateway"));
            return;
        }
        runtime.quitting.store(true, Ordering::Relaxed);
        runtime.poll_wake.notify_one();
        {
            let mut s = runtime.shell.login.lock();
            if let Some(tx) = s.cancel.take() {
                let _ = tx.send(());
                return;
            }
            if login_active(&s.state) {
                return;
            }
        }
        app.exit(0);
    });
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn get_gateway_freeze(r: tauri::State<'_, Arc<Runtime>>) -> gateway::freeze::View {
    r.gateway.freeze_gate().view()
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn set_gateway_frozen(
    frozen: bool,
    client_id: Option<gateway::ClientId>,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::freeze::View> {
    let gate = r.gateway.freeze_gate();
    match client_id {
        Some(client) => gate.set_selected(Some(client), frozen),
        None => gate.set(frozen),
    }
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn set_gateway_freeze_resume(
    client_id: gateway::ClientId,
    resume_at: Option<i64>,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::freeze::View> {
    r.gateway
        .freeze_gate()
        .schedule(client_id, resume_at, usage::model::now())
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn get_gateway(
    client_id: gateway::ClientId,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::View> {
    if let Ok(home) = r.home(client_id) {
        r.gateway(client_id).observe_home(&home);
    }
    r.gateway(client_id).try_view()
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn cancel_gateway_wait(
    client_id: gateway::ClientId,
    request_id: String,
    r: tauri::State<'_, Arc<Runtime>>,
) -> &'static str {
    r.gateway(client_id).interrupt_wait(&request_id)
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn get_gateway_models(
    client_id: gateway::ClientId,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::model_list::View> {
    r.gateway(client_id).gateway_models()
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn refresh_gateway_models(
    client_id: gateway::ClientId,
    provider_ids: Option<Vec<String>>,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::model_list::View> {
    r.gateway(client_id)
        .refresh_gateway_models(provider_ids)
        .await
}
/// Downloads OpenAI's Codex catalog now instead of at the day's turn.
#[cfg(feature = "desktop")]
#[tauri::command]
async fn update_codex_catalog(r: tauri::State<'_, Arc<Runtime>>) -> Result<gateway::View> {
    r.gateway.refresh_official_catalog(true).await?;
    r.gateway.try_view()
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn search_models_dev(
    query: String,
    limit: Option<usize>,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<models_dev::Search> {
    Ok(r.usage
        .prices()?
        .models_dev
        .search(&query, limit.unwrap_or(50)))
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn edit_model_list(
    client_id: gateway::ClientId,
    op: gateway::model_list::Operation,
    id: String,
    source: Option<String>,
    context: Option<u64>,
    efforts: Option<Vec<String>>,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::model_list::View> {
    r.gateway(client_id).edit_model_list(
        op,
        &id,
        source.as_deref(),
        gateway::model_list::Custom { context, efforts },
    )
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn dsh_official_models(
    refresh: bool,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::dsh::official::CatalogView> {
    r.dsh
        .dsh_official_models(&r.home(gateway::ClientId::Dsh)?, refresh)
        .await
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn preview_dsh_profile(
    profile: Option<String>,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::dsh::Preview> {
    r.dsh
        .preview_dsh(&r.home(gateway::ClientId::Dsh)?, profile.as_deref())
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn attach_dsh_profiles(
    options: gateway::dsh::Options,
    all: bool,
    expected_revision: String,
    preview_revision: String,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::dsh::Batch> {
    r.dsh
        .attach_dsh_profiles(
            &r.home(gateway::ClientId::Dsh)?,
            options,
            all,
            &expected_revision,
            Some(&preview_revision),
        )
        .await
        .map_err(|e| r.config_failure(gateway::ClientId::Dsh, e))
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn read_dsh_file(
    profile: Option<String>,
    key: gateway::dsh::editor::FileKey,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::dsh::editor::FileView> {
    r.dsh
        .read_dsh_file(&r.home(gateway::ClientId::Dsh)?, profile.as_deref(), key)
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn read_previous_dsh_file(
    profile: Option<String>,
    key: gateway::dsh::editor::FileKey,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<String> {
    r.dsh
        .previous_dsh_file(&r.home(gateway::ClientId::Dsh)?, profile.as_deref(), key)
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn save_dsh_file(
    profile: Option<String>,
    key: gateway::dsh::editor::FileKey,
    text: String,
    expected_revision: String,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::dsh::editor::FileView> {
    r.dsh
        .save_dsh_file(
            &r.home(gateway::ClientId::Dsh)?,
            profile.as_deref(),
            key,
            Some(&text),
            &expected_revision,
        )
        .await
        .map_err(|e| r.config_failure(gateway::ClientId::Dsh, e))
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn restore_dsh_file(
    profile: Option<String>,
    key: gateway::dsh::editor::FileKey,
    expected_revision: String,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::dsh::editor::FileView> {
    r.dsh
        .save_dsh_file(
            &r.home(gateway::ClientId::Dsh)?,
            profile.as_deref(),
            key,
            None,
            &expected_revision,
        )
        .await
        .map_err(|e| r.config_failure(gateway::ClientId::Dsh, e))
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn preview_grok_import(
    selected: Option<String>,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::GrokImportPreview> {
    r.grok
        .preview_grok_import(&r.home(gateway::ClientId::Grok)?, selected.as_deref())
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn cancel_grok_import(token: Option<String>, r: tauri::State<'_, Arc<Runtime>>) {
    r.grok.cancel_grok_import(token.as_deref());
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn commit_grok_import(
    token: String,
    expected_revision: String,
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::View> {
    let _guard = r.official_tx.lock().await;
    let view = r.grok.commit_grok_import(
        &r.home(gateway::ClientId::Grok)?,
        &token,
        &expected_revision,
    )?;
    let result = r
        .grok
        .start(&view.revision, &r.home(gateway::ClientId::Grok)?)
        .await;
    let _ = refresh(&app, &r);
    result
}
#[cfg(feature = "desktop")]
#[tauri::command]
fn reveal_provider_token(
    window: tauri::WebviewWindow,
    client_id: gateway::ClientId,
    provider_id: String,
    expected_revision: String,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<String> {
    r.gateway(client_id)
        .reveal_provider_token(window.label(), &provider_id, &expected_revision)
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn probe_provider_endpoints(
    client_id: gateway::ClientId,
    provider_id: String,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<gateway::View> {
    r.gateway(client_id)
        .probe_endpoints(&provider_id, true)
        .await
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn update_gateway(
    client_id: gateway::ClientId,
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
    edit: gateway::Edit,
    expected_revision: String,
    expected_config_revision: Option<String>,
) -> Result<gateway::View> {
    let _official_guard = r.official_tx.lock().await;
    let home = r.home(client_id)?;
    let selected = matches!(&edit, gateway::Edit::Select { .. });
    let edit = r.gateway(client_id).with_new_provider_defaults(edit);
    let result = r
        .gateway(client_id)
        .edit_checked(
            edit,
            &expected_revision,
            &home,
            expected_config_revision.as_deref(),
        )
        .map_err(|e| r.config_failure(client_id, e))?;
    let _ = refresh(&app, &r);
    if selected {
        emit_switch_notice(
            &app,
            if result.running {
                "已切换供应商，新请求立即生效".to_string()
            } else {
                format!("文件已切换，请重新打开 {}", client_id.name())
            },
        );
    }
    Ok(result)
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn start_gateway(
    client_id: gateway::ClientId,
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
    expected_revision: String,
    expected_config_revision: Option<String>,
) -> Result<gateway::View> {
    let result = start_gateway_for(
        &app,
        &r,
        client_id,
        &expected_revision,
        expected_config_revision.as_deref(),
    )
    .await?;
    emit_switch_notice(&app, format!("网关已启用，请重新打开 {}", client_id.name()));
    Ok(result)
}
/// Starts one client's gateway and refreshes every window. The main window
/// command and the native panel share it; each words its own notice.
#[cfg(feature = "desktop")]
pub(crate) async fn start_gateway_for(
    app: &tauri::AppHandle,
    r: &Arc<Runtime>,
    client_id: gateway::ClientId,
    expected_revision: &str,
    expected_config_revision: Option<&str>,
) -> Result<gateway::View> {
    let _official_guard = r.official_tx.lock().await;
    if client_id == gateway::ClientId::Codex && official::blocks_gateway(&r.data) {
        return Err(AppError::new(
            code::OFFICIAL_MODE,
            "请先关闭官方账号连接，再启动 Codex 网关",
        ));
    }
    let home = r.home(client_id)?;
    let result = r
        .gateway(client_id)
        .start_checked(expected_revision, &home, expected_config_revision)
        .await
        .map_err(|e| r.config_failure(client_id, e))?;
    r.poll_wake.notify_one();
    let _ = refresh(app, r);
    Ok(result)
}
#[cfg(feature = "desktop")]
pub(crate) async fn stop_gateway_for(
    app: &tauri::AppHandle,
    r: &Arc<Runtime>,
    client_id: gateway::ClientId,
    expected_config_revision: Option<&str>,
) -> Result<gateway::View> {
    let _official_guard = r.official_tx.lock().await;
    let result = r
        .gateway(client_id)
        .stop_checked(expected_config_revision)
        .await
        .map_err(|e| r.config_failure(client_id, e))?;
    r.poll_wake.notify_one();
    let _ = refresh(app, r);
    Ok(result)
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn stop_gateway(
    client_id: gateway::ClientId,
    app: tauri::AppHandle,
    r: tauri::State<'_, Arc<Runtime>>,
    expected_config_revision: Option<String>,
) -> Result<gateway::View> {
    let result = stop_gateway_for(&app, &r, client_id, expected_config_revision.as_deref()).await?;
    emit_switch_notice(
        &app,
        format!("已写入当前供应商，请重新打开 {}", client_id.name()),
    );
    Ok(result)
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn query_provider_quota(
    client_id: gateway::ClientId,
    r: tauri::State<'_, Arc<Runtime>>,
    provider_id: String,
    force: bool,
) -> Result<gateway::QuotaView> {
    r.gateway(client_id).query_quota(&provider_id, force).await
}
#[cfg(feature = "desktop")]
#[tauri::command]
async fn frontend_ready(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    r: tauri::State<'_, Arc<Runtime>>,
) -> Result<()> {
    if window.label() != "main" {
        return Ok(());
    }
    let Some((want_show, target)) = r.shell.main_window.lock().finish_ready() else {
        return Ok(());
    };
    if !want_show {
        destroy_main_window(&app)?;
        return Ok(());
    }
    present_main_window(&app, &window, target)
}
#[cfg(feature = "desktop")]
#[cfg(windows)]
fn tray_is_dark() -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
        .and_then(|key| key.get_value::<u32, _>("SystemUsesLightTheme"))
        .unwrap_or(0)
        == 0
}
#[cfg(feature = "desktop")]
pub fn run() {
    // Setup, sync commands and window callbacks all run on this thread, and
    // the services they call spawn onto whichever tokio runtime is current.
    let tokio = tauri::async_runtime::handle();
    let _tokio = tokio.inner().enter();
    let args: Vec<_> = std::env::args().collect();
    let context = tauri::generate_context!();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _| {
            let has_link = args.iter().any(|arg| arg.starts_with("ccswitch:"));
            links::receive(app, args.iter().cloned());
            if !has_link && !args.iter().any(|a| startup::is_login_argument(a)) {
                let _ = show(app, None);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(move |app| {
            let start_silently = startup::login_source(&args);
            let runtime = Runtime::new(Arc::new(Host {
                app: app.handle().clone(),
                power_attempted: Mutex::new(false),
                #[cfg(windows)]
                tray_dark: Mutex::new(None),
            }))?;
            app.manage(quick::Panel::new(&runtime.data)?);
            app.manage(runtime.clone());
            #[cfg(target_os = "macos")]
            quick_macos::connect(app.handle());
            provider_testing::commands::connect(app.handle());
            notifications::connect(app.handle(), &runtime.diagnostics);
            usage::commands::connect(app.handle(), &runtime);
            runtime.start()?;
            {
                let handle = app.handle().clone();
                let r = runtime.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = links::ensure_association(&handle).await {
                        r.record_startup_error(error, false);
                    }
                });
            }
            links::receive(app.handle(), args.iter().cloned());
            #[cfg(target_os = "macos")]
            if start_silently {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
            #[cfg(target_os = "macos")]
            app.set_menu(application_menu(app.handle())?)?;
            if !start_silently {
                let _ = show(app.handle(), None);
            }
            let mut tray = TrayIconBuilder::with_id("switch")
                .menu(&tray_menu(app.handle())?)
                .show_menu_on_left_click(false)
                .tooltip("Vela");
            #[cfg(target_os = "macos")]
            {
                tray = tray
                    .icon(tauri::image::Image::from_bytes(include_bytes!(
                        "../icons/tray.png"
                    ))?)
                    .icon_as_template(true);
            }
            #[cfg(not(target_os = "macos"))]
            {
                let bytes = if cfg!(windows) {
                    #[cfg(windows)]
                    {
                        if tray_is_dark() {
                            include_bytes!("../icons/tray-white.png").as_slice()
                        } else {
                            include_bytes!("../icons/tray-black.png").as_slice()
                        }
                    }
                    #[cfg(not(windows))]
                    {
                        include_bytes!("../icons/tray-white.png").as_slice()
                    }
                } else {
                    include_bytes!("../icons/tray-white.png").as_slice()
                };
                tray = tray.icon(tauri::image::Image::from_bytes(bytes)?);
            }
            let tray = tray
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button,
                        button_state,
                        rect,
                        ..
                    } = event
                    {
                        if button == MouseButton::Left && button_state == MouseButtonState::Up {
                            let app = tray.app_handle().clone();
                            let handle = app.clone();
                            let _ = app.run_on_main_thread(move || {
                                if let Err(e) = quick::Panel::show(&handle, Some(rect)) {
                                    emit_switch_error(&handle, e);
                                }
                            });
                        }
                        #[cfg(target_os = "macos")]
                        if button == MouseButton::Right && button_state == MouseButtonState::Down {
                            if let Err(e) = tray_macos::context_menu(tray) {
                                emit_switch_error(tray.app_handle(), e);
                            }
                        }
                    }
                })
                .on_menu_event(|app, e| match e.id().as_ref() {
                    "quit" => quit(app, &app.state::<Arc<Runtime>>()),
                    "open" => {
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(err) =
                                open_main(handle.clone(), None, None, None, None, None).await
                            {
                                emit_switch_error(&handle, err);
                            }
                        });
                    }
                    "config" | "gateway" | "settings" => {
                        let handle = app.clone();
                        let page = e.id().as_ref().to_owned();
                        tauri::async_runtime::spawn(async move {
                            if let Err(err) =
                                open_main(handle.clone(), Some(page), None, None, None, None).await
                            {
                                emit_switch_error(&handle, err);
                            }
                        });
                    }
                    "github" => {
                        let _ = update::open_repository();
                    }
                    "check_update" => {
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            match update::latest().await {
                                Ok(info) if info.has_update => {
                                    let version =
                                        info.latest_version.as_deref().unwrap_or("latest");
                                    emit_switch_notice(&handle, format!("发现新版本 v{version}"));
                                    let _ = update::open_releases();
                                }
                                Ok(_) => {
                                    emit_switch_notice(&handle, "已是最新版本");
                                }
                                Err(error) => {
                                    emit_switch_error(&handle, error);
                                }
                            }
                        });
                    }
                    _ => (),
                })
                .build(app)?;
            #[cfg(target_os = "macos")]
            tray_macos::install(&tray)?;
            #[cfg(not(target_os = "macos"))]
            let _ = tray;
            Ok(())
        })
        .on_menu_event(|app, event| {
            if event.id().as_ref() == "app-quit" {
                let r = app.state::<Arc<Runtime>>();
                quit(app, &r);
            }
        })
        .on_window_event(|w, event| {
            if matches!(event, tauri::WindowEvent::Destroyed) {
                if w.label() == "quick" {
                    quick::Panel::on_destroyed(w.app_handle());
                } else if w.label() == "main" {
                    on_main_window_destroyed(w.app_handle());
                }
                return;
            }
            let visible = w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false);
            if matches!(event, tauri::WindowEvent::Focused(true))
                || (matches!(event, tauri::WindowEvent::Focused(false)) && !visible)
                || (matches!(event, tauri::WindowEvent::Resized(_))
                    && w.is_minimized().unwrap_or(false))
            {
                let _ = w.emit(
                    if w.label() == "quick" {
                        "panel-visibility"
                    } else {
                        "app-visibility"
                    },
                    visible,
                );
                if visible {
                    sync_window_on_show(w.app_handle(), w.label());
                } else if let Some(r) = w.app_handle().try_state::<Arc<Runtime>>() {
                    r.mark_window_visible(w.label(), false);
                }
            }
            if w.label() == "quick" {
                match event {
                    tauri::WindowEvent::Focused(focused) => {
                        quick::Panel::focus(w.app_handle(), *focused)
                    }
                    tauri::WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        let _ = quick::hide_quick(w.app_handle().clone());
                    }
                    _ => (),
                }
                return;
            }
            if w.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    let r = w.state::<Arc<Runtime>>();
                    if !r.quitting.load(Ordering::Relaxed) {
                        api.prevent_close();
                        let phase = r.shell.main_window.lock().phase;
                        match phase {
                            MainWindowPhase::Ready => {
                                let seq =
                                    r.shell.main_close_seq.fetch_add(1, Ordering::Relaxed) + 1;
                                let _ = w.emit(
                                    "main-close-requested",
                                    serde_json::json!({ "hidden": !visible }),
                                );
                                // A crashed or hung page cannot answer; close anyway.
                                let app = w.app_handle().clone();
                                let r = r.inner().clone();
                                tauri::async_runtime::spawn(async move {
                                    tokio::time::sleep(MAIN_CLOSE_ACK_TIMEOUT).await;
                                    if r.shell.main_close_seq.load(Ordering::Relaxed) == seq
                                        && r.shell.main_close_ack.load(Ordering::Relaxed) < seq
                                    {
                                        let _ = destroy_main_window(&app);
                                    }
                                });
                            }
                            MainWindowPhase::Creating => {
                                let _ = destroy_main_window(w.app_handle());
                            }
                            MainWindowPhase::Absent | MainWindowPhase::Destroying => {}
                        }
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            antigravity::read_antigravity_config,
            antigravity::save_antigravity_config,
            antigravity::read_previous_antigravity_config,
            antigravity::validate_antigravity_config,
            antigravity::cli::read_antigravity_cli_config,
            antigravity::cli::save_antigravity_cli_config,
            antigravity::cli::read_previous_antigravity_cli_config,
            antigravity::cli::validate_antigravity_cli_config,
            configuration::tree::read_config_tree,
            configuration::tree::patch_config_tree,
            antigravity::auth::get_antigravity_auth_state,
            antigravity::auth::switch_antigravity_account,
            antigravity::auth::rename_antigravity_account,
            antigravity::auth::delete_antigravity_account,
            antigravity::auth::export_antigravity_accounts,
            antigravity::auth::import_antigravity_accounts,
            usage::commands::get_usage_state,
            usage::commands::export_usage_bill,
            usage::commands::get_usage_dashboard,
            usage::commands::get_antigravity_usage_stats,
            usage::commands::get_claude_usage_stats,
            usage::commands::get_claude_usage_quota,
            usage::commands::get_usage_logs,
            usage::commands::get_usage_detail,
            usage::commands::set_usage_settings,
            usage::commands::sync_usage,
            usage::commands::get_pricing,
            usage::commands::calculate_usage_cost,
            usage::commands::get_usage_calculator_models,
            usage::commands::get_claude_requested_models,
            usage::commands::get_usage_calculator_totals,
            usage::commands::search_pricing,
            usage::commands::get_models_dev,
            usage::commands::update_models_dev,
            usage::commands::compare_model_prices,
            usage::commands::configure_pricing,
            usage::commands::update_pricing,
            usage::commands::reload_pricing,
            usage::commands::open_pricing_directory,
            provider_testing::commands::provider_test_state,
            provider_testing::commands::provider_test_start,
            provider_testing::commands::provider_test_save_prompt,
            provider_testing::commands::provider_test_set_platform,
            provider_testing::commands::provider_test_query,
            provider_testing::commands::provider_test_cancel,
            provider_testing::commands::provider_test_retry,
            provider_testing::commands::provider_test_set_detection,
            provider_testing::commands::provider_test_clear_mark,
            provider_testing::commands::provider_test_models,
            provider_testing::commands::provider_test_update_bank,
            links::get_provider_imports,
            links::confirm_provider_import,
            links::cancel_provider_import,
            commands::get_clamshell_state,
            commands::set_clamshell_awake,
            commands::install_power_helper,
            commands::remove_power_helper,
            commands::force_quit_agent_clients,
            commands::get_startup_error,
            commands::get_app_events,
            commands::get_app_event,
            commands::get_app_event_raw,
            commands::clear_app_events,
            claude_profile::commands::get_claude_profile,
            claude_profile::commands::switch_claude_profile,
            claude_profile::commands::recover_claude_profile,
            claude_profile::commands::claude_login_status,
            claude_profile::commands::start_claude_login,
            claude_profile::commands::cancel_claude_login,
            notifications::notification_permission,
            notifications::test_notification,
            notifications::open_notification_settings,
            open_main,
            close_main,
            ack_main_close,
            quick::get_quick,
            quick::set_quick,
            quick::quick_ready,
            quick::hide_quick,
            get_gateway,
            get_gateway_freeze,
            set_gateway_frozen,
            set_gateway_freeze_resume,
            cancel_gateway_wait,
            preview_grok_import,
            get_gateway_models,
            refresh_gateway_models,
            update_codex_catalog,
            search_models_dev,
            edit_model_list,
            dsh_official_models,
            preview_dsh_profile,
            attach_dsh_profiles,
            read_dsh_file,
            read_previous_dsh_file,
            save_dsh_file,
            restore_dsh_file,
            cancel_grok_import,
            commit_grok_import,
            reveal_provider_token,
            probe_provider_endpoints,
            update_gateway,
            commands::preview_account_import,
            commands::commit_account_import,
            commands::cancel_account_exchange,
            commands::preview_account_export,
            commands::prepare_account_template,
            commands::select_account_template,
            commands::export_accounts,
            start_gateway,
            stop_gateway,
            query_provider_quota,
            list_provider_models,
            get_provider_seen_statuses,
            get_state,
            switch_account,
            use_official_account,
            disable_official_account,
            import_current,
            rename_account,
            delete_account,
            read_config,
            read_previous_config,
            validate_config,
            save_config,
            set_preferences,
            set_quota_refresh,
            get_startup,
            set_startup,
            get_login,
            start_login,
            cancel_login,
            open_login_url,
            copy_login_value,
            check_for_updates,
            open_github,
            open_update_release,
            frontend_ready
        ])
        .build(context)
        .expect("Vela initialization failed")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = &event {
                links::receive(app, urls.iter().map(|url| url.as_str().to_owned()));
            }
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = &event {
                if !startup::macos_login_event() {
                    let _ = show(app, None);
                }
            }
            if let tauri::RunEvent::ExitRequested { api, code, .. } = &event {
                let r = app.state::<Arc<Runtime>>();
                if !r.quitting.load(Ordering::Relaxed) {
                    api.prevent_exit();
                    if code.is_some() {
                        quit(app, &r);
                    }
                }
            }
            if let tauri::RunEvent::Exit = event {
                let r = app.state::<Arc<Runtime>>();
                // macOS Dock/system termination can bypass ExitRequested.
                // Serialize restoration with any in-flight takeover before returning to the OS.
                let _ = tauri::async_runtime::block_on(r.stop_gateways());
            }
        });
}

#[cfg(feature = "desktop")]
#[cfg(test)]
mod page_whitelist_tests {
    use super::{
        core::Core,
        events, gateway, resolve_window_background_color,
        runtime::{
            emit_account_sync_diagnostic, poll_account_state_step, should_poll_editor_configs,
            wait_next_background_poll, PollStateUpdate, HIDDEN_POLL_INTERVAL,
            VISIBLE_POLL_INTERVAL,
        },
        tray_tooltip_text, valid_main_page, AppError, MainSignal, MainTarget, MainWindowPhase,
        MainWindowState, PendingMainEvents, ProviderAction,
    };
    use parking_lot::Mutex;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn allows_usage_and_all_primary_pages_while_rejecting_unknown_pages() {
        for page in [
            "accounts", "config", "gateway", "freeze", "logs", "usage", "settings",
        ] {
            assert!(valid_main_page(page), "{page} should be allowed");
        }
        assert!(!valid_main_page("invalid"));
    }

    #[test]
    fn pending_main_events_coalesce_while_hidden_and_drain_once_on_show() {
        let pending = PendingMainEvents::default();
        pending.record_signal(MainSignal::ProviderTests);
        pending.record_signal(MainSignal::ProviderTests);
        pending.record_signal(MainSignal::PricingState);
        pending.record_signal(MainSignal::UsageState);
        pending.record_signal(MainSignal::UsageState);
        pending.record_config_state(
            "claude",
            serde_json::json!({"clientId":"claude","revision":"r1","guarded":false}),
        );
        pending.record_config_state(
            "claude",
            serde_json::json!({"clientId":"claude","revision":"r2","guarded":true}),
        );
        pending.record_config_state(
            "antigravity",
            serde_json::json!({"clientId":"antigravity","revision":"a1","guarded":false}),
        );
        pending.record_app_event(&events::Change {
            record: None,
            error: Some("first".into()),
            notify: true,
        });
        pending.record_app_event(&events::Change {
            record: None,
            error: Some("second".into()),
            notify: true,
        });
        pending.record_notification_state(crate::notifications::State {
            permission: "granted".into(),
            error: None,
            delivery: crate::notifications::delivery::Status::Pending,
        });
        pending.record_notification_state(crate::notifications::State {
            permission: "granted".into(),
            error: None,
            delivery: crate::notifications::delivery::Status::Accepted,
        });

        let drained = pending.drain();
        assert!(drained.provider_tests);
        assert!(drained.pricing_state);
        assert!(drained.usage_state);
        assert_eq!(drained.app_event.unwrap().error.as_deref(), Some("second"));
        assert_eq!(
            drained.notification_state,
            Some(crate::notifications::State {
                permission: "granted".into(),
                error: None,
                delivery: crate::notifications::delivery::Status::Accepted,
            })
        );
        assert_eq!(drained.config_states.len(), 2);
        assert!(drained
            .config_states
            .iter()
            .any(|v| v["clientId"] == "claude" && v["revision"] == "r2" && v["guarded"] == true));
        assert!(drained
            .config_states
            .iter()
            .any(|v| v["clientId"] == "antigravity" && v["revision"] == "a1"));

        let drained_again = pending.drain();
        assert!(drained_again.app_event.is_none());
        assert!(drained_again.notification_state.is_none());
        assert!(!drained_again.provider_tests);
        assert!(!drained_again.pricing_state);
        assert!(!drained_again.usage_state);
        assert!(drained_again.config_states.is_empty());
    }

    #[test]
    fn hidden_main_window_buffers_notification_state_without_leaking_to_quick_window() {
        let pending = PendingMainEvents::default();
        pending.record_notification_state(crate::notifications::State {
            permission: "granted".into(),
            error: None,
            delivery: crate::notifications::delivery::Status::Pending,
        });
        pending.record_notification_state(crate::notifications::State {
            permission: "denied".into(),
            error: Some("请在系统设置开启通知".into()),
            delivery: crate::notifications::delivery::Status::Failed,
        });
        pending.record_app_event(&events::Change {
            record: None,
            error: Some("background-notice".into()),
            notify: true,
        });

        // Showing the quick window must not consume notification_state or app_event buffered for main.
        assert!(pending.drain_for_window("quick").is_none());

        // Showing the main window replays the latest notification_state and strips notify=true on app_event.
        let main_drained = pending
            .drain_for_window("main")
            .expect("main window should drain pending notifications");
        assert_eq!(
            main_drained.notification_state,
            Some(crate::notifications::State {
                permission: "denied".into(),
                error: Some("请在系统设置开启通知".into()),
                delivery: crate::notifications::delivery::Status::Failed,
            })
        );
        let replayed_event = main_drained
            .app_event
            .expect("app_event should be replayed");
        assert_eq!(replayed_event.error.as_deref(), Some("background-notice"));
        assert!(
            !replayed_event.notify,
            "replayed hidden app_event must not re-trigger system notifications"
        );
        assert!(pending
            .drain_for_window("main")
            .unwrap()
            .notification_state
            .is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn background_poll_uses_two_speed_interval_and_wakes_immediately_on_window_show() {
        let wake = Arc::new(tokio::sync::Notify::new());

        // Visible mode polls every 2s.
        let wake_visible = wake.clone();
        let visible_wait =
            tokio::spawn(async move { wait_next_background_poll(true, &wake_visible).await });
        tokio::time::advance(Duration::from_millis(1900)).await;
        tokio::task::yield_now().await;
        assert!(!visible_wait.is_finished());
        tokio::time::advance(Duration::from_millis(150)).await;
        assert_eq!(VISIBLE_POLL_INTERVAL, Duration::from_secs(2));
        visible_wait.await.unwrap();

        // Hidden tray-resident mode never hangs forever: it completes after 10s even when idle.
        let wake_hidden = wake.clone();
        let hidden_wait =
            tokio::spawn(async move { wait_next_background_poll(false, &wake_hidden).await });
        tokio::time::advance(Duration::from_secs(9)).await;
        tokio::task::yield_now().await;
        assert!(!hidden_wait.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(HIDDEN_POLL_INTERVAL, Duration::from_secs(10));
        hidden_wait.await.unwrap();

        // Showing a window notifies wake and interrupts the 10s hidden sleep immediately.
        let wake_early = wake.clone();
        let early_wait =
            tokio::spawn(async move { wait_next_background_poll(false, &wake_early).await });
        tokio::task::yield_now().await;
        wake.notify_one();
        tokio::time::timeout(Duration::from_millis(50), early_wait)
            .await
            .expect("window wake should interrupt hidden sleep immediately")
            .unwrap();

        // UI-only editor config stamps skip only when hidden AND idle.
        assert!(!should_poll_editor_configs(false, false));
        assert!(should_poll_editor_configs(true, false));
        assert!(should_poll_editor_configs(false, true));
    }

    #[test]
    fn hidden_poll_step_still_syncs_external_auth_updates_tooltip_and_emits_account_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let home = dir.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();

        let core = Mutex::new(Core::new(data.clone(), home.clone()).unwrap());
        let gateway = gateway::Gateway::new(data.join("gateway")).unwrap();
        let diagnostics = events::Service::new(&data);
        let mut last = None;

        // Initial missing auth.json poll updates state without importing an account.
        match poll_account_state_step(&core, &gateway, &mut last) {
            PollStateUpdate::StateChanged(initial) => {
                assert_eq!(tray_tooltip_text(&initial), "Vela · 未保存账号");
            }
            _ => panic!("expected initial state update"),
        }

        // External terminal login writes a valid API key auth.json while windows remain hidden.
        crate::storage::atomic_write(
            &home.join("auth.json"),
            br#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-fixture-external-login"}"#,
            None,
        )
        .unwrap();
        let synced = loop {
            match poll_account_state_step(&core, &gateway, &mut last) {
                PollStateUpdate::StateChanged(state) => break state,
                PollStateUpdate::ErrorChanged(_) => continue,
                PollStateUpdate::Unchanged => panic!("expected external auth.json sync"),
            }
        };
        assert_eq!(synced.accounts.len(), 1);
        assert!(synced.accounts[0].current);
        assert_eq!(tray_tooltip_text(&synced), "Vela · API Key");
        assert!(!emit_account_sync_diagnostic(&diagnostics, &synced));

        // Corrupting auth.json while hidden triggers AccountSync diagnostic emission.
        crate::storage::atomic_write(&home.join("auth.json"), b"{invalid-json", None).unwrap();
        let broken = loop {
            match poll_account_state_step(&core, &gateway, &mut last) {
                PollStateUpdate::StateChanged(state) => break state,
                PollStateUpdate::ErrorChanged(_) => continue,
                PollStateUpdate::Unchanged => panic!("expected invalid auth.json state update"),
            }
        };
        assert!(emit_account_sync_diagnostic(&diagnostics, &broken));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let page = diagnostics.query(events::Filter::default());
            if page
                .items
                .iter()
                .any(|item| item.record.reason == events::Reason::AccountSync)
            {
                let record = &page
                    .items
                    .iter()
                    .find(|item| item.record.reason == events::Reason::AccountSync)
                    .unwrap()
                    .record;
                assert_eq!(
                    record.details.local_code.as_deref(),
                    Some(crate::errors::code::AUTH)
                );
                assert!(record
                    .details
                    .message
                    .as_ref()
                    .is_some_and(|s| !s.is_empty()));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "AccountSync diagnostic was not persisted"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn window_background_color_prioritizes_saved_theme_over_system() {
        let dark = tauri::window::Color(0x07, 0x0B, 0x14, 0xFF);
        let light = tauri::window::Color(0xF3, 0xF6, 0xFC, 0xFF);
        assert_eq!(resolve_window_background_color("dark", false), dark);
        assert_eq!(resolve_window_background_color("light", true), light);
        assert_eq!(resolve_window_background_color("system", true), dark);
        assert_eq!(resolve_window_background_color("system", false), light);
    }

    #[test]
    fn main_window_state_machine_preserves_target_and_blocks_recreate_until_destroyed() {
        let mut st = MainWindowState::default();
        let target = MainTarget::ProviderSettings {
            id: "p1".into(),
            client_id: gateway::ClientId::Claude,
            action: ProviderAction::Testing,
        };
        assert_eq!(
            st.request_open(Some(target.clone())),
            MainWindowPhase::Absent
        );
        let gen1 = st.begin_creating().expect("should enter Creating");
        assert_eq!(gen1, 1);
        assert_eq!(st.phase, MainWindowPhase::Creating);

        // A bare show(app, None) (e.g. Dock Reopen) while Creating must NOT overwrite target!
        assert_eq!(st.request_open(None), MainWindowPhase::Creating);
        assert_eq!(
            st.finish_ready(),
            Some((true, Some(target))),
            "frontend_ready must receive preserved ProviderSettings target"
        );
        assert_eq!(st.phase, MainWindowPhase::Ready);

        // Destroy and request reopen before WindowEvent::Destroyed completes.
        assert!(st.begin_destroying());
        assert_eq!(st.phase, MainWindowPhase::Destroying);
        let log_target = MainTarget::EventLog {
            id: "00000000-0000-4000-8000-000000000099".into(),
            client_id: gateway::ClientId::Codex,
            provider_id: Some("p1".into()),
        };
        assert_eq!(
            st.request_open(Some(log_target.clone())),
            MainWindowPhase::Destroying
        );
        assert!(
            st.begin_creating().is_none(),
            "must not build second main WebView before Destroyed"
        );
        assert_eq!(
            st.on_destroyed(),
            Some(true),
            "Destroyed must report queued reopen request"
        );
        assert_eq!(st.phase, MainWindowPhase::Absent);
        let gen2 = st
            .begin_creating()
            .expect("should recreate after Destroyed");
        assert_eq!(gen2, 2);
        // A late Destroyed event from the previous window must not reset the new one.
        assert_eq!(st.on_destroyed(), None);
        assert_eq!(st.phase, MainWindowPhase::Creating);
        assert_eq!(st.finish_ready(), Some((true, Some(log_target))));

        // Buffered switch_error drains once for main and never leaks to quick.
        let pending = PendingMainEvents::default();
        pending.record_switch_error(AppError::new("CONFLICT", "恢复冲突"));
        assert!(pending.drain_for_window("quick").is_none());
        let drained = pending.drain_for_window("main").unwrap();
        assert_eq!(
            drained.switch_error.as_ref().map(|e| e.message.as_str()),
            Some("恢复冲突")
        );
        assert!(pending
            .drain_for_window("main")
            .unwrap()
            .switch_error
            .is_none());
    }
}
