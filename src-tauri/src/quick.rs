//! Tray panel lifecycle: native NSPanel bridge on macOS, lazy WebView2 + 60s idle destroy on Windows.
use crate::errors::code;
use crate::storage::{self, AppError, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[cfg(any(not(target_os = "macos"), test))]
use std::time::Duration;
use tauri::Emitter;
#[cfg(not(target_os = "macos"))]
use tauri::Manager;

#[cfg(not(target_os = "macos"))]
pub const QUICK_DEFAULT_HEIGHT: f64 = 720.;
#[cfg(any(not(target_os = "macos"), test))]
pub const QUICK_IDLE_DESTROY_DELAY: Duration = Duration::from_secs(60);
#[cfg(not(target_os = "macos"))]
pub const QUICK_READY_FALLBACK: Duration = Duration::from_millis(400);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    pub pinned: bool,
    #[serde(default)]
    pub hidden_clients: Vec<crate::gateway::ClientId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_client: Option<crate::gateway::ClientId>,
}

#[cfg(any(not(target_os = "macos"), test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum QuickWindowPhase {
    #[default]
    Absent,
    Creating,
    Ready,
    Destroying,
}

#[cfg(any(not(target_os = "macos"), test))]
#[derive(Default)]
struct Lifecycle {
    phase: QuickWindowPhase,
    create_generation: u64,
    generation: u64,
    idle_generation: u64,
    want_show: bool,
    focused: bool,
    showing: bool,
    #[cfg(not(target_os = "macos"))]
    pending_anchor: Option<tauri::Rect>,
}

#[cfg(any(not(target_os = "macos"), test))]
impl Lifecycle {
    fn request_show(&mut self) -> QuickWindowPhase {
        self.want_show = true;
        self.idle_generation += 1;
        self.generation += 1;
        self.phase
    }

    fn begin_creating(&mut self) -> Option<u64> {
        if !self.want_show || self.phase != QuickWindowPhase::Absent {
            return None;
        }
        self.phase = QuickWindowPhase::Creating;
        self.create_generation += 1;
        self.generation += 1;
        Some(self.create_generation)
    }

    fn finish_ready(&mut self, expected_gen: Option<u64>) -> bool {
        if self.phase != QuickWindowPhase::Creating {
            return false;
        }
        if expected_gen.is_some_and(|g| g != self.create_generation) {
            return false;
        }
        self.phase = QuickWindowPhase::Ready;
        self.want_show
    }

    fn request_hide(&mut self) -> u64 {
        self.want_show = false;
        self.focused = false;
        self.showing = false;
        self.generation += 1;
        self.idle_generation += 1;
        self.idle_generation
    }

    fn begin_idle_destroy(&mut self, idle_gen: u64) -> bool {
        if self.phase != QuickWindowPhase::Ready
            || self.want_show
            || self.idle_generation != idle_gen
        {
            return false;
        }
        self.phase = QuickWindowPhase::Destroying;
        true
    }

    fn on_destroyed(&mut self) -> bool {
        self.phase = QuickWindowPhase::Absent;
        self.focused = false;
        self.showing = false;
        self.want_show
    }
}

pub struct Panel {
    path: PathBuf,
    preferences: Mutex<Preferences>,
    #[cfg(any(not(target_os = "macos"), test))]
    lifecycle: Mutex<Lifecycle>,
    #[cfg(not(target_os = "macos"))]
    create_lock: tokio::sync::Mutex<()>,
    #[cfg(not(target_os = "macos"))]
    destroyed_notify: tokio::sync::Notify,
}

