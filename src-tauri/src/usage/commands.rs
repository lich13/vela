use super::{model::*, pricing, store, State};
use crate::{
    runtime::{Event, Runtime},
    storage::Result,
};
pub fn get_usage_state(r: &Runtime) -> Result<State> {
    Ok(r.usage.state())
}
pub fn calculate_usage_cost(
    input: super::calculator::Input,
) -> Result<super::calculator::Calculation> {
    super::calculator::calculate(input)
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalculatorModel {
    pub model: String,
    pub rates: Option<[String; 5]>,
}
pub async fn get_usage_calculator_models(
    r: &Runtime,
    client: String,
) -> Result<Vec<CalculatorModel>> {
    let service = r.usage.clone();
    let prices = r.usage.prices()?.clone();
    tokio::task::spawn_blocking(move || {
        let models = service.read(|db| super::query::calculator_models(&db.connection, &client))?;
        Ok(models
            .into_iter()
            .map(|model| {
                let rates = prices.quote(Some(&model), "1").million_rates();
                CalculatorModel { model, rates }
            })
            .collect())
    })
    .await
    .map_err(|_| failure("用量模型查询中断"))?
}
#[derive(serde::Serialize)]
pub struct RequestedModel {
    pub model: String,
    pub count: u64,
}
/// The model IDs Claude clients asked the gateway for over the last 7 days.
pub async fn get_claude_requested_models(r: &Runtime) -> Result<Vec<RequestedModel>> {
    let service = r.usage.clone();
    tokio::task::spawn_blocking(move || {
        let since = now() - 7 * 86_400_000;
        let models = service
            .read(|db| super::storage_v12::requested_models(&db.connection, "claude", since, 50))?;
        Ok(models
            .into_iter()
            .map(|(model, count)| RequestedModel { model, count })
            .collect())
    })
    .await
    .map_err(|_| failure("用量模型查询中断"))?
}
pub async fn get_usage_calculator_totals(r: &Runtime, filter: Filter) -> Result<Totals> {
    let service = r.usage.clone();
    tokio::task::spawn_blocking(move || {
        service.read(|db| super::query::calculator_totals(&db.connection, &filter))
    })
    .await
    .map_err(|_| failure("用量查询中断"))?
}
pub async fn get_usage_dashboard(r: &Runtime, filter: Filter) -> Result<store::Dashboard> {
    let s = r.usage.clone();
    tokio::task::spawn_blocking(move || s.read(|db| db.dashboard(&filter)))
        .await
        .map_err(|_| failure("用量查询中断"))?
}
/// How often each provider's attempts failed and how soon it answered, bucket
/// by bucket over the filter's time and client. The journal says what failed,
/// the usage records what succeeded and how soon.
pub async fn get_usage_curves(r: &Runtime, filter: Filter) -> Result<super::curves::Curves> {
    let s = r.usage.clone();
    let journal = r.diagnostics.clone();
    tokio::task::spawn_blocking(move || {
        let at = now();
        let seconds = |ms: i64| ms.max(0) as u64 / 1000;
        let (since, failures, error) = journal.upstream_failures(
            seconds(filter.start.unwrap_or(0)),
            seconds(filter.end.unwrap_or(at)),
        );
        let since = i64::try_from(since.saturating_mul(1000)).unwrap_or(i64::MAX);
        let mut curves = s.read(|db| db.curves(&filter, since, &failures, at))?;
        curves.journal_error = error;
        Ok(curves)
    })
    .await
    .map_err(|_| failure("用量曲线查询中断"))?
}
/// The history and the account periods as they are saved. Asking for the
/// quota is `refresh_antigravity_usage_quota`, which the page sends afterwards.
pub async fn get_antigravity_usage_stats(
    r: &Runtime,
    filter: Filter,
) -> Result<super::antigravity::AntigravityUsageStats> {
    let s = r.usage.clone();
    let now_ms = now();
    tokio::task::spawn_blocking(move || s.read(|db| db.antigravity_stats(&filter, now_ms)))
        .await
        .map_err(|_| failure("Antigravity 统计查询中断"))?
}
/// What asking for the quota of the saved Antigravity accounts came to.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaRefresh {
    /// The saved periods are no longer the ones the statistics were read with.
    pub changed: bool,
    /// Why the quota of the signed-in account could not be read, to show
    /// beside the saved periods. Another account keeps what was last read.
    pub error: Option<String>,
}
pub async fn refresh_antigravity_usage_quota(r: &Runtime) -> Result<QuotaRefresh> {
    Ok(match r.usage.refresh_antigravity_quota().await {
        Ok(round) => QuotaRefresh {
            changed: round.changed,
            error: round.current.and_then(|id| round.failed.get(&id).cloned()),
        },
        Err(error) => QuotaRefresh {
            changed: false,
            error: Some(error.message),
        },
    })
}
pub async fn get_claude_usage_stats(
    r: &Runtime,
    filter: Filter,
) -> Result<super::antigravity::AntigravityUsageStats> {
    let s = r.usage.clone();
    tokio::task::spawn_blocking(move || s.read(|db| db.claude_stats(&filter, now())))
        .await
        .map_err(|_| failure("Claude 统计查询中断"))?
}
pub async fn get_claude_usage_quota(
    r: &Runtime,
    refresh: bool,
) -> Result<super::claude_quota::View> {
    Ok(r.usage
        .0
        .claude_quota
        .get(r.home(crate::gateway::ClientId::Claude)?, refresh)
        .await)
}
pub async fn get_usage_logs(r: &Runtime, filter: Filter) -> Result<store::Page> {
    let s = r.usage.clone();
    tokio::task::spawn_blocking(move || s.read(|db| db.logs(&filter)))
        .await
        .map_err(|_| failure("日志查询中断"))?
}
pub async fn get_usage_detail(r: &Runtime, id: String) -> Result<Record> {
    let s = r.usage.clone();
    tokio::task::spawn_blocking(move || s.read(|db| db.detail(&id)))
        .await
        .map_err(|_| failure("详情查询中断"))?
}
pub fn set_usage_settings(r: &Runtime, settings: Settings) -> Result<State> {
    r.usage.configure(settings)
}
pub async fn sync_usage(r: &Runtime, rebuild: Option<String>) -> Result<State> {
    if rebuild
        .as_deref()
        .is_some_and(|s| !matches!(s, "codex" | "claude" | "grok" | "antigravity"))
    {
        return Err(failure("未知的会话来源"));
    }
    let roots = r.usage_roots();
    let s = r.usage.clone();
    tokio::task::spawn_blocking(move || s.sync_roots(roots, rebuild.as_deref()))
        .await
        .map_err(|_| failure("同步中断"))?
}
pub fn get_pricing(r: &Runtime) -> Result<pricing::View> {
    Ok(r.usage.prices()?.view())
}
pub async fn search_pricing(
    r: &Runtime,
    query: String,
    offset: usize,
    limit: usize,
) -> Result<pricing::SearchPage> {
    if query.len() > 256 || limit == 0 || limit > 100 || offset > 100_000 {
        return Err(failure("定价查询参数无效"));
    }
    let prices = r.usage.prices()?.frozen();
    tokio::task::spawn_blocking(move || prices.search(&query, offset, limit))
        .await
        .map_err(|_| failure("定价查询中断"))
}
pub fn get_models_dev(r: &Runtime) -> Result<crate::models_dev::View> {
    Ok(r.usage.prices()?.models_dev.view())
}
pub async fn update_models_dev(r: &Runtime) -> Result<crate::models_dev::View> {
    let before = r.usage.prices()?.models_dev.view().version;
    let view = r.usage.prices()?.models_dev.refresh(true).await?;
    if view.error.is_none() && view.version != before {
        let service = r.usage.clone();
        tokio::task::spawn_blocking(move || service.backfill_prices())
            .await
            .map_err(|_| failure("价格补算中断"))??;
    }
    r.emit(Event::Pricing);
    Ok(view)
}
pub fn compare_model_prices(
    r: &Runtime,
    model: String,
    context: u64,
) -> Result<pricing::PriceComparison> {
    if model_id(Some(&model)).is_none() || context > 1_000_000_000_000 {
        return Err(failure("价格核对参数无效"));
    }
    Ok(r.usage.prices()?.compare(&model, context))
}
pub async fn configure_pricing(
    r: &Runtime,
    config: pricing::Config,
    expected_revision: String,
) -> Result<pricing::View> {
    let result = r.usage.prices()?.configure(config, &expected_revision)?;
    let service = r.usage.clone();
    tokio::task::spawn_blocking(move || service.backfill_prices())
        .await
        .map_err(|_| failure("价格补算中断"))??;
    r.emit(Event::Usage);
    r.emit(Event::Pricing);
    Ok(result)
}
pub async fn update_pricing(r: &Runtime) -> Result<pricing::View> {
    let result = r.usage.update_prices(true).await;
    r.emit(Event::Pricing);
    result
}
pub async fn reload_pricing(r: &Runtime) -> Result<pricing::View> {
    let result = r.usage.prices()?.reload()?;
    let service = r.usage.clone();
    tokio::task::spawn_blocking(move || service.backfill_prices())
        .await
        .map_err(|_| failure("价格补算中断"))??;
    r.emit(Event::Usage);
    r.emit(Event::Pricing);
    Ok(result)
}

