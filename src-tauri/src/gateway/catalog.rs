//! Read-only model discovery. Shares connection policy, never business admission.
use super::{
    circuit, connector,
    quota::{now, Query},
    replay,
};
use crate::errors::code;
use crate::storage::{self, AppError, Result};
use http_body_util::BodyExt;
use hyper::{header, Request};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, Semaphore};

const UNAVAILABLE: &str = "模型缓存不可用，可手动刷新";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, crate::models_dev::Metadata>,
    pub provider_id: String,
    pub version: String,
    pub models: Vec<String>,
    pub checked_at: Option<u64>,
    pub stale: bool,
    pub error: Option<String>,
    pub retry_at: Option<u64>,
}
struct Entry {
    view: View,
    completed: Option<Instant>,
}
pub struct Service {
    entries: Mutex<HashMap<String, Arc<AsyncMutex<Entry>>>>,
    permits: Semaphore,
    path: PathBuf,
    disk: Mutex<(File, bool)>,
}
#[derive(Clone, Serialize, Deserialize)]
struct File {
    version: u8,
    providers: BTreeMap<String, View>,
}
impl Default for File {
    fn default() -> Self {
        Self {
            version: 1,
            providers: BTreeMap::new(),
        }
    }
}
impl Service {
    pub fn new(path: PathBuf) -> Self {
        let loaded = storage::read_optional(&path).and_then(|bytes| {
            let file: File = bytes
                .map(|bytes| {
                    serde_json::from_slice(&bytes)
                        .map_err(|_| AppError::new(code::MODELS, UNAVAILABLE))
                })
                .transpose()?
                .unwrap_or_default();
            if file.version != 1 {
                return Err(AppError::new(code::MODELS, UNAVAILABLE));
            }
            Ok(file)
        });
        let corrupt = loaded.is_err();
        Self {
            entries: Mutex::new(HashMap::new()),
            permits: Semaphore::new(3),
            path,
            disk: Mutex::new((loaded.unwrap_or_default(), corrupt)),
        }
    }
    pub fn cached(&self, id: &str, version: &str) -> View {
        let disk = self.disk.lock();
        let mut view = disk.0.providers.get(id).cloned().unwrap_or(View {
            metadata: BTreeMap::new(),
            provider_id: id.into(),
            version: version.into(),
            models: vec![],
            checked_at: None,
            stale: false,
            error: None,
            retry_at: None,
        });
        view.stale = view.version != version;
        if disk.1 {
            view.error = Some(UNAVAILABLE.into());
            view.stale = true;
        }
        view
    }
    /// Called only while the gateway holds its current provider/configuration lock.
    pub fn commit(&self, view: &View) -> Result<()> {
        let mut disk = self.disk.lock();
        if disk.1 && view.error.is_some() {
            return Ok(());
        }
        let mut next = disk.0.clone();
        next.providers
            .insert(view.provider_id.clone(), view.clone());
        self.write(&next)?;
        *disk = (next, false);
        Ok(())
    }
    fn write(&self, file: &File) -> Result<()> {
        let bytes = serde_json::to_vec(file)
            .map_err(|_| AppError::new(code::MODELS, "无法保存模型缓存"))?;
        storage::atomic_write(&self.path, &bytes, None)
    }
    pub fn remove(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        // In-flight queries may finish through their Arc, but can no longer
        // retain a deleted provider in the cache or pass Gateway's live check.
        self.entries.lock().retain(|key, _| {
            !ids.iter().any(|id| {
                key.strip_prefix(id)
                    .is_some_and(|tail| tail.starts_with(':'))
            })
        });
        let mut disk = self.disk.lock();
        // Keep a damaged file for recovery rather than replacing it with an empty cache.
        if disk.1 {
            return Ok(());
        }
        let mut next = disk.0.clone();
        next.providers.retain(|id, _| !ids.contains(id));
        self.write(&next)?;
        disk.0 = next;
        Ok(())
    }
    pub fn discard_version(&self, id: &str, version: &str) {
        self.entries.lock().remove(&format!("{id}:{version}"));
    }
    pub async fn query_with_connection(
        &self,
        input: Query,
        force: bool,
        connection: Option<&super::grok_config::Connection>,
    ) -> Result<View> {
        if !force {
            return Ok(self.cached(&input.id, &input.version));
        }
        let began = Instant::now();
        let key = format!("{}:{}", input.id, input.version);
        let entry = {
            let mut entries = self.entries.lock();
            entries.retain(|k, _| !k.starts_with(&format!("{}:", input.id)) || k == &key);
            entries
                .entry(key)
                .or_insert_with(|| {
                    Arc::new(AsyncMutex::new(Entry {
                        view: self.cached(&input.id, &input.version),
                        completed: None,
                    }))
                })
                .clone()
        };
        let mut entry = entry.lock().await;
        if entry.completed.is_some_and(|t| t >= began)
            || entry.view.retry_at.is_some_and(|t| t > now())
        {
            return Ok(entry.view.clone());
        }
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| AppError::new(code::MODELS, "模型查询已停止"))?;
        let result = tokio::time::timeout(Duration::from_secs(10), fetch(&input, connection)).await;
        entry.view.checked_at = Some(now());
        entry.view.retry_at = None;
        match result {
            Ok(Ok(models)) => {
                entry.view.version = input.version.clone();
                entry.view.models = models;
                entry.view.stale = false;
                entry.view.error = None;
            }
            failure => {
                let (message, retry) = match failure {
                    Ok(Err(e)) => e,
                    _ => ("模型列表查询超时".into(), None),
                };
                entry.view.stale = entry.view.version != input.version;
                entry.view.error = Some(message);
                entry.view.retry_at = retry;
            }
        }
        entry.completed = Some(Instant::now());
        Ok(entry.view.clone())
    }
}
type Failure = (String, Option<u64>);