impl Panel {
    pub fn new(data: &std::path::Path) -> Result<Self> {
        let path = data.join("quick.json");
        let preferences = storage::read_optional(&path)?
            .map(|raw| {
                serde_json::from_slice::<serde_json::Value>(&raw)
                    .and_then(|mut value| {
                        if let Some(clients) = value
                            .get_mut("hiddenClients")
                            .and_then(serde_json::Value::as_array_mut)
                        {
                            clients.retain(|v| v != "agy");
                        }
                        if value.get("selectedClient").and_then(|v| v.as_str()) == Some("agy") {
                            if let Some(obj) = value.as_object_mut() {
                                obj.remove("selectedClient");
                            }
                        }
                        serde_json::from_value(value)
                    })
                    .map_err(|_| AppError::new(code::PANEL, "快捷面板设置无法读取"))
            })
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            path,
            preferences: Mutex::new(preferences),
            #[cfg(any(not(target_os = "macos"), test))]
            lifecycle: Mutex::new(Lifecycle::default()),
            #[cfg(not(target_os = "macos"))]
            create_lock: tokio::sync::Mutex::new(()),
            #[cfg(not(target_os = "macos"))]
            destroyed_notify: tokio::sync::Notify::new(),
        })
    }

    pub fn snapshot(&self) -> Preferences {
        self.preferences.lock().clone()
    }

    pub fn update(
        &self,
        app: &tauri::AppHandle,
        pinned: Option<bool>,
        hidden_clients: Option<Vec<crate::gateway::ClientId>>,
        selected_client: Option<crate::gateway::ClientId>,
    ) -> Result<Preferences> {
        let mut prefs = self.preferences.lock();
        let mut next = prefs.clone();
        if let Some(pinned) = pinned {
            next.pinned = pinned;
        }
        if let Some(hidden) = hidden_clients {
            next.hidden_clients = hidden.into_iter().fold(Vec::new(), |mut ids, id| {
                if !ids.contains(&id) {
                    ids.push(id);
                }
                ids
            });
        }
        if let Some(selected) = selected_client {
            next.selected_client = Some(selected);
        }
        storage::atomic_write(
            &self.path,
            &serde_json::to_vec(&next)
                .map_err(|_| AppError::new(code::PANEL, "无法保存快捷设置"))?,
            None,
        )?;
        *prefs = next.clone();
        drop(prefs);
        let _ = app.emit("quick-state", &next);
        #[cfg(target_os = "macos")]
        crate::quick_macos::notify_state_changed(app);
        Ok(next)
    }

    pub fn is_ready(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            true
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.lifecycle.lock().phase == QuickWindowPhase::Ready
        }
    }

    pub fn show(app: &tauri::AppHandle, anchor: Option<tauri::Rect>) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            let _ = anchor;
            crate::quick_macos::toggle(app);
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let resolved_anchor = anchor
                .or_else(|| {
                    app.tray_by_id("switch")
                        .and_then(|t| t.rect().ok().flatten())
                })
                .ok_or_else(|| AppError::new(code::PANEL, "无法定位托盘图标"))?;
            let panel = app.state::<Panel>();
            let phase = {
                let mut life = panel.lifecycle.lock();
                life.pending_anchor = Some(resolved_anchor);
                if life.phase == QuickWindowPhase::Ready
                    && app.get_webview_window("quick").is_none()
                {
                    life.phase = QuickWindowPhase::Absent;
                }
                life.request_show()
            };
            match phase {
                QuickWindowPhase::Ready => {
                    if let Some(window) = app.get_webview_window("quick") {
                        return Self::present_ready_window(app, &window, resolved_anchor);
                    }
                    Self::spawn_ensure_window(app.clone());
                    Ok(())
                }
                QuickWindowPhase::Creating => Ok(()),
                QuickWindowPhase::Absent | QuickWindowPhase::Destroying => {
                    Self::spawn_ensure_window(app.clone());
                    Ok(())
                }
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn spawn_ensure_window(app: tauri::AppHandle) {
        tauri::async_runtime::spawn(async move {
            let panel = app.state::<Panel>();
            let _guard = panel.create_lock.lock().await;
            let gen = loop {
                let notified = panel.destroyed_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                {
                    let mut life = panel.lifecycle.lock();
                    if !life.want_show {
                        return;
                    }
                    match life.phase {
                        QuickWindowPhase::Destroying => {
                            if app.get_webview_window("quick").is_none() {
                                life.on_destroyed();
                                continue;
                            }
                        }
                        QuickWindowPhase::Creating | QuickWindowPhase::Ready => return,
                        QuickWindowPhase::Absent => {
                            let Some(gen) = life.begin_creating() else {
                                return;
                            };
                            break gen;
                        }
                    }
                }
                let _ = tokio::time::timeout(Duration::from_millis(50), notified).await;
            };

            let bg = crate::window_background_color(&app);
            let build_app = app.clone();
            let built = tauri::async_runtime::spawn_blocking(move || {
                tauri::WebviewWindowBuilder::new(
                    &build_app,
                    "quick",
                    tauri::WebviewUrl::App("index.html?panel=quick".into()),
                )
                .title("Vela 快捷面板")
                .inner_size(420., QUICK_DEFAULT_HEIGHT)
                .decorations(false)
                .resizable(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .visible(false)
                .focused(false)
                .background_color(bg)
                .build()
            })
            .await;

            match built {
                Ok(Ok(_)) => {
                    let fallback_app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        tokio::time::sleep(QUICK_READY_FALLBACK).await;
                        let handle = fallback_app.clone();
                        let _ = fallback_app.run_on_main_thread(move || {
                            Self::finish_ready(&handle, Some(gen));
                        });
                    });
                }
                _ => {
                    app.state::<Panel>().lifecycle.lock().phase = QuickWindowPhase::Absent;
                    crate::emit_switch_error(&app, AppError::new(code::PANEL, "无法创建快捷面板"));
                }
            }
        });
    }

    #[cfg(not(target_os = "macos"))]
    fn finish_ready(app: &tauri::AppHandle, expected_gen: Option<u64>) {
        let panel = app.state::<Panel>();
        let (should_show, anchor) = {
            let mut life = panel.lifecycle.lock();
            let show = life.finish_ready(expected_gen);
            (show, life.pending_anchor)
        };
        if !should_show {
            return;
        }
        let Some(window) = app.get_webview_window("quick") else {
            return;
        };
        let resolved = anchor.or_else(|| {
            app.tray_by_id("switch")
                .and_then(|t| t.rect().ok().flatten())
        });
        let Some(anchor) = resolved else {
            return;
        };
        if let Err(e) = Self::present_ready_window(app, &window, anchor) {
            crate::emit_switch_error(app, e);
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn present_ready_window(
        app: &tauri::AppHandle,
        window: &tauri::WebviewWindow,
        anchor: tauri::Rect,
    ) -> Result<()> {
        let panel = app.state::<Panel>();
        panel.lifecycle.lock().showing = true;
        let result = (|| -> Result<()> {
            position(window, anchor, QUICK_DEFAULT_HEIGHT)?;
            window
                .show()
                .map_err(|_| AppError::new(code::PANEL, "无法显示快捷面板"))?;
            window
                .set_focus()
                .map_err(|_| AppError::new(code::PANEL, "无法聚焦快捷面板"))?;
            let _ = window.emit("panel-visibility", true);
            crate::sync_window_on_show(app, "quick");
            Ok(())
        })();
        let mut life = panel.lifecycle.lock();
        life.showing = false;
        life.focused = window.is_focused().unwrap_or(false);
        result
    }

    pub fn on_destroyed(app: &tauri::AppHandle) {
        #[cfg(target_os = "macos")]
        let _ = app;
        #[cfg(not(target_os = "macos"))]
        {
            let panel = app.state::<Panel>();
            let reopen = panel.lifecycle.lock().on_destroyed();
            panel.destroyed_notify.notify_waiters();
            if reopen {
                Self::spawn_ensure_window(app.clone());
            }
        }
    }

    pub fn focus(app: &tauri::AppHandle, focused: bool) {
        #[cfg(target_os = "macos")]
        let _ = (app, focused);
        #[cfg(not(target_os = "macos"))]
        {
            let panel = app.state::<Panel>();
            let generation = {
                let mut life = panel.lifecycle.lock();
                if life.phase != QuickWindowPhase::Ready
                    || (!focused && (!life.focused || life.showing))
                {
                    return;
                }
                life.generation += 1;
                life.focused = focused;
                life.generation
            };
            if focused {
                return;
            }
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || {
                    let panel = handle.state::<Panel>();
                    if panel.preferences.lock().pinned {
                        return;
                    }
                    let life = panel.lifecycle.lock();
                    if life.phase != QuickWindowPhase::Ready
                        || life.generation != generation
                        || life.focused
                        || life.showing
                    {
                        return;
                    }
                    drop(life);
                    if handle
                        .get_webview_window("quick")
                        .is_some_and(|w| !w.is_focused().unwrap_or(true))
                    {
                        let _ = hide_quick(handle);
                    }
                });
            });
        }
    }
}

