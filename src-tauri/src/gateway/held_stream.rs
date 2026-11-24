//! Keeps a client's streamed request alive while admission, retries or
//! buffering wait, in the events of the protocol the client is speaking.
use super::{connector::BoxError, replay::WireBody, response_decoder::Decoder, ClientId};
use bytes::Bytes;
use http_body_util::{BodyExt, StreamBody};
use hyper::{body::Frame, header, Response};
use serde_json::{json, Value};
use std::{
    future::Future,
    io::{self, Write},
    time::Duration,
};

const ERROR_BODY_LIMIT: usize = 64 * 1024;
const RAW_ERROR_LIMIT: usize = 256 * 1024;

struct BoundedSink {
    buf: Vec<u8>,
}

impl Write for BoundedSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let remaining = ERROR_BODY_LIMIT.saturating_sub(self.buf.len());
        if remaining == 0 {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "limit"));
        }
        let take = data.len().min(remaining);
        self.buf.extend_from_slice(&data[..take]);
        if take < data.len() {
            Err(io::Error::new(io::ErrorKind::WriteZero, "limit"))
        } else {
            Ok(take)
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The generation call a request is, by its address. Only these have an end
/// of their own to wait for, so only these can be answered whole. Gemini's
/// stream of a JSON array is not among them: its end is the array's, which
/// says nothing of the answer in it, and it is sent on as it comes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Call {
    Messages,
    Responses,
    Chat,
    Gemini,
}

impl Call {
    pub(super) fn of(uri: &hyper::Uri) -> Option<Self> {
        let path = uri.path().trim_end_matches('/');
        if path == "/v1/messages" {
            return Some(Self::Messages);
        }
        if super::gemini::model(path).is_some() {
            let events = uri
                .query()
                .is_some_and(|query| query.split('&').any(|pair| pair == "alt=sse"));
            return (!super::gemini::counts(path) && (events || !super::gemini::streams(path)))
                .then_some(Self::Gemini);
        }
        match path.strip_prefix("/v1").unwrap_or(path) {
            "/responses" => Some(Self::Responses),
            "/chat/completions" => Some(Self::Chat),
            _ => None,
        }
    }
    /// What is sent while there is nothing to say. Codex counts only events
    /// against its idle limit, so it gets one its parser passes over; other
    /// clients of the OpenAI and Gemini protocols get a comment line, which
    /// every event-stream parser drops.
    fn keep_alive(self, client: ClientId) -> &'static [u8] {
        match self {
            Self::Messages => b"event: ping\ndata: {\"type\":\"ping\"}\n\n",
            Self::Responses if client == ClientId::Codex => {
                b"event: response.in_progress\ndata: {\"type\":\"response.in_progress\"}\n\n"
            }
            _ => b": keepalive\n\n",
        }
    }
    /// The failure that ends a stream already opened, as this protocol says
    /// it. `kind` is the upstream's own name for the error where it gave one.
    fn failure(self, status: u16, kind: Option<&str>, message: &str) -> Frame<Bytes> {
        let (event, payload) = match self {
            Self::Messages => (
                Some("error"),
                json!({
                    "type": "error",
                    "error": {"type": kind.unwrap_or(anthropic_error_type(status)), "message": message},
                }),
            ),
            Self::Responses => (
                Some("response.failed"),
                json!({
                    "type": "response.failed",
                    "response": {
                        "status": "failed",
                        "error": {"code": kind.unwrap_or(openai_error_code(status)), "message": message},
                    },
                }),
            ),
            Self::Chat => (
                None,
                json!({
                    "error": {
                        "message": message,
                        "type": kind.unwrap_or(openai_error_code(status)),
                        "code": kind.unwrap_or(openai_error_code(status)),
                    },
                }),
            ),
            Self::Gemini => (
                None,
                json!({
                    "error": {"code": status, "message": message, "status": google_status(status)},
                }),
            ),
        };
        let event = event
            .map(|name| format!("event: {name}\n"))
            .unwrap_or_default();
        Frame::data(Bytes::from(format!("\n\n{event}data: {payload}\n\n")))
    }
}

fn openai_error_code(status: u16) -> &'static str {
    match status {
        400 | 422 => "invalid_request_error",
        401 => "invalid_api_key",
        403 => "permission_denied",
        404 => "model_not_found",
        429 => "rate_limit_exceeded",
        _ => "server_error",
    }
}

fn google_status(status: u16) -> &'static str {
    match status {
        400 | 422 => "INVALID_ARGUMENT",
        401 => "UNAUTHENTICATED",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        429 => "RESOURCE_EXHAUSTED",
        504 => "DEADLINE_EXCEEDED",
        _ => "UNAVAILABLE",
    }
}

pub(super) fn anthropic_error_type(status: u16) -> &'static str {
    match status {
        400 | 422 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        529 => "overloaded_error",
        _ => "api_error",
    }
}

fn fallback_failure(call: Call, status: u16) -> Frame<Bytes> {
    call.failure(
        status,
        None,
        &format!("Upstream request failed (HTTP {status})"),
    )
}