pub(super) fn local_list(
    gateway: &super::Gateway,
    uri: &hyper::Uri,
    headers: &hyper::HeaderMap,
) -> Option<hyper::Response<replay::WireBody>> {
    if matches!(gateway.client_id(), super::ClientId::Codex)
        || !matches!(uri.path(), "/models" | "/v1/models")
    {
        return None;
    }
    let pairs: Vec<_> = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()).collect();
    if pairs.iter().any(|(key, _)| key == "client_version") {
        return None;
    }
    let anthropic =
        gateway.client_id() == super::ClientId::Claude || headers.contains_key("anthropic-version");
    let selected_protocol = if gateway.client_id() == super::ClientId::Dsh {
        gateway
            .0
            .inner
            .lock()
            .store
            .dsh
            .as_ref()
            .map(|s| s.options.protocol)
    } else {
        None
    };
    let models = match gateway.model_union(selected_protocol) {
        Ok(models) if models.is_empty() => return None,
        Ok(models) => models,
        Err(e) => {
            return Some(super::forward::error(
                hyper::StatusCode::SERVICE_UNAVAILABLE,
                e.code.as_str(),
                &e.message,
            ))
        }
    };
    let mut value = if anthropic {
        match anthropic_list(&models, &pairs) {
            Ok(value) => value,
            Err(()) => {
                return Some(super::forward::error(
                    hyper::StatusCode::BAD_REQUEST,
                    code::MODELS,
                    "模型分页参数无效",
                ))
            }
        }
    } else {
        serde_json::json!({"object":"list", "data":models.iter().map(|id| serde_json::json!({
            "id":id,"object":"model","created":0,"owned_by":"vela"
        })).collect::<Vec<_>>()})
    };
    if anthropic {
        let usage = gateway.0.usage.lock().clone();
        let prices = usage.as_ref().and_then(|u| u.prices().ok());
        if let Some(prices) = prices {
            let (choices, _) = gateway.0.model_list.snapshot();
            for model in value["data"].as_array_mut().into_iter().flatten() {
                if let Some(name) = model["id"]
                    .as_str()
                    .and_then(|id| prices.models_dev.metadata_from(id, choices.source(id)))
                    .and_then(|m| m.name)
                {
                    model["display_name"] = name.into();
                }
            }
        }
    }
    Some(
        hyper::Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(replay::full(
                serde_json::to_vec(&value).expect("model list JSON"),
            ))
            .expect("model list response"),
    )
}
fn anthropic_list(
    models: &[String],
    pairs: &[(std::borrow::Cow<'_, str>, std::borrow::Cow<'_, str>)],
) -> std::result::Result<serde_json::Value, ()> {
    let get = |key: &str| -> std::result::Result<Option<&str>, ()> {
        let mut values = pairs
            .iter()
            .filter(|(name, _)| name == key)
            .map(|(_, value)| value.as_ref());
        let first = values.next();
        if values.next().is_some() {
            return Err(());
        }
        Ok(first)
    };
    let limit = get("limit")?
        .map(str::parse::<usize>)
        .transpose()
        .map_err(|_| ())?
        .unwrap_or(1000);
    if !(1..=1000).contains(&limit) {
        return Err(());
    }
    let after = get("after_id")?;
    let before = get("before_id")?;
    if after.is_some() && before.is_some() {
        return Err(());
    }
    let (start, end) = if let Some(id) = before {
        let end = models.iter().position(|v| v == id).ok_or(())?;
        (end.saturating_sub(limit), end)
    } else {
        let start = after
            .map(|id| models.iter().position(|v| v == id).map(|n| n + 1).ok_or(()))
            .transpose()?
            .unwrap_or(0);
        (start, (start + limit).min(models.len()))
    };
    let page = &models[start..end];
    Ok(serde_json::json!({
        "data":page.iter().map(|id| serde_json::json!({"id":id,"type":"model","display_name":id,
            "created_at":"1970-01-01T00:00:00Z"})).collect::<Vec<_>>(),
        "first_id":page.first(), "last_id":page.last(), "has_more":if before.is_some(){start>0}else{end<models.len()}
    }))
}
const CODEX_EFFORTS: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];
type CodexCache = Mutex<HashMap<String, (Instant, Arc<Vec<serde_json::Value>>)>>;
static CODEX_UPSTREAM: std::sync::LazyLock<CodexCache> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Codex's own instructions for a model its catalog does not list
/// (openai/codex rust-v0.169.0, models-manager/prompt.md, Apache-2.0).
const CODEX_PROMPT: &str = include_str!("codex_prompt.md");
const CODEX_VERSION: &str = "0.169.0";
/// The current provider answered nothing usable; another try may.
struct Unavailable;

