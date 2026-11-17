use crate::errors::code;
use crate::{
    power, process_control,
    storage::{AppError, Result},
    Runtime,
};
use std::sync::{atomic::Ordering, Arc};
use tauri::Emitter;
type R<'a> = tauri::State<'a, Arc<Runtime>>;
fn publish_power_result(
    app: &tauri::AppHandle,
    power: &power::Service,
    result: Result<power::State>,
) -> Result<power::State> {
    match &result {
        Ok(state) => {
            #[cfg(target_os = "macos")]
            {
                crate::quick_macos::push_power_state(app, state.clone());
                crate::quick_macos::push_power_error(app, None);
            }
            let _ = app.emit("clamshell-state", state);
            let _ = app.emit("clamshell-error", Option::<AppError>::None);
        }
        Err(err) => {
            if let Ok(state) = power.state() {
                #[cfg(target_os = "macos")]
                crate::quick_macos::push_power_state(app, state.clone());
                let _ = app.emit("clamshell-state", state);
            }
            #[cfg(target_os = "macos")]
            crate::quick_macos::push_power_error(app, Some(err.message.clone()));
        }
    }
    result
}
#[tauri::command]
pub async fn get_clamshell_state(r: R<'_>) -> Result<power::State> {
    let r = r.inner().clone();
    tauri::async_runtime::spawn_blocking(move || r.power.state())
        .await
        .map_err(|_| AppError::new(code::POWER, "电源状态读取失败"))?
}
#[tauri::command]
pub async fn set_clamshell_awake(
    app: tauri::AppHandle,
    r: R<'_>,
    enabled: bool,
    expected_revision: String,
) -> Result<power::State> {
    let r = r.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = r.power.set(enabled, &expected_revision);
        publish_power_result(&app, &r.power, result)
    })
    .await
    .map_err(|_| AppError::new(code::POWER, "电源设置失败"))?
}
#[tauri::command]
pub async fn install_power_helper(app: tauri::AppHandle, r: R<'_>) -> Result<power::State> {
    let r = r.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = r.power.install();
        publish_power_result(&app, &r.power, result)
    })
    .await
    .map_err(|_| AppError::new(code::POWER, "助手安装失败"))?
}
#[tauri::command]
pub async fn remove_power_helper(app: tauri::AppHandle, r: R<'_>) -> Result<power::State> {
    let r = r.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = r.power.remove();
        publish_power_result(&app, &r.power, result)
    })
    .await
    .map_err(|_| AppError::new(code::POWER, "助手移除失败"))?
}
#[tauri::command]
pub async fn force_quit_agent_clients(
    target: process_control::Target,
    r: R<'_>,
) -> Result<process_control::Outcome> {
    if r.force_quitting.swap(true, Ordering::AcqRel) {
        return Err(AppError::new(code::BUSY, "正在退出客户端"));
    }
    let runtime = r.inner().clone();
    let result = tauri::async_runtime::spawn_blocking(move || process_control::force_quit(target))
        .await
        .map_err(|_| AppError::new(code::PROCESS, "退出客户端失败"));
    runtime.force_quitting.store(false, Ordering::Release);
    result?
}
pub(crate) fn consume_startup_error_for_label(
    slot: &parking_lot::Mutex<Option<AppError>>,
    label: &str,
) -> Option<AppError> {
    let mut guard = slot.lock();
    if label == "main" {
        guard.take()
    } else {
        guard.clone()
    }
}
#[tauri::command]
pub fn get_startup_error(window: tauri::WebviewWindow, r: R<'_>) -> Option<AppError> {
    consume_startup_error_for_label(&r.startup_error, window.label())
}

#[tauri::command]
pub async fn get_app_events(
    r: R<'_>,
    filter: crate::events::Filter,
) -> Result<crate::events::Page> {
    use crate::events::{ExternalClient, JournalClient, Origin};
    let service = r.diagnostics.clone();
    let claude_home = r.home(crate::gateway::ClientId::Claude);
    tauri::async_runtime::spawn_blocking(move || {
        // Each external source is read only when the page shows it.
        let wanted = |origin, client| {
            filter.origin.is_none_or(|o| o == origin)
                && filter
                    .client_id
                    .is_none_or(|c| c == JournalClient::External(client))
        };
        if wanted(Origin::Antigravity, ExternalClient::Antigravity) {
            if let Some(home) = dirs::home_dir() {
                service.sync_antigravity_logs(&home);
            }
        }
        if wanted(Origin::ClaudeCode, ExternalClient::ClaudeCode) {
            if let Ok(home) = claude_home {
                service.sync_claude_logs(&home);
            }
        }
        service.query(filter)
    })
    .await
    .map_err(|_| AppError::new(code::LOG_READ, "日志读取失败"))
}
#[tauri::command]
pub fn get_app_event(r: R<'_>, id: String) -> Option<crate::events::ClassifiedRecord> {
    r.diagnostics.detail(&id)
}
#[tauri::command]
pub async fn get_app_event_raw(r: R<'_>, id: String) -> Result<Option<crate::event_raw::View>> {
    let service = r.diagnostics.clone();
    tauri::async_runtime::spawn_blocking(move || service.raw(&id))
        .await
        .map_err(|_| AppError::new(code::LOG_READ, "错误原文读取失败"))?
}
#[tauri::command]
pub async fn clear_app_events(r: R<'_>) -> Result<()> {
    r.diagnostics
        .clear(
            dirs::home_dir(),
            r.home(crate::gateway::ClientId::Claude).ok(),
        )
        .await
}