#[cfg(feature = "desktop")]
mod desktop {
    use super::*;
    use std::sync::Arc;
    type R<'a> = tauri::State<'a, Arc<Runtime>>;

    #[tauri::command]
    pub fn open_pricing_directory(r: R<'_>) -> Result<()> {
        let p = r
            .usage
            .prices()?
            .path()
            .parent()
            .ok_or_else(|| failure("定价目录无效"))?;
        open::that(p).map_err(|_| failure("无法打开定价目录"))
    }

    fn format_bill_file_name(host: Option<&str>, platform: &str) -> String {
        let cleaned = host
            .map(str::trim)
            .map(|h| h.strip_suffix(".local").unwrap_or(h))
            .map(|h| {
                h.chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect::<String>()
                    .trim_matches(['-', '.'])
                    .to_string()
            })
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "localhost".into());
        format!("{cleaned}+{platform}.csv")
    }

    fn default_bill_file_name() -> String {
        format_bill_file_name(
            sysinfo::System::host_name().as_deref(),
            std::env::consts::OS,
        )
    }

    #[tauri::command]
    pub async fn export_usage_bill(
        r: R<'_>,
        app: tauri::AppHandle,
        request: crate::usage::billing::Request,
    ) -> Result<Option<crate::usage::billing::Exported>> {
        use tauri_plugin_dialog::DialogExt;
        request.validate()?;
        let service = r.usage.clone();
        let default_name = default_bill_file_name();
        tauri::async_runtime::spawn_blocking(move || {
            let Some(file) = app
                .dialog()
                .file()
                .set_title(if request.clean {
                    "导出账单并清理明细"
                } else {
                    "导出账单"
                })
                .set_file_name(&default_name)
                .add_filter("CSV", &["csv"])
                .blocking_save_file()
            else {
                return Ok(None);
            };
            let path = file
                .into_path()
                .map_err(|_| failure("请选择本地账单保存路径"))?;
            service.export_bill(&request, &path).map(Some)
        })
        .await
        .map_err(|_| failure("账单导出中断，明细清理结果请检查用量页"))?
    }

    #[cfg(test)]
    mod tests {
        use super::format_bill_file_name;

        #[test]
        fn formats_default_bill_file_name_with_hostname_and_platform() {
            assert_eq!(
                format_bill_file_name(Some("MacBook-Pro.local"), "macos"),
                "MacBook-Pro+macos.csv"
            );
            assert_eq!(
                format_bill_file_name(Some("  my host/1  "), "windows"),
                "my-host-1+windows.csv"
            );
            assert_eq!(format_bill_file_name(None, "linux"), "localhost+linux.csv");
        }
    }
}
#[cfg(feature = "desktop")]
pub use desktop::*;