/// Codex's model menu lists every model the queued providers offer: the
/// current provider's own entry when it has one, a copy of one of its entries
/// under the missing id otherwise, and Codex's unknown-model defaults when the
/// provider has no Codex catalog. `None` leaves the request to be forwarded.
pub(super) async fn codex_catalog(
    gateway: &super::Gateway,
    providers: &[String],
    uri: &hyper::Uri,
    headers: &hyper::HeaderMap,
) -> Option<hyper::Response<replay::WireBody>> {
    let query = uri.query().unwrap_or("");
    if gateway.client_id() != super::ClientId::Codex
        || !matches!(uri.path(), "/models" | "/v1/models")
        || !url::form_urlencoded::parse(query.as_bytes()).any(|(key, _)| key == "client_version")
    {
        return None;
    }
    let models = gateway.model_union(None).ok().filter(|m| !m.is_empty())?;
    let upstream = codex_template(gateway, providers.first(), query, headers)
        .await
        .ok()?;
    let body = codex_body(gateway, &models, &upstream);
    let etag = storage::digest(&body);
    Some(
        hyper::Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ETAG, format!("\"{etag}\""))
            .header("x-models-etag", etag)
            .body(replay::full(body))
            .expect("model catalog response"),
    )
}

/// The catalog file Codex loads at startup. With the provider out of reach,
/// the entries Vela wrote last stand in for its catalog. `None` when the
/// queue offers no model.
pub(super) async fn codex_file(
    gateway: &super::Gateway,
    previous: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let models = gateway.model_union(None).ok().filter(|m| !m.is_empty())?;
    // Ask as Codex would: providers answer a catalog request by client.
    let agent = gateway.request_state().store.successful_user_agent.clone();
    let agent = agent.and_then(|raw| header::HeaderValue::from_bytes(&raw).ok());
    let version = agent
        .as_ref()
        .and_then(|agent| agent.to_str().ok()?.split_once('/'))
        .map(|(_, rest)| {
            rest.split(|c: char| !c.is_ascii_digit() && c != '.')
                .next()
                .unwrap_or_default()
        })
        .filter(|v| v.split('.').count() == 3 && v.split('.').all(|part| !part.is_empty()))
        .unwrap_or(CODEX_VERSION);
    let mut headers = hyper::HeaderMap::new();
    if let Some(agent) = &agent {
        headers.insert(header::USER_AGENT, agent.clone());
    }
    let providers = gateway.routing_ids(None);
    let query = format!("client_version={version}");
    let upstream = match codex_template(gateway, providers.first(), &query, &headers).await {
        Ok(entries) => entries,
        Err(Unavailable) => Arc::new(previous.and_then(codex_models).unwrap_or_default()),
    };
    Some(codex_body(gateway, &models, &upstream))
}

