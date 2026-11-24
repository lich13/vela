//! The shared commands that belong to no single service: accounts,
//! preferences, the clients' configuration files, the gateways and the journal.
use crate::{
    claude_profile, configuration,
    core::{Preferences, ViewState},
    errors::code,
    events, gateway,
    gateway::ClientId,
    models_dev, official,
    runtime::Runtime,
    storage::{AppError, Result},
    usage,
};
use std::sync::Arc;

pub(crate) fn get_state(r: &Runtime) -> Result<ViewState> {
    r.state()
}
pub(crate) async fn switch_account(
    r: &Runtime,
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
            r.publish(s.clone());
            Ok(s)
        }
        Err(e) => {
            let _ = r.refresh();
            Err(e)
        }
    }
}
pub(crate) async fn use_official_account(
    r: &Runtime,
    account_id: String,
    expected_auth_revision: String,
    expected_config_revision: String,
) -> Result<ViewState> {
    let _official_guard = r.official_tx.lock().await;
    let state = r.state()?;
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
    let home = r.home(ClientId::Codex)?;
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
    official::enable_with_switch(&r.data, &home, &account_id, &config_revision, || {
        r.core
            .lock()
            .switch_account(&account_id, &expected_auth_revision)
            .map(|_| ())
    })?;
    let next = r.refresh()?;
    r.notice("官方账号已启用，请重新打开 Codex");
    Ok(next)
}
pub(crate) async fn disable_official_account(r: &Runtime) -> Result<ViewState> {
    let _official_guard = r.official_tx.lock().await;
    let home = r.home(ClientId::Codex)?;
    official::disable(&r.data, &home)?;
    let next = r.refresh()?;
    r.notice("官方连接已关闭，配置已恢复");
    Ok(next)
}
pub(crate) fn import_current(r: &Runtime) -> Result<ViewState> {
    let mut c = r.core.lock();
    let path = c.home().join("auth.json");
    c.import_file(&path, None)?;
    drop(c);
    r.refresh()
}
pub(crate) fn rename_account(r: &Runtime, id: String, name: String) -> Result<ViewState> {
    r.core.lock().rename(&id, &name)?;
    r.refresh()
}
pub(crate) fn delete_account(r: &Runtime, id: String) -> Result<ViewState> {
    if r.sign_ins().codex_account.as_deref() == Some(id.as_str()) {
        return Err(AppError::new(
            code::LOGIN_BUSY,
            "该账号正在刷新登录，请先取消或等待完成",
        ));
    }
    r.core.lock().delete(&id)?;
    r.refresh()
}
pub(crate) fn read_config(r: &Runtime, client_id: ClientId) -> Result<configuration::Document> {
    r.gateway(client_id).read_config(&r.home(client_id)?)
}
pub(crate) fn read_previous_config(r: &Runtime, client_id: ClientId) -> Result<String> {
    r.gateway(client_id).previous_config()
}
pub(crate) fn validate_config(client_id: ClientId, text: String) -> Result<()> {
    configuration::validate(client_id, &text)
}
pub(crate) async fn save_config(
    r: &Runtime,
    text: String,
    expected_revision: String,
    client_id: ClientId,
) -> Result<configuration::Document> {
    let _official_guard = r.official_tx.lock().await;
    let home = r.home(client_id)?;
    if client_id == ClientId::Codex {
        official::guard_save(&r.data, &home, &text)?;
    } else if client_id == ClientId::Claude {
        claude_profile::guard_save(&r.data, &home, &r.user_home, &text)?;
    }
    let doc = r
        .gateway(client_id)
        .save_config(&home, &text, &expected_revision)
        .map_err(|e| r.config_failure(client_id, e))?;
    r.publish_config(client_id, &doc.revision, doc.guarded);
    r.refresh()?;
    Ok(doc)
}
pub(crate) fn set_quota_refresh(
    r: &Runtime,
    seconds: u64,
    expected_seconds: u64,
) -> Result<ViewState> {
    let state = r.core.lock().set_quota_refresh(seconds, expected_seconds)?;
    r.set_quota_interval(seconds);
    r.publish(state.clone());
    Ok(state)
}
pub(crate) async fn set_preferences(
    r: &Runtime,
    mut preferences: Preferences,
) -> Result<ViewState> {
    let _official_guard = r.official_tx.lock().await;
    let sign_ins = r.sign_ins();
    if sign_ins.claude && r.core.lock().preferences().claude_home != preferences.claude_home {
        return Err(AppError::new(
            code::LOGIN_BUSY,
            "请先完成或取消 Claude 登录",
        ));
    }
    if sign_ins.codex {
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
    r.set_quota_interval(s.preferences.quota_refresh_seconds);
    r.gateway.observe_home(&r.home(ClientId::Codex)?);
    r.claude.observe_home(&r.home(ClientId::Claude)?);
    r.grok.observe_home(&r.home(ClientId::Grok)?);
    r.dsh.observe_home(&r.home(ClientId::Dsh)?);
    r.publish(s.clone());
    Ok(s)
}

pub(crate) fn get_gateway_freeze(r: &Runtime) -> Result<gateway::freeze::View> {
    Ok(r.gateway.freeze_gate().view())
}
pub(crate) fn set_gateway_frozen(
    r: &Runtime,
    frozen: bool,
    client_id: Option<ClientId>,
) -> Result<gateway::freeze::View> {
    let gate = r.gateway.freeze_gate();
    match client_id {
        Some(client) => gate.set_selected(Some(client), frozen),
        None => gate.set(frozen),
    }
}
pub(crate) fn set_gateway_freeze_resume(
    r: &Runtime,
    client_id: ClientId,
    resume_at: Option<i64>,
) -> Result<gateway::freeze::View> {
    r.gateway
        .freeze_gate()
        .schedule(client_id, resume_at, usage::model::now())
}
pub(crate) fn get_gateway(r: &Runtime, client_id: ClientId) -> Result<gateway::View> {
    if let Ok(home) = r.home(client_id) {
        r.gateway(client_id).observe_home(&home);
    }
    r.gateway(client_id).try_view()
}
pub(crate) fn quick_snapshot(
    r: &Runtime,
    client_id: ClientId,
    cancelling: Option<Vec<String>>,
) -> Result<crate::quick_model::Snapshot> {
    let cancelling = cancelling.unwrap_or_default().into_iter().collect();
    Ok(crate::quick_model::snapshot(r, client_id, &cancelling))
}
pub(crate) fn cancel_gateway_wait(
    r: &Runtime,
    client_id: ClientId,
    request_id: String,
) -> Result<&'static str> {
    Ok(r.gateway(client_id).interrupt_wait(&request_id))
}
pub(crate) fn get_gateway_models(
    r: &Runtime,
    client_id: ClientId,
) -> Result<gateway::model_list::View> {
    r.gateway(client_id).gateway_models()
}
pub(crate) async fn refresh_gateway_models(
    r: &Runtime,
    client_id: ClientId,
    provider_ids: Option<Vec<String>>,
) -> Result<gateway::model_list::View> {
    r.gateway(client_id)
        .refresh_gateway_models(provider_ids)
        .await
}
/// Downloads OpenAI's Codex catalog now instead of at the day's turn.
pub(crate) async fn update_codex_catalog(r: &Runtime) -> Result<gateway::View> {
    r.gateway.refresh_official_catalog(true).await?;
    r.gateway.try_view()
}
pub(crate) fn search_models_dev(
    r: &Runtime,
    query: String,
    limit: Option<usize>,
) -> Result<models_dev::Search> {
    Ok(r.usage
        .prices()?
        .models_dev
        .search(&query, limit.unwrap_or(50)))
}
pub(crate) fn edit_model_list(
    r: &Runtime,
    client_id: ClientId,
    op: gateway::model_list::Operation,
    id: String,
    source: Option<String>,
    context: Option<u64>,
    efforts: Option<Vec<String>>,
) -> Result<gateway::model_list::View> {
    r.gateway(client_id).edit_model_list(
        op,
        &id,
        source.as_deref(),
        gateway::model_list::Custom { context, efforts },
    )
}
pub(crate) async fn dsh_official_models(
    r: &Runtime,
    refresh: bool,
) -> Result<gateway::dsh::official::CatalogView> {
    r.dsh
        .dsh_official_models(&r.home(ClientId::Dsh)?, refresh)
        .await
}
pub(crate) fn preview_dsh_profile(
    r: &Runtime,
    profile: Option<String>,
) -> Result<gateway::dsh::Preview> {
    r.dsh
        .preview_dsh(&r.home(ClientId::Dsh)?, profile.as_deref())
}
pub(crate) async fn attach_dsh_profiles(
    r: &Runtime,
    options: gateway::dsh::Options,
    all: bool,
    expected_revision: String,
    preview_revision: String,
) -> Result<gateway::dsh::Batch> {
    r.dsh
        .attach_dsh_profiles(
            &r.home(ClientId::Dsh)?,
            options,
            all,
            &expected_revision,
            Some(&preview_revision),
        )
        .await
        .map_err(|e| r.config_failure(ClientId::Dsh, e))
}
pub(crate) fn read_dsh_file(
    r: &Runtime,
    profile: Option<String>,
    key: gateway::dsh::editor::FileKey,
) -> Result<gateway::dsh::editor::FileView> {
    r.dsh
        .read_dsh_file(&r.home(ClientId::Dsh)?, profile.as_deref(), key)
}
pub(crate) fn read_previous_dsh_file(
    r: &Runtime,
    profile: Option<String>,
    key: gateway::dsh::editor::FileKey,
) -> Result<String> {
    r.dsh
        .previous_dsh_file(&r.home(ClientId::Dsh)?, profile.as_deref(), key)
}
pub(crate) async fn save_dsh_file(
    r: &Runtime,
    profile: Option<String>,
    key: gateway::dsh::editor::FileKey,
    text: String,
    expected_revision: String,
) -> Result<gateway::dsh::editor::FileView> {
    r.dsh
        .save_dsh_file(
            &r.home(ClientId::Dsh)?,
            profile.as_deref(),
            key,
            Some(&text),
            &expected_revision,
        )
        .await
        .map_err(|e| r.config_failure(ClientId::Dsh, e))
}
pub(crate) async fn restore_dsh_file(
    r: &Runtime,
    profile: Option<String>,
    key: gateway::dsh::editor::FileKey,
    expected_revision: String,
) -> Result<gateway::dsh::editor::FileView> {
    r.dsh
        .save_dsh_file(
            &r.home(ClientId::Dsh)?,
            profile.as_deref(),
            key,
            None,
            &expected_revision,
        )
        .await
        .map_err(|e| r.config_failure(ClientId::Dsh, e))
}
pub(crate) fn preview_grok_import(
    r: &Runtime,
    selected: Option<String>,
) -> Result<gateway::GrokImportPreview> {
    r.grok
        .preview_grok_import(&r.home(ClientId::Grok)?, selected.as_deref())
}
pub(crate) fn cancel_grok_import(r: &Runtime, token: Option<String>) -> Result<()> {
    r.grok.cancel_grok_import(token.as_deref());
    Ok(())
}
pub(crate) async fn commit_grok_import(
    r: &Runtime,
    token: String,
    expected_revision: String,
) -> Result<gateway::View> {
    let _guard = r.official_tx.lock().await;
    let view = r
        .grok
        .commit_grok_import(&r.home(ClientId::Grok)?, &token, &expected_revision)?;
    let result = r.grok.start(&view.revision, &r.home(ClientId::Grok)?).await;
    let _ = r.refresh();
    result
}
pub(crate) async fn probe_provider_endpoints(
    r: &Runtime,
    client_id: ClientId,
    provider_id: String,
) -> Result<gateway::View> {
    r.gateway(client_id)
        .probe_endpoints(&provider_id, true)
        .await
}
pub(crate) async fn update_gateway(
    r: &Runtime,
    client_id: ClientId,
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
    let _ = r.refresh();
    if selected {
        if result.running {
            r.notice("已切换供应商，新请求立即生效");
        } else {
            r.notice(&format!("文件已切换，请重新打开 {}", client_id.name()));
        }
    }
    Ok(result)
}
/// A provider of this instance, with its tokens, for another to add.
pub(crate) fn export_provider_copy(
    r: &Runtime,
    client_id: ClientId,
    provider_id: String,
) -> Result<gateway::ProviderCopy> {
    r.gateway(client_id).provider_copy(&provider_id)
}
/// Adds what another instance exported as the last provider here.
pub(crate) async fn import_provider_copy(
    r: &Runtime,
    client_id: ClientId,
    copy: gateway::ProviderCopy,
) -> Result<()> {
    let _official_guard = r.official_tx.lock().await;
    let home = r.home(client_id)?;
    let gateway = r.gateway(client_id);
    let revision = gateway.try_view()?.revision;
    let copy = Box::new(copy);
    gateway
        .edit_checked(gateway::Edit::ImportCopy { copy }, &revision, &home, None)
        .map_err(|e| r.config_failure(client_id, e))?;
    let _ = r.refresh();
    Ok(())
}
pub(crate) async fn start_gateway(
    r: &Runtime,
    client_id: ClientId,
    expected_revision: String,
    expected_config_revision: Option<String>,
) -> Result<gateway::View> {
    let result = start_gateway_for(
        r,
        client_id,
        &expected_revision,
        expected_config_revision.as_deref(),
    )
    .await?;
    r.notice(&format!("网关已启用，请重新打开 {}", client_id.name()));
    Ok(result)
}
/// Starts one client's gateway and publishes the new state. The command and
/// the native panel share it; each words its own notice.
pub(crate) async fn start_gateway_for(
    r: &Runtime,
    client_id: ClientId,
    expected_revision: &str,
    expected_config_revision: Option<&str>,
) -> Result<gateway::View> {
    let _official_guard = r.official_tx.lock().await;
    if client_id == ClientId::Codex && official::blocks_gateway(&r.data) {
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
    let _ = r.refresh();
    Ok(result)
}
pub(crate) async fn stop_gateway_for(
    r: &Runtime,
    client_id: ClientId,
    expected_config_revision: Option<&str>,
) -> Result<gateway::View> {
    let _official_guard = r.official_tx.lock().await;
    let result = r
        .gateway(client_id)
        .stop_checked(expected_config_revision)
        .await
        .map_err(|e| r.config_failure(client_id, e))?;
    r.poll_wake.notify_one();
    let _ = r.refresh();
    Ok(result)
}
pub(crate) async fn stop_gateway(
    r: &Runtime,
    client_id: ClientId,
    expected_config_revision: Option<String>,
) -> Result<gateway::View> {
    let result = stop_gateway_for(r, client_id, expected_config_revision.as_deref()).await?;
    r.notice(&format!(
        "已写入当前供应商，请重新打开 {}",
        client_id.name()
    ));
    Ok(result)
}
pub(crate) async fn query_provider_quota(
    r: &Runtime,
    client_id: ClientId,
    provider_id: String,
    force: bool,
) -> Result<gateway::QuotaView> {
    r.gateway(client_id).query_quota(&provider_id, force).await
}
pub(crate) async fn list_provider_models(
    r: &Runtime,
    client_id: ClientId,
    provider_id: String,
    force: bool,
) -> Result<gateway::catalog::View> {
    r.gateway(client_id).list_models(&provider_id, force).await
}
/// The upstream error statuses met by the providers that share a host with
/// the given provider, or with the address of one that is not saved yet.
pub(crate) fn get_provider_seen_statuses(
    r: &Runtime,
    client_id: ClientId,
    provider_id: Option<String>,
    base_url: Option<String>,
) -> Result<Vec<events::SeenStatus>> {
    let providers = r
        .gateway(client_id)
        .same_host_providers(provider_id.as_deref(), base_url.as_deref());
    Ok(r.diagnostics.seen_statuses(client_id, &providers))
}

pub(crate) async fn get_app_events(
    r: &Arc<Runtime>,
    filter: events::Filter,
) -> Result<events::Page> {
    use events::{ExternalClient, JournalClient, Origin};
    let r = r.clone();
    tokio::task::spawn_blocking(move || {
        // Each external source is read only when the page shows it.
        let wanted = |origin, client| {
            filter.origin.is_none_or(|o| o == origin)
                && filter
                    .client_id
                    .is_none_or(|c| c == JournalClient::External(client))
        };
        if wanted(Origin::Antigravity, ExternalClient::Antigravity) {
            r.diagnostics.sync_antigravity_logs(&r.user_home);
        }
        if wanted(Origin::ClaudeCode, ExternalClient::ClaudeCode) {
            if let Ok(home) = r.home(ClientId::Claude) {
                r.diagnostics.sync_claude_logs(&home);
            }
        }
        r.diagnostics.query(filter)
    })
    .await
    .map_err(|_| AppError::new(code::LOG_READ, "日志读取失败"))
}
pub(crate) fn get_app_event(r: &Runtime, id: String) -> Result<Option<events::ClassifiedRecord>> {
    Ok(r.diagnostics.detail(&id))
}
pub(crate) async fn get_app_event_raw(
    r: &Runtime,
    id: String,
) -> Result<Option<crate::event_raw::View>> {
    let service = r.diagnostics.clone();
    tokio::task::spawn_blocking(move || service.raw(&id))
        .await
        .map_err(|_| AppError::new(code::LOG_READ, "错误原文读取失败"))?
}
pub(crate) async fn clear_app_events(r: &Runtime) -> Result<()> {
    r.diagnostics
        .clear(Some(r.user_home.clone()), r.home(ClientId::Claude).ok())
        .await
}
