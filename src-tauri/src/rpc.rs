//! The commands this machine and a server answer alike, in one table. The
//! desktop app reaches it through its `call` command, a server through its
//! socket.
use crate::{
    antigravity::{
        auth::{
            delete_antigravity_account, export_antigravity_account_document,
            get_antigravity_auth_state, import_antigravity_account_document,
            rename_antigravity_account, switch_antigravity_account,
        },
        cli::{
            read_antigravity_cli_config, read_previous_antigravity_cli_config,
            save_antigravity_cli_config, validate_antigravity_cli_config,
        },
        read_antigravity_config, read_previous_antigravity_config, save_antigravity_config,
        validate_antigravity_config,
    },
    claude_profile::commands::{get_claude_profile, recover_claude_profile, switch_claude_profile},
    configuration::tree::{patch_config_tree, read_config_tree},
    errors::code,
    provider_testing::commands::{
        provider_test_cancel, provider_test_clear_mark, provider_test_models, provider_test_query,
        provider_test_retry, provider_test_save_prompt, provider_test_set_detection,
        provider_test_set_platform, provider_test_start, provider_test_state,
        provider_test_update_bank,
    },
    runtime::Runtime,
    storage::{AppError, Result},
    usage::commands::{
        calculate_usage_cost, compare_model_prices, configure_pricing, get_antigravity_usage_stats,
        get_claude_requested_models, get_claude_usage_quota, get_claude_usage_stats,
        get_models_dev, get_pricing, get_usage_calculator_models, get_usage_calculator_totals,
        get_usage_dashboard, get_usage_detail, get_usage_logs, get_usage_state,
        refresh_antigravity_usage_quota, reload_pricing, search_pricing, set_usage_settings,
        sync_usage, update_models_dev, update_pricing,
    },
};
use handlers::*;
use serde_json::{Map, Value};
use std::sync::Arc;

pub(crate) mod handlers;

/// Runs the command `name` with `args`, an object keyed by the camelCase
/// names of the command's parameters.
pub(crate) async fn call(rt: &Arc<Runtime>, name: &str, args: Value) -> Result<Value> {
    dispatch(rt, name, Args::new(args)?).await
}

/// Every command `call` answers. A server lists them in its handshake, and
/// the desktop app refuses the ones a server does not list.
pub(crate) fn names() -> &'static [&'static str] {
    NAMES
}

/// What one instance's Rust asks of another's and no window may ask: these
/// carry a provider with its tokens, or the documents of account files with
/// their logins. A server lists them like the rest and answers them only to a
/// request that says it is internal; the two commands a window reaches the
/// table through refuse them.
const INTERNAL: &[&str] = &[
    "export_provider_copy",
    "import_provider_copy",
    "preview_account_documents",
    "prepare_account_template_document",
    "export_account_documents",
    "export_antigravity_account_document",
    "import_antigravity_account_document",
];

pub(crate) fn internal(name: &str) -> bool {
    INTERNAL.contains(&name)
}

pub(crate) fn unsupported() -> AppError {
    AppError::new(code::UNSUPPORTED, "此实例不支持该操作")
}

/// Builds the table. Each line names a function of the same name as its
/// command and the parameters it takes, in its own order: `async` ones are
/// awaited with the runtime in front, `blocking` ones touch files or the
/// database and run on the blocking pool with the runtime in front, and
/// `plain` ones run there without it.
macro_rules! commands {
    ($($mode:ident $name:ident($($arg:ident),*);)*) => {
        const NAMES: &[&str] = &[$(stringify!($name)),*];
        async fn dispatch(rt: &Arc<Runtime>, name: &str, mut args: Args) -> Result<Value> {
            match name {
                $(stringify!($name) => commands!(@$mode rt, args, $name($($arg),*)),)*
                _ => Err(unsupported()),
            }
        }
    };
    (@async $rt:ident, $args:ident, $name:ident($($arg:ident),*)) => {{
        $(let $arg = $args.take(stringify!($arg))?;)*
        answer($name($rt, $($arg),*).await)
    }};
    (@blocking $rt:ident, $args:ident, $name:ident($($arg:ident),*)) => {{
        $(let $arg = $args.take(stringify!($arg))?;)*
        let rt = $rt.clone();
        blocking(move || answer($name(&rt, $($arg),*))).await
    }};
    (@plain $rt:ident, $args:ident, $name:ident($($arg:ident),*)) => {{
        $(let $arg = $args.take(stringify!($arg))?;)*
        blocking(move || answer($name($($arg),*))).await
    }};
}