#[cfg(any(not(target_os = "macos"), test))]
fn bounds(
    anchor: (f64, f64, f64, f64),
    area: (f64, f64, f64, f64),
    scale: f64,
    height: f64,
) -> (f64, f64, f64, f64) {
    let (ax, ay, aw, ah) = anchor;
    let (x, y, w, h) = area;
    let width = (420. * scale).min(w);
    let height = (height.clamp(128., 720.) * scale).min(h);
    let left = (ax + aw / 2. - width / 2.).clamp(x, x + w - width);
    let top = if ay >= y + h / 2. {
        ay - height - 4. * scale
    } else {
        ay + ah + 4. * scale
    };
    (left, top.clamp(y, y + h - height), width, height)
}

#[cfg(not(target_os = "macos"))]
fn position(window: &tauri::WebviewWindow, anchor: tauri::Rect, height: f64) -> Result<()> {
    let p = anchor.position.to_physical::<f64>(1.);
    let size = anchor.size.to_physical::<f64>(1.);
    let monitors = window
        .available_monitors()
        .map_err(|_| AppError::new(code::PANEL, "无法读取屏幕区域"))?;
    let monitor = monitors
        .iter()
        .find(|m| {
            let mp = m.position();
            let ms = m.size();
            p.x >= mp.x as f64
                && p.x < mp.x as f64 + ms.width as f64
                && p.y >= mp.y as f64
                && p.y < mp.y as f64 + ms.height as f64
        })
        .or_else(|| monitors.first())
        .ok_or_else(|| AppError::new(code::PANEL, "没有可用屏幕"))?;
    let area = monitor.work_area();
    let (x, y, width, height) = bounds(
        (p.x, p.y, size.width, size.height),
        (
            area.position.x as f64,
            area.position.y as f64,
            area.size.width as f64,
            area.size.height as f64,
        ),
        monitor.scale_factor(),
        height,
    );
    window
        .set_size(tauri::PhysicalSize::new(width, height))
        .and_then(|_| window.set_position(tauri::PhysicalPosition::new(x, y)))
        .map_err(|_| AppError::new(code::PANEL, "无法定位快捷面板"))
}

