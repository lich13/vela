//! Keep Claude's streamed request alive while admission, retries or buffering wait.
use super::{connector::BoxError, replay::WireBody, response_decoder::Decoder};
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

fn error_frame(error_type: &str, message: &str) -> Frame<Bytes> {
    let payload = serde_json::to_string(&json!({
        "type": "error",
        "error": {
            "type": error_type,
            "message": message,
        }
    }))
    .expect("Anthropic error frame serializes");
    Frame::data(Bytes::from(format!(
        "\n\nevent: error\ndata: {payload}\n\n"
    )))
}

fn fallback_failure(status: u16) -> Frame<Bytes> {
    error_frame(
        anthropic_error_type(status),
        &format!("Upstream request failed (HTTP {status})"),
    )
}

async fn response_error_frame(response: Response<WireBody>, fallback_status: u16) -> Frame<Bytes> {
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
        return fallback_failure(effective_status);
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
        let raw_type = err_obj
            .get("type")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let raw_message = err_obj
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| value.get("message").and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let mapped_type = match raw_type {
            Some("vela_gateway") | None => anthropic_error_type(effective_status),
            Some(other) => other,
        };
        if let Some(message) = raw_message {
            return error_frame(mapped_type, message);
        }
        if let Some(explicit_type) = raw_type.filter(|t| *t != "vela_gateway" && *t != "message") {
            return error_frame(
                explicit_type,
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
                return error_frame(anthropic_error_type(effective_status), &snippet);
            }
        }
    }
    fallback_failure(effective_status)
}

pub(super) async fn waiting(
    mut pending: std::pin::Pin<Box<impl Future<Output = Response<WireBody>> + Send + 'static>>,
    started: tokio::sync::oneshot::Receiver<()>,
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
                _ = ticks.tick() => yield Ok::<_, BoxError>(Frame::data(Bytes::from_static(b"event: ping\ndata: {\"type\":\"ping\"}\n\n"))),
            }
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            yield Ok(response_error_frame(response, status).await);
            return;
        }
        let is_sse = response.headers().get(header::CONTENT_TYPE).and_then(|h| h.to_str().ok())
            .is_some_and(|value| value.split(';').next().is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream")));
        // A streamed Messages request must get events; JSON cannot be spliced into an SSE body.
        if !is_sse {
            yield Ok(response_error_frame(response, 502).await);
            return;
        }
        let encoding = response.headers().get(header::CONTENT_ENCODING).and_then(|h| h.to_str().ok()).unwrap_or("");
        let (mut decoder, unsupported) = Decoder::new(Vec::new(), encoding);
        if unsupported { yield Ok(fallback_failure(502)); return; }
        let mut body = response.into_body();
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => if let Some(bytes) = frame.data_ref() {
                    if decoder.feed(bytes).is_err() { yield Ok(fallback_failure(502)); return; }
                    let bytes = std::mem::take(decoder.sink());
                    if !bytes.is_empty() { yield Ok(Frame::data(Bytes::from(bytes))); }
                },
                Err(_) => { yield Ok(fallback_failure(502)); return; }
            }
        }
        if decoder.finish().is_err() { yield Ok(fallback_failure(502)); return; }
        let bytes = std::mem::take(decoder.sink());
        if !bytes.is_empty() { yield Ok(Frame::data(Bytes::from(bytes))); }
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(StreamBody::new(Box::pin(body)).boxed_unsync())
        .unwrap()
}