#[tauri::command]
pub async fn preview_account_import(
    app: tauri::AppHandle,
    r: R<'_>,
) -> Result<Option<crate::core::exchange::Preview>> {
    use tauri_plugin_dialog::DialogExt;
    let r = r.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(files) = app
            .dialog()
            .file()
            .set_title("选择 Codex、Sub2API 或 CLIProxyAPI 账号文件")
            .add_filter("JSON", &["json"])
            .blocking_pick_files()
        else {
            return Ok(None);
        };
        let paths = files
            .into_iter()
            .map(|f| {
                f.into_path()
                    .map_err(|_| AppError::new(code::PATH, "请选择本地 JSON 文件"))
            })
            .collect::<Result<Vec<_>>>()?;
        r.core.lock().exchange_preview(&paths).map(Some)
    })
    .await
    .map_err(|_| AppError::new(code::IMPORT, "账号文件读取失败"))?
}
#[tauri::command]
pub fn cancel_account_exchange(r: R<'_>, token: String) {
    r.core.lock().exchange_cancel(&token);
}
#[tauri::command]
pub async fn commit_account_import(
    app: tauri::AppHandle,
    r: R<'_>,
    token: String,
    selected: Vec<crate::core::exchange::Selection>,
    expected_revision: String,
) -> Result<crate::core::ViewState> {
    let _guard = r.official_tx.lock().await;
    r.core
        .lock()
        .exchange_commit(&token, &selected, &expected_revision)?;
    crate::refresh(&app, &r)
}
#[tauri::command]
pub fn preview_account_export(r: R<'_>) -> crate::core::exchange::ExportPreview {
    let mut core = r.core.lock();
    core.exchange_prune();
    core.exchange_export_preview()
}
#[tauri::command]
pub async fn prepare_account_template(
    app: tauri::AppHandle,
    r: R<'_>,
    ids: Vec<String>,
    replace_template: Option<bool>,
) -> Result<Option<crate::exchange_template::Prepared>> {
    use tauri_plugin_dialog::DialogExt;
    if !replace_template.unwrap_or(false) && r.core.lock().exchange_template_ready(&ids) {
        return Ok(Some(crate::exchange_template::Prepared::ready()));
    }
    let r = r.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(file) = app
            .dialog()
            .file()
            .set_title("选择独立的 Sub2API OAuth 模板")
            .add_filter("JSON", &["json"])
            .blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = file
            .into_path()
            .map_err(|_| AppError::new(code::PATH, "请选择本地 JSON 文件"))?;
        r.core.lock().exchange_prepare_template(&path).map(Some)
    })
    .await
    .map_err(|_| AppError::new(code::EXPORT, "OAuth 模板读取失败"))?
}
#[tauri::command]
pub fn select_account_template(r: R<'_>, token: String, id: usize) -> Result<()> {
    r.core.lock().exchange_select_template(&token, id)
}
#[tauri::command]
pub async fn export_accounts(
    app: tauri::AppHandle,
    r: R<'_>,
    ids: Vec<String>,
    expected_revision: String,
    format: String,
) -> Result<bool> {
    use tauri_plugin_dialog::DialogExt;
    let r = r.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let file = if format == "cliproxyapi" {
            app.dialog()
                .file()
                .set_title("选择 CLIProxyAPI 账号导出目录")
                .blocking_pick_folder()
        } else if format == "sub2api" {
            app.dialog()
                .file()
                .set_file_name("sub2api-oauth-accounts.json")
                .add_filter("JSON", &["json"])
                .blocking_save_file()
        } else {
            return Err(AppError::new(code::EXPORT, "导出格式不支持"));
        };
        let Some(file) = file else {
            return Ok(false);
        };
        let path = file
            .into_path()
            .map_err(|_| AppError::new(code::PATH, "请选择本地路径"))?;
        r.core
            .lock()
            .exchange_export(&ids, &expected_revision, &format, &path)?;
        Ok(true)
    })
    .await
    .map_err(|_| AppError::new(code::EXPORT, "账号导出失败"))?
}

#[cfg(test)]
mod startup_error_tests {
    use super::*;
    use parking_lot::Mutex;

    #[test]
    fn quick_reads_preserve_startup_error_until_main_consumes_it_once() {
        let slot = Mutex::new(Some(AppError::new(code::GATEWAY, "端口占用")));
        assert_eq!(
            consume_startup_error_for_label(&slot, "quick")
                .as_ref()
                .map(|e| e.message.as_str()),
            Some("端口占用")
        );
        assert_eq!(
            consume_startup_error_for_label(&slot, "main")
                .as_ref()
                .map(|e| e.message.as_str()),
            Some("端口占用")
        );
        assert!(consume_startup_error_for_label(&slot, "main").is_none());
        assert!(consume_startup_error_for_label(&slot, "quick").is_none());
    }
}
