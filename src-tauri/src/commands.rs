use crate::errors::code;
use crate::{
    core::exchange,
    instances::on,
    power, process_control,
    storage::{self, AppError, Result},
    Runtime,
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
};
use tauri::Emitter;
#[cfg(target_os = "macos")]
use tauri::Manager;
type R<'a> = tauri::State<'a, Arc<Runtime>>;
type Instances<'a> = tauri::State<'a, Arc<crate::instances::Service>>;
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
        #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
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
    tauri::async_runtime::spawn_blocking(move || r.shell.power.state())
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
    set_clamshell(&app, r.inner(), enabled, expected_revision).await
}
/// Changes the lid setting and publishes the outcome to every window; the
/// main window command and the native panel both come through here.
pub(crate) async fn set_clamshell(
    app: &tauri::AppHandle,
    r: &Arc<crate::Runtime>,
    enabled: bool,
    expected_revision: String,
) -> Result<power::State> {
    let (app, r) = (app.clone(), r.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let result = r.shell.power.set(enabled, &expected_revision);
        publish_power_result(&app, &r.shell.power, result)
    })
    .await
    .map_err(|_| AppError::new(code::POWER, "电源设置失败"))?
}
#[tauri::command]
pub async fn install_power_helper(app: tauri::AppHandle, r: R<'_>) -> Result<power::State> {
    let r = r.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = r.shell.power.install();
        publish_power_result(&app, &r.shell.power, result)
    })
    .await
    .map_err(|_| AppError::new(code::POWER, "助手安装失败"))?
}
#[tauri::command]
pub async fn remove_power_helper(app: tauri::AppHandle, r: R<'_>) -> Result<power::State> {
    let r = r.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = r.shell.power.remove();
        publish_power_result(&app, &r.shell.power, result)
    })
    .await
    .map_err(|_| AppError::new(code::POWER, "助手移除失败"))?
}
#[tauri::command]
pub async fn force_quit_agent_clients(
    target: process_control::Target,
    r: R<'_>,
) -> Result<process_control::Outcome> {
    force_quit(r.inner(), target).await
}
/// Ends the chosen client's processes, one request at a time.
pub(crate) async fn force_quit(
    r: &Arc<crate::Runtime>,
    target: process_control::Target,
) -> Result<process_control::Outcome> {
    if r.shell.force_quitting.swap(true, Ordering::AcqRel) {
        return Err(AppError::new(code::BUSY, "正在退出客户端"));
    }
    let result = tauri::async_runtime::spawn_blocking(move || process_control::force_quit(target))
        .await
        .map_err(|_| AppError::new(code::PROCESS, "退出客户端失败"));
    r.shell.force_quitting.store(false, Ordering::Release);
    result?
}
pub(crate) fn take_startup_error_for_label(
    slot: &parking_lot::Mutex<Option<AppError>>,
    consumed_by_main: &std::sync::atomic::AtomicBool,
    label: &str,
) -> Option<AppError> {
    let mut guard = slot.lock();
    if label == "main" {
        consumed_by_main.store(true, Ordering::Release);
        guard.take()
    } else {
        guard.clone()
    }
}
#[tauri::command]
pub fn get_startup_error(window: tauri::WebviewWindow, r: R<'_>) -> Option<AppError> {
    let label = window.label();
    let out = take_startup_error_for_label(&r.startup_error, &r.startup_error_consumed, label);
    #[cfg(target_os = "macos")]
    if label == "main" && out.is_some() {
        crate::quick_macos::notify_state_changed(window.app_handle());
    }
    out
}