#[tauri::command]
pub fn get_quick(app: tauri::AppHandle, panel: tauri::State<'_, Panel>) -> serde_json::Value {
    let prefs = panel.snapshot();
    #[cfg(target_os = "macos")]
    let visible = {
        let _ = &app;
        crate::quick_macos::is_visible()
    };
    #[cfg(not(target_os = "macos"))]
    let visible = app
        .get_webview_window("quick")
        .is_some_and(|w| w.is_visible().unwrap_or(false));
    let mut out = serde_json::to_value(&prefs).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = out.as_object_mut() {
        obj.insert("visible".into(), serde_json::Value::Bool(visible));
    }
    out
}

#[tauri::command]
pub fn set_quick(
    app: tauri::AppHandle,
    panel: tauri::State<'_, Panel>,
    pinned: Option<bool>,
    hidden_clients: Option<Vec<crate::gateway::ClientId>>,
    selected_client: Option<crate::gateway::ClientId>,
) -> Result<Preferences> {
    panel.update(&app, pinned, hidden_clients, selected_client)
}

#[tauri::command]
pub fn quick_ready(app: tauri::AppHandle) -> Result<()> {
    #[cfg(target_os = "macos")]
    let _ = app;
    #[cfg(not(target_os = "macos"))]
    {
        let handle = app.clone();
        app.run_on_main_thread(move || {
            Panel::finish_ready(&handle, None);
        })
        .map_err(|_| AppError::new(code::PANEL, "无法就绪快捷面板"))?;
    }
    Ok(())
}