async fn response_error_frame(
    call: Call,
    response: Response<WireBody>,
    fallback_status: u16,
) -> Frame<Bytes> {
    let status = response.status().as_u16();
    let effective_status = if status >= 400 {
        status
    } else {
        fallback_status
    };
    let encoding = response
        .headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let (mut decoder, unsupported) = Decoder::new(
        BoundedSink {
            buf: Vec::with_capacity(512),
        },
        encoding,
    );
    if unsupported {
        return fallback_failure(call, effective_status);
    }
    let mut body = response.into_body();
    let mut raw_read = 0usize;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            break;
        };
        if let Some(bytes) = frame.data_ref() {
            let take = bytes.len().min(RAW_ERROR_LIMIT.saturating_sub(raw_read));
            if take == 0 {
                break;
            }
            raw_read += take;
            if decoder.feed(&bytes[..take]).is_err() {
                break;
            }
        }
    }
    let _ = decoder.finish();
    let bytes = std::mem::take(&mut decoder.sink().buf);
    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
        let err_obj = value.get("error").unwrap_or(&value);
        let text = |key: &str| {
            err_obj
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        let raw_type = text("type");
        // The gateway's own errors are named for the client by their status.
        let kind = match call {
            _ if raw_type == Some("vela_gateway") => None,
            Call::Messages => raw_type,
            _ => text("code").or(raw_type),
        };
        let raw_message = text("message").or_else(|| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        });
        if let Some(message) = raw_message {
            return call.failure(effective_status, kind, message);
        }
        if let Some(kind) = kind.filter(|kind| *kind != "message") {
            return call.failure(
                effective_status,
                Some(kind),
                &format!("Upstream request failed (HTTP {effective_status})"),
            );
        }
    }
    if status >= 400 {
        if let Ok(text) = std::str::from_utf8(&bytes) {
            let trimmed = text.trim();
            let lower = trimmed.to_ascii_lowercase();
            if !trimmed.is_empty() && !lower.starts_with("<!doctype") && !lower.starts_with("<html")
            {
                let snippet: String = trimmed.chars().take(512).collect();
                return call.failure(effective_status, None, &snippet);
            }
        }
    }
    fallback_failure(call, effective_status)
}

pub(super) async fn waiting(
    mut pending: std::pin::Pin<Box<impl Future<Output = Response<WireBody>> + Send + 'static>>,
    started: tokio::sync::oneshot::Receiver<()>,
    call: Call,
    client: ClientId,
) -> Response<WireBody> {
    tokio::select! {
        response = &mut pending => return response,
        result = started => if result.is_err() { return pending.await; },
    }
    let body = async_stream::stream! {
        let mut ticks = tokio::time::interval(Duration::from_secs(5));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let response = loop {
            tokio::select! {
                response = &mut pending => break response,
                _ = ticks.tick() => yield Ok::<_, BoxError>(Frame::data(Bytes::from_static(call.keep_alive(client)))),
            }
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            yield Ok(response_error_frame(call, response, status).await);
            return;
        }
        let is_sse = response.headers().get(header::CONTENT_TYPE).and_then(|h| h.to_str().ok())
            .is_some_and(|value| value.split(';').next().is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream")));
        // A streamed request must get events; JSON cannot be spliced into an SSE body.
        if !is_sse {
            yield Ok(response_error_frame(call, response, 502).await);
            return;
        }
        let encoding = response.headers().get(header::CONTENT_ENCODING).and_then(|h| h.to_str().ok()).unwrap_or("");
        let (mut decoder, unsupported) = Decoder::new(Vec::new(), encoding);
        if unsupported { yield Ok(fallback_failure(call, 502)); return; }
        let mut body = response.into_body();
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => if let Some(bytes) = frame.data_ref() {
                    if decoder.feed(bytes).is_err() { yield Ok(fallback_failure(call, 502)); return; }
                    let bytes = std::mem::take(decoder.sink());
                    if !bytes.is_empty() { yield Ok(Frame::data(Bytes::from(bytes))); }
                },
                Err(_) => { yield Ok(fallback_failure(call, 502)); return; }
            }
        }
        if decoder.finish().is_err() { yield Ok(fallback_failure(call, 502)); return; }
        let bytes = std::mem::take(decoder.sink());
        if !bytes.is_empty() { yield Ok(Frame::data(Bytes::from(bytes))); }
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(StreamBody::new(Box::pin(body)).boxed_unsync())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::Call;

    #[test]
    fn only_generation_calls_with_an_end_of_their_own_are_answered_whole() {
        for (uri, call) in [
            ("/v1/messages", Some(Call::Messages)),
            ("/v1/messages?beta=true", Some(Call::Messages)),
            ("/v1/messages/count_tokens", None),
            ("/v1/responses", Some(Call::Responses)),
            ("/responses", Some(Call::Responses)),
            ("/v1/responses/compact", None),
            ("/v1/chat/completions", Some(Call::Chat)),
            ("/v1/models", None),
            (
                "/v1beta/models/gemini-fixture:generateContent",
                Some(Call::Gemini),
            ),
            (
                "/v1beta/models/gemini-fixture:streamGenerateContent?key=fixture&alt=sse",
                Some(Call::Gemini),
            ),
            // A JSON array has no event to wait for.
            ("/v1beta/models/gemini-fixture:streamGenerateContent", None),
            ("/v1beta/models/gemini-fixture:countTokens", None),
        ] {
            assert_eq!(Call::of(&uri.parse().unwrap()), call, "{uri}");
        }
    }
}