/// The documents of the files of one import, read on this machine whichever
/// instance takes them in.
fn read_account_documents(paths: &[PathBuf]) -> Result<Vec<Value>> {
    exchange::check_count(paths.len())?;
    let mut total = 0;
    paths
        .iter()
        .map(|path| {
            let document = crate::exchange_template::read(path)?;
            exchange::check_size(&mut total, &document)?;
            Ok(document)
        })
        .collect()
}
/// Writes the documents of an export where the user chose: each as a new file
/// of the directory for CLIProxyAPI, the only one as the file for Sub2API.
fn write_account_documents(format: &str, path: &Path, documents: &[Value]) -> Result<()> {
    let unwritable = || AppError::new(code::ACCOUNT_EXCHANGE, "导出无法生成");
    let bytes = |document| serde_json::to_vec_pretty(document).map_err(|_| unwritable());
    if format == "cliproxyapi" {
        // Stage the whole batch before publishing. UUID names avoid overwriting user files.
        let staged = documents
            .iter()
            .map(|document| {
                let file = storage::stage_write(path, &bytes(document)?)?;
                Ok((
                    file,
                    path.join(format!("codex-{}.json", uuid::Uuid::new_v4())),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        return storage::publish_batch(staged);
    }
    let [document] = documents else {
        return Err(unwritable());
    };
    storage::atomic_write(path, &bytes(document)?, None)
}
/// Lists the accounts of the files the user picks here, for `instance` to
/// take in. The documents go from this app's Rust to that instance's; the
/// window gets the list, which holds no login.
#[tauri::command]
pub async fn preview_account_import(
    app: tauri::AppHandle,
    r: R<'_>,
    instances: Instances<'_>,
    instance: String,
) -> Result<Option<Value>> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || {
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
        read_account_documents(&paths).map(Some)
    })
    .await
    .map_err(|_| AppError::new(code::IMPORT, "账号文件读取失败"))??;
    let Some(documents) = picked else {
        return Ok(None);
    };
    let args = json!({ "documents": documents });
    on(&instances, &r, &instance, "preview_account_documents", args)
        .await
        .map(Some)
}
/// Makes the OAuth template of `instance` ready for a Sub2API export: it is
/// already, or the user picks the Sub2API file here that it is taken from.
#[tauri::command]
pub async fn prepare_account_template(
    app: tauri::AppHandle,
    r: R<'_>,
    instances: Instances<'_>,
    instance: String,
    ids: Vec<String>,
    replace_template: Option<bool>,
) -> Result<Option<Value>> {
    use tauri_plugin_dialog::DialogExt;
    if !replace_template.unwrap_or(false) {
        let args = json!({ "ids": ids });
        let ready = on(&instances, &r, &instance, "account_template_ready", args).await?;
        if ready == Value::Bool(true) {
            return Ok(Some(json!(crate::exchange_template::Prepared::ready())));
        }
    }
    let picked = tauri::async_runtime::spawn_blocking(move || {
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
        crate::exchange_template::read(&path).map(Some)
    })
    .await
    .map_err(|_| AppError::new(code::EXPORT, "OAuth 模板读取失败"))??;
    let Some(document) = picked else {
        return Ok(None);
    };
    let args = json!({ "document": document });
    on(
        &instances,
        &r,
        &instance,
        "prepare_account_template_document",
        args,
    )
    .await
    .map(Some)
}
/// Writes the chosen accounts of `instance` where the user chooses here. The
/// documents, logins included, come from that instance's Rust to this app's
/// and reach no window. They are asked for once; after a failure the user
/// exports again.
#[tauri::command]
pub async fn export_accounts(
    app: tauri::AppHandle,
    r: R<'_>,
    instances: Instances<'_>,
    instance: String,
    ids: Vec<String>,
    expected_revision: String,
    format: String,
) -> Result<bool> {
    use tauri_plugin_dialog::DialogExt;
    let failed = || AppError::new(code::EXPORT, "账号导出失败");
    let chosen = format.clone();
    let path = tauri::async_runtime::spawn_blocking(move || {
        let file = if chosen == "cliproxyapi" {
            app.dialog()
                .file()
                .set_title("选择 CLIProxyAPI 账号导出目录")
                .blocking_pick_folder()
        } else if chosen == "sub2api" {
            app.dialog()
                .file()
                .set_file_name("sub2api-oauth-accounts.json")
                .add_filter("JSON", &["json"])
                .blocking_save_file()
        } else {
            return Err(AppError::new(code::EXPORT, "导出格式不支持"));
        };
        let Some(file) = file else {
            return Ok(None);
        };
        let path = file
            .into_path()
            .map_err(|_| AppError::new(code::PATH, "请选择本地路径"))?;
        if chosen == "cliproxyapi" && !path.is_dir() {
            return Err(AppError::new(code::ACCOUNT_EXCHANGE, "请选择导出目录"));
        }
        Ok(Some(path))
    })
    .await
    .map_err(|_| failed())??;
    let Some(path) = path else {
        return Ok(false);
    };
    let args = json!({ "ids": ids, "expectedRevision": expected_revision, "format": format });
    let answer = on(&instances, &r, &instance, "export_account_documents", args).await?;
    let documents: Vec<Value> = serde_json::from_value(answer).map_err(|_| failed())?;
    tauri::async_runtime::spawn_blocking(move || {
        write_account_documents(&format, &path, &documents)
    })
    .await
    .map_err(|_| failed())??;
    Ok(true)
}

#[cfg(test)]
mod startup_error_tests {
    use super::*;
    use parking_lot::Mutex;

    #[test]
    fn quick_reads_preserve_startup_error_until_main_consumes_it_once() {
        let slot = Mutex::new(Some(AppError::new(code::GATEWAY, "端口占用")));
        let consumed = std::sync::atomic::AtomicBool::new(false);
        let take = |label| take_startup_error_for_label(&slot, &consumed, label);
        assert_eq!(
            take("quick").as_ref().map(|e| e.message.as_str()),
            Some("端口占用")
        );
        assert_eq!(
            take("main").as_ref().map(|e| e.message.as_str()),
            Some("端口占用")
        );
        assert!(take("main").is_none());
        assert!(take("quick").is_none());
    }

    #[test]
    fn main_marks_consumed_even_before_async_startup_error_arrives() {
        let slot = Mutex::new(None);
        let consumed = std::sync::atomic::AtomicBool::new(false);
        assert!(take_startup_error_for_label(&slot, &consumed, "quick").is_none());
        assert!(!consumed.load(Ordering::Acquire));
        assert!(take_startup_error_for_label(&slot, &consumed, "main").is_none());
        assert!(consumed.load(Ordering::Acquire));
    }
}

#[cfg(test)]
mod account_file_tests {
    use super::*;

    #[test]
    fn exported_documents_are_written_where_chosen_and_read_back_for_an_import() {
        let temp = tempfile::tempdir().unwrap();
        let documents = [
            json!({"type": "codex", "name": "fixture-one"}),
            json!({"type": "codex", "name": "fixture-two"}),
        ];
        // CLIProxyAPI: a new file for each, beside what the directory holds.
        let kept = temp.path().join("codex-kept.json");
        std::fs::write(&kept, b"{}").unwrap();
        write_account_documents("cliproxyapi", temp.path(), &documents).unwrap();
        let written: Vec<PathBuf> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| *path != kept)
            .collect();
        assert_eq!(written.len(), 2);
        assert!(written.iter().all(|path| {
            let name = path.file_name().unwrap().to_string_lossy();
            name.starts_with("codex-") && name.ends_with(".json")
        }));
        assert_eq!(std::fs::read(&kept).unwrap(), b"{}");
        let read = read_account_documents(&written).unwrap();
        assert!(read.len() == 2 && documents.iter().all(|document| read.contains(document)));
        // Sub2API: the one document as the chosen file, and nothing otherwise.
        let file = temp.path().join("sub2api.json");
        write_account_documents("sub2api", &file, &documents[..1]).unwrap();
        assert_eq!(
            read_account_documents(std::slice::from_ref(&file)).unwrap(),
            documents[..1]
        );
        let other = temp.path().join("other.json");
        assert!(write_account_documents("sub2api", &other, &documents).is_err());
        assert!(!other.exists());
        // An import of no file is refused before anything is read.
        assert!(read_account_documents(&[]).is_err());
    }
}
