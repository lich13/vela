//! Bearer mode owns two `custom` values; API mode owns auth.json and the active base_url.
//! Edits use parser byte spans: unrelated TOML is never serialized again.
//! A Codex config that chose no provider reads as an empty pair, and gets the
//! `custom` selection and table added by the first write.
use super::{
    codex_api::{self, Connection},
    ClientId,
};
use crate::errors::code;
use crate::storage::{self, AppError, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use toml_edit::{Document, Item};
const FILE: &str = "gateway-recovery.json";
const KEYS: [&str; 2] = ["base_url", "experimental_bearer_token"];
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pair {
    pub base_url: Option<String>,
    pub token: Option<String>,
}
impl Pair {
    pub fn is_gateway(&self, client: ClientId, token: &str, ports: &[u16]) -> bool {
        let (Some(base), Some(actual_token)) = (&self.base_url, &self.token) else {
            return false;
        };
        if !super::listener::token_matches(actual_token.as_bytes(), token.as_bytes())
            || !super::client::loopback_address(base)
        {
            return false;
        }
        let Ok(url) = url::Url::parse(base) else {
            return false;
        };
        if url.scheme() != "http"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return false;
        }
        ports
            .iter()
            .filter_map(|port| url::Url::parse(&client.address(*port)).ok())
            .any(|expected| {
                url.port_or_known_default() == expected.port_or_known_default()
                    && url.path().trim_end_matches('/') == expected.path().trim_end_matches('/')
            })
    }
    pub fn fingerprint(&self) -> String {
        storage::digest(&serde_json::to_vec(self).expect("credential pair serializes"))
    }
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            base_url: Some(base.into()),
            token: Some(token.into()),
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Journal {
    client: ClientId,
    version: u32,
    connection: Connection,
    grok: Option<super::grok_config::Snapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    claude_hint: Option<super::claude_config::HintSnapshot>,
    path: PathBuf,
    before: Pair,
    applied: Pair,
    exit: Pair,
    live: bool,
    config_applied: bool,
    store_before: Option<String>,
    store_after: Option<String>,
}
/// Whether `current` is the gateway connection `applied` wrote, on the port it
/// was applied with or on the default shared one.
pub(super) fn gateway_alias(client: ClientId, applied: &Pair, current: &Pair) -> bool {
    let (Some(base), Some(token)) = (&applied.base_url, &applied.token) else {
        return false;
    };
    let Some(port) = url::Url::parse(base)
        .ok()
        .and_then(|url| url.port_or_known_default())
    else {
        return false;
    };
    applied.is_gateway(client, token, &[port])
        && current.is_gateway(client, token, &[port, super::client::DEFAULT_SHARED_PORT])
}
fn managed_alias(record: &Journal, current: &Pair) -> bool {
    record.live && gateway_alias(record.client, &record.applied, current)
}
fn parse(text: &str) -> Result<Document<&str>> {
    Document::parse(text)
        .map_err(|_| AppError::new(code::TOML, "配置不是有效的 TOML，请先修复配置"))
}
/// The provider choice in effect: the active profile's, else the top-level one.
fn selection<'a>(doc: &'a Document<&str>) -> Option<&'a Item> {
    doc.get("profile")
        .and_then(Item::as_str)
        .and_then(|p| doc.get("profiles")?.get(p)?.get("model_provider"))
        .or_else(|| doc.get("model_provider"))
}
fn custom<'a>(doc: &'a Document<&str>) -> Result<&'a Item> {
    if selection(doc).and_then(Item::as_str) != Some("custom") {
        return Err(AppError::new(
            code::CUSTOM,
            "当前生效的 provider 不是 custom，请在配置编辑器中处理",
        ));
    }
    doc.get("model_providers")
        .and_then(|p| p.get("custom"))
        .filter(|p| p.as_table_like().is_some())
        .ok_or_else(|| {
            AppError::new(
                code::CUSTOM,
                "缺少现有 model_providers.custom，请在配置编辑器中处理",
            )
        })
}
const SELECTION: &str = "model_provider = \"custom\"";
const TABLE: [&str; 3] = [
    "[model_providers.custom]",
    "name = \"custom\"",
    "wire_api = \"responses\"",
];
/// `text` with the `custom` selection and table it lacks, every existing byte
/// kept, or `None` when it lacks neither. A choice of another provider is
/// never overridden: whatever the additions cannot make pass `custom` fails
/// with the error `custom` gave for `text`.
fn complete(text: &str, doc: &Document<&str>) -> Result<Option<String>> {
    let Err(error) = custom(doc) else {
        return Ok(None);
    };
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    // Root keys are only read before the first table header, and a byte order
    // mark only at the very start.
    let start = if text.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    let mut output = text.to_owned();
    if selection(doc).is_none() {
        output.insert_str(start, &format!("{SELECTION}{newline}"));
    }
    if doc
        .get("model_providers")
        .and_then(|p| p.get("custom"))
        .is_none()
    {
        if output.len() > start {
            if !output.ends_with('\n') {
                output.push_str(newline);
            }
            output.push_str(newline);
        }
        for line in TABLE {
            output.push_str(line);
            output.push_str(newline);
        }
    }
    if parse(&output).is_ok_and(|next| custom(&next).is_ok()) {
        Ok(Some(output))
    } else {
        Err(error)
    }
}
/// Whether Codex has no `custom` connection yet and the first write would add
/// one. Codex itself then talks to its built-in provider.
pub(super) fn lacks_custom(raw: Option<&[u8]>) -> bool {
    std::str::from_utf8(raw.unwrap_or_default())
        .is_ok_and(|text| parse(text).is_ok_and(|doc| matches!(complete(text, &doc), Ok(Some(_)))))
}
pub(super) fn codex_lacks_custom(home: &Path) -> Result<bool> {
    Ok(lacks_custom(
        storage::read_user_optional(&ClientId::Codex.config(home))?.as_deref(),
    ))
}
fn codex_pair(text: &str) -> Result<Pair> {
    let doc = parse(text)?;
    if complete(text, &doc)?.is_some() {
        return Ok(Pair {
            base_url: None,
            token: None,
        });
    }
    pair(&doc)
}
fn pair(doc: &Document<&str>) -> Result<Pair> {
    let p = custom(doc)?;
    let read = |key| -> Result<Option<String>> {
        p.get(key)
            .map(|v| {
                v.as_str().map(str::to_owned).ok_or_else(|| {
                    AppError::new(code::CUSTOM, "custom 的地址和 Token 必须是字符串")
                })
            })
            .transpose()
    };
    Ok(Pair {
        base_url: read(KEYS[0])?,
        token: read(KEYS[1])?,
    })
}
pub fn read_connection(
    client: ClientId,
    home: &Path,
    connection: &Connection,
) -> Result<(String, Pair)> {
    match (client, connection) {
        (ClientId::Codex, Connection::ApiKey { provider }) => codex_api::read(home, provider),
        _ => read_for(client, home),
    }
}
fn stored_connection(store: Option<&str>) -> Result<Connection> {
    #[derive(Deserialize)]
    struct Stored {
        #[serde(default)]
        connection: Connection,
    }
    let Some(store) = store else {
        return Ok(Connection::Bearer);
    };
    serde_json::from_str::<Stored>(store)
        .map(|v| v.connection)
        .map_err(|_| AppError::new(code::STORE, "网关连接方式无效"))
}
fn write_record(
    record: &Journal,
    target: &Pair,
    expected: Option<&str>,
    allowed: Option<&[&Pair]>,
) -> Result<()> {
    if let Some(snapshot) = &record.grok {
        if record.live && target != &record.applied {
            return snapshot.restore_gateway(&record.path);
        }
        return snapshot.apply(&record.path, expected);
    }
    if let (ClientId::Codex, Connection::ApiKey { provider }) = (record.client, &record.connection)
    {
        let home = record
            .path
            .parent()
            .ok_or_else(|| AppError::new(code::RECOVERY, "API 恢复路径无效"))?;
        codex_api::write(home, provider, target, expected, allowed)
    } else {
        write_pair(
            record.client,
            &record.path,
            target,
            expected,
            allowed,
            record
                .claude_hint
                .as_ref()
                .map(|hint| (hint, target == &record.applied)),
        )
    }
}
/// Account synchronization must never adopt the gateway's local API credential.
pub(crate) fn manages_auth(data: &Path, home: &Path) -> bool {
    match load(data) {
        Ok(Some(record)) => {
            record.client == ClientId::Codex
                && matches!(record.connection, Connection::ApiKey { .. })
                && record.path == home.join("config.toml")
        }
        Ok(None) => false,
        Err(_) => true,
    }
}
pub fn read_for(client: ClientId, home: &Path) -> Result<(String, Pair)> {
    let raw = storage::read_user_optional(&client.config(home))?;
    read_raw(client, raw.as_deref())
}
pub(super) fn read_raw(client: ClientId, raw: Option<&[u8]>) -> Result<(String, Pair)> {
    let text = std::str::from_utf8(raw.unwrap_or(if client == ClientId::Claude {
        b"{}"
    } else {
        b""
    }))
    .map_err(|_| AppError::new(code::CONFIG, "配置不是 UTF-8"))?;
    Ok((storage::revision(raw), pair_for(client, text)?))
}
pub(super) fn pair_for(client: ClientId, text: &str) -> Result<Pair> {
    match client {
        ClientId::Codex => codex_pair(text),
        ClientId::Claude => super::claude_config::pair(text),
        ClientId::Grok => super::grok_config::pair(text),
        ClientId::Dsh => Err(AppError::new(code::CONFIG, "请使用 DSH profile 接入")),
        ClientId::Agy => Err(unmanaged()),
    }
}
/// Antigravity keeps no connection in a file: its gateway comes from the
/// environment of the shell that starts it.
fn unmanaged() -> AppError {
    AppError::new(code::CONFIG, "Antigravity 没有可接管的连接配置")
}
/// The edit that gives the provider table `table` the assignments it lacks.
/// An implicit or dotted table takes them under a dotted prefix in its parent
/// or at the top of the document, an inline table before its closing brace,
/// and any other table on the line after its last. `id` is the provider's key
/// as TOML writes it; `unlocated` names the inline or standard table that has
/// no span.
pub(super) fn missing_assignments(
    text: &str,
    doc: &Document<&str>,
    table: &Item,
    id: &str,
    assignments: &[(&str, String)],
    unlocated: impl Fn(bool) -> AppError,
) -> Result<(std::ops::Range<usize>, String)> {
    let parent = doc.get("model_providers").unwrap();
    let (container, prefix, root) = if table.as_table().is_some_and(|t| t.is_implicit())
        || table.as_inline_table().is_some_and(|t| t.is_dotted())
    {
        if parent.as_table().is_some_and(|t| t.is_implicit()) {
            (doc.as_item(), format!("model_providers.{id}."), true)
        } else {
            (parent, format!("{id}."), false)
        }
    } else {
        (table, String::new(), false)
    };
    let items: Vec<_> = assignments
        .iter()
        .map(|(k, v)| format!("{prefix}{k} = {v}"))
        .collect();
    if let Some(t) = container.as_inline_table() {
        let at = t.span().ok_or_else(|| unlocated(true))?.end - 1;
        let join = if t.is_empty() { "" } else { ", " };
        return Ok((at..at, format!("{join}{}", items.join(", "))));
    }
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let end = if root {
        0
    } else {
        let span = container
            .as_table()
            .and_then(|t| t.span())
            .ok_or_else(|| unlocated(false))?;
        text[span.end..]
            .find('\n')
            .map(|i| span.end + i + 1)
            .unwrap_or(text.len())
    };
    let lead = if end > 0 && !text[..end].ends_with('\n') {
        newline
    } else {
        ""
    };
    Ok((end..end, format!("{lead}{}{newline}", items.join(newline))))
}
pub fn patch(text: &str, target: &Pair) -> Result<String> {
    let doc = parse(text)?;
    let p = custom(&doc)?;
    pair(&doc)?;
    let mut edits = vec![];
    let mut missing = vec![];
    for (key, next) in KEYS.into_iter().zip([&target.base_url, &target.token]) {
        let next = next
            .as_ref()
            .ok_or_else(|| AppError::new(code::CUSTOM, "供应商地址和 Token 不完整"))?;
        let encoded = toml_edit::Value::from(next.clone()).to_string();
        if let Some(item) = p.get(key) {
            if item.as_str() != Some(next) {
                let span = item
                    .as_value()
                    .and_then(|v| v.span())
                    .ok_or_else(|| AppError::new(code::TOML, "无法定位配置字段"))?;
                edits.push((span, encoded));
            }
        } else {
            missing.push((key, encoded));
        }
    }
    if !missing.is_empty() {
        edits.push(missing_assignments(
            text,
            &doc,
            p,
            "custom",
            &missing,
            |inline| {
                let message = if inline {
                    "无法定位内联表"
                } else {
                    "无法定位 custom 表"
                };
                AppError::new(code::TOML, message)
            },
        )?);
    }
    edits.sort_by_key(|(s, _)| std::cmp::Reverse(s.start));
    let mut output = text.to_owned();
    for (span, value) in edits {
        output.replace_range(span, &value);
    }
    Ok(output)
}
fn write_pair(
    client: ClientId,
    path: &Path,
    target: &Pair,
    expected: Option<&str>,
    allowed: Option<&[&Pair]>,
    hint: Option<(&super::claude_config::HintSnapshot, bool)>,
) -> Result<()> {
    let raw = storage::read_user_optional(path)?;
    let revision = storage::revision(raw.as_deref());
    if expected.is_some_and(|e| e != revision) {
        return Err(AppError::new(code::CONFLICT, "配置已变化，请刷新后重试"));
    }
    let text = std::str::from_utf8(raw.as_deref().unwrap_or(if client == ClientId::Claude {
        b"{}"
    } else {
        b""
    }))
    .map_err(|_| AppError::new(code::TOML, "配置不是 UTF-8"))?;
    let current = pair_for(client, text)?;
    if allowed.is_some_and(|pairs| !pairs.contains(&&current)) {
        return Err(AppError::new(
            code::CONFLICT,
            "受管地址或 Token 已被外部修改，已保留现场与事务记录",
        ));
    }
    let output = match client {
        ClientId::Dsh => return Err(AppError::new(code::CONFIG, "请使用 DSH profile 接入")),
        ClientId::Agy => return Err(unmanaged()),
        ClientId::Codex => {
            let completed = complete(text, &parse(text)?)?;
            patch(completed.as_deref().unwrap_or(text), target)?
        }
        ClientId::Claude => super::claude_config::patch(text, target)?,
        ClientId::Grok => super::grok_config::Snapshot::new(text, target)?
            .applied()
            .to_owned(),
    };
    let output = if let Some((hint, apply)) = hint {
        hint.patch(&output, apply)?
    } else {
        output
    };
    if raw.is_none() || output != text {
        storage::atomic_write_user(path, output.as_bytes(), Some(&revision))?;
    }
    Ok(())
}
fn save(data: &Path, record: &Journal) -> Result<()> {
    storage::atomic_write(
        &data.join(FILE),
        &serde_json::to_vec(record)
            .map_err(|_| AppError::new(code::RECOVERY, "无法生成事务记录"))?,
        None,
    )
}
fn load(data: &Path) -> Result<Option<Journal>> {
    let Some(raw) = storage::read_optional(&data.join(FILE))? else {
        return Ok(None);
    };
    let record: Journal = serde_json::from_slice(&raw)
        .map_err(|_| AppError::new(code::RECOVERY, "恢复记录损坏，请保留并检查"))?;
    if record.version != 2 {
        return Err(AppError::new(
            code::RECOVERY,
            "恢复记录版本不受支持，请保留并检查",
        ));
    }
    Ok(Some(record))
}
fn complete_store(data: &Path, record: &mut Journal, keep_journal: bool) -> Result<()> {
    if let Some(after) = &record.store_after {
        let path = data.join("gateway.json");
        let current = storage::read_optional(&path)?;
        if current.as_deref() != Some(after.as_bytes()) {
            storage::atomic_write(
                &path,
                after.as_bytes(),
                Some(&storage::revision(
                    record.store_before.as_deref().map(str::as_bytes),
                )),
            )?;
        }
        record.store_before = None;
        record.store_after = None;
        if keep_journal {
            save(data, record)?;
        }
    }
    Ok(())
}
pub fn import_for(client: ClientId, home: &Path) -> Result<(String, String)> {
    let (_, p) = read_for(client, home)?;
    let base = p
        .base_url
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::new(code::IMPORT, "配置缺少供应商地址"))?;
    let token = p
        .token
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::new(code::IMPORT, "配置缺少供应商 Token"))?;
    if token.starts_with("gs_") && super::client::loopback_address(&base) {
        return Err(AppError::new(code::MANAGED, "不能导入网关的本地凭据"));
    }
    Ok((base, token))
}
#[allow(clippy::too_many_arguments)]
pub fn attach_store_for(
    client: ClientId,
    data: &Path,
    home: &Path,
    port: u16,
    token: &str,
    exit: Pair,
    expected: &str,
    store: Option<(Option<String>, String)>,
) -> Result<()> {
    if data.join(FILE).exists() {
        return Err(AppError::new(code::RECOVERY, "请先处理现有配置事务"));
    }
    let connection = stored_connection(store.as_ref().map(|(_, after)| after.as_str()))?;
    let (revision, before) = read_connection(client, home, &connection)?;
    if revision != expected {
        return Err(AppError::new(code::CONFLICT, "配置已变化，请刷新后重试"));
    }
    let mut record = Journal {
        claude_hint: if client == ClientId::Claude {
            let raw = storage::read_user_optional(&client.config(home))?;
            let text = std::str::from_utf8(raw.as_deref().unwrap_or(b"{}"))
                .map_err(|_| AppError::new(code::CLAUDE_CONFIG, "配置不是 UTF-8"))?;
            Some(super::claude_config::HintSnapshot::new(text)?)
        } else {
            None
        },
        version: 2,
        grok: grok_snapshot(
            client,
            home,
            &Pair::new(&client.address(port), token),
            store.as_ref().map(|(_, s)| s.as_str()),
            Some(&exit),
        )?,
        connection,
        client,
        path: client.config(home),
        exit: if client == ClientId::Grok {
            before.clone()
        } else {
            exit
        },
        before,
        applied: Pair::new(&client.address(port), token),
        live: true,
        config_applied: false,
        store_before: store.as_ref().and_then(|(before, _)| before.clone()),
        store_after: store.map(|(_, after)| after),
    };
    save(data, &record)?;
    if let Err(e) = write_record(&record, &record.applied, Some(expected), None) {
        if read_connection(client, home, &record.connection).is_ok_and(|(_, p)| p != record.applied)
        {
            fs::remove_file(data.join(FILE)).map_err(storage::io_error)?;
        }
        return Err(e);
    }
    record.config_applied = true;
    if record.store_after.is_some() {
        complete_store(data, &mut record, true)
    } else {
        save(data, &record)
    }
}
fn grok_snapshot(
    client: ClientId,
    home: &Path,
    target: &Pair,
    store: Option<&str>,
    provider_pair: Option<&Pair>,
) -> Result<Option<super::grok_config::Snapshot>> {
    if client != ClientId::Grok {
        return Ok(None);
    }
    let store = store
        .map(serde_json::from_str::<super::model::Store>)
        .transpose()
        .map_err(|_| AppError::new(code::STORE, "Grok 网关配置无效"))?;
    let provider = store.as_ref().and_then(|s| {
        s.providers
            .iter()
            .find(|p| {
                provider_pair.is_some_and(|pair| {
                    pair.base_url.as_deref() == Some(&p.base_url)
                        && pair.token.as_deref() == Some(&p.token)
                })
            })
            .or_else(|| {
                s.selected
                    .as_ref()
                    .and_then(|id| s.providers.iter().find(|p| &p.id == id))
            })
            .map(|p| s.grok_connections.get(&p.id).cloned().unwrap_or_default())
    });
    let text = super::grok_config::read(home)?.1;
    // With no provider to speak for it, the model keeps the protocol it has.
    let connection = provider.unwrap_or_else(|| super::grok_config::Connection {
        protocol: super::grok_config::api_profile(&text)
            .map(|p| p.connection.protocol)
            .unwrap_or_default(),
        ..Default::default()
    });
    Ok(Some(
        if store.as_ref().is_some_and(|s| s.grok_api_model.is_some())
            && super::grok_config::api_profile(&text)
                .is_ok_and(|p| p.selected == super::grok_config::MANAGED)
        {
            super::grok_config::Snapshot::api(&text, target, &connection, provider_pair.is_none())?
        } else if provider_pair.is_some() {
            super::grok_config::Snapshot::with_protocol(&text, target, Some(connection.protocol))?
        } else {
            super::grok_config::Snapshot::with_connection(&text, target, &connection)?
        },
    ))
}
fn grok_record_unchanged(record: &Journal, text: &str) -> Result<bool> {
    let snapshot = record
        .grok
        .as_ref()
        .ok_or_else(|| AppError::new(code::RECOVERY, "缺少 Grok 配置事务"))?;
    if record.live {
        snapshot.gateway_unchanged(text)
    } else {
        snapshot.unchanged(text)
    }
}
pub fn grok_unchanged(data: &Path, text: &str) -> Result<bool> {
    let record = load(data)?.ok_or_else(|| AppError::new(code::RECOVERY, "缺少 Grok 配置事务"))?;
    grok_record_unchanged(&record, text)
}
pub fn exit_pair(data: &Path) -> Result<Option<Pair>> {
    Ok(load(data)?.map(|r| r.exit))
}
/// What stopping writes when no provider is left to write: the connection the
/// takeover replaced. Codex cannot take a field out of its files again, so a
/// field that was not there keeps the gateway's value, and an API key that was
/// not there is removed so the local one is never taken for an account.
pub fn unprovided(
    client: ClientId,
    connection: &Connection,
    before: &Pair,
    applied: &Pair,
) -> Pair {
    match (client, connection) {
        (ClientId::Codex, Connection::ApiKey { .. }) => Pair {
            base_url: before.base_url.clone().or_else(|| applied.base_url.clone()),
            token: before.token.clone(),
        },
        (ClientId::Codex, Connection::Bearer)
            if before.base_url.is_none() || before.token.is_none() =>
        {
            applied.clone()
        }
        _ => before.clone(),
    }
}
/// The same for the takeover in progress.
pub fn unprovided_exit(data: &Path) -> Result<Pair> {
    let record = load(data)?.ok_or_else(|| AppError::new(code::RECOVERY, "缺少网关事务记录"))?;
    Ok(unprovided(
        record.client,
        &record.connection,
        &record.before,
        &record.applied,
    ))
}
#[allow(clippy::too_many_arguments)]
pub fn commit_store_for(
    client: ClientId,
    data: &Path,
    home: &Path,
    before_store: Option<String>,
    after_store: String,
    target: Pair,
    running: bool,
    expected_config: Option<&str>,
) -> Result<()> {
    let mut record = if running {
        let record =
            load(data)?.ok_or_else(|| AppError::new(code::RECOVERY, "缺少网关事务记录"))?;
        if record.store_after.is_some() {
            return Err(AppError::new(
                code::RECOVERY,
                "请先停止网关并完成待处理事务",
            ));
        }
        let (_, current) = read_connection(client, home, &record.connection)?;
        if (current != record.applied && !managed_alias(&record, &current))
            || (client == ClientId::Grok
                && !grok_record_unchanged(&record, &super::grok_config::read(home)?.1)?)
        {
            return Err(AppError::new(
                code::CONFLICT,
                "受管地址或 Token 已被外部修改",
            ));
        }
        record
    } else {
        if data.join(FILE).exists() {
            return Err(AppError::new(code::RECOVERY, "请先处理待恢复的配置事务"));
        }
        let connection = stored_connection(Some(&after_store))?;
        let (revision, before) = read_connection(client, home, &connection)?;
        if expected_config.is_some_and(|e| e != revision) {
            return Err(AppError::new(code::CONFLICT, "配置已变化，请刷新后重试"));
        }
        Journal {
            claude_hint: None,
            version: 2,
            grok: grok_snapshot(client, home, &target, Some(&after_store), None)?,
            connection,
            client,
            path: client.config(home),
            before,
            applied: target.clone(),
            exit: target.clone(),
            live: false,
            config_applied: false,
            store_before: None,
            store_after: None,
        }
    };
    if client != ClientId::Grok || !running {
        record.exit = target;
    }
    record.store_before = before_store;
    record.store_after = Some(after_store);
    if running {
        record.config_applied = true;
        save(data, &record)?;
        complete_store(data, &mut record, true)?;
    } else {
        save(data, &record)?;
        write_record(
            &record,
            &record.applied,
            expected_config,
            Some(&[&record.before, &record.applied]),
        )?;
        record.config_applied = true;
        save(data, &record)?;
        complete_store(data, &mut record, false)?;
        fs::remove_file(data.join(FILE)).map_err(storage::io_error)?;
    }
    Ok(())
}
fn detach_record(data: &Path, mut record: Journal) -> Result<()> {
    let current = if record.grok.is_none() {
        let home = record
            .path
            .parent()
            .ok_or_else(|| AppError::new(code::RECOVERY, "恢复路径无效"))?;
        Some(read_connection(record.client, home, &record.connection)?.1)
    } else {
        None
    };
    let mut allowed = if record.config_applied {
        vec![&record.applied, &record.exit]
    } else {
        vec![&record.before, &record.applied, &record.exit]
    };
    if let Some(current) = &current {
        if managed_alias(&record, current) {
            allowed.push(current);
        }
    }
    if let Some(snapshot) = &record.grok {
        if record.live {
            snapshot.restore_gateway(&record.path)?;
        } else {
            snapshot.apply(&record.path, None)?;
        }
    } else {
        write_record(&record, &record.exit, None, Some(&allowed))?;
    }
    // The published model list follows the gateway out. It is not part of the
    // transaction: whatever is left is valid and the next start rewrites it.
    match (record.client, record.path.parent()) {
        (ClientId::Codex, Some(home)) => {
            let _ = super::codex_catalog::clear(home, false);
        }
        (ClientId::Grok, _) => {
            let _ = super::grok_config::sync_catalog(&record.path, None);
        }
        _ => {}
    }
    complete_store(data, &mut record, false)?;
    fs::remove_file(data.join(FILE)).map_err(storage::io_error)
}
/// Startup, normal stop and exit all finish the same transaction, preserving unrelated edits.
pub fn recover(data: &Path) -> Result<()> {
    let Some(record) = load(data)? else {
        return Ok(());
    };
    detach_record(data, record)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    pub fn read(home: &Path) -> Result<(String, Pair)> {
        read_for(ClientId::Codex, home)
    }
    pub fn attach(
        data: &Path,
        home: &Path,
        port: u16,
        token: &str,
        exit: Pair,
        expected: &str,
    ) -> Result<()> {
        attach_store_for(
            ClientId::Codex,
            data,
            home,
            port,
            token,
            exit,
            expected,
            None,
        )
    }
    fn update_exit(data: &Path, target: Pair) -> Result<()> {
        let mut record =
            load(data)?.ok_or_else(|| AppError::new(code::RECOVERY, "缺少网关事务记录"))?;
        if record.store_after.is_some() {
            return Err(AppError::new(code::RECOVERY, "供应商事务尚未完成"));
        }
        if record.exit != target {
            record.exit = target;
            save(data, &record)?;
        }
        Ok(())
    }
    const ORIGINAL: &str = "# settings\r\nmodel_provider='custom'\r\nmodel='unchanged'\r\n[model_providers.custom] # comment\r\nbase_url = 'https://original.test/v1' # address\r\nexperimental_bearer_token = 'old' # key\r\nsupports_websockets = false\r\nwire_api = 'responses'\r\n[other]\r\na=42\r\n";
    pub fn masked(text: &str) -> String {
        let doc = parse(text).unwrap();
        let p = custom(&doc).unwrap();
        let mut spans: Vec<_> = KEYS
            .iter()
            .filter_map(|k| p.get(k)?.as_value()?.span())
            .collect();
        spans.sort_by_key(|s| std::cmp::Reverse(s.start));
        let mut s = text.to_owned();
        for span in spans {
            s.replace_range(span, "<value>");
        }
        s
    }
    #[test]
    fn replacements_are_byte_exact_outside_two_values() {
        for text in [ORIGINAL,"profile='work'\nprofiles={work={model_provider='custom',model='keep'}}\nmodel_providers = { custom = { base_url='https://x.test', experimental_bearer_token='a', other=true }, spare={x=1} }\n"] {
            let out=patch(text,&Pair::new("https://next.test/prefix/v1","a\\b\"c\n" )).unwrap();
            assert_eq!(masked(text),masked(&out));
            assert_eq!(pair(&parse(&out).unwrap()).unwrap().token.as_deref(),Some("a\\b\"c\n"));
        }
    }
    #[test]
    fn missing_fields_insert_into_existing_normal_inline_and_dotted_tables() {
        for text in ["model_provider='custom'\r\n[model_providers.custom] # keep\r\nname='stay'\r\n[next]\r\nx=1", "model_provider='custom'\nmodel_providers={custom={name='stay'},spare={x=1}}", "model_provider='custom'\n[model_providers]\ncustom.name='stay'\n", "model_provider='custom'\nmodel_providers.custom.name='stay'\n", "model_provider='custom'\nmodel_providers={custom.name='stay'}\n"] {
            let out=patch(text,&Pair::new("https://x.test","k")).unwrap();
            assert_eq!(pair(&parse(&out).unwrap()).unwrap().token.as_deref(),Some("k"));
            assert!(out.contains("name='stay'"));
        }
        for text in [
            "",
            "model_provider='other'\n[model_providers.custom]\nx=1",
            "model_provider='custom'\n",
            "model_provider='custom'\n[model_providers.custom]\nbase_url=42",
        ] {
            assert!(patch(text, &Pair::new("https://x.test", "k")).is_err());
        }
    }
    fn completed(text: &str) -> Result<Option<String>> {
        complete(text, &parse(text)?)
    }
    #[test]
    fn missing_selection_and_table_are_added_around_untouched_bytes() {
        const BOM: &str = "\u{feff}";
        // (text, selection added, table added)
        let cases = [
            ("", true, true),
            ("model = 'keep'\n[other]\na = 1", true, true),
            (
                "# top\nmodel = 'keep' # note\n\n[model_providers.other] # spare\nbase_url = 'https://other.example.invalid/v1'\n",
                true,
                true,
            ),
            (
                "profile = 'work'\n[profiles.work]\nmodel = 'keep'\n",
                true,
                true,
            ),
            ("model_provider = 'custom'\nmodel = 'keep'\n", false, true),
            (
                "profile = 'work'\n[profiles.work]\nmodel_provider = 'custom'\n",
                false,
                true,
            ),
            (
                "model = 'keep'\n[model_providers.custom]\nname = 'mine'\n",
                true,
                false,
            ),
            ("# top\r\nmodel = 'keep'\r\n[other]\r\na = 1\r\n", true, true),
            ("\u{feff}model = 'keep'\n[other]\na = 1\n", true, true),
            (BOM, true, true),
        ];
        let target = Pair::new("http://127.0.0.1:43721/v1", "a\\b\"c");
        for (text, selection_added, table_added) in cases {
            let output = completed(text).unwrap().unwrap();
            let crlf = text.contains("\r\n");
            let newline = if crlf { "\r\n" } else { "\n" };
            let bom = if text.starts_with(BOM) { BOM } else { "" };
            let body = &text[bom.len()..];
            let head = if selection_added {
                format!("{bom}{SELECTION}{newline}{body}")
            } else {
                text.to_owned()
            };
            let tail = TABLE.map(|line| format!("{line}{newline}")).concat();
            assert!(output.starts_with(&head), "{text:?}");
            if table_added {
                assert!(output.ends_with(&tail), "{text:?}");
                assert!(output[head.len()..output.len() - tail.len()]
                    .chars()
                    .all(|c| c == '\r' || c == '\n'));
            } else {
                assert_eq!(output, head);
            }
            assert_eq!(crlf, output.contains("\r\n"));
            assert!(!output
                .replace("\r\n", "")
                .contains(if crlf { '\n' } else { '\r' }));
            // A completed config has nothing left to add and reads as custom
            // without its two values.
            assert_eq!(completed(&output).unwrap(), None, "{text:?}");
            assert!(
                pair_for(ClientId::Codex, text).unwrap()
                    == Pair {
                        base_url: None,
                        token: None
                    }
            );
            assert!(lacks_custom(Some(text.as_bytes())) && !lacks_custom(Some(output.as_bytes())));
            let patched = patch(&output, &target).unwrap();
            assert!(pair_for(ClientId::Codex, &patched).unwrap() == target);
            // The two values go into the table, wherever it is.
            if table_added {
                assert!(patched.starts_with(&head), "{text:?}");
            } else {
                assert!(body.lines().all(|line| patched.contains(line)));
            }
            let doc = parse(&patched).unwrap();
            let table = custom(&doc).unwrap();
            let expected_name = if table_added { "custom" } else { "mine" };
            assert_eq!(
                table.get("name").and_then(Item::as_str),
                Some(expected_name)
            );
            if table_added {
                assert_eq!(
                    table.get("wire_api").and_then(Item::as_str),
                    Some("responses")
                );
            }
        }
        assert!(lacks_custom(None));
        assert_eq!(completed(ORIGINAL).unwrap(), None);
        assert!(!lacks_custom(Some(ORIGINAL.as_bytes())));
    }
    #[test]
    fn another_choice_or_an_unusable_shape_is_reported_and_never_written() {
        let other = "当前生效的 provider 不是 custom，请在配置编辑器中处理";
        let missing = "缺少现有 model_providers.custom，请在配置编辑器中处理";
        for (text, message) in [
            ("model_provider = 'other'\n", other),
            (
                "model_provider = 'other'\n[model_providers.custom]\nbase_url = 'https://x.example.invalid'\n",
                other,
            ),
            (
                "profile = 'work'\nmodel_provider = 'custom'\n[profiles.work]\nmodel_provider = 'other'\n[model_providers.custom]\nname = 'mine'\n",
                other,
            ),
            ("model_provider = 42\n", other),
            (
                "model_providers = { other = { name = 'spare' } }\n",
                other,
            ),
            (
                "model_provider = 'custom'\nmodel_providers = { other = { name = 'spare' } }\n",
                missing,
            ),
            ("model_provider = 'custom'\nmodel_providers.custom = 1\n", missing),
        ] {
            assert_eq!(completed(text).unwrap_err().message, message, "{text}");
            assert_eq!(
                pair_for(ClientId::Codex, text).err().unwrap().message,
                message
            );
            assert!(!lacks_custom(Some(text.as_bytes())));
            let t = tempfile::tempdir().unwrap();
            let path = t.path().join("config.toml");
            fs::write(&path, text).unwrap();
            assert!(read(t.path()).is_err());
            let error = write_pair(
                ClientId::Codex,
                &path,
                &Pair::new("http://127.0.0.1:43721/v1", "local"),
                None,
                None,
                None,
            )
            .unwrap_err();
            assert_eq!(error.message, message);
            assert_eq!(fs::read_to_string(&path).unwrap(), text);
        }
        assert!(!lacks_custom(Some(b"not = [toml")));
        assert!(!lacks_custom(Some(&[0xff])));
    }
    #[test]
    fn start_without_config_creates_custom_and_stop_points_it_at_the_provider() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let path = home.join("config.toml");
        let (revision, before) = read(&home).unwrap();
        assert_eq!(revision, "missing");
        assert!(before.base_url.is_none() && before.token.is_none());
        assert!(!path.exists());
        let provider = Pair::new("https://a.example.invalid/v1", "fixture-a");
        attach(t.path(), &home, 15722, "local", provider.clone(), &revision).unwrap();
        let live = fs::read_to_string(&path).unwrap();
        assert_eq!(
            live,
            "model_provider = \"custom\"\n\n[model_providers.custom]\nbase_url = \"http://127.0.0.1:15722/v1\"\nexperimental_bearer_token = \"local\"\nname = \"custom\"\nwire_api = \"responses\"\n"
        );
        assert!(read(&home)
            .unwrap()
            .1
            .is_gateway(ClientId::Codex, "local", &[15722]));
        recover(t.path()).unwrap();
        assert!(!t.path().join(FILE).exists());
        assert!(read(&home).unwrap().1 == provider);
        assert_eq!(masked(&fs::read_to_string(&path).unwrap()), masked(&live));
        attach(
            t.path(),
            &home,
            15722,
            "local",
            provider,
            &read(&home).unwrap().0,
        )
        .unwrap();
        assert!(read(&home)
            .unwrap()
            .1
            .is_gateway(ClientId::Codex, "local", &[15722]));
    }
    #[test]
    fn config_removed_while_running_is_a_conflict_and_is_not_recreated() {
        for original in [None, Some(ORIGINAL), Some("model = 'keep'\n")] {
            let t = tempfile::tempdir().unwrap();
            let home = t.path().join("home");
            let path = home.join("config.toml");
            if let Some(text) = original {
                fs::create_dir_all(&home).unwrap();
                fs::write(&path, text).unwrap();
            }
            attach(
                t.path(),
                &home,
                15722,
                "local",
                Pair::new("https://a.example.invalid/v1", "fixture-a"),
                &read(&home).unwrap().0,
            )
            .unwrap();
            let journal = fs::read(t.path().join(FILE)).unwrap();
            fs::remove_file(&path).unwrap();
            assert_eq!(recover(t.path()).err().unwrap().code, code::CONFLICT);
            assert!(!path.exists());
            assert_eq!(fs::read(t.path().join(FILE)).unwrap(), journal);
            // The same holds for a file put back without the managed values.
            fs::write(&path, "model = 'keep'\n").unwrap();
            assert_eq!(recover(t.path()).err().unwrap().code, code::CONFLICT);
            assert_eq!(fs::read_to_string(&path).unwrap(), "model = 'keep'\n");
        }
    }
    #[test]
    fn stop_writes_current_provider_preserving_unrelated_edits_and_auth() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("config.toml");
        fs::write(&path, ORIGINAL).unwrap();
        fs::write(t.path().join("auth.json"), "untouched").unwrap();
        attach(
            t.path(),
            t.path(),
            15722,
            "local",
            Pair::new("https://a.test", "a"),
            &read(t.path()).unwrap().0,
        )
        .unwrap();
        let live = fs::read_to_string(&path)
            .unwrap()
            .replace("model='unchanged'", "model='external'");
        fs::write(&path, &live).unwrap();
        update_exit(t.path(), Pair::new("https://b.test", "b")).unwrap();
        recover(t.path()).unwrap();
        let output = fs::read_to_string(&path).unwrap();
        assert_eq!(masked(&output), masked(&live));
        assert_eq!(
            read(t.path()).unwrap().1.base_url.as_deref(),
            Some("https://b.test")
        );
        assert_eq!(
            fs::read_to_string(t.path().join("auth.json")).unwrap(),
            "untouched"
        );
        assert!(!t.path().join(FILE).exists());
    }
    #[test]
    fn owned_field_conflict_and_revision_failure_preserve_scene() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("config.toml");
        fs::write(&path, ORIGINAL).unwrap();
        assert!(attach(
            t.path(),
            t.path(),
            15722,
            "local",
            Pair::new("https://a.test", "a"),
            "stale"
        )
        .is_err());
        assert!(!t.path().join(FILE).exists());
        attach(
            t.path(),
            t.path(),
            15722,
            "local",
            Pair::new("https://a.test", "a"),
            &read(t.path()).unwrap().0,
        )
        .unwrap();
        let changed = fs::read_to_string(&path)
            .unwrap()
            .replace("local", "externally-edited");
        fs::write(&path, &changed).unwrap();
        assert!(recover(t.path()).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), changed);
        assert!(t.path().join(FILE).exists());
    }
    #[test]
    fn crash_between_config_and_store_finishes_transaction_and_protects_permissions() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("config.toml");
        fs::write(&path, ORIGINAL).unwrap();
        let old = Pair::new("https://original.test/v1", "old");
        let new = Pair::new("https://next.test", "new");
        let mut record = Journal {
            claude_hint: None,
            client: ClientId::Codex,
            connection: Connection::Bearer,
            grok: None,
            version: 2,
            path: path.clone(),
            before: old,
            applied: new.clone(),
            exit: new.clone(),
            live: false,
            config_applied: false,
            store_before: None,
            store_after: Some("{\"selected\":\"new\"}".into()),
        };
        save(t.path(), &record).unwrap();
        write_pair(ClientId::Codex, &path, &new, None, None, None).unwrap();
        record.config_applied = true;
        save(t.path(), &record).unwrap();
        recover(t.path()).unwrap();
        assert_eq!(
            fs::read_to_string(t.path().join("gateway.json")).unwrap(),
            "{\"selected\":\"new\"}"
        );
        assert_eq!(
            masked(&fs::read_to_string(&path).unwrap()),
            masked(ORIGINAL)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
    #[test]
    fn unreadable_record_is_kept_and_never_touches_config() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("config.toml");
        fs::write(&path, ORIGINAL).unwrap();
        let applied="model_provider='gpt_switch_gateway'\n[model_providers.gpt_switch_gateway]\nbase_url='http://127.0.0.1:15722/v1'\nexperimental_bearer_token='gs_local'\n";
        let old =
            serde_json::json!({"path":path,"original":ORIGINAL,"applied":applied,"profile":null});
        fs::write(t.path().join(FILE), old.to_string()).unwrap();
        assert!(recover(t.path()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), ORIGINAL);
        assert!(t.path().join(FILE).exists());
    }
    #[test]
    fn journal_write_failure_never_changes_config() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("config.toml");
        fs::write(&path, ORIGINAL).unwrap();
        fs::create_dir(t.path().join(FILE)).unwrap();
        assert!(attach(
            t.path(),
            t.path(),
            15722,
            "local",
            Pair::new("https://a.test", "a"),
            &read(t.path()).unwrap().0
        )
        .is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), ORIGINAL);
    }
}
