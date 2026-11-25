use super::connector::BoxError;
use crate::storage;
use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full, StreamBody};
use hyper::{
    body::{Body, Frame},
    HeaderMap,
};
use std::{io, path::Path, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
pub type WireBody = UnsyncBoxBody<Bytes, BoxError>;
const MEMORY_LIMIT: usize = 2 * 1024 * 1024;
const MAX_BODY: u64 = 1024 * 1024 * 1024;
pub(super) fn decode_prefix(
    bytes: &[u8],
    encoding: &str,
    limit: usize,
) -> Result<Vec<u8>, BoxError> {
    use std::io::Read;
    let reader = std::io::Cursor::new(bytes);
    let decoded: Box<dyn Read + '_> = match encoding.trim().to_ascii_lowercase().as_str() {
        "" | "identity" => Box::new(reader),
        "gzip" => Box::new(flate2::read::GzDecoder::new(reader)),
        "deflate" => Box::new(flate2::read::ZlibDecoder::new(reader)),
        "zstd" => Box::new(zstd::stream::read::Decoder::new(reader)?),
        "br" => Box::new(brotli::Decompressor::new(reader, 4096)),
        _ => return Err(io::Error::other("unsupported inspection encoding").into()),
    };
    let mut text = Vec::new();
    if let Err(error) = decoded.take(limit as u64).read_to_end(&mut text) {
        // Inspection may receive only a compressed prefix. Keep successfully
        // decoded bytes when the remainder was deliberately cut off.
        if text.is_empty() || error.kind() != io::ErrorKind::UnexpectedEof {
            return Err(error.into());
        }
    }
    Ok(text)
}
pub fn empty() -> WireBody {
    Full::new(Bytes::new())
        .map_err(|e| match e {})
        .boxed_unsync()
}
pub fn full(bytes: impl Into<Bytes>) -> WireBody {
    Full::new(bytes.into())
        .map_err(|e| match e {})
        .boxed_unsync()
}
enum Payload {
    Memory(Bytes),
    Disk(Arc<tempfile::NamedTempFile>),
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum TurnEvidence {
    First,
    History,
    #[default]
    Unknown,
}
/// The web searches among the tools a request declares. The provider runs
/// them, so the request needs a provider that has them.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum SearchTools {
    #[default]
    Absent,
    /// Declared beside other tools.
    Among,
    /// The only tools the request declares.
    Alone,
}
impl SearchTools {
    fn of(tools: Option<&serde_json::Value>) -> Self {
        let Some(tools) = tools.and_then(serde_json::Value::as_array) else {
            return Self::Absent;
        };
        // Anthropic dates the tool (`web_search_20250305`); OpenAI and xAI
        // call it `web_search`, some versions with a suffix.
        let searches = tools
            .iter()
            .filter(|tool| {
                tool.get("type")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|kind| kind == "web_search" || kind.starts_with("web_search_"))
            })
            .count();
        match searches {
            0 => Self::Absent,
            n if n == tools.len() => Self::Alone,
            _ => Self::Among,
        }
    }
}
#[derive(serde::Deserialize, Default)]
pub struct RequestHints {
    pub previous_response_id: Option<String>,
    pub model: Option<String>,
    pub speed: Option<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(skip)]
    pub compaction_trigger: bool,
    #[serde(skip)]
    pub compacted_window: Option<String>,
    #[serde(skip)]
    pub first_turn: bool,
    #[serde(skip)]
    pub turn: TurnEvidence,
    #[serde(skip)]
    pub capacity_replay_safe: bool,
    #[serde(skip)]
    pub auxiliary: bool,
    #[serde(skip)]
    pub search_tools: SearchTools,
    #[serde(skip)]
    pub routing_evidence: crate::usage::model::RoutingEvidence,
}
impl RequestHints {
    pub fn from_value(value: serde_json::Value, projected: bool) -> Result<Self, BoxError> {
        if !value.is_object() {
            return Err(io::Error::other("invalid request metadata").into());
        }
        let mut hints = Self {
            speed: crate::usage::model::speed(&value["speed"]),
            previous_response_id: value
                .get("previous_response_id")
                .filter(|v| !v.is_null())
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| io::Error::other("invalid response cursor"))
                })
                .transpose()?,
            model: value
                .get("model")
                .filter(|v| !v.is_null())
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| io::Error::other("invalid model"))
                })
                .transpose()?,
            stream: value
                .get("stream")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            auxiliary: value.get("generate").and_then(serde_json::Value::as_bool) == Some(false),
            search_tools: SearchTools::of(value.get("tools")),
            ..Default::default()
        };
        if let Some(input) = value.get("input").and_then(serde_json::Value::as_array) {
            hints.compaction_trigger = input.iter().any(|v| {
                v.get("type").and_then(serde_json::Value::as_str) == Some("compaction_trigger")
            });
            hints.compacted_window = input
                .iter()
                .rev()
                .find_map(|v| super::compaction::fingerprint(v, projected));
        }
        hints.auxiliary |= value.get("input").is_some_and(|input| match input {
            serde_json::Value::String(s) => s.is_empty(),
            serde_json::Value::Array(a) => a.is_empty(),
            _ => false,
        });
        hints.first_turn = !hints.auxiliary
            && hints.previous_response_id.is_none()
            && !hints.compaction_trigger
            && hints.compacted_window.is_none()
            && first_turn_input(value.get("input"));
        hints.turn = if hints.first_turn {
            TurnEvidence::First
        } else if hints.previous_response_id.is_some()
            || hints.compacted_window.is_some()
            || value
                .get("input")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
                            || item
                                .get("type")
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|kind| {
                                    matches!(
                                        kind,
                                        "function_call"
                                            | "function_call_output"
                                            | "reasoning"
                                            | "compaction"
                                            | "context_compaction"
                                            | "tool_result"
                                    )
                                })
                    })
                })
        {
            TurnEvidence::History
        } else {
            TurnEvidence::Unknown
        };
        // A Messages request carries its whole conversation and no `input`:
        // one nobody has answered yet is a first turn.
        if value.get("input").is_none() {
            if let Some(messages) = value.get("messages").and_then(serde_json::Value::as_array) {
                hints.turn = messages_turn(messages);
                hints.first_turn = hints.turn == TurnEvidence::First;
            }
        }
        // History evidence alone can coexist with unknown or provider-local
        // references. Capacity spillover requires every input item to be known.
        hints.capacity_replay_safe = hints.turn == TurnEvidence::History
            && hints.previous_response_id.is_none()
            && !hints.compaction_trigger
            && !hints.auxiliary
            && ["conversation", "conversation_id"]
                .iter()
                .all(|key| value.get(key).is_none_or(serde_json::Value::is_null))
            && value
                .get("input")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    !items.is_empty()
                        && items.iter().all(|item| {
                            let kind = match item.get("type") {
                                None => "message",
                                Some(serde_json::Value::String(kind)) => kind,
                                _ => return false,
                            };
                            match kind {
                                "message" | "input_message" => matches!(
                                    item.get("role").and_then(serde_json::Value::as_str),
                                    Some("user" | "assistant" | "system" | "developer")
                                ),
                                "additional_tools" => {
                                    item.get("role").and_then(serde_json::Value::as_str)
                                        == Some("developer")
                                }
                                "function_call"
                                | "function_call_output"
                                | "reasoning"
                                | "compaction"
                                | "context_compaction"
                                | "tool_result" => true,
                                _ => false,
                            }
                        })
                });
        use crate::usage::model::{RoutingEvidence, RoutingInput};
        hints.routing_evidence = RoutingEvidence {
            input: if hints.auxiliary {
                RoutingInput::Warmup
            } else if hints.previous_response_id.is_some() {
                RoutingInput::ResponseCursor
            } else if hints.compaction_trigger || hints.compacted_window.is_some() {
                RoutingInput::Compaction
            } else {
                match hints.turn {
                    TurnEvidence::First => RoutingInput::FirstTurn,
                    TurnEvidence::History => RoutingInput::History,
                    TurnEvidence::Unknown if value.get("input").is_none() => {
                        RoutingInput::MissingInput
                    }
                    TurnEvidence::Unknown => RoutingInput::UnknownStructure,
                }
            },
            has_tool_declarations: value
                .get("input")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("type").and_then(serde_json::Value::as_str)
                            == Some("additional_tools")
                            && item.get("role").and_then(serde_json::Value::as_str)
                                == Some("developer")
                    })
                }),
            restriction: None,
            ..Default::default()
        };
        Ok(hints)
    }
}
/// What the roles of a Messages conversation tell: only the user and the
/// system have spoken in a first turn, and an assistant's message is history.
/// Anything else proves neither.
fn messages_turn(messages: &[serde_json::Value]) -> TurnEvidence {
    let role = |message: &serde_json::Value| {
        message
            .get("role")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_owned()
    };
    let roles: Vec<_> = messages.iter().map(role).collect();
    if roles.iter().any(|role| role == "assistant") {
        TurnEvidence::History
    } else if roles.iter().any(|role| role == "user")
        && roles.iter().all(|role| role == "user" || role == "system")
    {
        TurnEvidence::First
    } else {
        TurnEvidence::Unknown
    }
}
// Structural evidence only. Unknown/truncated arrays never prove a new conversation.
fn first_turn_input(input: Option<&serde_json::Value>) -> bool {
    use serde_json::Value;
    match input {
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(items)) if !items.is_empty() => {
            let mut users = 0;
            for item in items {
                let kind = match item.get("type") {
                    None => "message",
                    Some(Value::String(kind)) => kind,
                    _ => return false,
                };
                // The current Codex client sends tool declarations inside input.
                // They are initialization metadata, and cannot supply user/history evidence.
                if kind == "additional_tools" {
                    if item.get("role").and_then(Value::as_str) != Some("developer") {
                        return false;
                    }
                    continue;
                }
                if !matches!(kind, "message" | "input_message") {
                    return false;
                }
                match item.get("role").and_then(Value::as_str) {
                    Some("user") => users += 1,
                    Some("system" | "developer") => (),
                    _ => return false,
                }
            }
            users > 0
        }
        _ => false,
    }
}
pub struct Replay {
    payload: Payload,
    trailers: Option<HeaderMap>,
    pub length: u64,
}
impl Replay {
    pub async fn capture_progress<B>(
        mut body: B,
        dir: &Path,
        idle: Option<std::time::Duration>,
    ) -> Result<Self, BoxError>
    where
        B: Body<Data = Bytes> + Unpin,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        Self::capture_observed(&mut body, dir, idle, |_| {}).await
    }
    pub async fn capture_observed<B>(
        mut body: B,
        dir: &Path,
        idle: Option<std::time::Duration>,
        mut observed: impl FnMut(&[u8]),
    ) -> Result<Self, BoxError>
    where
        B: Body<Data = Bytes> + Unpin,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        Self::capture_until(&mut body, dir, idle, |bytes| {
            observed(bytes);
            false
        })
        .await
    }
    /// Bounded memory staging until an inspected protocol boundary. The caller
    /// retains the unread upstream body; temporary files are removed on drop.
    pub async fn capture_until<B>(
        body: &mut B,
        dir: &Path,
        idle: Option<std::time::Duration>,
        mut observed: impl FnMut(&[u8]) -> bool,
    ) -> Result<Self, BoxError>
    where
        B: Body<Data = Bytes> + Unpin,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        let mut memory = BytesMut::new();
        let mut disk = None;
        let mut writer = None;
        let mut length = 0;
        let mut trailers = None;
        loop {
            let frame = if let Some(idle) = idle {
                tokio::time::timeout(idle, body.frame())
                    .await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "upstream body idle"))?
            } else {
                body.frame().await
            };
            let Some(frame) = frame else {
                break;
            };
            let frame = frame?;
            match frame.into_data() {
                Ok(chunk) => {
                    let stop = observed(&chunk);
                    length += chunk.len() as u64;
                    if length > MAX_BODY {
                        return Err(io::Error::other("请求超过 1 GiB").into());
                    }
                    if writer.is_none() && memory.len() + chunk.len() > MEMORY_LIMIT {
                        let f = storage::private_tempfile(dir)?;
                        let mut file = tokio::fs::File::from_std(f.reopen()?);
                        file.write_all(&memory).await?;
                        memory = BytesMut::new();
                        writer = Some(file);
                        disk = Some(Arc::new(f));
                    }
                    if let Some(f) = writer.as_mut() {
                        f.write_all(&chunk).await?;
                    } else {
                        memory.extend_from_slice(&chunk);
                    }
                    if stop {
                        break;
                    }
                }
                Err(frame) => {
                    if let Ok(t) = frame.into_trailers() {
                        trailers = Some(t);
                    }
                }
            }
        }
        if let Some(f) = writer.as_mut() {
            f.flush().await?;
        }
        Ok(Self {
            payload: disk
                .map(Payload::Disk)
                .unwrap_or_else(|| Payload::Memory(memory.freeze())),
            trailers,
            length,
        })
    }
    pub fn body(&self) -> WireBody {
        let trailers = self.trailers.clone();
        let body = match &self.payload {
            Payload::Memory(bytes) => {
                let bytes = bytes.clone();
                StreamBody::new(
                    async_stream::try_stream! {if !bytes.is_empty(){yield Frame::data(bytes);}
                    if let Some(t)=trailers{yield Frame::trailers(t);}},
                )
                .boxed_unsync()
            }
            Payload::Disk(file) => {
                let file = file.clone();
                StreamBody::new(async_stream::try_stream! {
                    let reader=tokio::fs::File::open(file.path()).await?;
                    let mut stream=tokio_util::io::ReaderStream::with_capacity(reader,64*1024);
                    while let Some(bytes)=stream.next().await{yield Frame::data(bytes?);}
                    if let Some(t)=trailers{yield Frame::trailers(t);}
                    drop(file);
                })
                .boxed_unsync()
            }
        };
        body
    }
    pub async fn inspect(
        &self,
        encoding: &str,
        content_type: &str,
    ) -> Result<RequestHints, BoxError> {
        let reader: Box<dyn std::io::Read + Send> = match &self.payload {
            Payload::Memory(bytes) => Box::new(std::io::Cursor::new(bytes.clone())),
            Payload::Disk(file) => Box::new(file.reopen()?),
        };
        let encoding = encoding.to_owned();
        let content_type = content_type.to_owned();
        tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let decoded: Box<dyn Read + Send> = match encoding.trim().to_ascii_lowercase().as_str()
            {
                "" | "identity" => reader,
                "gzip" => Box::new(flate2::read::GzDecoder::new(reader)),
                "deflate" => Box::new(flate2::read::ZlibDecoder::new(reader)),
                "zstd" => Box::new(zstd::stream::read::Decoder::new(reader)?),
                "br" => Box::new(brotli::Decompressor::new(reader, 4096)),
                _ => return Err(io::Error::other("unsupported inspection encoding").into()),
            };
            // serde ignores unknown fields while streaming, including large input arrays.
            let decoded = decoded.take(MAX_BODY + 1);
            let mime = content_type.parse::<mime::Mime>().ok();
            let hints: RequestHints = if mime
                .as_ref()
                .is_some_and(|m| m.type_() == mime::MULTIPART && m.subtype() == mime::FORM_DATA)
            {
                let boundary = mime
                    .as_ref()
                    .and_then(|m| m.get_param(mime::BOUNDARY))
                    .ok_or_else(|| io::Error::other("missing boundary"))?
                    .as_str();
                if boundary.len() > 200 {
                    return Err(io::Error::other("invalid boundary").into());
                }
                tokio::runtime::Handle::current().block_on(async move {
                    let stream =
                        futures_util::stream::try_unfold(decoded, |mut reader| async move {
                            let mut chunk = vec![0; 64 * 1024];
                            let count = reader.read(&mut chunk)?;
                            if count == 0 {
                                return Ok::<_, io::Error>(None);
                            }
                            chunk.truncate(count);
                            Ok(Some((Bytes::from(chunk), reader)))
                        });
                    let mut parts = multer::Multipart::new(stream, boundary);
                    let mut hints = RequestHints::default();
                    while let Some(mut part) = parts.next_field().await? {
                        let name = part.name().unwrap_or("").to_owned();
                        if ["model", "previous_response_id", "stream"].contains(&name.as_str()) {
                            let mut bytes = Vec::new();
                            while let Some(chunk) = part.chunk().await? {
                                if bytes.len() + chunk.len() > 1024 {
                                    return Err::<_, BoxError>(
                                        io::Error::other("oversized field").into(),
                                    );
                                }
                                bytes.extend_from_slice(&chunk);
                            }
                            let value = String::from_utf8(bytes)?;
                            match name.as_str() {
                                "model" if hints.model.is_none() => hints.model = Some(value),
                                "previous_response_id" if hints.previous_response_id.is_none() => {
                                    hints.previous_response_id = Some(value)
                                }
                                "stream" => hints.stream = value == "true",
                                _ => return Err(io::Error::other("ambiguous field").into()),
                            }
                        }
                    }
                    Ok(hints)
                })?
            } else {
                let mut decoded = decoded;
                let mut projection = super::metadata::Projector::default();
                let mut buffer = [0; 64 * 1024];
                loop {
                    let count = decoded.read(&mut buffer)?;
                    if count == 0 {
                        break;
                    }
                    projection.feed(&buffer[..count]);
                }
                RequestHints::from_value(
                    projection
                        .finish()
                        .ok_or_else(|| io::Error::other("invalid request metadata"))?,
                    true,
                )?
            };
            if hints
                .model
                .as_ref()
                .is_some_and(|m| m.is_empty() || m.len() > 256 || m.chars().any(char::is_control))
            {
                return Err(io::Error::other("invalid model").into());
            }
            if hints
                .previous_response_id
                .as_ref()
                .is_some_and(|id| id.len() > 1024)
            {
                return Err(io::Error::other("invalid response cursor").into());
            }
            Ok(hints)
        })
        .await
        .map_err(|_| io::Error::other("request inspection interrupted"))?
    }
    /// The request sent as another model, at another reasoning effort, or
    /// both. `effort` names the object of the request that holds the effort
    /// and the level to set in it; the rest of that object is kept. For
    /// Claude on Vertex AI, `vertex` is the provider's address and the
    /// request's path: the body is made Vertex AI's, and where it goes comes
    /// back with it.
    pub async fn rewritten(
        &self,
        encoding: &str,
        model: Option<&str>,
        effort: Option<(&'static str, &str)>,
        vertex: Option<(&str, &str)>,
    ) -> Result<(Bytes, Option<String>), BoxError> {
        let bytes = self.prefix(MAX_BODY as usize).await?;
        let encoding = encoding.to_owned();
        let model = model.map(str::to_owned);
        let effort = effort.map(|(holder, level)| (holder, level.to_owned()));
        let vertex = vertex.map(|(base, path)| (base.to_owned(), path.to_owned()));
        tokio::task::spawn_blocking(move || {
            let decoded = decode_prefix(&bytes, &encoding, MAX_BODY as usize + 1)?;
            if decoded.len() as u64 > MAX_BODY {
                return Err(io::Error::other("request too large").into());
            }
            let mut value: serde_json::Value = serde_json::from_slice(&decoded)?;
            let object = value
                .as_object_mut()
                .ok_or_else(|| io::Error::other("invalid request"))?;
            if let Some(model) = model {
                object.insert("model".into(), model.into());
            }
            if let Some((holder, level)) = effort {
                match object
                    .get_mut(holder)
                    .and_then(serde_json::Value::as_object_mut)
                {
                    Some(held) => {
                        held.insert("effort".into(), level.into());
                    }
                    // Absent, or the null a client sends for no reasoning.
                    None => {
                        object.insert(holder.into(), serde_json::json!({ "effort": level }));
                    }
                }
            }
            let address = match vertex {
                Some((base, path)) => Some(
                    super::vertex::request(&base, &path, object)
                        .ok_or_else(|| io::Error::other("no vertex address"))?,
                ),
                None => None,
            };
            Ok((Bytes::from(serde_json::to_vec(&value)?), address))
        })
        .await
        .map_err(|_| io::Error::other("request rewriting interrupted"))?
    }
    /// The request without one top-level member, or `None` to send it as it
    /// came: when it has no such member, or is not a JSON object at all.
    pub async fn without(&self, encoding: &str, member: &'static str) -> Option<Bytes> {
        let bytes = self.prefix(MAX_BODY as usize).await.ok()?;
        let encoding = encoding.to_owned();
        tokio::task::spawn_blocking(move || {
            let decoded = decode_prefix(&bytes, &encoding, MAX_BODY as usize + 1).ok()?;
            let needle = format!("\"{member}\"");
            if decoded.len() as u64 > MAX_BODY
                || !decoded
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes())
            {
                return None;
            }
            let mut value: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
            value.as_object_mut()?.remove(member)?;
            serde_json::to_vec(&value).ok().map(Bytes::from)
        })
        .await
        .ok()
        .flatten()
    }
    pub fn has_trailers(&self) -> bool {
        self.trailers.is_some()
    }
    pub async fn prefix(&self, limit: usize) -> Result<Vec<u8>, BoxError> {
        match &self.payload {
            Payload::Memory(bytes) => Ok(bytes[..bytes.len().min(limit)].to_vec()),
            Payload::Disk(file) => {
                let reader = tokio::fs::File::open(file.path()).await?;
                let mut bytes = Vec::new();
                reader.take(limit as u64).read_to_end(&mut bytes).await?;
                Ok(bytes)
            }
        }
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn a_messages_conversation_is_told_from_its_first_turn_by_who_has_spoken() {
        let block = json!([{"type":"text","text":"fixture"},{"type":"tool_result","content":"x"}]);
        let said = |role: &str| json!({"role": role, "content": block});
        let many: Vec<_> = (0..300)
            .map(|turn| said(if turn % 2 == 0 { "user" } else { "assistant" }))
            .collect();
        for (messages, turn) in [
            (json!([said("user"), said("system")]), TurnEvidence::First),
            (json!(vec![said("user"); 200]), TurnEvidence::First),
            (
                json!([said("user"), said("assistant"), said("user")]),
                TurnEvidence::History,
            ),
            (json!(many), TurnEvidence::History),
            (json!([said("system")]), TurnEvidence::Unknown),
            (json!([said("user"), said("tool")]), TurnEvidence::Unknown),
            (
                json!([said("user"), {"content": "fixture"}]),
                TurnEvidence::Unknown,
            ),
            (json!([]), TurnEvidence::Unknown),
        ] {
            let value = json!({"model": "claude-fixture", "messages": messages, "stream": true});
            for projected in [false, true] {
                let value = if projected {
                    let mut projector = super::super::metadata::Projector::default();
                    projector.feed(value.to_string().as_bytes());
                    let kept = projector.finish().unwrap();
                    // However long the conversation, a role is kept once.
                    let roles = kept["messages"].as_array().unwrap();
                    assert!(roles.len() <= 3);
                    // Nothing a message said is kept, only who said it.
                    assert!(
                        roles.iter().all(|message| {
                            let message = message.as_object().unwrap();
                            message.len() == 1 && message["role"].is_string()
                        }),
                        "{kept}"
                    );
                    kept
                } else {
                    value.clone()
                };
                let hints = RequestHints::from_value(value, projected).unwrap();
                assert_eq!(hints.turn, turn, "{projected}");
                assert_eq!(hints.first_turn, turn == TurnEvidence::First);
                assert!(!hints.capacity_replay_safe && !hints.auxiliary);
                assert_eq!(hints.model.as_deref(), Some("claude-fixture"));
            }
        }
        // A request with an `input` is told by that, whatever else it carries.
        let both = json!({"input": [{"role": "user", "content": "x"}, {"role": "assistant", "content": "x"}],
            "messages": [{"role": "user", "content": "x"}]});
        let hints = RequestHints::from_value(both, false).unwrap();
        assert!(hints.turn == TurnEvidence::History && !hints.first_turn);
    }

    #[test]
    fn cross_provider_capacity_requires_known_history_and_no_server_local_references() {
        let base = json!({"input":[{"role":"assistant","content":"fixture"},{"role":"user","content":"fixture"}]});
        for projected in [false, true] {
            for (value, safe) in [
                (base.clone(), true),
                (
                    json!({"input":[{"role":"assistant","content":"fixture"},{"type":"item_reference","id":"fixture"}]}),
                    false,
                ),
                (
                    json!({"input":[{"role":"assistant","content":"fixture"},{"type":12,"role":"user"}]}),
                    false,
                ),
                (
                    json!({"input":[{"role":"assistant","content":"fixture"},{"type":"future"}]}),
                    false,
                ),
                (
                    json!({"input":base["input"],"conversation":"fixture"}),
                    false,
                ),
                (
                    json!({"input":base["input"],"previous_response_id":"fixture"}),
                    false,
                ),
                (json!({"input":"fixture"}), false),
            ] {
                let value = if projected {
                    let mut projector = super::super::metadata::Projector::default();
                    projector.feed(value.to_string().as_bytes());
                    projector.finish().unwrap()
                } else {
                    value
                };
                assert_eq!(
                    RequestHints::from_value(value, projected)
                        .unwrap()
                        .capacity_replay_safe,
                    safe
                );
            }
        }
    }
}