fn codex_body(
    gateway: &super::Gateway,
    models: &[String],
    upstream: &[serde_json::Value],
) -> Vec<u8> {
    let usage = gateway.0.usage.lock().clone();
    let prices = usage.as_ref().and_then(|u| u.prices().ok());
    let (choices, _) = gateway.0.model_list.snapshot();
    let value = codex_merge(models, upstream, |id| {
        prices
            .as_ref()?
            .models_dev
            .metadata_from(id, choices.source(id))
    });
    serde_json::to_vec(&value).expect("model catalog JSON")
}

/// The current provider's Codex catalog, empty when it has none.
async fn codex_template(
    gateway: &super::Gateway,
    provider: Option<&String>,
    query: &str,
    headers: &hyper::HeaderMap,
) -> std::result::Result<Arc<Vec<serde_json::Value>>, Unavailable> {
    let Some((input, _)) = provider.and_then(|id| gateway.query_snapshot(id, false).ok()) else {
        return Ok(Arc::default());
    };
    let key = format!("{}:{}:{query}", input.id, input.version);
    let cached = CODEX_UPSTREAM
        .lock()
        .get(&key)
        .filter(|(at, _)| at.elapsed() < Duration::from_secs(300))
        .map(|(_, entries)| entries.clone());
    if let Some(entries) = cached {
        return Ok(entries);
    }
    // Codex gives a catalog request five seconds; answer inside them.
    let entries = tokio::time::timeout(
        Duration::from_secs(3),
        codex_upstream(&input, query, headers),
    )
    .await
    .map_err(|_| Unavailable)??;
    let entries = Arc::new(entries);
    let mut cache = CODEX_UPSTREAM.lock();
    cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(300));
    cache.insert(key, (Instant::now(), entries.clone()));
    Ok(entries)
}

async fn codex_upstream(
    input: &Query,
    query: &str,
    headers: &hyper::HeaderMap,
) -> std::result::Result<Vec<serde_json::Value>, Unavailable> {
    let path = format!("/v1/models?{query}");
    let target = path
        .parse()
        .ok()
        .and_then(|path| super::forward::target(&input.base, &path).ok())
        .ok_or(Unavailable)?;
    let mut builder = Request::builder()
        .method("GET")
        .uri(target)
        .header(header::AUTHORIZATION, format!("Bearer {}", input.token))
        .header(header::ACCEPT, "application/json")
        .header(header::ACCEPT_ENCODING, "identity");
    for name in ["user-agent", "originator", "version"] {
        if let Some(value) = headers.get(name) {
            builder = builder.header(name, value);
        }
    }
    let request = builder.body(replay::empty()).map_err(|_| Unavailable)?;
    let mut response = input
        .client
        .request(request)
        .await
        .map_err(|_| Unavailable)?;
    let status = response.status();
    if status.is_server_error() || status.as_u16() == 429 {
        return Err(Unavailable);
    }
    if !status.is_success() {
        return Ok(vec![]);
    }
    let mut bytes = Vec::new();
    while let Some(frame) = response.body_mut().frame().await {
        if let Some(data) = frame.map_err(|_| Unavailable)?.data_ref() {
            if bytes.len() + data.len() > 16_000_000 {
                return Ok(vec![]);
            }
            bytes.extend_from_slice(data);
        }
    }
    Ok(codex_models(&bytes).unwrap_or_default())
}

/// The entries of a Codex-format catalog; `None` for any other model list.
fn codex_models(bytes: &[u8]) -> Option<Vec<serde_json::Value>> {
    let mut value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let entries: Vec<_> = std::mem::take(value.get_mut("models")?.as_array_mut()?)
        .into_iter()
        .filter(|m| m["slug"].as_str().is_some_and(|slug| !slug.is_empty()))
        .collect();
    (!entries.is_empty()).then_some(entries)
}