#[tauri::command]
pub fn hide_quick(app: tauri::AppHandle) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        crate::quick_macos::hide(&app);
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let idle_gen = app.state::<Panel>().lifecycle.lock().request_hide();
        if let Some(w) = app.get_webview_window("quick") {
            w.hide()
                .map_err(|_| AppError::new(code::PANEL, "无法关闭快捷面板"))?;
            let _ = w.emit("panel-visibility", false);
        }
        if let Some(r) = app.try_state::<std::sync::Arc<crate::Runtime>>() {
            r.mark_window_visible("quick", false);
        }
        let idle_app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(QUICK_IDLE_DESTROY_DELAY).await;
            let handle = idle_app.clone();
            let _ = idle_app.run_on_main_thread(move || {
                let should_destroy = handle
                    .state::<Panel>()
                    .lifecycle
                    .lock()
                    .begin_idle_destroy(idle_gen);
                if !should_destroy {
                    return;
                }
                if let Some(w) = handle.get_webview_window("quick") {
                    if w.destroy().is_err() {
                        Panel::on_destroyed(&handle);
                    }
                } else {
                    Panel::on_destroyed(&handle);
                }
            });
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn panel_fits_secondary_and_scaled_taskbars() {
        for anchor in [
            (2500., 0., 22., 22.),
            (3800., 2100., 44., 44.),
            (-1200., 10., 22., 22.),
        ] {
            let area = if anchor.0 < 0. {
                (-1440., 24., 1440., 876.)
            } else {
                (1920., 48., 1920., 2072.)
            };
            let (x, y, w, h) = bounds(anchor, area, 2., 720.);
            assert!(
                x >= area.0 && y >= area.1 && x + w <= area.0 + area.2 && y + h <= area.1 + area.3
            );
        }
    }
    #[test]
    fn taller_panel_clamps_to_work_area_without_padding_short_content() {
        let anchor = (500., 0., 22., 22.);
        let large = (0., 24., 1440., 900.);
        assert_eq!(bounds(anchor, large, 1., 1000.).3, 720.);
        assert_eq!(bounds(anchor, large, 1., 250.).3, 250.);
        let (x, y, w, h) = bounds(anchor, (0., 24., 760., 480.), 1., 720.);
        assert_eq!(h, 480.);
        assert!(x >= 0. && x + w <= 760. && y >= 24. && y + h <= 504.);
    }

    #[tokio::test(start_paused = true)]
    async fn lazy_lifecycle_handles_ready_warm_reopen_pinned_60s_destroy_and_destroying_reopen() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("quick.json"),
            r#"{"pinned":true,"selectedClient":"claude"}"#,
        )
        .unwrap();
        let panel = std::sync::Arc::new(Panel::new(dir.path()).unwrap());
        assert!(panel.snapshot().pinned);

        // 1. Cold tray click: Absent -> Creating -> quick_ready -> Ready (fallback is a no-op).
        assert_eq!(
            panel.lifecycle.lock().request_show(),
            QuickWindowPhase::Absent
        );
        let gen = panel.lifecycle.lock().begin_creating().unwrap();
        assert_eq!(panel.lifecycle.lock().phase, QuickWindowPhase::Creating);
        assert!(panel.lifecycle.lock().finish_ready(None));
        assert_eq!(panel.lifecycle.lock().phase, QuickWindowPhase::Ready);
        assert!(!panel.lifecycle.lock().finish_ready(Some(gen)));

        // 2. Hide starts 60s timer; warm reopen at 15s cancels that idle destroy.
        let idle_gen_1 = panel.lifecycle.lock().request_hide();
        let p_timer1 = panel.clone();
        let timer1 = tokio::spawn(async move {
            tokio::time::sleep(QUICK_IDLE_DESTROY_DELAY).await;
            p_timer1.lifecycle.lock().begin_idle_destroy(idle_gen_1)
        });
        tokio::time::advance(Duration::from_secs(15)).await;
        assert_eq!(
            panel.lifecycle.lock().request_show(),
            QuickWindowPhase::Ready
        );
        tokio::time::advance(Duration::from_secs(45)).await;
        assert!(
            !timer1.await.unwrap(),
            "warm reopen within 60s must cancel idle destroy"
        );
        assert_eq!(panel.lifecycle.lock().phase, QuickWindowPhase::Ready);

        // 3. Explicit hide for full 60s destroys even when pinned == true (Decision 4a),
        //    while keeping quick.json pinned preference intact.
        let idle_gen_2 = panel.lifecycle.lock().request_hide();
        let p_timer2 = panel.clone();
        let timer2 = tokio::spawn(async move {
            tokio::time::sleep(QUICK_IDLE_DESTROY_DELAY).await;
            p_timer2.lifecycle.lock().begin_idle_destroy(idle_gen_2)
        });
        tokio::time::advance(Duration::from_secs(59)).await;
        tokio::task::yield_now().await;
        assert!(!timer2.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(timer2.await.unwrap());
        assert_eq!(panel.lifecycle.lock().phase, QuickWindowPhase::Destroying);
        assert!(panel.snapshot().pinned);

        // 4. Reopen requested during async Destroying queues want_show until Destroyed arrives.
        assert_eq!(
            panel.lifecycle.lock().request_show(),
            QuickWindowPhase::Destroying
        );
        assert!(
            panel.lifecycle.lock().begin_creating().is_none(),
            "cannot build WebView while previous instance is still Destroying"
        );
        assert!(
            panel.lifecycle.lock().on_destroyed(),
            "Destroyed must report pending reopen"
        );
        assert_eq!(panel.lifecycle.lock().phase, QuickWindowPhase::Absent);
        let gen2 = panel.lifecycle.lock().begin_creating().unwrap();
        // Hide while Creating suppresses show when ready arrives.
        let _ = panel.lifecycle.lock().request_hide();
        assert!(!panel.lifecycle.lock().finish_ready(Some(gen2)));
        assert_eq!(panel.lifecycle.lock().phase, QuickWindowPhase::Ready);
    }
}

