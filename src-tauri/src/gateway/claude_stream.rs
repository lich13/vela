//! Keep Claude's streamed request alive while admission, retries or buffering wait.
use super::{connector::BoxError, replay::WireBody, response_decoder::Decoder};
use bytes::Bytes;
use http_body_util::{BodyExt, StreamBody};
use hyper::{body::Frame, header, Response};
use std::{future::Future, time::Duration};

fn failure(status: u16) -> Frame<Bytes> {
    Frame::data(Bytes::from(format!(
        "\n\nevent: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"api_error\",\"message\":\"Upstream request failed (HTTP {status})\"}}}}\n\n"
    )))
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
            yield Ok(failure(response.status().as_u16()));
            return;
        }
        let is_sse = response.headers().get(header::CONTENT_TYPE).and_then(|h| h.to_str().ok())
            .is_some_and(|value| value.split(';').next().is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream")));
        // A streamed Messages request must get events; JSON cannot be spliced into an SSE body.
        if !is_sse { yield Ok(failure(502)); return; }
        let encoding = response.headers().get(header::CONTENT_ENCODING).and_then(|h| h.to_str().ok()).unwrap_or("");
        let (mut decoder, unsupported) = Decoder::new(Vec::new(), encoding);
        if unsupported { yield Ok(failure(502)); return; }
        let mut body = response.into_body();
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => if let Some(bytes) = frame.data_ref() {
                    if decoder.feed(bytes).is_err() { yield Ok(failure(502)); return; }
                    let bytes = std::mem::take(decoder.sink());
                    if !bytes.is_empty() { yield Ok(Frame::data(Bytes::from(bytes))); }
                },
                Err(_) => { yield Ok(failure(502)); return; }
            }
        }
        if decoder.finish().is_err() { yield Ok(failure(502)); return; }
        let bytes = std::mem::take(decoder.sink());
        if !bytes.is_empty() { yield Ok(Frame::data(Bytes::from(bytes))); }
    };
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(StreamBody::new(Box::pin(body)).boxed_unsync())
        .unwrap()
}