fn codex_merge(
    models: &[String],
    upstream: &[serde_json::Value],
    metadata: impl Fn(&str) -> Option<crate::models_dev::Metadata>,
) -> serde_json::Value {
    let template = upstream.first();
    let levels = |meta: Option<&crate::models_dev::Metadata>| -> Vec<&'static str> {
        let known: Vec<_> = CODEX_EFFORTS
            .into_iter()
            .filter(|effort| {
                *effort != "none"
                    && meta
                        .and_then(|m| m.reasoning_efforts.as_ref())
                        .is_some_and(|efforts| efforts.contains_key(*effort))
            })
            .collect();
        if known.is_empty() {
            vec!["low", "medium", "high"]
        } else {
            known
        }
    };
    let set_levels = |entry: &mut serde_json::Value, efforts: Vec<&'static str>| {
        let described = |effort: &str| {
            template
                .and_then(|template| template["supported_reasoning_levels"].as_array())
                .into_iter()
                .flatten()
                .find(|level| level["effort"] == effort)
                .and_then(|level| level["description"].as_str())
                .map_or_else(|| format!("{effort} reasoning effort"), str::to_owned)
        };
        entry["default_reasoning_level"] = if efforts.contains(&"medium") {
            "medium"
        } else {
            efforts[0]
        }
        .into();
        entry["supported_reasoning_levels"] = efforts
            .iter()
            .map(|effort| serde_json::json!({"effort": effort, "description": described(effort)}))
            .collect();
    };
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::new();
    for id in models.iter().filter(|id| seen.insert(id.as_str())) {
        let meta = metadata(id);
        let name = meta.as_ref().and_then(|m| m.name.clone());
        let entry = if let Some(own) = upstream.iter().find(|m| m["slug"] == id.as_str()) {
            let mut entry = own.clone();
            let usable = entry["supported_reasoning_levels"]
                .as_array()
                .is_some_and(|levels| levels.iter().any(|level| level["effort"] != "none"));
            if !usable {
                set_levels(&mut entry, levels(meta.as_ref()));
            }
            if entry["display_name"] == id.as_str() {
                if let Some(name) = name {
                    entry["display_name"] = name.into();
                }
            }
            entry
        } else if let Some(template) = template {
            let mut entry = template.clone();
            entry["slug"] = id.as_str().into();
            entry["display_name"] = name.unwrap_or_else(|| id.clone()).into();
            entry["description"] = "Vela 网关提供的模型".into();
            entry["visibility"] = "list".into();
            if let Some(context) = meta.as_ref().and_then(|m| m.context) {
                entry["context_window"] = context.into();
                entry["max_context_window"] = context.into();
            }
            if meta.as_ref().is_some_and(|m| m.reasoning_efforts.is_some()) {
                set_levels(&mut entry, levels(meta.as_ref()));
            }
            entry
        } else {
            // What Codex assumes of a model it does not know, made listable.
            let context = meta.as_ref().and_then(|m| m.context).unwrap_or(272_000);
            let reasons = meta.as_ref().and_then(|m| m.reasoning) != Some(false);
            let mut entry = serde_json::json!({
                "slug": id,
                "display_name": name.unwrap_or_else(|| id.clone()),
                "description": "Vela 网关提供的模型",
                "supported_reasoning_levels": [],
                "shell_type": "shell_command",
                "visibility": "list",
                "supported_in_api": true,
                "priority": entries.len() + 1,
                "availability_nux": null,
                "upgrade": null,
                "base_instructions": CODEX_PROMPT,
                "include_apps_usage_instructions": false,
                "supports_reasoning_summaries": reasons,
                "support_verbosity": false,
                "default_verbosity": null,
                "apply_patch_tool_type": null,
                "truncation_policy": {"mode": "bytes", "limit": 10_000},
                "supports_parallel_tool_calls": false,
                "context_window": context,
                "max_context_window": context,
                "effective_context_window_percent": 95,
                "experimental_supported_tools": [],
                "input_modalities": ["text", "image"],
            });
            if reasons {
                set_levels(&mut entry, levels(meta.as_ref()));
            }
            entry
        };
        entries.push(entry);
    }
    serde_json::json!({ "models": entries })
}
async fn fetch(
    input: &Query,
    connection: Option<&super::grok_config::Connection>,
) -> std::result::Result<Vec<String>, Failure> {
    let fail = |message: &str| (message.to_owned(), None);
    let claude = input.client_id == super::ClientId::Claude
        || connection.is_some_and(|c| c.protocol == super::grok_config::Protocol::Messages);
    let target = super::forward::target(
        &input.base,
        &"/v1/models".parse().expect("static model path"),
    )
    .map_err(|_| fail("模型接口地址无效"))?;
    let target = if let Some(connection) = connection {
        connection
            .query_uri(target)
            .map_err(|_| fail("模型接口地址无效"))?
    } else {
        target
    };
    let mut url = url::Url::parse(&target.to_string()).map_err(|_| fail("模型接口地址无效"))?;
    let mut models = Vec::new();
    let mut total = 0usize;
    let mut cursors = std::collections::HashSet::new();
    loop {
        let mut builder = Request::builder()
            .method("GET")
            .uri(url.as_str())
            .header(header::AUTHORIZATION, format!("Bearer {}", input.token))
            .header(header::ACCEPT, "application/json")
            .header(header::ACCEPT_ENCODING, "identity");
        if claude {
            builder = builder.header("anthropic-version", "2023-06-01");
        }
        let mut request = builder
            .body(replay::empty())
            .map_err(|_| fail("供应商地址或 Key 格式无效"))?;
        if let Some(connection) = connection {
            connection
                .authenticate(request.headers_mut(), &input.token)
                .map_err(|_| fail("供应商请求头无效"))?;
        }
        let mut response = input.client.request(request).await.map_err(|e| {
            fail(
                &connector::classify(&e)
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "模型列表连接失败".into()),
            )
        })?;
        let status = response.status().as_u16();
        if status == 429 {
            let seconds = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok())
                .and_then(circuit::retry_after)
                .map(|d| d.as_secs().max(1))
                .unwrap_or(60);
            return Err(("模型列表查询被限流".into(), Some(now() + seconds)));
        }
        if (300..400).contains(&status) {
            return Err(fail("模型接口要求跳转，已停止发送凭据"));
        }
        if matches!(status, 401 | 403) {
            return Err(fail(&format!("模型列表认证失败（HTTP {status}）")));
        }
        if !(200..300).contains(&status) {
            return Err(fail(&format!("模型列表不可用（HTTP {status}）")));
        }
        let mut bytes = Vec::new();
        while let Some(frame) = response.body_mut().frame().await {
            let frame = frame.map_err(|_| fail("模型列表响应中断"))?;
            if let Some(data) = frame.data_ref() {
                if total + data.len() > 2_000_000 {
                    return Err(fail("模型列表超过 2 MB"));
                }
                total += data.len();
                bytes.extend_from_slice(data);
            }
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| fail("模型接口没有返回有效 JSON"))?;
        let data = value
            .get("data")
            .and_then(|v| v.as_array())
            .ok_or_else(|| fail("无法识别模型列表，可手动添加模型 ID"))?;
        let page: Vec<String> = data
            .iter()
            .filter_map(|v| v.get("id").and_then(|v| v.as_str()))
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 256
                    && !id.chars().any(char::is_control)
                    && !id.contains(&input.token)
            })
            .map(str::to_owned)
            .collect();
        models.extend(page);
        if models.len() > 4096 {
            return Err(fail("单供应商模型列表超过 4,096 个"));
        }
        if !claude || value.get("has_more").and_then(|v| v.as_bool()) != Some(true) {
            break;
        }
        let cursor = value
            .get("last_id")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty() && v.len() <= 256)
            .ok_or_else(|| fail("模型分页标记无效"))?;
        if !cursors.insert(cursor.to_owned()) || cursors.len() > 100 {
            return Err(fail("模型分页重复或超出上限"));
        }
        let pairs: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key != "after_id")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(pairs)
            .append_pair("after_id", cursor);
    }
    models.sort();
    models.dedup();
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::super::{
        model::{Edit, Store},
        ClientId, Gateway,
    };
    use super::*;
    const LIMIT: usize = 2 * 1024 * 1024;
    #[test]
    fn deleting_provider_evicts_all_versions_and_late_stale_query_cannot_keep_an_entry() {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::new(dir.path().join("models.json"));
        let entry = Arc::new(AsyncMutex::new(Entry {
            view: service.cached("fixture", "v1"),
            completed: None,
        }));
        let stale = Arc::downgrade(&entry);
        service
            .entries
            .lock()
            .insert("fixture:v1".into(), entry.clone());
        service
            .entries
            .lock()
            .insert("fixture:v2".into(), entry.clone());
        service.entries.lock().insert(
            "fixture-other:v1".into(),
            Arc::new(AsyncMutex::new(Entry {
                view: service.cached("fixture-other", "v1"),
                completed: None,
            })),
        );
        service.remove(&["fixture".into()]).unwrap();
        assert_eq!(service.entries.lock().len(), 1);
        // A query captured before deletion may acquire its memory entry afterwards.
        service
            .entries
            .lock()
            .insert("fixture:v1".into(), entry.clone());
        service.discard_version("fixture", "v1");
        drop(entry);
        assert!(stale.upgrade().is_none());
        assert_eq!(service.entries.lock().len(), 1);
    }
    #[test]
    fn corrupt_and_oversized_cache_is_preserved_until_success_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider-models.json");
        for bad in [b"broken".to_vec(), vec![b'x'; LIMIT + 1]] {
            std::fs::write(&path, &bad).unwrap();
            let service = Service::new(path.clone());
            let mut view = service.cached("provider", "v1");
            assert!(view.stale);
            assert_eq!(view.error.as_deref(), Some(UNAVAILABLE));
            service.commit(&view).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), bad);
            view.error = None;
            view.stale = false;
            view.models = vec!["Model-A".into(), "model-a".into()];
            view.checked_at = Some(1);
            service.commit(&view).unwrap();
            let reopened = Service::new(path.clone());
            assert_eq!(reopened.cached("provider", "v1").models, view.models);
            assert!(reopened.cached("provider", "v2").stale);
            reopened.remove(&["provider".into()]).unwrap();
            assert!(Service::new(path.clone())
                .cached("provider", "v1")
                .models
                .is_empty());
        }
    }
    #[test]
    fn codex_catalog_lists_the_union_with_upstream_entries_and_filled_gaps() {
        let upstream = serde_json::json!({"models":[
            {"slug":"fixture-a","display_name":"Fixture A","description":"fixture","priority":5,
             "default_reasoning_level":"medium","context_window":1000,"max_context_window":1000,
             "base_instructions":"fixture instructions","visibility":"hide",
             "supported_reasoning_levels":[{"effort":"low","description":"fixture low"},{"effort":"medium","description":"fixture medium"}]},
            {"slug":"fixture-b","display_name":"fixture-b","supported_reasoning_levels":[{"effort":"none","description":"off"}]},
            {"slug":"fixture-unlisted","display_name":"Unlisted","supported_reasoning_levels":[]},
            {"slug":""},
        ]});
        let entries = codex_models(&serde_json::to_vec(&upstream).unwrap()).unwrap();
        assert_eq!(entries.len(), 3);
        let meta = |id: &str| {
            (id != "fixture-a").then(|| crate::models_dev::Metadata {
                sources: vec![],
                model: id.into(),
                matched_by: "exact".into(),
                name: Some(format!("Named {id}")),
                reasoning: Some(true),
                context: (id == "fixture-new").then_some(2000),
                output: None,
                reasoning_efforts: (id == "fixture-new").then(|| {
                    ["high", "max", "xhigh"]
                        .into_iter()
                        .map(|k| (k.to_owned(), serde_json::Value::Null))
                        .collect()
                }),
            })
        };
        let models: Vec<String> = [
            "fixture-new",
            "fixture-a",
            "fixture-b",
            "fixture-plain",
            "fixture-a",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let value = codex_merge(&models, &entries, meta);
        let list = value["models"].as_array().unwrap();
        let slugs: Vec<_> = list.iter().map(|m| m["slug"].as_str().unwrap()).collect();
        // The union decides membership and order; upstream extras are left out.
        assert_eq!(
            slugs,
            ["fixture-new", "fixture-a", "fixture-b", "fixture-plain"]
        );
        let efforts = |m: &serde_json::Value| -> Vec<String> {
            m["supported_reasoning_levels"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l["effort"].as_str().unwrap().to_owned())
                .collect()
        };
        // An upstream entry is kept as it is.
        assert_eq!(list[1], entries[0]);
        // A missing id copies an upstream entry and takes its own facts.
        let new = &list[0];
        assert_eq!(new["display_name"], "Named fixture-new");
        assert_eq!(new["base_instructions"], "fixture instructions");
        assert_eq!(
            (&new["context_window"], &new["max_context_window"]),
            (&2000.into(), &2000.into())
        );
        assert_eq!(new["visibility"], "list");
        // Efforts Codex cannot parse are never listed.
        assert_eq!(efforts(new), ["high", "xhigh"]);
        assert_eq!(new["default_reasoning_level"], "high");
        // Unusable upstream levels and a raw display name are repaired.
        assert_eq!(efforts(&list[2]), ["low", "medium", "high"]);
        assert_eq!(list[2]["default_reasoning_level"], "medium");
        assert_eq!(
            list[2]["supported_reasoning_levels"][0]["description"],
            "fixture low"
        );
        assert_eq!(list[2]["display_name"], "Named fixture-b");
        // Without known efforts the copied entry keeps the template's levels.
        assert_eq!(efforts(&list[3]), ["low", "medium"]);
        assert_eq!(list[3]["context_window"], 1000);
        // Other list formats and empty catalogs are no Codex catalog.
        for other in [
            serde_json::json!({"object":"list","data":[{"id":"fixture-a"}]}),
            serde_json::json!({"models":[]}),
            serde_json::json!({"models":[{"id":"fixture-a"}]}),
            serde_json::json!({"error":{"message":"fixture"}}),
        ] {
            assert!(codex_models(&serde_json::to_vec(&other).unwrap()).is_none());
        }
        assert!(codex_models(b"<html>").is_none());
        // Without one, every model gets Codex's unknown-model defaults.
        let value = codex_merge(&models, &[], meta);
        let list = value["models"].as_array().unwrap();
        assert_eq!(list.len(), 4);
        for (index, entry) in list.iter().enumerate() {
            for key in [
                "slug",
                "display_name",
                "description",
                "supported_reasoning_levels",
                "shell_type",
                "visibility",
                "supported_in_api",
                "priority",
                "availability_nux",
                "upgrade",
                "support_verbosity",
                "default_verbosity",
                "apply_patch_tool_type",
                "truncation_policy",
                "experimental_supported_tools",
            ] {
                assert!(entry.get(key).is_some(), "{key}");
            }
            assert_eq!(entry["priority"], index + 1);
            assert_eq!(entry["visibility"], "list");
            assert!(entry["base_instructions"]
                .as_str()
                .unwrap()
                .starts_with("You are a coding agent running in the Codex CLI"));
        }
        assert_eq!(list[0]["display_name"], "Named fixture-new");
        assert_eq!(list[0]["context_window"], 2000);
        assert_eq!(efforts(&list[0]), ["high", "xhigh"]);
        assert_eq!(list[1]["display_name"], "fixture-a");
        assert_eq!(list[1]["context_window"], 272_000);
        assert_eq!(efforts(&list[1]), ["low", "medium", "high"]);
        assert_eq!(list[1]["default_reasoning_level"], "medium");
    }
    #[tokio::test]
    async fn union_is_ordered_exact_and_client_isolated_with_paginated_anthropic_lists() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("claude");
        storage::private_dir(&data).unwrap();
        let mut store = Store::default();
        for (n, models) in [vec!["Model-A", "shared"], vec!["shared", "model-a"]]
            .into_iter()
            .enumerate()
        {
            store
                .edit(
                    Edit::SaveProvider {
                        search_only: None,
                        end_on_message_stop: None,
                        filter_count_tokens: None,
                        buffer_complete_response: None,
                        endless_mode: None,
                        upstream_retry_seconds: None,
                        quota_access_token: None,
                        id: None,
                        base_url: format!("https://p{n}.example.invalid"),
                        token: format!("fixture-{n}"),
                        name: None,
                    },
                    false,
                )
                .unwrap();
            let p = store.providers.last_mut().unwrap();
            p.queued = true;
            p.allowed_models = Some(models.into_iter().map(str::to_owned).collect());
        }
        store
            .persist(&data.join("gateway.json"), "missing")
            .unwrap();
        let gateway = Gateway::new_client(data, ClientId::Claude, None).unwrap();
        assert_eq!(
            gateway.model_union(None).unwrap(),
            ["Model-A", "shared", "model-a"]
        );
        let response = local_list(
            &gateway,
            &"/v1/models?limit=1&after_id=Model-A".parse().unwrap(),
            &hyper::HeaderMap::new(),
        )
        .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["data"][0]["id"], "shared");
        assert_eq!(body["has_more"], true);
        assert_eq!(body["data"][0]["created_at"], "1970-01-01T00:00:00Z");
        for uri in ["/v1/models?client_version=1", "/v1/models/specific"] {
            assert!(
                local_list(&gateway, &uri.parse().unwrap(), &hyper::HeaderMap::new()).is_none()
            );
        }
        let codex = Gateway::new(dir.path().join("codex")).unwrap();
        assert!(codex.model_union(None).unwrap().is_empty());
        // Without queued models the catalog request is forwarded unchanged.
        for uri in ["/v1/models?client_version=1", "/v1/models"] {
            assert!(codex_catalog(
                &codex,
                &["fixture".into()],
                &uri.parse().unwrap(),
                &hyper::HeaderMap::new()
            )
            .await
            .is_none());
        }
        assert!(codex_catalog(
            &gateway,
            &["fixture".into()],
            &"/v1/models?client_version=1".parse().unwrap(),
            &hyper::HeaderMap::new()
        )
        .await
        .is_none());
        assert!(local_list(
            &codex,
            &"/v1/models".parse().unwrap(),
            &hyper::HeaderMap::new()
        )
        .is_none());
        assert_eq!(
            local_list(
                &gateway,
                &"/models?limit=0".parse().unwrap(),
                &hyper::HeaderMap::new()
            )
            .unwrap()
            .status(),
            400
        );
    }
}