#[cfg(test)]
mod preference_tests {
    use super::*;
    #[test]
    fn removed_cli_preferences_preserve_other_choices() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("quick.json"),
            r#"{"pinned":true,"tab":"accounts","hiddenClients":["agy","claude"],"selectedClient":"agy"}"#,
        )
        .unwrap();
        let panel = Panel::new(dir.path()).unwrap();
        let prefs = panel.preferences.lock();
        assert!(prefs.pinned);
        assert_eq!(prefs.hidden_clients, [crate::gateway::ClientId::Claude]);
        assert_eq!(prefs.selected_client, None);
    }
    #[test]
    fn legacy_preferences_default_visible_and_hidden_platforms_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("quick.json"),
            r#"{"pinned":true,"tab":"providers"}"#,
        )
        .unwrap();
        let panel = Panel::new(dir.path()).unwrap();
        let mut prefs = panel.preferences.lock().clone();
        assert!(prefs.hidden_clients.is_empty());
        assert_eq!(prefs.selected_client, None);
        prefs.hidden_clients = vec![crate::gateway::ClientId::Codex];
        prefs.selected_client = Some(crate::gateway::ClientId::Grok);
        std::fs::write(
            dir.path().join("quick.json"),
            serde_json::to_vec(&prefs).unwrap(),
        )
        .unwrap();
        let reopened = Panel::new(dir.path()).unwrap();
        let loaded = reopened.preferences.lock();
        assert!(loaded.pinned);
        assert_eq!(loaded.hidden_clients, prefs.hidden_clients);
        assert_eq!(loaded.selected_client, Some(crate::gateway::ClientId::Grok));
    }
}