commands! {
    blocking get_state();
    async switch_account(id, expected_revision);
    async use_official_account(account_id, expected_auth_revision, expected_config_revision);
    async disable_official_account();
    blocking import_current();
    blocking rename_account(id, name);
    blocking delete_account(id);
    blocking preview_account_documents(documents);
    async commit_account_import(token, selected, expected_revision);
    blocking cancel_account_exchange(token);
    blocking preview_account_export();
    blocking account_template_ready(ids);
    blocking prepare_account_template_document(document);
    blocking select_account_template(token, id);
    blocking export_account_documents(ids, expected_revision, format);
    blocking read_antigravity_config();
    blocking save_antigravity_config(text, expected_revision);
    blocking read_previous_antigravity_config();
    plain validate_antigravity_config(text);
    blocking read_antigravity_cli_config();
    blocking save_antigravity_cli_config(text, expected_revision);
    blocking read_previous_antigravity_cli_config();
    plain validate_antigravity_cli_config(text);
    plain read_config_tree(format, text);
    plain patch_config_tree(format, text, path, value);
    async get_antigravity_auth_state();
    async switch_antigravity_account(id, expected_revision);
    blocking rename_antigravity_account(id, name, expected_revision);
    blocking delete_antigravity_account(id, expected_revision);
    blocking export_antigravity_account_document(ids, expected_revision);
    blocking import_antigravity_account_document(document, expected_revision);
    blocking read_config(client_id);
    blocking read_previous_config(client_id);
    plain validate_config(client_id, text);
    async save_config(text, expected_revision, client_id);
    async set_preferences(preferences);
    blocking set_quota_refresh(seconds, expected_seconds);
    blocking cancel_gateway_wait(client_id, request_id);
    blocking get_gateway(client_id);
    blocking quick_snapshot(client_id, cancelling);
    blocking get_gateway_freeze();
    blocking set_gateway_frozen(frozen, client_id);
    blocking set_gateway_freeze_resume(client_id, resume_at);
    blocking get_gateway_models(client_id);
    async refresh_gateway_models(client_id, provider_ids);
    async update_codex_catalog();
    blocking search_models_dev(query, limit);
    blocking edit_model_list(client_id, op, id, source, context, efforts);
    async dsh_official_models(refresh);
    blocking read_dsh_file(profile, key);
    blocking read_previous_dsh_file(profile, key);
    async save_dsh_file(profile, key, text, expected_revision);
    async restore_dsh_file(profile, key, expected_revision);
    async attach_dsh_profiles(options, all, expected_revision, preview_revision);
    blocking preview_dsh_profile(profile);
    blocking preview_grok_import(selected);
    async commit_grok_import(token, expected_revision);
    blocking cancel_grok_import(token);
    async probe_provider_endpoints(client_id, provider_id);
    async update_gateway(client_id, edit, expected_revision, expected_config_revision);
    blocking export_provider_copy(client_id, provider_id);
    async import_provider_copy(client_id, copy);
    async start_gateway(client_id, expected_revision, expected_config_revision);
    async stop_gateway(client_id, expected_config_revision);
    async query_provider_quota(client_id, provider_id, force);
    async list_provider_models(client_id, provider_id, force);
    blocking get_provider_seen_statuses(client_id, provider_id, base_url);
    async get_app_events(filter);
    blocking get_app_event(id);
    async get_app_event_raw(id);
    async clear_app_events();
    blocking provider_test_state(client_id, provider_id);
    blocking provider_test_save_prompt(prompt);
    blocking provider_test_set_platform(client_id, platform);
    async provider_test_start(request);
    blocking provider_test_query(id);
    blocking provider_test_cancel(id);
    blocking provider_test_retry(id);
    blocking provider_test_set_detection(client_id, provider_id, detection, expected_revision);
    blocking provider_test_clear_mark(client_id, provider_id, expected_revision);
    async provider_test_models(client_id, provider_id, expected_revision, endpoint, refresh);
    async provider_test_update_bank(engine);
    blocking get_claude_profile();
    async switch_claude_profile(mode, expected_revision);
    async recover_claude_profile();
    blocking get_usage_state();
    async get_usage_dashboard(filter);
    async get_antigravity_usage_stats(filter);
    async refresh_antigravity_usage_quota();
    async get_claude_usage_stats(filter);
    async get_claude_usage_quota(refresh);
    async get_usage_logs(filter);
    async get_usage_detail(id);
    blocking set_usage_settings(settings);
    async sync_usage(rebuild);
    blocking get_pricing();
    plain calculate_usage_cost(input);
    async get_usage_calculator_models(client);
    async get_claude_requested_models();
    async get_usage_calculator_totals(filter);
    async search_pricing(query, offset, limit);
    blocking get_models_dev();
    async update_models_dev();
    blocking compare_model_prices(model, context);
    async configure_pricing(config, expected_revision);
    async update_pricing();
    async reload_pricing();
}

/// The arguments of one call.
struct Args(Map<String, Value>);
impl Args {
    fn new(args: Value) -> Result<Self> {
        match args {
            Value::Object(map) => Ok(Self(map)),
            Value::Null => Ok(Self(Map::new())),
            _ => Err(AppError::new(code::JSON, "参数必须是对象")),
        }
    }
    /// The argument for the parameter `name`, which arrives under the
    /// camelCase form of it. One that is absent reads as `null`, so an
    /// optional parameter is `None` and any other reports itself missing.
    fn take<T: serde::de::DeserializeOwned>(&mut self, name: &str) -> Result<T> {
        let key = camel(name);
        let value = self.0.remove(&key);
        let missing = value.is_none();
        serde_json::from_value(value.unwrap_or(Value::Null)).map_err(|_| {
            let message = if missing {
                format!("缺少参数 {key}")
            } else {
                format!("参数 {key} 无效")
            };
            AppError::new(code::JSON, &message)
        })
    }
}

fn camel(name: &str) -> String {
    let mut key = String::with_capacity(name.len());
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            key.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            key.push(c);
        }
    }
    key
}

fn answer<T: serde::Serialize>(result: Result<T>) -> Result<Value> {
    serde_json::to_value(result?).map_err(|_| AppError::new(code::JSON, "结果无法编码"))
}

async fn blocking(work: impl FnOnce() -> Result<Value> + Send + 'static) -> Result<Value> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| AppError::new(code::PROCESS, "操作异常中断"))?
}

#[cfg(test)]
mod tests;
