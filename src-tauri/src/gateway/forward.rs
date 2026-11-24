use super::{
    admission::{Admission, Budget, CapacitySource, Rejected},
    attempt::{DeliveryGuard, DownstreamEnd},
    circuit,
    connector::{self, BoxError},
    health_settlement::observe_protocol,
    model::{Delegated, RetryClass, Settings},
    replay::{self, Replay, WireBody},
    routing::{self, Requirement},
    wait_budget::{Quiet, Silence},
    Active, Gateway, Route,
};
use crate::errors::code;
use crate::events::{Action, Reason};
use bytes::Bytes;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Body as _;
use hyper::{body::Incoming, header, HeaderMap, Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use std::{
    convert::Infallible,
    time::{Duration, Instant},
};
use tokio::sync::watch;
#[cfg(test)]
#[path = "error_frame_delivery_tests.rs"]
mod error_frame_delivery_tests;
#[derive(Clone, Copy)]
struct ReceivedAt(i64);
pub async fn serve(
    registry: std::sync::Arc<super::registry::Registry>,
    listener: tokio::net::TcpListener,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _=shutdown.changed()=>break,
            result=listener.accept()=>{
                let Ok((socket,_))=result else{break};let registry=registry.clone();let mut stop=shutdown.clone();
                tokio::spawn(async move{
                    let activity = std::sync::Arc::new(super::wire_progress::Activity::default());
                    let (socket, socket_error) = super::wire_progress::observed_tcp(socket, activity.clone());
                    let socket = super::wire_progress::Tracked::new(socket, activity.clone());
                    let connection_activity = activity.clone();
                    let service=hyper::service::service_fn(move|mut request: Request<Incoming>|{
                        let routed = super::listener::route(request.uri(), request.headers(), &registry).and_then(|(client, uri)| {
                            *request.uri_mut() = uri;
                            registry.gateway(client).ok_or(super::listener::RouteError::NotFound)
                        });
                        let delivery = std::sync::Arc::new(super::protocol::Delivery::default());
                        delivery.track(activity.clone());
                        request.extensions_mut().insert(delivery);
                        request.extensions_mut().insert(ReceivedAt(crate::usage::model::now()));
                        request.extensions_mut().insert(activity.clone());
                        async move{
                            Ok::<_,Infallible>(match routed {
                            Ok(g) => {
                                let mut client_stop = g.request_state().shutdown.as_ref().map(watch::Sender::subscribe);
                                tokio::select! {
                                    biased;
                                    _ = stopping(&mut client_stop) => error(StatusCode::SERVICE_UNAVAILABLE, code::GATEWAY_STOPPED, "客户端网关已停止"),
                                    response = forward(g,request) => response,
                                }
                            },
                            Err(super::listener::RouteError::NotFound) => error(StatusCode::NOT_FOUND, code::CLIENT, "客户端网关路径不存在"),
                            Err(super::listener::RouteError::Unauthorized) => error(StatusCode::UNAUTHORIZED, code::LOCAL_AUTH, "客户端网关凭据不匹配"),
                        })}
                    });
                    let connection=hyper::server::conn::http1::Builder::new().preserve_header_case(true).half_close(true)
                        .serve_connection(TokioIo::new(socket),service).with_upgrades();
                    tokio::pin!(connection);
                    let result = tokio::select!{
                        result= &mut connection=>result,
                        error=socket_error=>{connection_activity.failed(&error, false);return;},
                        _=stop.changed()=>{connection_activity.event("gateway_stopping");connection.as_mut().graceful_shutdown();connection.await}
                    };
                    if let Err(error) = result {
                        connection_activity.event(if error.is_parse() { "http_parse_failed" } else if error.is_canceled() { "http_cancelled" } else { "http_connection_failed" });
                        connection_activity.connection_failed(if connection_activity.error_code() != 0 { connection_activity.error_code() } else if error.is_canceled() { super::protocol::DownstreamError::Cancelled.into() } else if error.is_closed() || error.is_incomplete_message() { super::protocol::DownstreamError::Closed.into() } else if error.is_parse() { super::protocol::DownstreamError::ProtocolError.into() } else { super::protocol::DownstreamError::ConnectionError.into() });
                    }
                });
            }
        }
    }
}
type ForwardFrame = (
    Result<hyper::body::Frame<Bytes>, BoxError>,
    bool,
    bool,
    tokio::sync::oneshot::Sender<()>,
);
async fn usage_tail(
    body: &mut WireBody,
    protocol: &mut super::protocol::Protocol,
    shutdown: &mut Option<watch::Receiver<bool>>,
) {
    protocol.begin_usage_tail(false);
    let deadline = tokio::time::Instant::now() + super::model::USAGE_TAIL_DEADLINE;
    let end = loop {
        tokio::select! {
            biased;
            _ = stopping(shutdown) => break "gateway_stopping",
            _ = tokio::time::sleep_until(deadline) => break "usage_tail_deadline",
            next = body.frame() => match next {
                Some(Ok(frame)) => if let Some(data) = frame.data_ref() { protocol.feed(data); },
                None => break "upstream_eof",
                Some(Err(_)) => break "upstream_read_failed",
            }
        }
    };
    protocol.finish_usage_tail(end);
}
async fn finish_delivery(
    delivery: &super::protocol::Delivery,
    protocol: &mut super::protocol::Protocol,
    activity: &super::wire_progress::Activity,
    shutdown: &mut Option<watch::Receiver<bool>>,
    status: u16,
    seconds: u64,
) {
    tokio::select! {
        biased;
        _ = stopping(shutdown) => { protocol.finish(Some(status), code::GATEWAY_STOPPED); }
        result = super::wire_progress::writing(delivery.wait(), activity, seconds) => {
            if result.is_err() {
                DownstreamEnd::Timeout(seconds).record(delivery, protocol);
            }
            if delivery.error.load(std::sync::atomic::Ordering::Acquire) != 0 {
                protocol.finish(Some(status), code::CLIENT_DISCONNECTED);
            }
        }
    }
}
async fn send_frame(
    sender: &tokio::sync::mpsc::Sender<ForwardFrame>,
    frame: Result<hyper::body::Frame<Bytes>, BoxError>,
    terminal: bool,
    complete: bool,
    activity: &super::wire_progress::Activity,
    seconds: u64,
) -> Result<(), DownstreamEnd> {
    super::wire_progress::writing(
        async {
            let (ack, received) = tokio::sync::oneshot::channel();
            sender
                .send((frame, terminal, complete, ack))
                .await
                .map_err(|_| DownstreamEnd::Closed)?;
            received.await.map_err(|_| DownstreamEnd::Closed)
        },
        activity,
        seconds,
    )
    .await
    .map_err(|_| DownstreamEnd::Timeout(seconds))?
}

/// An error frame has the same delivery deadline as data. Its failed handoff
/// must settle delivery now, not start a second wait after discarding the error.
async fn send_error_frame(
    sender: &tokio::sync::mpsc::Sender<ForwardFrame>,
    protocol: &mut super::protocol::Protocol,
    activity: &super::wire_progress::Activity,
    shutdown: &mut Option<watch::Receiver<bool>>,
    status: u16,
    seconds: u64,
) -> bool {
    let delivery = protocol.delivery();
    tokio::select! {
        biased;
        _ = stopping(shutdown) => {
            delivery.event("gateway_stopping");
            delivery.settle();
            protocol.finish(Some(status), code::GATEWAY_STOPPED);
            false
        }
        result = send_frame(sender, Err(std::io::Error::other("上游流中断").into()), false, false, activity, seconds) => {
            if let Err(reason) = result {
                reason.record(&delivery, protocol);
                true
            } else {
                false
            }
        }
    }
}
pub(super) fn error(status: StatusCode, error_code: &str, message: &str) -> Response<WireBody> {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json");
    if matches!(
        error_code,
        code::CANCELLED | code::RETRY_BUDGET_EXHAUSTED | code::STOPPED
    ) {
        builder = builder.header("x-should-retry", "false");
    }
    builder
        .body(replay::full(
            serde_json::to_vec(
                &serde_json::json!({"error":{"type":"vela_gateway","code":error_code,"message":message}}),
            )
            .unwrap(),
        ))
        .unwrap()
}
fn is_messages_path(path: &str) -> bool {
    path.trim_end_matches('/') == "/v1/messages"
}
fn is_count_tokens_path(path: &str) -> bool {
    path.trim_end_matches('/') == "/v1/messages/count_tokens"
}
fn count_tokens_not_found() -> Response<WireBody> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "application/json")
        .body(replay::full(Bytes::from_static(
            br#"{"type":"error","error":{"type":"not_found_error","message":"Token counting is not supported by this provider"}}"#,
        )))
        .unwrap()
}
fn claude_rejection_response(
    status: Option<u16>,
    kind: &'static str,
    details: &crate::events::Details,
    model_unavailable: bool,
) -> Response<WireBody> {
    use crate::events::ErrorBasis;
    let upstream_type = details
        .upstream_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mapped_status = if let Some(s) = status.filter(|s| *s >= 400) {
        StatusCode::from_u16(s).unwrap_or(StatusCode::BAD_GATEWAY)
    } else if matches!(kind, code::FIRST_BYTE_TIMEOUT | code::STREAM_TIMEOUT) {
        StatusCode::GATEWAY_TIMEOUT
    } else {
        let lower = upstream_type.map(str::to_ascii_lowercase);
        match (details.error_basis, lower.as_deref(), model_unavailable) {
            (_, _, true)
            | (Some(ErrorBasis::ModelUnavailable), _, _)
            | (_, Some("not_found_error" | "not_found"), _) => StatusCode::NOT_FOUND,
            (
                Some(
                    ErrorBasis::InvalidRequest
                    | ErrorBasis::RequestTooLarge
                    | ErrorBasis::SafetyRejection,
                ),
                _,
                _,
            )
            | (_, Some("invalid_request_error" | "invalid_request" | "request_too_large"), _) => {
                StatusCode::BAD_REQUEST
            }
            (Some(ErrorBasis::Authentication), _, _) | (_, Some("authentication_error"), _) => {
                StatusCode::UNAUTHORIZED
            }
            (Some(ErrorBasis::Permission), _, _) | (_, Some("permission_error"), _) => {
                StatusCode::FORBIDDEN
            }
            (_, Some("rate_limit_error"), _) => StatusCode::TOO_MANY_REQUESTS,
            _ => StatusCode::BAD_GATEWAY,
        }
    };
    // "error" is the envelope's own type, not a kind Claude Code understands.
    let err_type = upstream_type
        .filter(|kind| !kind.eq_ignore_ascii_case("error"))
        .unwrap_or_else(|| {
            if details.error_basis == Some(ErrorBasis::Capacity) {
                "overloaded_error"
            } else {
                super::claude_stream::anthropic_error_type(mapped_status.as_u16())
            }
        });
    let err_msg = details
        .message
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(match kind {
            code::FIRST_BYTE_TIMEOUT => "上游首字节超时",
            code::STREAM_TIMEOUT => "上游流式响应超时",
            code::STREAM_INTERRUPTED => "上游响应提前中断",
            code::NETWORK => "连接上游供应商失败",
            _ => "上游拒绝了请求",
        });
    Response::builder()
        .status(mapped_status)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-should-retry", "false")
        .body(replay::full(
            serde_json::to_vec(&serde_json::json!({
                "type": "error",
                "error": {
                    "type": err_type,
                    "message": err_msg,
                }
            }))
            .unwrap(),
        ))
        .unwrap()
}
pub(super) fn target(base: &str, incoming: &Uri) -> Result<Uri, BoxError> {
    let mut base = url::Url::parse(base)?;
    super::model::normalize_api_path(&mut base);
    let path = incoming.path();
    let suffix = path
        .strip_prefix("/v1/")
        .map(|s| format!("/{s}"))
        .unwrap_or_else(|| {
            if path == "/v1" {
                String::new()
            } else {
                path.into()
            }
        });
    format!(
        "{}{}{}",
        base.as_str().trim_end_matches('/'),
        suffix,
        incoming
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default()
    )
    .parse()
    .map_err(Into::into)
}
pub(super) fn clean_headers(headers: &mut HeaderMap, upgrade: bool) {
    let nominated: Vec<_> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(',').map(str::trim).map(str::to_owned))
        .collect();
    for name in nominated {
        if !(upgrade && name.eq_ignore_ascii_case("upgrade")) {
            headers.remove(&name);
        }
    }
    for name in [
        "connection",
        "proxy-connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
    ] {
        headers.remove(name);
    }
    if upgrade {
        headers.insert(
            header::CONNECTION,
            header::HeaderValue::from_static("upgrade"),
        );
    } else {
        headers.remove(header::UPGRADE);
    }
}
pub(super) fn capacity_delay(settings: &Settings, retry: Option<Duration>) -> Duration {
    retry.unwrap_or(Duration::from_secs(settings.capacity_retry_seconds))
}
/// Everything the relay task owns once the response head has been accepted.
struct Relay {
    active: Active,
    traffic: super::freeze::Ticket,
    slot: super::admission::Slot,
    protocol: super::protocol::Protocol,
    g: Gateway,
    route: Route,
    model: Option<String>,
    permits: super::health_settlement::Permits,
    cfg: Settings,
    status: StatusCode,
    attempted: usize,
    /// Frames read while inspecting the start of the response.
    prefix: Vec<(hyper::body::Frame<Bytes>, bool, bool)>,
    request_budget: super::wait_budget::WaitBudget,
    sender: tokio::sync::mpsc::Sender<ForwardFrame>,
    activity: std::sync::Arc<super::wire_progress::Activity>,
    shutdown: Option<watch::Receiver<bool>>,
    ended: bool,
    failed: bool,
    delivery: std::sync::Arc<super::protocol::Delivery>,
    /// A success makes this the provider the client is left with when the
    /// gateway stops. Not so for a provider that only took the request
    /// over.
    lasting: bool,
    body: WireBody,
    inspected_bytes: u64,
    neutral: bool,
    /// The response may end at `message_stop` rather than the upstream's EOF.
    end_at_stop: bool,
    operation: crate::usage::model::Operation,
}
/// Whether a response may be ended at `message_stop`: only an uncompressed
/// event stream whose length was never promised can be cut short unnoticed.
fn ends_at_message_stop(route: &Route, status: StatusCode, headers: &HeaderMap) -> bool {
    route.provider.end_on_message_stop
        && status.as_u16() < 400
        && !headers.contains_key(header::CONTENT_LENGTH)
        && headers
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .and_then(|h| h.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
        && headers.get(header::CONTENT_ENCODING).is_none_or(|h| {
            let encoding = h.as_bytes().trim_ascii();
            encoding.is_empty() || encoding.eq_ignore_ascii_case(b"identity")
        })
}
/// Relays the inspected prefix and the rest of the upstream body to the
/// client, settling usage, health and delivery when the stream ends.
async fn relay(relay: Relay) {
    let Relay {
        active,
        traffic,
        slot,
        mut protocol,
        g,
        route,
        model,
        mut permits,
        cfg,
        status,
        attempted,
        prefix,
        request_budget,
        sender,
        activity,
        mut shutdown,
        ended,
        failed,
        delivery,
        lasting,
        mut body,
        mut inspected_bytes,
        neutral,
        end_at_stop,
        operation,
    } = relay;
    let mut active = Some(active);
    let traffic = traffic;
    // The concurrency slot is held until the response has been relayed.
    let _slot = slot;
    remember(&protocol, &g, &route, model.as_deref());
    observe_protocol(
        &protocol,
        &mut permits,
        &cfg,
        &g,
        &route,
        model.as_deref(),
        Some(status.as_u16()),
        attempted,
    );
    let mut disconnected = false;
    let prefix_len = prefix.len();
    for (index, (frame, terminal, business)) in prefix.into_iter().enumerate() {
        tokio::select! {
            biased;
            _ = request_budget.cancelled() => { protocol.finish(Some(status.as_u16()), code::CANCELLED); permits.neutral(&cfg); let _ = send_error_frame(&sender, &mut protocol, &activity, &mut shutdown, status.as_u16(), cfg.idle_seconds).await; return; }
            _ = stopping(&mut shutdown) => { protocol.finish(Some(status.as_u16()), code::GATEWAY_STOPPED); permits.neutral(&cfg); return; }
            result = send_frame(&sender, Ok(frame), terminal, ended && !failed && index + 1 == prefix_len, &activity, cfg.idle_seconds) => { if let Err(reason) = result { reason.record(&delivery, &mut protocol); disconnected = true; break; } }
        }
        protocol.output_handed(business);
    }
    if ended {
        if failed && !disconnected {
            // Preserve the received prefix, then expose the transport failure.
            // A clean EOF here would hide an incomplete generation.
            disconnected = send_error_frame(
                &sender,
                &mut protocol,
                &activity,
                &mut shutdown,
                status.as_u16(),
                cfg.idle_seconds,
            )
            .await;
        }
        drop(sender);
        if disconnected {
            protocol.finish(Some(status.as_u16()), code::CLIENT_DISCONNECTED);
        } else {
            finish_delivery(
                &delivery,
                &mut protocol,
                &activity,
                &mut shutdown,
                status.as_u16(),
                cfg.idle_seconds,
            )
            .await;
        }
        if protocol.succeeded() && lasting {
            g.persist_success(&route.provider).await;
        }
        return;
    }
    loop {
        if end_at_stop && !disconnected && inspected_bytes == 0 && protocol.message_stopped() {
            // The client holds the whole message: bytes inspected ahead of
            // the relay have all been sent. Some upstreams keep the
            // connection open long after it; stop reading and release it.
            protocol.finish_at_terminal(Some(status.as_u16()));
            break;
        }
        if disconnected || sender.is_closed() {
            disconnected = true;
            DownstreamEnd::Closed.record(&delivery, &mut protocol);
            active.take();
            permits.neutral(&cfg);
            usage_tail(&mut body, &mut protocol, &mut shutdown).await;
            break;
        }
        // Backpressure is awaited separately. Only time spent polling the
        // upstream measures its silence; completed events do not cut off tails.
        let limit = tokio::time::Instant::now() + Duration::from_secs(cfg.idle_seconds);
        // A compaction is given its own limit until the model writes, and none after.
        let silence = if operation != crate::usage::model::Operation::Compaction {
            if route.client_id == super::ClientId::Claude && protocol.output_started() {
                Silence::Flowing(Duration::from_secs(cfg.stall_seconds))
            } else {
                Silence::Started
            }
        } else if protocol.observation.meter.first_token_ms.is_some()
            || protocol.business_observed()
        {
            Silence::Compaction(None)
        } else {
            Silence::Compaction(Some(Duration::from_secs(cfg.compaction_seconds)))
        };
        let next = tokio::select! {
            biased;
            _ = request_budget.cancelled() => {
                protocol.finish(Some(status.as_u16()), code::CANCELLED);
                permits.neutral(&cfg);
                disconnected = send_error_frame(&sender, &mut protocol, &activity, &mut shutdown, status.as_u16(), cfg.idle_seconds).await;
                break;
            }
            _ = request_budget.wait() => {
                protocol.timeout("retry_budget", cfg.retry_budget_seconds);
                protocol.finish(Some(status.as_u16()), code::RETRY_BUDGET_EXHAUSTED);
                disconnected = send_error_frame(&sender, &mut protocol, &activity, &mut shutdown, status.as_u16(), cfg.idle_seconds).await;
                break;
            }
            _ = stopping(&mut shutdown) => { protocol.finish(Some(status.as_u16()), code::GATEWAY_STOPPED); permits.neutral(&cfg); break; }
            _ = sender.closed(), if !disconnected => { disconnected = true; continue; }
            frame = request_budget.response(limit, silence, body.frame()) => frame,
        };
        match next {
            Ok(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    let skipped = inspected_bytes.min(data.len() as u64) as usize;
                    inspected_bytes -= skipped as u64;
                    if skipped < data.len() {
                        protocol.feed(&data[skipped..]);
                    }
                }
                let complete = body.is_end_stream();
                if complete {
                    protocol.finish(
                        Some(status.as_u16()),
                        if neutral { code::HTTP } else { code::OK },
                    );
                }
                remember(&protocol, &g, &route, model.as_deref());
                observe_protocol(
                    &protocol,
                    &mut permits,
                    &cfg,
                    &g,
                    &route,
                    model.as_deref(),
                    Some(status.as_u16()),
                    attempted,
                );
                if !disconnected {
                    tokio::select! {
                        biased;
                        _ = request_budget.cancelled() => { protocol.finish(Some(status.as_u16()), code::CANCELLED); permits.neutral(&cfg); disconnected = send_error_frame(&sender, &mut protocol, &activity, &mut shutdown, status.as_u16(), cfg.idle_seconds).await; break; }
                        _ = stopping(&mut shutdown) => { protocol.finish(Some(status.as_u16()), code::GATEWAY_STOPPED); permits.neutral(&cfg); break; }
                        result = send_frame(&sender, Ok(frame), protocol.terminal_observed(), complete, &activity, cfg.idle_seconds) => { if let Err(reason) = result { reason.record(&delivery, &mut protocol); disconnected = true; } }
                    }
                    if !disconnected {
                        protocol.output_handed(protocol.business_observed());
                    }
                }
                if complete {
                    break;
                }
            }
            Ok(None) => {
                protocol.finish(
                    Some(status.as_u16()),
                    if neutral { code::HTTP } else { code::OK },
                );
                // EOF may finish a JSON value or an SSE event without a
                // trailing newline. All preceding body frames were acknowledged.
                delivery.handed(0, protocol.terminal_observed());
                protocol.output_handed(protocol.business_observed());
                break;
            }
            result => {
                let unwritten = matches!(silence, Silence::Compaction(Some(_)));
                let category = if result.is_ok() {
                    code::STREAM_INTERRUPTED
                } else if unwritten {
                    code::FIRST_BYTE_TIMEOUT
                } else {
                    code::STREAM_TIMEOUT
                };
                match result.as_ref().err() {
                    Some(Quiet::Stalled) => protocol.timeout("stream_stall", cfg.stall_seconds),
                    Some(Quiet::Held) => protocol.timeout("hold_idle", cfg.hold_idle_seconds),
                    Some(Quiet::Deadline) if unwritten => {
                        protocol.timeout("first_byte", cfg.compaction_seconds)
                    }
                    Some(Quiet::Deadline) => protocol.timeout("stream_idle", cfg.idle_seconds),
                    None => {}
                }
                protocol.finish(Some(status.as_u16()), category);
                if !disconnected && protocol.transport_failure() {
                    disconnected = send_error_frame(
                        &sender,
                        &mut protocol,
                        &activity,
                        &mut shutdown,
                        status.as_u16(),
                        cfg.idle_seconds,
                    )
                    .await;
                }
                break;
            }
        }
    }
    if route.endless()
        && route.client_id == super::ClientId::Claude
        && !disconnected
        && !delivery.is_settled()
        && !protocol.terminal_observed()
    {
        disconnected = send_error_frame(
            &sender,
            &mut protocol,
            &activity,
            &mut shutdown,
            status.as_u16(),
            cfg.idle_seconds,
        )
        .await;
    }
    // Close the body channel before waiting: Hyper must see its end in
    // order to flush the final chunk/trailers. Socket completion is local
    // write evidence, never an acknowledgement by the client.
    traffic.finishing();
    drop(body);
    drop(sender);
    if disconnected {
        protocol.finish(Some(status.as_u16()), code::CLIENT_DISCONNECTED);
    } else {
        finish_delivery(
            &delivery,
            &mut protocol,
            &activity,
            &mut shutdown,
            status.as_u16(),
            cfg.idle_seconds,
        )
        .await;
    }
    if disconnected {
        return;
    }
    remember(&protocol, &g, &route, model.as_deref());
    observe_protocol(
        &protocol,
        &mut permits,
        &cfg,
        &g,
        &route,
        model.as_deref(),
        Some(status.as_u16()),
        attempted,
    );
    if protocol.succeeded() && lasting {
        g.persist_success(&route.provider).await;
    }
}
/// The client-facing body: frames from the relay task, with delivery marked
/// before each frame is yielded.
fn delivered(
    mut receiver: tokio::sync::mpsc::Receiver<ForwardFrame>,
    output_delivery: std::sync::Arc<super::protocol::Delivery>,
    empty: bool,
) -> WireBody {
    let guard = DeliveryGuard {
        delivery: output_delivery.clone(),
        complete: empty,
    };
    let output = async_stream::stream! {
        let mut guard = guard;
        while let Some((frame, terminal, complete, ack)) = receiver.recv().await {
            let bytes = frame.as_ref().ok().and_then(|f| f.data_ref()).map_or(0, Bytes::len);
            output_delivery.handed(bytes, terminal);
            if frame.is_err() {
                guard.complete = true;
                output_delivery.event("response_body_aborted_upstream");
                output_delivery.settle();
            } else if complete {
                guard.complete = true;
                output_delivery.body_complete();
            }
            let _ = ack.send(());
            yield frame;
        }
        guard.complete = true;
        output_delivery.body_complete();
    };
    StreamBody::new(output).boxed_unsync()
}
/// Why no upstream request was built for a candidate.
enum Unbuilt {
    /// The provider cannot take this request; try the next candidate.
    Skip,
    Reject(Box<Response<WireBody>>),
}
/// The request sent to one provider: the replayed body, the provider's
/// address and its credentials. `model` is what the request is routed as and
/// `client_model` what the client wrote: the body is rewritten when the
/// provider maps the model, or when the request was given another one. Also
/// returns the model sent in the client's place, if any.
#[allow(clippy::too_many_arguments)]
async fn upstream_request(
    route: &Route,
    parts: &hyper::http::request::Parts,
    replay: &Replay,
    model: Option<&str>,
    client_model: Option<&str>,
    effort: Option<&str>,
    operation: crate::usage::model::Operation,
    request_budget: &super::wait_budget::WaitBudget,
    websocket: bool,
) -> Result<(Request<WireBody>, Option<String>), Unbuilt> {
    let mut uri = match route.target(&parts.uri) {
        Ok(uri) => uri,
        Err(_) => return Err(Unbuilt::Skip),
    };
    let vertex = route.client_id == super::ClientId::Claude
        && super::vertex::anthropic(&route.provider.base_url);
    let upstream_model = route
        .dsh_models
        .as_ref()
        .and_then(|mapping| model.and_then(|m| mapping.resolve(m)))
        .or_else(|| model.and_then(|m| super::model::mapped_model(&route.model_map, m)))
        .or(model.filter(|_| model != client_model))
        .map(str::to_owned);
    let sent_model = upstream_model
        .as_deref()
        .filter(|upstream| Some(*upstream) != client_model);
    // Anthropic keeps the effort among the output settings of a message,
    // OpenAI among the reasoning settings of a response.
    let effort = effort.map(|level| {
        let holder = if is_messages_path(parts.uri.path()) {
            "output_config"
        } else {
            "reasoning"
        };
        (holder, level)
    });
    let encoding = parts
        .headers
        .get(header::CONTENT_ENCODING)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("identity");
    let mapped = if route.client_id == super::ClientId::Agy {
        // Antigravity labels every request for billing, which only Vertex AI
        // takes: the Gemini API refuses a request that carries the field.
        if parts.method == hyper::Method::POST && !super::gemini::vertex(&route.provider.base_url) {
            replay.without(encoding, "labels").await
        } else {
            None
        }
    } else if vertex {
        // Vertex AI has the Messages API alone, and names the model and the
        // method in an address of its own.
        if parts.method != hyper::Method::POST {
            return Err(Unbuilt::Skip);
        }
        let address = (route.provider.base_url.as_str(), parts.uri.path());
        let built = replay
            .rewritten(encoding, sent_model, effort, Some(address))
            .await
            .ok()
            .and_then(|(body, address)| Some((body, address?.parse().ok()?)));
        let Some((body, address)) = built else {
            return Err(Unbuilt::Reject(Box::new(error(
                StatusCode::BAD_REQUEST,
                code::CONFIG,
                "Vertex AI 只接受带有效模型 ID 的 Messages 请求",
            ))));
        };
        uri = address;
        Some(body)
    } else if sent_model.is_some() || effort.is_some() {
        match replay.rewritten(encoding, sent_model, effort, None).await {
            Ok((body, _)) => Some(body),
            Err(_) => {
                return Err(Unbuilt::Reject(Box::new(error(
                    StatusCode::BAD_REQUEST,
                    code::CONFIG,
                    "改写模型或思考强度需要有效的 JSON 请求",
                ))))
            }
        }
    } else {
        None
    };
    let mapped_length = mapped.as_ref().map(|b| b.len());
    let mut request_body = mapped.map(replay::full).unwrap_or_else(|| replay.body());
    if operation == crate::usage::model::Operation::Compaction {
        let budget = request_budget.clone();
        let identity = budget.deadline();
        let mut dispatched = false;
        request_body = request_body
            .map_frame(move |frame| {
                // Hyper polls the upload only after acquiring a connection.
                // Queueing and connection setup retain their original budget.
                if !dispatched {
                    dispatched = true;
                    budget.compaction_execution(identity, true);
                }
                frame
            })
            .boxed_unsync();
    }
    let mut upstream = Request::new(request_body);
    *upstream.method_mut() = parts.method.clone();
    *upstream.uri_mut() = uri;
    *upstream.headers_mut() = parts.headers.clone();
    clean_headers(upstream.headers_mut(), websocket);
    upstream.headers_mut().remove(header::HOST);
    upstream.headers_mut().remove(header::AUTHORIZATION);
    upstream.headers_mut().remove("x-api-key");
    upstream.headers_mut().remove("x-goog-api-key");
    let Ok(auth) = header::HeaderValue::from_str(&format!("Bearer {}", route.provider.token))
    else {
        return Err(Unbuilt::Skip);
    };
    upstream.headers_mut().insert(header::AUTHORIZATION, auth);
    if vertex && super::vertex::headers(upstream.headers_mut(), &route.provider.token).is_none() {
        return Err(Unbuilt::Skip);
    }
    if let Some(connection) = &route.grok {
        if connection
            .authenticate(upstream.headers_mut(), &route.provider.token)
            .is_err()
        {
            return Err(Unbuilt::Reject(Box::new(error(
                StatusCode::BAD_GATEWAY,
                code::GROK_AUTH,
                "供应商认证配置无效",
            ))));
        }
    }
    if let Some(length) = mapped_length {
        upstream.headers_mut().remove(header::CONTENT_ENCODING);
        upstream.headers_mut().remove(header::TRAILER);
        upstream
            .headers_mut()
            .insert(header::CONTENT_LENGTH, length.into());
    } else if !replay.has_trailers() {
        upstream.headers_mut().insert(
            header::CONTENT_LENGTH,
            header::HeaderValue::from(replay.length),
        );
    } else {
        upstream.headers_mut().remove(header::CONTENT_LENGTH);
    }
    Ok((upstream, upstream_model))
}
/// What is known about a request before the first attempt: what it asks for,
/// which conversation it belongs to and which providers may serve it.
struct Plan {
    hints: Result<replay::RequestHints, BoxError>,
    stream_hint: bool,
    model: Option<String>,
    /// The only provider allowed to serve this request, when one is fixed.
    pinned: Option<String>,
    /// A continuation whose owner is unknown: try one provider, never move.
    unknown_affinity: bool,
    continuation: bool,
    operation: crate::usage::model::Operation,
    requirement: Requirement,
    fixed_dsh: Option<String>,
    responses: bool,
    thread: Option<String>,
    thread_identity: crate::usage::model::ThreadIdentity,
    auxiliary: bool,
    owned_search: bool,
    ownership: Option<std::sync::Arc<super::compaction::Lease>>,
    established_capacity_owner: Option<String>,
    usage_trace: Option<crate::usage::Trace>,
    /// The kind of request this is, when a provider may take it over. A
    /// continuation stays where its conversation is.
    delegated: Option<Delegated>,
}
/// Inspects the request and narrows `ids` to the providers it may use.
async fn plan(
    gateway: &Gateway,
    parts: &hyper::http::request::Parts,
    replay: &Replay,
    mode: &str,
    websocket: bool,
    ids: &mut Vec<String>,
    routes: &std::collections::HashMap<String, Route>,
) -> Result<Plan, Box<Response<WireBody>>> {
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let hints = if content_type.contains("json") || content_type.starts_with("multipart/form-data")
    {
        replay
            .inspect(
                parts
                    .headers
                    .get(header::CONTENT_ENCODING)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("identity"),
                content_type,
            )
            .await
    } else {
        Ok(replay::RequestHints::default())
    };
    // A Gemini request says in its address what the others say in the body.
    let gemini = gateway.client_id() == super::ClientId::Agy;
    let stream_hint = websocket
        || (gemini && super::gemini::streams(parts.uri.path()))
        || hints.as_ref().is_ok_and(|h| h.stream)
        || parts
            .headers
            .get(header::ACCEPT)
            .is_some_and(|h| h.as_bytes().windows(17).any(|w| w == b"text/event-stream"));
    let mut model = if gemini {
        super::gemini::model(parts.uri.path())
    } else {
        hints.as_ref().ok().and_then(|h| h.model.clone())
    };
    let compaction_trigger = (gateway.client_id() != super::ClientId::Claude
        && hints.as_ref().is_ok_and(|h| h.compaction_trigger))
        || (gateway.client_id() == super::ClientId::Claude
            && parts.method == hyper::Method::POST
            && is_messages_path(parts.uri.path())
            && parts
                .headers
                .get("x-claude-code-compaction")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|v| !v.is_empty() && v != "0" && v != "false"));
    let mut pinned = (mode == "manual").then(|| ids.first().cloned()).flatten();
    let mut unknown_affinity = false;
    let continuation = match &hints {
        Ok(hints) if hints.previous_response_id.is_some() => {
            let previous = hints.previous_response_id.as_ref().unwrap();
            let owner = gateway
                .request_state()
                .affinity
                .get(previous)
                .filter(|(_, _, at)| at.elapsed() < Duration::from_secs(3600))
                .cloned();
            if let Some((owner, previous_model, _)) = owner {
                pinned = Some(owner.clone());
                *ids = vec![owner];
                if model.is_none() {
                    model = previous_model;
                }
            } else {
                ids.truncate(1);
                unknown_affinity = true;
            }
            true
        }
        // The original bytes may contain an uninspectable continuation. Try only
        // one eligible (unrestricted) route, without cutting off earlier filters.
        Err(_) => {
            unknown_affinity = true;
            true
        }
        _ => false,
    };
    let operation = if compaction_trigger {
        crate::usage::model::Operation::Compaction
    } else {
        crate::usage::model::Operation::for_path(parts.uri.path())
    };
    let requirement = if operation == crate::usage::model::Operation::WebSearch
        || (!websocket && routing::resource(&parts.method, parts.uri.path()))
    {
        Requirement::Resource
    } else {
        Requirement::model(model.as_deref())
    };
    let fixed_dsh = if gateway.client_id() == super::ClientId::Dsh {
        let mut eligible = ids.iter().filter(|id| {
            routes
                .get(*id)
                .is_some_and(|route| requirement.allows(route.provider.allowed_models.as_deref()))
        });
        let first = eligible.next().cloned();
        if eligible.next().is_none() {
            first
        } else {
            None
        }
    } else {
        None
    };
    let responses = matches!(
        parts.uri.path().trim_end_matches('/'),
        "/responses" | "/v1/responses" | "/responses/compact" | "/v1/responses/compact"
    );
    let (thread, thread_identity) = if gateway.client_id() == super::ClientId::Codex
        && operation == crate::usage::model::Operation::WebSearch
        && !websocket
    {
        super::compaction::search_identity(&parts.headers)
    } else {
        super::compaction::identity(&parts.headers)
    };
    let auxiliary = hints.as_ref().is_ok_and(|h| h.auxiliary);
    if (auxiliary || hints.is_err())
        && gateway.client_id() == super::ClientId::Codex
        && mode == "auto"
    {
        if let Some(owner) = thread
            .as_deref()
            .and_then(|s| gateway.0.compaction.owner(s))
            .filter(|id| ids.contains(id))
        {
            *ids = vec![owner.clone()];
            if hints.is_err() {
                // Metadata failure cannot move a conversation we already know.
                pinned = Some(owner);
            }
        }
    }
    let owned_search = gateway.client_id() == super::ClientId::Codex
        && mode == "auto"
        && !websocket
        && operation == crate::usage::model::Operation::WebSearch
        && !unknown_affinity
        && pinned.is_none();
    let ownership = if owned_search {
        match thread
            .clone()
            .map(|session| gateway.0.compaction.prepare_search(session))
            .transpose()
        {
            Ok(lease) => lease.flatten(),
            Err(e) => {
                return Err(Box::new(error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &e.code,
                    &e.message,
                )))
            }
        }
    } else if gateway.client_id() == super::ClientId::Codex
        && mode == "auto"
        && responses
        && !unknown_affinity
        && !auxiliary
    {
        let queued = match super::compaction::queued(gateway) {
            Ok(queued) => queued,
            Err(e) => {
                return Err(Box::new(error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &e.code,
                    &e.message,
                )))
            }
        };
        if let Some(session) = thread.clone().filter(|_| {
            queued
                .iter()
                .any(|p| requirement.allows(p.allowed_models.as_deref()))
        }) {
            let lease = gateway.0.compaction.prepare_request(
                session,
                hints.as_ref().ok(),
                !continuation && operation == crate::usage::model::Operation::Model,
                &queued,
                &requirement,
                pinned.as_deref(),
            );
            if lease.is_none() {
                return Err(Box::new(error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    code::CONVERSATION_OWNERSHIP,
                    "线程归属记录暂不可用，请检查网关状态",
                )));
            }
            lease
        } else {
            None
        }
    } else {
        None
    };
    let established_capacity_owner =
        routing::capacity_owner(gateway.client_id(), mode == "manual", ownership.as_deref());
    let usage_trace = if (matches!(requirement, Requirement::Resource)
        && operation != crate::usage::model::Operation::WebSearch)
        || (gemini && super::gemini::counts(parts.uri.path()))
    {
        None
    } else {
        gateway.usage_operation(model.as_deref(), operation)
    };
    let delegated = (!websocket && !continuation)
        .then(|| {
            // In manual mode only the selected provider takes anything over.
            let searches_taken = routes.iter().any(|(id, route)| {
                (mode != "manual" || ids.contains(id))
                    && route.delegate.takes(Delegated::WebSearch).is_some()
            });
            super::delegation::class(
                gateway.client_id(),
                parts,
                operation,
                hints.as_ref().ok(),
                searches_taken,
            )
        })
        .flatten();
    Ok(Plan {
        hints,
        stream_hint,
        model,
        pinned,
        unknown_affinity,
        continuation,
        operation,
        requirement,
        fixed_dsh,
        responses,
        thread,
        thread_identity,
        auxiliary,
        owned_search,
        ownership,
        established_capacity_owner,
        usage_trace,
        delegated,
    })
}
/// A fresh queue wait for the request as it is planned.
fn admission_budget(settings: &Settings, traffic: &super::freeze::Ticket, plan: &Plan) -> Budget {
    let mut budget = Budget::new(settings.queue_seconds);
    budget.traffic = Some(traffic.clone());
    budget.follow(plan.ownership.as_deref());
    if plan.pinned.is_some() || plan.unknown_affinity {
        budget.pin_provider();
    }
    budget
}
/// How a request comes to a provider of the queue for its kind.
#[derive(Clone, Copy, PartialEq)]
enum Turn {
    /// Asked whether it has room now.
    Offered,
    /// It had no room when asked; the request now waits in line for it.
    Busy,
    /// It is cooling; the request waits for it to recover.
    Cooling,
}
/// A request in the hands of a provider that takes its kind over, and what
/// was planned for it before: the model the client asked for and the
/// ordinary queue, which the next provider of the kind's queue is picked from.
struct Ordinary {
    kind: Delegated,
    /// The reasoning effort the provider runs the kind on, when it names one.
    effort: Option<String>,
    /// No provider of the kind's queue could serve the request when offered
    /// it, which now waits for this one.
    waits: bool,
    model: Option<String>,
    requirement: Requirement,
    pinned: Option<String>,
    ownership: Option<std::sync::Arc<super::compaction::Lease>>,
    established_capacity_owner: Option<String>,
    ids: Vec<String>,
    routes: std::collections::HashMap<String, Route>,
}
fn stopped() -> Response<WireBody> {
    error(StatusCode::SERVICE_UNAVAILABLE, code::STOPPED, "网关已停止")
}
/// Waits out a transient retry delay, then reports whether the same provider
/// is still queued and available to be tried again first.
async fn transient_wait(
    gateway: &Gateway,
    route: &Route,
    reset_generation: u64,
    delay: Duration,
    max_waiting: usize,
    pinned: Option<&str>,
    requirement: &Requirement,
) -> Result<bool, Rejected> {
    gateway
        .0
        .admission
        .wait_transient(
            CapacitySource {
                provider_id: route.provider.id.clone(),
                reset_generation,
            },
            delay,
            max_waiting,
        )
        .await?;
    Ok(gateway.routing_ids(pinned).contains(&route.provider.id)
        && route.health_for(requirement).available)
}
/// How an attempt ends when it has no response to relay.
enum End {
    /// Go around the attempt loop again.
    Again,
    /// Leave the loop; the request is answered from what the attempts left.
    Stop,
    Reply(Box<Response<WireBody>>),
}
impl End {
    fn reply(response: Response<WireBody>) -> Self {
        Self::Reply(Box::new(response))
    }
}
/// A response head an attempt may hand to the client.
enum Accepted {
    Tunnel(Attempt, Box<Response<Incoming>>),
    Stream(Attempt, Box<Opening>),
}
/// One provider's turn at the request, from admission to its response head.
struct Attempt {
    admission: Admission,
    route: Route,
    protocol: super::protocol::Protocol,
    reset_generation: u64,
    /// This attempt went to a different provider than the previous one.
    rerouted: bool,
    started: Instant,
}
/// The start of an accepted response, read before anything reaches the client.
struct Opening {
    status: StatusCode,
    parts: hyper::http::response::Parts,
    body: WireBody,
    /// Each frame with whether a terminal event and business output were seen by then.
    prefix: Vec<(hyper::body::Frame<Bytes>, bool, bool)>,
    prefix_bytes: usize,
    ended: bool,
    failed: bool,
    failure_kind: &'static str,
    /// The start was spooled to disk; `body` replays it before the rest.
    preinspected: bool,
    inspected_bytes: u64,
    /// The response went silent in a way an outage explains, so the network
    /// is probed before the request is sent again.
    probe: bool,
}
/// One HTTP request across its attempts: what was planned for it, the queue it
/// still has, and what the attempts so far have left behind.
struct Exchange<'a> {
    gateway: &'a Gateway,
    settings: &'a Settings,
    mode: &'a str,
    parts: &'a hyper::http::request::Parts,
    replay: &'a Replay,
    request_budget: &'a super::wait_budget::WaitBudget,
    websocket: bool,
    active: Active,
    traffic: super::freeze::Ticket,
    downstream_upgrade: Option<hyper::upgrade::OnUpgrade>,
    plan: Plan,
    ids: Vec<String>,
    routes: std::collections::HashMap<String, Route>,
    failover: super::failover::Tracker,
    wait_budget: Budget,
    last: Option<Response<WireBody>>,
    last_error: Option<super::health_settlement::Reported>,
    last_category: &'static str,
    attempted: usize,
    ordinary_attempts: usize,
    capacity_protected: bool,
    previous_provider: Option<String>,
    capacity_pending: bool,
    capacity_wait_owner: Option<String>,
    hard_denied: std::collections::HashSet<(String, String)>,
    capacity_retry_after: Option<Duration>,
    rpm_retry_after: Option<Duration>,
    capacity_sources: Vec<CapacitySource>,
    transient_retries: u8,
    retry_owner: Option<String>,
    claude_owner: Option<String>,
    /// What endless mode set aside when it fixed the request to a provider:
    /// the providers not yet tried and the attempts counted until then.
    unpinned: Option<(Vec<String>, usize)>,
    /// Every provider this request can go to is in endless mode: it waits
    /// and retries without the limits that would answer it with an error.
    unbounded: bool,
    /// Set while a provider that takes this kind of request has it.
    ordinary: Option<Ordinary>,
    /// The providers of the kind's queue that have had the request.
    asked: Vec<String>,
    /// Those of them that had no room for it when asked.
    busy: Vec<String>,
}
impl Exchange<'_> {
    /// The provider of the queue for `kind` the request goes to next, and
    /// how it comes to it. Each provider of the queue is offered the request
    /// once, in list order and without waiting. When none of them had room,
    /// the request waits in line for the first that was only busy; and when
    /// the rest are cooling, for the one that recovers first.
    fn next_taker(&self, kind: Delegated) -> Option<(super::delegation::Taker, Turn)> {
        let (ids, routes, model, requirement) = match &self.ordinary {
            Some(o) => (&o.ids, &o.routes, o.model.as_deref(), &o.requirement),
            None => (
                &self.ids,
                &self.routes,
                self.plan.model.as_deref(),
                &self.plan.requirement,
            ),
        };
        let resource = matches!(requirement, Requirement::Resource);
        let (order, owner) = if self.mode == "manual" {
            (ids.clone(), None)
        } else {
            let order: Vec<_> = self
                .gateway
                .request_state()
                .store
                .providers
                .iter()
                .map(|p| p.id.clone())
                .collect();
            // A compaction is read back where the conversation is.
            let owner = self
                .plan
                .thread
                .as_deref()
                .filter(|_| kind == Delegated::Compaction)
                .and_then(|thread| self.gateway.0.compaction.owner(thread));
            (order, owner)
        };
        let unasked: Vec<_> = order
            .into_iter()
            .filter(|id| !self.asked.contains(id))
            .collect();
        super::delegation::taker(&unasked, routes, kind, model, owner.as_deref(), resource)
            .map(|taker| (taker, Turn::Offered))
            .or_else(|| {
                super::delegation::taker(&self.busy, routes, kind, model, None, resource)
                    .map(|taker| (taker, Turn::Busy))
            })
            .or_else(|| {
                super::delegation::recovering(&unasked, routes, kind, model, resource)
                    .map(|taker| (taker, Turn::Cooling))
            })
    }
    /// Hands the request to the next provider in the queue that takes its
    /// kind. The request is then that provider's alone, on the model and at
    /// the reasoning effort the provider names. Reports whether there was
    /// such a provider.
    fn delegate(&mut self, kind: Delegated) -> bool {
        let Some((taker, turn)) = self.next_taker(kind) else {
            return false;
        };
        if let Some(ordinary) = self.ordinary.take() {
            // The plan the last provider of the queue ran it on is undone.
            self.plan.model = ordinary.model;
            self.plan.requirement = ordinary.requirement;
            self.plan.pinned = ordinary.pinned;
            self.plan.ownership = ordinary.ownership;
            self.plan.established_capacity_owner = ordinary.established_capacity_owner;
            self.ids = ordinary.ids;
            self.routes = ordinary.routes;
        }
        let Some(route) = self.routes.get(&taker.id).cloned() else {
            return false;
        };
        let id = taker.id;
        let waits = turn != Turn::Offered;
        if turn == Turn::Busy {
            self.busy.clear();
        } else {
            self.asked.push(id.clone());
        }
        // A request for a resource stays one where the provider names no
        // model of its own for it.
        let requirement = if matches!(self.plan.requirement, Requirement::Resource)
            && taker.model == self.plan.model
        {
            Requirement::Resource
        } else {
            Requirement::model(taker.model.as_deref())
        };
        self.ordinary = Some(Ordinary {
            kind,
            effort: taker.effort,
            waits,
            model: std::mem::replace(&mut self.plan.model, taker.model),
            requirement: std::mem::replace(&mut self.plan.requirement, requirement),
            pinned: self.plan.pinned.replace(id.clone()),
            ownership: self.plan.ownership.take(),
            established_capacity_owner: self.plan.established_capacity_owner.take(),
            ids: std::mem::replace(&mut self.ids, vec![id.clone()]),
            routes: std::mem::replace(&mut self.routes, [(id.clone(), route)].into()),
        });
        self.wait_budget = admission_budget(self.settings, &self.traffic, &self.plan);
        if waits {
            // A queue wait, which ends before the request's own waiting
            // budget does.
            self.wait_budget.restart(
                self.settings
                    .queue_seconds
                    .min(self.settings.retry_budget_seconds.saturating_sub(1)),
            );
        }
        self.request_budget
            .describe(Some(&id), self.plan.model.as_deref(), "admission");
        true
    }
    /// The provider that took the request over did not serve it: the request
    /// goes on to the next provider of that queue, as though it had just
    /// arrived. It never leaves the queue: after the last provider of it the
    /// request is over. `reply` is what the client would have been answered
    /// with, kept in case nothing better comes. Reports whether the request
    /// went on.
    fn hand_back(&mut self, reply: Option<Response<WireBody>>) -> bool {
        let Some(kind) = self.ordinary.as_ref().map(|o| o.kind) else {
            return false;
        };
        if reply.is_some() {
            self.last = reply;
        }
        if !self.delegate(kind) {
            self.ids.clear();
            return false;
        }
        self.claude_owner = None;
        self.unpinned = None;
        self.set_unbounded(false);
        self.request_budget.begin();
        self.request_budget.attach_traffic(&self.traffic);
        self.attempted = 0;
        self.ordinary_attempts = 0;
        self.transient_retries = 0;
        self.retry_owner = None;
        // What that provider refused says nothing about the next.
        self.hard_denied.clear();
        self.capacity_protected = false;
        self.capacity_pending = false;
        self.capacity_wait_owner = None;
        self.capacity_retry_after = None;
        self.rpm_retry_after = None;
        self.capacity_sources.clear();
        true
    }
    /// A provider this request may go to is set to take requests of `kind`:
    /// the kind has a queue, and the request is not the ordinary queue's.
    fn queued_for(&self, kind: Delegated) -> bool {
        self.routes.iter().any(|(id, route)| {
            (self.mode != "manual" || self.ids.contains(id)) && route.delegate.takes(kind).is_some()
        })
    }
    /// The model the client asked for, whatever the request is routed as.
    fn client_model(&self) -> Option<&str> {
        self.ordinary
            .as_ref()
            .map_or(self.plan.model.as_deref(), |o| o.model.as_deref())
    }
    /// What follows a failure the client would otherwise be answered with or
    /// kept waiting on: a request a provider took over goes on to the next
    /// provider of that queue, while there is one.
    fn after(&self, action: Action) -> Action {
        if matches!(action, Action::Returned | Action::Waiting)
            && self
                .ordinary
                .as_ref()
                .is_some_and(|o| self.next_taker(o.kind).is_some())
        {
            Action::TryingNext
        } else {
            action
        }
    }
    /// Endless mode applies to this request at `route`, whichever queue the
    /// request is in.
    fn endless(&self, route: &Route) -> bool {
        route.endless()
    }
    fn max_waiting(&self) -> usize {
        if self.unbounded {
            usize::MAX
        } else {
            self.settings.max_waiting
        }
    }
    fn set_unbounded(&mut self, unbounded: bool) {
        if self.unbounded != unbounded {
            self.unbounded = unbounded;
            self.request_budget.set_endless(unbounded);
            self.wait_budget.set_endless(unbounded);
        }
    }
    /// The provider endless mode fixed the request to cannot serve it, which
    /// no retry cures: the request goes on to the providers it had not tried
    /// that serve its model, and the attempts made here are not counted.
    /// Answers whether there is such a provider, when `needed`; otherwise
    /// the request is released whatever is left.
    fn unpin(&mut self, needed: bool) -> bool {
        if self.claude_owner.is_none() {
            return false;
        }
        let (mut ids, attempts) = self.unpinned.clone().unwrap_or_default();
        ids.retain(|id| {
            self.routes.get(id).is_some_and(|r| {
                self.plan
                    .requirement
                    .allows(r.provider.allowed_models.as_deref())
            })
        });
        if needed && ids.is_empty() {
            return false;
        }
        self.unpinned = None;
        self.claude_owner = None;
        self.retry_owner = None;
        self.set_unbounded(false);
        self.wait_budget = admission_budget(self.settings, &self.traffic, &self.plan);
        self.ids = ids;
        self.ordinary_attempts = attempts;
        true
    }
    /// Applies the providers' current endless settings to this request, so an
    /// edit reaches a request already in flight on its next attempt.
    fn refresh_endless(&mut self) {
        self.gateway.refresh_endless(&mut self.routes);
        if let Some(owner) = &self.claude_owner {
            if !self.routes.get(owner).is_some_and(|r| self.endless(r)) {
                // Endless mode was switched off: the ordinary limits apply again.
                self.claude_owner = None;
                self.set_unbounded(false);
            }
        }
    }
    /// A manual interruption drops the attempt or wait in progress and sends
    /// the request again, to the same provider when it has one. The dropped
    /// attempt is not counted, and the waiting budget starts over.
    fn resend(&mut self, before: (usize, usize, Vec<String>)) {
        self.request_budget.begin();
        self.request_budget.attach_traffic(&self.traffic);
        if self.unbounded {
            self.request_budget.set_endless(true);
        }
        (self.attempted, self.ordinary_attempts, self.ids) = before;
        if let Some(owner) = self.claude_owner.clone() {
            self.ids = vec![owner.clone()];
            self.retry_owner = Some(owner);
        } else {
            if self.ids.is_empty() && self.capacity_pending {
                // Interrupted while waiting out a capacity cooldown: the
                // queue is rebuilt now instead of after the wait.
                (self.ids, self.routes) = super::attempt::queue(
                    self.gateway,
                    self.plan.pinned.as_deref(),
                    &self.hard_denied,
                    self.parts.uri.path(),
                );
                self.capacity_pending = false;
                self.capacity_wait_owner = None;
                self.capacity_retry_after = None;
                self.capacity_sources.clear();
            }
            if let Some(position) = self
                .previous_provider
                .as_ref()
                .and_then(|id| self.ids.iter().position(|other| other == id))
            {
                // The provider it was interrupted at is still in the queue,
                // so it is not one that already failed this request.
                let id = self.ids.remove(position);
                let ready = self
                    .gateway
                    .routing_ids(self.plan.pinned.as_deref())
                    .contains(&id)
                    && self
                        .routes
                        .get(&id)
                        .is_some_and(|r| r.health_for(&self.plan.requirement).available);
                self.ids.insert(0, id.clone());
                self.retry_owner = ready.then_some(id);
            }
        }
        // The resend does not skip a cooldown: with every provider still
        // cooling, the queue wait also covers the first one to recover, so
        // the request is not answered with the failure it was waiting out.
        let cooling = self
            .ids
            .iter()
            .filter_map(|id| self.routes.get(id))
            .map(|route| {
                let health = route.health_for(&self.plan.requirement);
                if health.available {
                    0
                } else {
                    health.retry_in.max(1)
                }
            })
            .min()
            .unwrap_or(0);
        self.wait_budget
            .restart(self.settings.queue_seconds + cooling);
        self.request_budget.describe(
            self.claude_owner.as_deref().or(self.plan.pinned.as_deref()),
            self.plan.model.as_deref(),
            "admission",
        );
    }
    /// A dispatched compaction has its own limit for the wait before output.
    fn first_byte_seconds(&self) -> u64 {
        if self.plan.operation == crate::usage::model::Operation::Compaction {
            self.settings.compaction_seconds
        } else {
            self.settings.first_byte_seconds
        }
    }
    fn first_silence(&self) -> Silence {
        if self.plan.operation == crate::usage::model::Operation::Compaction {
            Silence::Compaction(Some(Duration::from_secs(self.settings.compaction_seconds)))
        } else {
            Silence::Ordinary
        }
    }
    /// The silence allowed once the response head has arrived.
    fn started_silence(&self) -> Silence {
        match self.first_silence() {
            Silence::Ordinary => Silence::Started,
            silence => silence,
        }
    }
    async fn claude_retry(
        &mut self,
        mut attempt: Attempt,
        status: Option<u16>,
        kind: &'static str,
        mut details: crate::events::Details,
        retry_after: Option<Duration>,
        probe: bool,
    ) -> End {
        let route = attempt.route.clone();
        let infinite = self.endless(&route);
        let can_retry = infinite || self.ordinary_attempts <= self.settings.max_retries;
        let source = CapacitySource {
            provider_id: route.provider.id.clone(),
            reset_generation: attempt.reset_generation,
        };
        // An endless retry is paced by the provider's own settings alone; the
        // upstream's Retry-After does not move it.
        let retry = infinite.then(|| {
            use crate::events::ErrorBasis;
            let class = super::model::retry_class(route.risk_codes(), status, &details);
            match class {
                RetryClass::Risk => details.error_basis = Some(ErrorBasis::RiskControl),
                // Without a named limit this is a rate-limit retry, also for
                // a 429 that only said the service was unavailable: not a
                // capacity cooldown.
                RetryClass::Limit if !super::failure_policy::dimensioned(&details) => {
                    details.error_basis = Some(ErrorBasis::LimitRetry)
                }
                _ => {}
            }
            // Any 429 rests the provider, also one listed as risk control.
            let limited = class == RetryClass::Limit
                || status == Some(429)
                || details.upstream_code.as_deref() == Some("429");
            self.gateway.0.admission.retry(&source, class, limited)
        });
        let delay = match &retry {
            Some((_, delay)) => *delay,
            None => Duration::from_secs(self.settings.rate_limit_seconds)
                .max(retry_after.unwrap_or_default()),
        };
        let wait_seconds = delay.as_millis().div_ceil(1000) as u64;
        details.local_code.get_or_insert(kind.into());
        details.wait_seconds = can_retry.then_some(wait_seconds);
        details.phase.get_or_insert(if kind == code::NETWORK {
            crate::events::Phase::Connect
        } else if kind == code::FIRST_BYTE_TIMEOUT {
            crate::events::Phase::Headers
        } else {
            crate::events::Phase::Response
        });
        let policy = super::failure_policy::decision(status, &details);
        let rate_limited = status == Some(429)
            || policy.temporary
            || (infinite && details.error_basis == Some(crate::events::ErrorBasis::QuotaExhausted));
        if infinite {
            details.health_scope = Some(crate::events::HealthScope::Provider);
            details.counted_failure = Some(false);
        } else {
            details.counted_failure =
                Some(!rate_limited && (details.error_basis.is_none() || policy.counted_failure));
        }
        self.failover.failure(if rate_limited {
            super::failover::Failure::RateLimit
        } else {
            super::failover::Failure::Transport
        });
        let reason = if status == Some(429) && details.error_basis.is_none() {
            Reason::RateLimit
        } else if status.is_some_and(|s| s >= 400) || details.error_basis.is_some() {
            policy.reason
        } else {
            Reason::Network
        };
        let mut event = attempt.admission.permits.prepare_report(
            self.gateway,
            &route,
            self.plan.model.as_deref(),
            reason,
            if can_retry {
                Action::RetryingSame
            } else {
                self.after(Action::Returned)
            },
            status,
            self.attempted,
            details.clone(),
        );
        event.details.wait_seconds = can_retry.then_some(wait_seconds);
        if infinite {
            attempt.admission.permits.observed(self.settings);
        } else if matches!(
            kind,
            code::NETWORK
                | code::FIRST_BYTE_TIMEOUT
                | code::STREAM_TIMEOUT
                | code::STREAM_INTERRUPTED
        ) {
            attempt
                .admission
                .permits
                .failure(self.settings, retry_after);
        } else if status == Some(429) && details.error_basis.is_none() {
            attempt
                .admission
                .permits
                .rate_limited(self.settings, retry_after);
        } else {
            attempt
                .admission
                .permits
                .policy(self.settings, status, &details, retry_after);
        }
        self.last_error = Some(self.failover.report_failure(Some(&attempt.protocol), || {
            super::health_settlement::publish(self.gateway, event)
        }));
        self.last_category = kind;
        drop(attempt);
        if !can_retry {
            if self.last.is_none() || kind != code::HTTP {
                self.last = Some(claude_rejection_response(status, kind, &details, false));
            }
            return End::Stop;
        }
        // A request held until the network came back is sent at once.
        let restored = probe
            && super::connectivity::settle(
                self.gateway,
                &route,
                self.settings,
                self.request_budget,
                self.plan.model.as_deref(),
                true,
            )
            .await
                == super::connectivity::Verdict::Restored;
        self.failover.wait(
            super::failover::WaitKind::TransientRetry,
            if restored { 0 } else { wait_seconds },
        );
        self.request_budget.describe(
            Some(&route.provider.id),
            self.plan.model.as_deref(),
            "retry",
        );
        let result = match retry {
            Some((retry, _)) => {
                if restored {
                    retry.release();
                }
                retry.wait().await
            }
            None => {
                self.gateway
                    .0
                    .admission
                    .wait_transient(
                        source,
                        if restored { Duration::ZERO } else { delay },
                        self.max_waiting(),
                    )
                    .await
            }
        };
        match result {
            Ok(()) => {
                self.ids = vec![route.provider.id.clone()];
                self.retry_owner = Some(route.provider.id.clone());
                End::Again
            }
            Err(Rejected::Stopped) => End::reply(stopped()),
            Err(_) => End::Stop,
        }
    }
    /// The queue is empty: waits out a pending capacity cooldown and rebuilds
    /// the queue, or stops.
    async fn await_capacity(&mut self) -> End {
        if self.capacity_pending && self.ordinary_attempts <= self.settings.max_retries {
            let delay = capacity_delay(self.settings, self.capacity_retry_after);
            self.failover.wait(
                if self.capacity_retry_after.is_some() {
                    super::failover::WaitKind::RetryAfter
                } else {
                    super::failover::WaitKind::CapacityRetry
                },
                delay.as_secs(),
            );
            let owner_route = self
                .capacity_wait_owner
                .as_deref()
                .or_else(|| {
                    self.last_error
                        .as_ref()
                        .and_then(|e| e.provider_id.as_deref())
                })
                .and_then(|id| self.routes.get(id));
            self.request_budget
                .describe(None, self.plan.model.as_deref(), "capacity");
            let waited = self
                .gateway
                .wait_capacity_checked(
                    &self.capacity_sources,
                    delay,
                    self.max_waiting(),
                    self.capacity_wait_owner.is_some(),
                    owner_route,
                    self.last_error.as_deref(),
                )
                .await;
            match waited {
                Ok(true) => {
                    if let Some(route) = owner_route {
                        self.hard_denied
                            .insert((route.provider.id.clone(), route.provider.version.clone()));
                    }
                    self.capacity_wait_owner = None;
                    self.failover
                        .failure(super::failover::Failure::AccountRejected);
                }
                Ok(false) => {}
                Err(Rejected::Stopped) => {
                    self.failover.finish(super::failover::Exit::Stopped);
                    return End::reply(stopped());
                }
                Err(_) => return End::Stop,
            }

            (self.ids, self.routes) = super::attempt::queue(
                self.gateway,
                self.plan.pinned.as_deref(),
                &self.hard_denied,
                self.parts.uri.path(),
            );
            if self.ids.is_empty() {
                return End::Stop;
            }
            self.retry_owner = self
                .capacity_wait_owner
                .take()
                .filter(|owner| self.ids.contains(owner));
            self.capacity_pending = false;
            self.capacity_retry_after = None;
            self.capacity_sources.clear();
            return End::Again;
        }
        End::Stop
    }
    /// The providers this attempt may be admitted to, and whether a provider
    /// that takes new threads was passed over for not serving the model.
    async fn candidates(&mut self) -> Result<(Vec<Route>, bool), End> {
        let mut candidates: Vec<_> = self
            .ids
            .iter()
            .filter_map(|id| self.routes.get(id).cloned())
            .collect();
        let mut observed_candidates = candidates.clone();
        observed_candidates.extend(
            self.gateway
                .routing_ids(self.plan.pinned.as_deref())
                .iter()
                .filter_map(|id| self.gateway.route(id))
                .filter(|r| !r.accepts_protocol(self.parts.uri.path())),
        );
        let (matches_model, new_thread_model_denied) = super::attempt::serving_model(
            &mut candidates,
            &self.plan.requirement,
            self.plan.ownership.as_deref(),
        );
        self.wait_budget.capacity_owner = None;
        if let Some(owner) = self.retry_owner.take() {
            candidates.retain(|route| route.provider.id == owner);
        } else if let Some(lease) = self
            .plan
            .ownership
            .as_ref()
            .filter(|_| self.plan.pinned.is_none())
        {
            candidates = if self.attempted > 0 || !self.hard_denied.is_empty() {
                lease.candidates_after_failure(candidates, &self.plan.requirement)
            } else {
                lease.candidates_for_admission(
                    candidates,
                    &self.plan.requirement,
                    &mut self.wait_budget,
                )
            };
        } else if self.gateway.client_id() == super::ClientId::Codex
            && self.mode == "auto"
            && self.plan.pinned.is_none()
            && !self.plan.auxiliary
        {
            candidates = super::compaction::unidentified_candidates(candidates);
        }
        let unbounded = self.claude_owner.is_some()
            || (!candidates.is_empty() && candidates.iter().all(|r| self.endless(r)));
        self.set_unbounded(unbounded);
        // A second request entering during its owner's capacity cooldown must
        // wait as well. Read the reset generation before the health snapshot.
        if self.attempted == 0 && candidates.len() == 1 {
            let owner = &candidates[0];
            let source = self.gateway.0.admission.capacity_source(&owner.provider.id);
            let health = owner.health_for(&self.plan.requirement);
            if self.plan.established_capacity_owner.as_deref() == Some(&owner.provider.id)
                && routing::capacity_cooling(&health)
            {
                self.wait_budget.protect_capacity();
                self.failover
                    .wait(super::failover::WaitKind::CapacityRetry, health.retry_in);
                let cause = self.gateway.capacity_cause(
                    &owner.provider.id,
                    self.plan.model.as_deref(),
                    &health,
                );
                match self
                    .gateway
                    .wait_capacity_checked(
                        &[source],
                        Duration::from_secs(health.retry_in),
                        self.max_waiting(),
                        true,
                        Some(owner),
                        cause.as_ref(),
                    )
                    .await
                {
                    Ok(true) => {
                        self.hard_denied
                            .insert((owner.provider.id.clone(), owner.provider.version.clone()));
                        self.failover
                            .failure(super::failover::Failure::AccountRejected);
                        self.last = Some(error(
                            StatusCode::TOO_MANY_REQUESTS,
                            code::HTTP,
                            "本次额度查询确认耗尽，没有可用的替代供应商",
                        ));
                        self.last_category = code::HTTP;
                        self.last_error = cause.map(Into::into);
                    }
                    Ok(false) => {}
                    Err(Rejected::Stopped) => return Err(End::reply(stopped())),
                    Err(_) => {
                        return Err(End::reply(error(
                            StatusCode::TOO_MANY_REQUESTS,
                            code::CAPACITY,
                            "等待队列已满",
                        )))
                    }
                }
                (self.ids, self.routes) = super::attempt::queue(
                    self.gateway,
                    self.plan.pinned.as_deref(),
                    &self.hard_denied,
                    self.parts.uri.path(),
                );
                return Err(End::Again);
            }
        }
        let stage = if self.ordinary.is_some() {
            super::failover::Stage::Delegated
        } else if self.plan.unknown_affinity {
            super::failover::Stage::UnknownOwner
        } else if self.plan.pinned.is_some() {
            super::failover::Stage::CursorPinned
        } else if self.attempted > 0 || !self.hard_denied.is_empty() {
            super::failover::Stage::FailureFallback
        } else if self.plan.ownership.as_ref().is_some_and(|l| l.handoff) {
            super::failover::Stage::ProactiveHandoff
        } else {
            super::failover::Stage::Initial
        };
        self.failover.candidates(
            self.gateway,
            stage,
            &observed_candidates,
            &candidates,
            &self.plan.requirement,
            self.parts.uri.path(),
        );
        if matches_model
            && !candidates.iter().any(|r| {
                self.plan
                    .requirement
                    .allows(r.provider.allowed_models.as_deref())
            })
        {
            if self.attempted > 0 {
                if self.capacity_pending {
                    self.ids.clear();
                    return Err(End::Again);
                }
                return Err(End::Stop);
            }
            self.failover
                .finish(super::failover::Exit::NoEligibleProvider);
            self.gateway.record_policy_rejection(
                self.plan.model.as_deref(),
                false,
                super::compaction::rejection_reason(
                    self.plan.thread.as_deref(),
                    self.plan.hints.as_ref().ok(),
                ),
            );
            return Err(End::reply(error(
                StatusCode::SERVICE_UNAVAILABLE,
                code::NO_ELIGIBLE_PROVIDER,
                "没有符合线程接管策略的供应商",
            )));
        }
        if self.gateway.client_id() == super::ClientId::Codex
            && self.mode == "auto"
            && self.plan.pinned.is_none()
            && !self.plan.auxiliary
        {
            let (intake, owner) = self
                .plan
                .ownership
                .as_ref()
                .map_or((super::compaction::Intake::Ordinary, None), |lease| {
                    lease.admission_policy()
                });
            self.wait_budget.set_intake(intake, owner);
        }
        if self.attempted > 0 || !self.hard_denied.is_empty() {
            // Admission rechecks policy atomically. Its proactive priority gate
            // must also yield to the failure queue, without dropping the claim.
            self.wait_budget.handoff_owner = None;
        }
        Ok((candidates, new_thread_model_denied))
    }
    /// Takes a slot at one of the candidates.
    async fn admit(&mut self, candidates: &[Route]) -> Result<Admission, End> {
        let taken = self.ordinary.is_some();
        // A provider that takes the request over is first only offered it:
        // with no room there, the next of its queue is asked before the
        // request waits for any of them.
        // A provider endless mode has fixed the request to is waited for.
        let offered =
            self.ordinary.as_ref().is_some_and(|o| !o.waits) && self.claude_owner.is_none();
        let admission = self
            .gateway
            .0
            .admission
            .acquire_for_immediate(
                candidates,
                self.mode == "manual",
                self.max_waiting(),
                &mut self.wait_budget,
                &self.plan.requirement,
                self.capacity_pending || offered,
            )
            .await
            .inspect_err(|reason| {
                if !taken {
                    self.failover.rejected(reason)
                }
            });
        if offered
            && self.attempted == 0
            && matches!(
                admission,
                Err(Rejected::Full | Rejected::Timeout | Rejected::RateLimited(_))
            )
        {
            self.busy
                .extend(candidates.first().map(|r| r.provider.id.clone()));
        }
        let admission = match admission {
            Ok(value) => value,
            Err(Rejected::Full | Rejected::Timeout) => {
                if self.attempted > 0 {
                    if self.capacity_pending {
                        self.ids.clear();
                        return Err(End::Again);
                    }
                    return Err(End::Stop);
                }
                let mut response = error(
                    StatusCode::TOO_MANY_REQUESTS,
                    code::CAPACITY,
                    "供应商并发已满，等待队列已满或等待超时",
                );
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, header::HeaderValue::from_static("5"));
                return Err(End::reply(response));
            }
            Err(Rejected::Cooling(seconds)) => {
                if self.attempted > 0 {
                    if self.capacity_pending {
                        self.ids.clear();
                        return Err(End::Again);
                    }
                    return Err(End::Stop);
                }
                let mut response = error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    code::PROVIDERS_COOLING_DOWN,
                    "供应商正在冷却或恢复探测，请稍后重试",
                );
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, seconds.max(1).into());
                return Err(End::reply(response));
            }
            Err(Rejected::RateLimited(seconds)) => {
                self.rpm_retry_after = Some(Duration::from_secs(seconds.max(1)));
                if self.attempted > 0 {
                    self.last_category = code::RPM_LIMIT;
                    return Err(End::Stop);
                }
                let mut response = error(
                    StatusCode::TOO_MANY_REQUESTS,
                    code::RPM_LIMIT,
                    "供应商已达到 RPM 上限，等待后重试",
                );
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, seconds.max(1).into());
                return Err(End::reply(response));
            }
            Err(Rejected::Stopped) => {
                return Err(End::reply(stopped()));
            }
            Err(Rejected::Model) => {
                if self.attempted > 0 {
                    if self.capacity_pending {
                        self.ids.clear();
                        return Err(End::Again);
                    }
                    return Err(End::Stop);
                }
                return Err(End::reply(error(
                    StatusCode::BAD_REQUEST,
                    self.plan.requirement.code(),
                    if self.gateway.client_id() == super::ClientId::Dsh {
                        "没有供应商配置此 DSH 模型映射，请编辑 Flash / Pro 映射"
                    } else {
                        self.plan.requirement.message()
                    },
                )));
            }
            Err(Rejected::Policy) => {
                if self.attempted > 0 {
                    if self.capacity_pending {
                        self.ids.clear();
                        return Err(End::Again);
                    }
                    return Err(End::Stop);
                }
                self.gateway.record_policy_rejection(
                    self.plan.model.as_deref(),
                    false,
                    super::compaction::rejection_reason(
                        self.plan.thread.as_deref(),
                        self.plan.hints.as_ref().ok(),
                    ),
                );
                return Err(End::reply(error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    code::NO_ELIGIBLE_PROVIDER,
                    "没有符合线程接管策略的供应商",
                )));
            }
            Err(reason @ (Rejected::Unavailable | Rejected::Reconnect)) => {
                if reason == Rejected::Unavailable && self.unbounded && self.reroute(candidates) {
                    self.retry_owner = self.claude_owner.clone();
                    return Err(End::Again);
                }
                if self.capacity_pending {
                    self.ids.clear();
                    return Err(End::Again);
                }
                return Err(End::Stop);
            }
        };
        Ok(admission)
    }
    /// A provider edited while an endless request waited for it is picked up
    /// in its new version, as long as it still runs endless mode and serves
    /// this path. Reports whether any candidate was replaced.
    fn reroute(&mut self, candidates: &[Route]) -> bool {
        let mut replaced = false;
        for old in candidates {
            let Some(new) = self.gateway.route(&old.provider.id) else {
                continue;
            };
            if new.provider.version != old.provider.version
                && self.endless(&new)
                && new.accepts_protocol(self.parts.uri.path())
            {
                self.routes.insert(new.provider.id.clone(), new);
                replaced = true;
            }
        }
        replaced
    }
    /// Settles the admitted provider's endpoint and the thread's claim on it.
    async fn endpoint(&mut self, admission: &mut Admission) -> Result<Route, End> {
        let route = match self
            .gateway
            .endpoint_route(
                admission.route.clone(),
                self.plan.thread.as_deref(),
                self.plan
                    .hints
                    .as_ref()
                    .ok()
                    .and_then(|h| h.previous_response_id.as_deref()),
                self.plan
                    .ownership
                    .as_ref()
                    .is_some_and(|lease| lease.handoff || lease.establishing())
                    || self.plan.hints.as_ref().is_ok_and(|h| h.first_turn),
            )
            .await
        {
            Ok(route) => route,
            Err(reason) => {
                self.failover
                    .finish(super::failover::Exit::AdmissionRejected);
                admission.permits.neutral(self.settings);
                return Err(End::reply(error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &reason.code,
                    &reason.message,
                )));
            }
        };
        let owner_admitted = self.plan.ownership.as_ref().map_or(Ok(true), |lease| {
            lease.admit_request(
                self.gateway,
                &route.provider,
                &self.plan.requirement,
                self.plan.pinned.is_some(),
            )
        });
        let owner_admitted = match owner_admitted {
            Ok(value) => value,
            Err(e) => {
                admission.permits.neutral(self.settings);
                return Err(End::reply(error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &e.code,
                    &e.message,
                )));
            }
        };
        if !owner_admitted {
            admission.permits.neutral(self.settings);
            if self.gateway.0.compaction.error().is_some() {
                return Err(End::reply(error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    code::CONVERSATION_OWNERSHIP,
                    "线程归属记录暂不可用",
                )));
            }
            return Err(End::Again);
        }
        if self.plan.unknown_affinity {
            self.plan.pinned = Some(route.provider.id.clone());
            self.wait_budget.pin_provider();
        }
        self.ids.retain(|id| id != &route.provider.id);
        Ok(route)
    }
    /// Counts the attempt and prepares the protocol observer for it.
    fn begin(
        &mut self,
        mut admission: Admission,
        route: Route,
        upstream_model: Option<String>,
        new_thread_model_denied: bool,
    ) -> Attempt {
        let reset_generation = admission.reset_generation;
        admission.commit_rpm();
        self.attempted = self.attempted.saturating_add(1);
        self.ordinary_attempts = self.ordinary_attempts.saturating_add(1);
        let rerouted = self
            .previous_provider
            .as_deref()
            .is_some_and(|id| id != admission.route.provider.id);
        self.previous_provider = Some(admission.route.provider.id.clone());
        let started = Instant::now();
        let mut protocol = super::protocol::Protocol::new(self.plan.stream_hint);
        protocol.attach_wait_budget(self.request_budget);
        protocol.buffer_output(
            route.provider.buffer_complete_response && is_messages_path(self.parts.uri.path()),
        );
        protocol.attach_failure_rules(&self.settings.custom_failure_rules);
        self.request_budget.describe(
            Some(&route.provider.id),
            self.plan.model.as_deref(),
            "headers",
        );
        self.request_budget
            .watch_endpoint(self.settings.network_probe.then(|| {
                (
                    self.gateway.0.links.clone(),
                    super::connectivity::endpoint_key(&route),
                    Duration::from_secs(self.settings.hold_idle_seconds),
                )
            }));
        protocol.set_operation(self.plan.operation);
        protocol.diagnostics(self.gateway, &route.provider.id, self.plan.model.as_deref());
        match &self.ordinary {
            // The conversation stays with its owner; only a compaction the
            // provider writes for it is recorded.
            Some(ordinary) => protocol.attach_takeover(
                ordinary
                    .ownership
                    .clone()
                    .filter(|_| ordinary.kind == Delegated::Compaction),
                &route.provider.id,
            ),
            None => protocol.attach_ownership(self.plan.ownership.clone(), &route.provider.id),
        }
        protocol.attach_routing_log(self.failover.routing());
        protocol.request_headers(&self.parts.headers);
        if !routing::resource(&self.parts.method, self.parts.uri.path()) {
            protocol.business_request(self.gateway, &self.parts.headers);
        }
        protocol.request_speed(self.plan.hints.as_ref().ok().and_then(|h| h.speed.clone()));
        protocol.thread_identity(self.plan.thread_identity);
        if let Some(at) = self.parts.extensions.get::<ReceivedAt>() {
            protocol.request_received(at.0);
        }
        if let Some(delivery) = self
            .parts
            .extensions
            .get::<std::sync::Arc<super::protocol::Delivery>>()
        {
            protocol.set_delivery(delivery.clone());
        }
        if let Some(trace) = &self.plan.usage_trace {
            let mut attempt = trace.attempt(
                &route.provider.id,
                self.plan.stream_hint,
                if self.websocket { "websocket" } else { "http" },
            );
            if self.endless(&route) {
                attempt.omit_unbilled_rejections();
            }
            attempt.upstream_model(upstream_model.clone());
            protocol.attach_usage(attempt);
        }
        let decision = match &self.ordinary {
            Some(ordinary) => Some(match ordinary.kind {
                Delegated::Compaction => crate::usage::model::RoutingDecision::CompactionTakeover,
                Delegated::Auxiliary => crate::usage::model::RoutingDecision::AuxiliaryTakeover,
                Delegated::WebSearch => crate::usage::model::RoutingDecision::WebSearchTakeover,
            }),
            None => (self.gateway.client_id() == super::ClientId::Codex
                && self.mode == "auto"
                && (self.plan.responses
                    || (self.plan.owned_search && self.plan.ownership.is_some())))
            .then(|| {
                admission
                    .handoff_wait
                    .or(
                        (new_thread_model_denied && !route.provider.take_new_threads)
                            .then_some(crate::usage::model::RoutingDecision::NewThreadModelDenied),
                    )
                    .unwrap_or_else(|| {
                        super::compaction::decision(
                            self.plan.thread.as_deref(),
                            self.plan.hints.as_ref().ok(),
                            self.plan.continuation,
                            self.plan.ownership.as_deref(),
                            &route.provider.id,
                        )
                    })
            }),
        };
        protocol.context(self.plan.thread.clone(), decision);
        if self.gateway.client_id() == super::ClientId::Codex
            && self.mode == "auto"
            && (self.plan.responses || (self.plan.owned_search && self.plan.ownership.is_some()))
        {
            protocol.routing_evidence(self.plan.hints.as_ref().ok());
        }
        protocol.watch_lifecycle(
            self.gateway
                .request_state()
                .shutdown
                .as_ref()
                .map(watch::Sender::subscribe),
            None,
        );
        Attempt {
            admission,
            route,
            protocol,
            reset_generation,
            rerouted,
            started,
        }
    }
    /// The request never got a response head: the connection failed.
    async fn unreached(
        &mut self,
        attempt: Attempt,
        e: &(dyn std::error::Error + Send + Sync + 'static),
    ) -> End {
        let mut attempt = attempt;
        let category = connector::classify(e);
        // A failed TLS handshake is a failed connection: same retries, same
        // answer. Only its diagnostic code tells the two apart.
        let finished = match category {
            Some(connector::ConnectError::Tls) => code::TLS,
            _ => code::NETWORK,
        };
        let connection = !matches!(category, Some(connector::ConnectError::Loop));
        if self.endless(&attempt.route) && connection {
            attempt.protocol.finish(None, finished);
            let details = attempt.protocol.observation.error.clone();
            return self
                .claude_retry(attempt, None, code::NETWORK, details, None, true)
                .await;
        }
        let Attempt {
            mut admission,
            route,
            mut protocol,
            reset_generation,
            ..
        } = attempt;
        self.failover.failure(super::failover::Failure::Transport);
        self.last_category = code::NETWORK;
        // Use the same owner and budget as 5xx for connection failures,
        // including a peer closing before response headers arrive.
        // Header timeouts still use the separate no-replay boundary.
        let retry_same = connection
            && !self.plan.unknown_affinity
            && self.ordinary_attempts <= self.settings.max_retries
            && ((route.client_id == super::ClientId::Codex
                && (self.plan.pinned.as_deref() == Some(&route.provider.id)
                    || self
                        .plan
                        .ownership
                        .as_ref()
                        .is_some_and(|lease| lease.owns(&route.provider.id))))
                || self.plan.fixed_dsh.as_deref() == Some(&route.provider.id));
        protocol.finish(None, finished);
        let mut event = admission.permits.prepare_report(
            self.gateway,
            &route,
            self.plan.model.as_deref(),
            Reason::Network,
            self.after(super::attempt::retained_fallback(
                self.gateway,
                &route.provider.id,
                &self.ids,
                &self.routes,
                &self.plan.requirement,
                self.parts.uri.path(),
                self.plan.ownership.as_deref(),
                self.gateway.client_id() == super::ClientId::Codex && !self.plan.auxiliary,
                super::attempt::may_fail_over(
                    self.mode == "manual",
                    self.plan.pinned.as_deref(),
                    self.plan.unknown_affinity,
                    self.ordinary_attempts,
                    self.settings.max_retries,
                ),
            )),
            None,
            self.attempted,
            crate::events::Details {
                phase: Some(crate::events::Phase::Connect),
                local_code: Some(connector::diagnostic_code(e).into()),
                counted_failure: Some(true),
                ..Default::default()
            },
        );
        admission.permits.failure(self.settings, None);
        let retry_delay = super::attempt::same_provider_delay(
            retry_same,
            &route.health_for(&self.plan.requirement),
            &mut self.transient_retries,
            None,
        );
        // With nowhere else to go, a request whose network is down is held
        // rather than answered with the failure.
        let stranded = event.action == Action::Returned;
        if let Some(delay) = retry_delay {
            event.action = Action::RetryingSame;
            event.details.wait_seconds = Some(delay.as_secs());
        }
        self.last_error = Some(self.failover.report_failure(Some(&protocol), || {
            super::health_settlement::publish(self.gateway, event)
        }));
        let mut retry_delay = retry_delay;
        drop(admission);
        if connection && (retry_delay.is_some() || stranded) {
            match super::connectivity::settle(
                self.gateway,
                &route,
                self.settings,
                self.request_budget,
                self.plan.model.as_deref(),
                stranded,
            )
            .await
            {
                super::connectivity::Verdict::Reachable => {}
                super::connectivity::Verdict::Unreachable => retry_delay = None,
                super::connectivity::Verdict::Restored => {
                    self.ordinary_attempts = self.ordinary_attempts.saturating_sub(1);
                    self.transient_retries = 0;
                    retry_delay = Some(Duration::ZERO);
                }
            }
        }
        if let Some(delay) = retry_delay {
            self.failover
                .wait(super::failover::WaitKind::TransientRetry, delay.as_secs());
            match transient_wait(
                self.gateway,
                &route,
                reset_generation,
                delay,
                self.max_waiting(),
                self.plan.pinned.as_deref(),
                &self.plan.requirement,
            )
            .await
            {
                Ok(true) => {
                    self.ids.insert(0, route.provider.id.clone());
                    self.retry_owner = Some(route.provider.id.clone());
                }
                Ok(false) => {}
                Err(Rejected::Stopped) => {
                    self.failover.finish(super::failover::Exit::Stopped);
                    return End::reply(stopped());
                }
                Err(_) => return End::Stop,
            }
        }
        End::Again
    }
    /// No response head arrived in time.
    async fn headers_timeout(&mut self, mut attempt: Attempt) -> End {
        if self.endless(&attempt.route) {
            attempt
                .protocol
                .timeout("response_headers", self.first_byte_seconds());
            attempt.protocol.finish(None, code::FIRST_BYTE_TIMEOUT);
            let details = attempt.protocol.observation.error.clone();
            return self
                .claude_retry(
                    attempt,
                    None,
                    code::FIRST_BYTE_TIMEOUT,
                    details,
                    None,
                    false,
                )
                .await;
        }
        let Attempt {
            mut admission,
            route,
            mut protocol,
            ..
        } = attempt;
        self.failover.failure(super::failover::Failure::Transport);
        let retry_dsh = route.client_id == super::ClientId::Dsh
            && super::attempt::retained_fallback(
                self.gateway,
                &route.provider.id,
                &self.ids,
                &self.routes,
                &self.plan.requirement,
                self.parts.uri.path(),
                self.plan.ownership.as_deref(),
                false,
                super::attempt::may_fail_over(
                    self.mode == "manual",
                    self.plan.pinned.as_deref(),
                    self.plan.unknown_affinity,
                    self.ordinary_attempts,
                    self.settings.max_retries,
                ),
            ) == Action::TryingNext;
        protocol.timeout("response_headers", self.first_byte_seconds());
        protocol.finish(None, code::FIRST_BYTE_TIMEOUT);
        self.last_error = Some(self.failover.report_failure(Some(&protocol), || {
            admission.permits.report(
                self.gateway,
                &route,
                self.plan.model.as_deref(),
                Reason::Network,
                self.after(if retry_dsh {
                    Action::TryingNext
                } else {
                    Action::Returned
                }),
                None,
                self.attempted,
                crate::events::Details {
                    phase: Some(crate::events::Phase::Headers),
                    local_code: Some(code::FIRST_BYTE_TIMEOUT.into()),
                    counted_failure: Some(true),
                    ..protocol.observation.error.clone()
                },
            )
        }));
        admission.permits.failure(self.settings, None);
        if retry_dsh {
            self.last_category = code::FIRST_BYTE_TIMEOUT;
            return End::Again;
        }
        self.failover.finish(super::failover::Exit::ReplayUnsafe);
        // The HTTP client already owns the request. Not seeing headers
        // cannot establish that its body never reached the provider.
        End::reply(error(
            StatusCode::GATEWAY_TIMEOUT,
            code::FIRST_BYTE_TIMEOUT,
            "上游首字节超时，请求可能已开始处理，未自动重放",
        ))
    }
    /// The provider accepted a WebSocket upgrade: joins the two sockets.
    fn tunnel(mut self, attempt: Attempt, mut response: Response<Incoming>) -> Response<WireBody> {
        let Attempt {
            mut admission,
            route,
            mut protocol,
            rerouted,
            ..
        } = attempt;
        let status = response.status();
        let (active, traffic) = (self.active, self.traffic);
        self.failover.finish(super::failover::Exit::Forwarded);
        admission.permits.success(self.settings);
        let Admission { permits, slot, .. } = admission;
        let upstream_upgrade = hyper::upgrade::on(&mut response);
        let (mut response_parts, _) = response.into_parts();
        clean_headers(&mut response_parts.headers, true);
        let downstream = self.downstream_upgrade.expect("upgrade exists");
        let g = self.gateway.clone();
        let cfg = self.settings.clone();
        let provider_for_tunnel = route.provider.clone();
        tokio::spawn(async move {
            let _active = active;
            let _traffic = traffic;
            let _slot = slot;
            let mut permits = permits;
            let connected = tokio::try_join!(upstream_upgrade, downstream);
            if let Ok((a, b)) = connected {
                // Opaque tunnel preserves every data/control/close frame, including binary payloads.
                let result =
                    tokio::io::copy_bidirectional(&mut TokioIo::new(a), &mut TokioIo::new(b)).await;
                protocol.finish(
                    Some(101),
                    if result.is_ok() {
                        code::OK
                    } else {
                        code::STREAM_INTERRUPTED
                    },
                );
                if protocol.succeeded() {
                    g.persist_success(&provider_for_tunnel).await;
                }
                permits.neutral(&cfg);
            }
        });
        if rerouted {
            self.gateway.record(
                Some(&route.provider.id),
                self.plan.model.as_deref(),
                Reason::Failover,
                Action::Routed,
                Some(status.as_u16()),
                Some(self.attempted.min(u32::MAX as usize) as u32),
            );
        }
        Response::from_parts(response_parts, replay::empty())
    }
    /// The provider answered with an error status.
    async fn rejected(&mut self, attempt: Attempt, response: Response<Incoming>) -> End {
        let Attempt {
            mut admission,
            route,
            mut protocol,
            reset_generation,
            ..
        } = attempt;
        let status = response.status();
        let cooldown = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|h| h.to_str().ok())
            .and_then(circuit::retry_after);
        let (mut response_parts, body) = response.into_parts();
        clean_headers(&mut response_parts.headers, false);
        let captured = Replay::capture_observed(
            body,
            self.gateway.0.spool.path(),
            Some(Duration::from_secs(self.settings.idle_seconds)),
            |bytes| protocol.feed(bytes),
        )
        .await;
        if captured
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<std::io::Error>())
            .is_some_and(|e| e.kind() == std::io::ErrorKind::TimedOut)
        {
            protocol.timeout("error_body_idle", self.settings.idle_seconds);
        }
        let decoded = match &captured {
            Ok(body) => {
                let encoded = body.prefix(2 * 1024 * 1024).await.unwrap_or_default();
                protocol.finish(Some(status.as_u16()), code::HTTP);
                let encoding = response_parts
                    .headers
                    .get(header::CONTENT_ENCODING)
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("identity");
                replay::decode_prefix(&encoded, encoding, 128 * 1024).unwrap_or_default()
            }
            _ => {
                protocol.finish(Some(status.as_u16()), code::STREAM_INTERRUPTED);
                Vec::new()
            }
        };
        let (classified, server_retry) = super::failure_policy::http_with_rules(
            status.as_u16(),
            &response_parts.headers,
            &decoded,
            &self.settings.custom_failure_rules,
        );
        let mut cooldown = cooldown
            .max(server_retry)
            .max(protocol.observation.retry_after);
        let compact_error = self.plan.ownership.as_ref().is_some_and(|lease| {
            lease.handoff && lease.owner.as_deref() != Some(&route.provider.id)
        }) && protocol.observation.compaction_incompatible;
        if compact_error {
            if let Some(owner) = self
                .plan
                .ownership
                .as_ref()
                .and_then(|lease| lease.owner.as_ref())
                .filter(|owner| self.ids.contains(owner))
            {
                self.retry_owner = Some(owner.clone());
            }
        }
        let model_error = protocol.observation.terminal
            == Some(super::protocol::Terminal::ModelUnavailable)
            || super::failure_policy::model_http(status.as_u16(), &decoded);
        let mut details = if protocol.observation.error.raw.is_some() {
            protocol.observation.error.clone()
        } else {
            classified
        };
        if details == crate::events::Details::default() {
            details = protocol.observation.error.clone();
        }
        if route.client_id == super::ClientId::Dsh
            && !model_error
            && matches!(status.as_u16(), 401..=403)
        {
            details.health_scope = Some(crate::events::HealthScope::Provider);
        }
        let policy = super::failure_policy::decision(Some(status.as_u16()), &details);
        // A model this provider does not have is not waited out, whatever the
        // status: the request goes on to the next provider at once.
        let moved = model_error && self.unpin(true);
        if !moved && self.endless(&route) {
            let reply = match captured {
                Ok(body) => Response::from_parts(response_parts, body.body()),
                Err(_) => error(status, code::UPSTREAM_BODY, "上游错误响应未完整接收"),
            };
            let code = status.as_u16();
            let transient = !policy.hard
                && policy.retryable
                && (policy.temporary
                    || matches!(code, 408 | 425 | 429)
                    || circuit::is_server_error(code));
            // A day, week or month's quota comes back on its own, so endless
            // mode waits for it; a balance, a key or an account does not.
            let window = details.error_basis == Some(crate::events::ErrorBasis::QuotaExhausted);
            // A listed status is retried whatever the body says.
            let listed = route.risk_codes().contains(&code);
            if !model_error && !compact_error && (transient || window || listed) {
                self.last = Some(reply);
                return self
                    .claude_retry(
                        Attempt {
                            admission,
                            route,
                            protocol,
                            reset_generation,
                            rerouted: false,
                            started: Instant::now(),
                        },
                        Some(status.as_u16()),
                        code::HTTP,
                        details,
                        cooldown,
                        false,
                    )
                    .await;
            }
            let event = admission.permits.prepare_report(
                self.gateway,
                &route,
                self.plan.model.as_deref(),
                policy.reason,
                self.after(Action::Returned),
                Some(status.as_u16()),
                self.attempted,
                details.clone(),
            );
            admission
                .permits
                .policy(self.settings, Some(status.as_u16()), &details, cooldown);
            self.failover.report_failure(Some(&protocol), || {
                super::health_settlement::publish(self.gateway, event)
            });
            return End::reply(reply);
        }
        let hard_rejection = policy.hard;
        if hard_rejection {
            self.hard_denied
                .insert((route.provider.id.clone(), route.provider.version.clone()));
        }
        let capacity = !model_error && !hard_rejection && policy.temporary;
        let wait_same =
            capacity && self.plan.established_capacity_owner.as_deref() == Some(&route.provider.id);
        if capacity && !self.websocket {
            self.ordinary_attempts = self.ordinary_attempts.saturating_sub(1);
            self.capacity_protected = true;
            self.wait_budget.protect_capacity();
        }
        let retryable = compact_error || model_error || hard_rejection || policy.retryable;
        let retry_same = circuit::is_server_error(status.as_u16())
            && !compact_error
            && !model_error
            && !capacity
            && !hard_rejection
            && !self.plan.unknown_affinity
            && self.ordinary_attempts <= self.settings.max_retries
            && ((route.client_id == super::ClientId::Codex
                && (self.plan.pinned.as_deref() == Some(&route.provider.id)
                    || self
                        .plan
                        .ownership
                        .as_ref()
                        .is_some_and(|lease| lease.owns(&route.provider.id))))
                || self.plan.fixed_dsh.as_deref() == Some(&route.provider.id));
        self.failover.failure(if hard_rejection {
            super::failover::Failure::AccountRejected
        } else if model_error {
            super::failover::Failure::ModelUnavailable
        } else if status == StatusCode::TOO_MANY_REQUESTS || capacity {
            super::failover::Failure::RateLimit
        } else if compact_error {
            super::failover::Failure::Protocol
        } else {
            super::failover::Failure::UpstreamService
        });
        let will_retry = retryable
            && (super::attempt::retained_fallback(
                self.gateway,
                &route.provider.id,
                &self.ids,
                &self.routes,
                &self.plan.requirement,
                self.parts.uri.path(),
                self.plan.ownership.as_deref(),
                self.gateway.client_id() == super::ClientId::Codex && !self.plan.auxiliary,
                super::attempt::may_fail_over(
                    self.mode == "manual",
                    self.plan.pinned.as_deref(),
                    self.plan.unknown_affinity,
                    self.ordinary_attempts,
                    self.settings.max_retries,
                ),
            ) == Action::TryingNext);
        let reason = if compact_error {
            Reason::ProtocolError
        } else {
            policy.reason
        };
        details.phase = Some(crate::events::Phase::Response);
        details.counted_failure = Some(!compact_error && policy.counted_failure);
        details.wait_seconds =
            cooldown.map(|d| d.as_secs().saturating_add(u64::from(d.subsec_nanos() > 0)));
        protocol.finish(
            Some(status.as_u16()),
            if captured.is_ok() {
                code::HTTP
            } else {
                code::STREAM_INTERRUPTED
            },
        );
        let mut event = admission.permits.prepare_report(
            self.gateway,
            &route,
            self.plan.model.as_deref(),
            reason,
            self.after(if wait_same {
                Action::RetryingSame
            } else if will_retry {
                Action::TryingNext
            } else if capacity {
                Action::Waiting
            } else {
                Action::Returned
            }),
            Some(status.as_u16()),
            self.attempted,
            details,
        );
        if model_error || compact_error {
            admission.permits.neutral(self.settings);
        } else if capacity {
            admission.permits.capacity_limited(self.settings, cooldown);
        } else if hard_rejection {
            admission.permits.failure(self.settings, cooldown);
        } else if status == StatusCode::TOO_MANY_REQUESTS {
            admission.permits.rate_limited(self.settings, cooldown);
        } else if retryable {
            admission.permits.failure(self.settings, cooldown);
        } else {
            admission.permits.neutral(self.settings);
        }
        let retry_delay = super::attempt::same_provider_delay(
            retry_same,
            &route.health_for(&self.plan.requirement),
            &mut self.transient_retries,
            cooldown,
        );
        if let Some(delay) = retry_delay {
            event.action = Action::RetryingSame;
            event.details.wait_seconds = Some(delay.as_secs());
        }
        self.last_error = Some(self.failover.report_failure(Some(&protocol), || {
            super::health_settlement::publish(self.gateway, event)
        }));
        if capacity {
            cooldown = cooldown.max(Some(Duration::from_secs(
                route.health_for(&self.plan.requirement).retry_in,
            )));
        }
        if !retryable {
            self.ids.clear();
        }
        if let Ok(body) = captured {
            self.last = Some(Response::from_parts(response_parts, body.body()));
        }
        self.last_category = code::HTTP;
        if !retryable {
            self.failover
                .finish(super::failover::Exit::PermanentRejection);
            return End::reply(self.last.take().unwrap_or_else(|| {
                error(
                    StatusCode::BAD_GATEWAY,
                    code::UPSTREAM_BODY,
                    "上游错误响应读取失败",
                )
            }));
        }
        if let Some(delay) = retry_delay {
            self.failover
                .wait(super::failover::WaitKind::TransientRetry, delay.as_secs());
            // Release the failed attempt before registering a bounded wait.
            // Keep the original spool, model, and response cursor untouched.
            drop(admission);
            self.request_budget
                .describe(None, self.plan.model.as_deref(), "retry");
            match transient_wait(
                self.gateway,
                &route,
                reset_generation,
                delay,
                self.max_waiting(),
                self.plan.pinned.as_deref(),
                &self.plan.requirement,
            )
            .await
            {
                Ok(true) => {
                    self.ids.insert(0, route.provider.id.clone());
                    self.retry_owner = Some(route.provider.id.clone());
                }
                Ok(false) => {}
                Err(Rejected::Stopped) => {
                    self.failover.finish(super::failover::Exit::Stopped);
                    return End::reply(stopped());
                }
                Err(_) => return End::Stop,
            }
        }
        if capacity {
            if wait_same {
                self.capacity_wait_owner = Some(route.provider.id.clone());
                self.ids.clear();
            }
            self.capacity_pending = true;
            self.capacity_sources.push(CapacitySource {
                provider_id: route.provider.id.clone(),
                reset_generation,
            });
            self.capacity_retry_after = self.capacity_retry_after.max(cooldown);
        }
        End::Again
    }
    /// Reads the start of an accepted response until it shows output, a
    /// terminal outcome or a failure.
    async fn opening(
        &mut self,
        attempt: &mut Attempt,
        response: Response<Incoming>,
        stream: bool,
    ) -> Opening {
        if attempt.route.provider.buffer_complete_response
            && is_messages_path(self.parts.uri.path())
        {
            return self.buffered_opening(attempt, response).await;
        }
        let status = response.status();
        let protocol = &mut attempt.protocol;
        let started = attempt.started;
        let (mut response_parts, body) = response.into_parts();
        let mut body = body
            .map_err(|error| Box::new(error) as BoxError)
            .boxed_unsync();
        clean_headers(&mut response_parts.headers, false);
        let mut first_deadline = tokio::time::Instant::now()
            + Duration::from_secs(self.settings.first_byte_seconds)
                .saturating_sub(started.elapsed());
        // Buffer through initialization and heartbeats until effective output or a
        // terminal outcome (or bounded JSON); preserve
        // the original frames and encoding exactly, including comments/trailers.
        let json = response_parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("json"));
        let mut preinspected = false;
        let mut inspected_bytes = 0u64;
        let mut prefix = Vec::new();
        let mut prefix_bytes = 0;
        let mut ended = false;
        let mut failed = false;
        let mut failure_kind = code::FIRST_BYTE_TIMEOUT;
        let mut quiet = None;
        loop {
            match self
                .request_budget
                .response(first_deadline, self.started_silence(), body.frame())
                .await
            {
                Ok(Some(Ok(frame))) => {
                    if let Some(bytes) = frame.data_ref() {
                        prefix_bytes += bytes.len();
                        protocol.feed(bytes);
                        if !stream
                            && !json
                            && !bytes.is_empty()
                            && !protocol.possible_bare_capacity()
                        {
                            protocol.effective_output();
                        }
                        if !bytes.is_empty() {
                            first_deadline = tokio::time::Instant::now()
                                + Duration::from_secs(self.settings.idle_seconds);
                        }
                    }
                    prefix.push((
                        frame,
                        protocol.terminal().is_some(),
                        protocol.business_observed(),
                    ));
                    if body.is_end_stream() {
                        ended = true;
                    }
                    if !ended
                        && (prefix_bytes >= 2 * 1024 * 1024 || prefix.len() >= 4096)
                        && !protocol.search_content_observed()
                        && protocol.terminal().is_none()
                    {
                        let frames = std::mem::take(&mut prefix);
                        let mut observed = prefix_bytes;
                        let captured = {
                            let upstream = &mut body;
                            let remainder = async_stream::try_stream! {
                                for (frame, _, _) in frames { yield frame; }
                                while let Some(frame) = upstream.frame().await { yield frame.map_err(std::io::Error::other)?; }
                            };
                            let mut remainder = StreamBody::new(Box::pin(remainder))
                                .map_err(|error: std::io::Error| error);
                            Replay::capture_until(
                                &mut remainder,
                                self.gateway.0.spool.path(),
                                Some(Duration::from_secs(self.settings.idle_seconds)),
                                |bytes| {
                                    if observed > 0 {
                                        observed = observed.saturating_sub(bytes.len());
                                    } else {
                                        protocol.feed(bytes);
                                    }
                                    stream
                                        && (protocol.search_content_observed()
                                            || protocol.terminal().is_some())
                                },
                            )
                            .await
                        };
                        ended = body.is_end_stream();
                        match captured {
                            Ok(spooled) => {
                                inspected_bytes = spooled.length;
                                let mut buffered = spooled.body();
                                let mut upstream = body;
                                body = StreamBody::new(async_stream::try_stream! {
                                    while let Some(frame) = buffered.frame().await { yield frame?; }
                                    while let Some(frame) = upstream.frame().await { yield frame?; }
                                })
                                .boxed_unsync();
                                preinspected = true;
                            }
                            Err(_) => {
                                failed = true;
                                failure_kind = code::UPSTREAM_BODY;
                            }
                        }
                        break;
                    }
                    if ended
                        || (!stream && !json && !protocol.possible_bare_capacity())
                        || (stream
                            && (protocol.search_content_observed()
                                || protocol.terminal().is_some()))
                    {
                        break;
                    }
                }
                Ok(None) => {
                    ended = true;
                    break;
                }
                result => {
                    failed = true;
                    quiet = result.as_ref().err().copied();
                    failure_kind = if result.is_err() && prefix_bytes != 0 {
                        code::STREAM_TIMEOUT
                    } else if result.is_err() {
                        code::FIRST_BYTE_TIMEOUT
                    } else {
                        code::STREAM_INTERRUPTED
                    };
                    break;
                }
            }
        }
        if failed {
            if quiet == Some(Quiet::Held) {
                protocol.timeout("hold_idle", self.settings.hold_idle_seconds);
            } else if failure_kind.ends_with("TIMEOUT") {
                protocol.timeout(
                    if prefix_bytes == 0 {
                        "first_byte"
                    } else {
                        "first_event_idle"
                    },
                    if prefix_bytes == 0 {
                        self.first_byte_seconds()
                    } else {
                        self.settings.idle_seconds
                    },
                );
            }
            protocol.finish(Some(status.as_u16()), failure_kind);
        }
        Opening {
            status,
            parts: response_parts,
            body,
            prefix,
            prefix_bytes,
            ended,
            failed,
            failure_kind,
            preinspected,
            inspected_bytes,
            probe: quiet == Some(Quiet::Held),
        }
    }
    async fn buffered_opening(
        &mut self,
        attempt: &mut Attempt,
        response: Response<Incoming>,
    ) -> Opening {
        let status = response.status();
        let end_at_stop = ends_at_message_stop(&attempt.route, status, response.headers());
        let (mut parts, mut upstream) = response.into_parts();
        clean_headers(&mut parts.headers, false);
        let mut deadline = tokio::time::Instant::now()
            + Duration::from_secs(self.settings.first_byte_seconds)
                .saturating_sub(attempt.started.elapsed());
        let mut failure_kind = code::STREAM_INTERRUPTED;
        let mut received = false;
        let mut quiet = None;
        let output = std::sync::atomic::AtomicBool::new(false);
        // The model is writing: a Claude stream is then held to the stall limit.
        let flowing = std::sync::atomic::AtomicBool::new(false);
        let stall = (attempt.route.client_id == super::ClientId::Claude)
            .then(|| Duration::from_secs(self.settings.stall_seconds));
        let first_byte_seconds = self.first_byte_seconds();
        let protocol = &mut attempt.protocol;
        let captured = {
            let failure_kind = &mut failure_kind;
            let received = &mut received;
            let quiet = &mut quiet;
            let request_budget = self.request_budget;
            let compaction = self.plan.operation == crate::usage::model::Operation::Compaction;
            let idle_seconds = self.settings.idle_seconds;
            let first_silence = self.started_silence();
            let (output, flowing) = (&output, &flowing);
            let frames = async_stream::try_stream! {
                loop {
                    let silence = match (first_silence, stall) {
                        (Silence::Compaction(_), _) if output.load(std::sync::atomic::Ordering::Relaxed) => Silence::Compaction(None),
                        (Silence::Started, Some(stall)) if flowing.load(std::sync::atomic::Ordering::Relaxed) => Silence::Flowing(stall),
                        (silence, _) => silence,
                    };
                    match request_budget.response(deadline, silence, upstream.frame()).await {
                        Ok(Some(Ok(frame))) => {
                            if frame.data_ref().is_some_and(|data| !data.is_empty()) {
                                *received = true;
                                deadline = tokio::time::Instant::now() + Duration::from_secs(idle_seconds);
                            }
                            yield frame;
                        }
                        Ok(None) => break,
                        Ok(Some(Err(error))) => Err(std::io::Error::other(error))?,
                        Err(cause) => {
                            *quiet = Some(cause);
                            // Keep-alives are not a compaction's output.
                            *failure_kind = if *received && !matches!(silence, Silence::Compaction(Some(_))) { code::STREAM_TIMEOUT } else { code::FIRST_BYTE_TIMEOUT };
                            Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "上游响应超时"))?;
                        }
                    }
                }
            };
            let mut body = StreamBody::new(Box::pin(frames)).map_err(|error: std::io::Error| error);
            let mut streaming = false;
            Replay::capture_until(&mut body, self.gateway.0.spool.path(), None, |data| {
                protocol.feed(data);
                if protocol.output_started() {
                    flowing.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                if !streaming
                    && !protocol.explicit_failure()
                    && (protocol.observation.meter.first_token_ms.is_some()
                        || protocol.business_observed())
                {
                    streaming = true;
                    output.store(true, std::sync::atomic::Ordering::Relaxed);
                    self.traffic.streaming();
                    if !compaction {
                        self.request_budget.buffering();
                    }
                }
                end_at_stop && protocol.message_stopped()
            })
            .await
        };
        if captured.is_err() && failure_kind.ends_with("TIMEOUT") {
            let idle = failure_kind == code::STREAM_TIMEOUT;
            let (phase, seconds) = match quiet {
                Some(Quiet::Stalled) => ("stream_stall", self.settings.stall_seconds),
                Some(Quiet::Held) => ("hold_idle", self.settings.hold_idle_seconds),
                _ if idle => ("buffer_idle", self.settings.idle_seconds),
                _ => ("first_byte", first_byte_seconds),
            };
            protocol.timeout(phase, seconds);
        }
        let complete = protocol.finish_buffer(
            status.as_u16(),
            captured.as_ref().err().map(|_| failure_kind),
            end_at_stop,
        );
        let (body, inspected_bytes) = captured.map_or_else(
            |_| (replay::empty(), 0),
            |replay| (replay.body(), replay.length),
        );
        Opening {
            status,
            parts,
            body,
            prefix: Vec::new(),
            prefix_bytes: 0,
            ended: true,
            failed: !complete,
            failure_kind,
            preinspected: true,
            inspected_bytes,
            probe: matches!(quiet, Some(Quiet::Stalled | Quiet::Held)),
        }
    }
    /// The response broke off before any output, where a replay is still safe.
    async fn interrupted(
        &mut self,
        attempt: Attempt,
        status: StatusCode,
        failure_kind: &'static str,
        probe: bool,
    ) -> End {
        if attempt.route.provider.buffer_complete_response || self.endless(&attempt.route) {
            let details = attempt.protocol.observation.error.clone();
            return self
                .claude_retry(
                    attempt,
                    Some(status.as_u16()),
                    failure_kind,
                    details,
                    None,
                    probe,
                )
                .await;
        }
        let Attempt {
            mut admission,
            route,
            protocol,
            reset_generation,
            ..
        } = attempt;
        self.failover.failure(super::failover::Failure::Transport);
        let fallback = super::attempt::retained_fallback(
            self.gateway,
            &route.provider.id,
            &self.ids,
            &self.routes,
            &self.plan.requirement,
            self.parts.uri.path(),
            self.plan.ownership.as_deref(),
            self.gateway.client_id() == super::ClientId::Codex && !self.plan.auxiliary,
            super::attempt::may_fail_over(
                self.mode == "manual",
                self.plan.pinned.as_deref(),
                self.plan.unknown_affinity,
                self.ordinary_attempts,
                self.settings.max_retries,
            ),
        );
        let mut event = admission.permits.prepare_report(
            self.gateway,
            &route,
            self.plan.model.as_deref(),
            Reason::Network,
            self.after(fallback),
            Some(status.as_u16()),
            self.attempted,
            crate::events::Details {
                phase: Some(crate::events::Phase::Response),
                local_code: Some(failure_kind.into()),
                counted_failure: Some(true),
                ..protocol.observation.error.clone()
            },
        );
        admission.permits.failure(self.settings, None);
        let retry_delay = if failure_kind == code::STREAM_INTERRUPTED
            && self.plan.fixed_dsh.as_deref() == Some(&route.provider.id)
            && !self.plan.unknown_affinity
            && self.ordinary_attempts <= self.settings.max_retries
            && route.health_for(&self.plan.requirement).available
        {
            routing::transient_delay(&mut self.transient_retries, None)
        } else {
            None
        };
        if let Some(delay) = retry_delay {
            event.action = Action::RetryingSame;
            event.details.wait_seconds = Some(delay.as_secs());
        }
        self.last_error = Some(self.failover.report_failure(Some(&protocol), || {
            super::health_settlement::publish(self.gateway, event)
        }));
        self.last_category = failure_kind;
        if let Some(delay) = retry_delay {
            self.failover
                .wait(super::failover::WaitKind::TransientRetry, delay.as_secs());
            drop(admission);
            match transient_wait(
                self.gateway,
                &route,
                reset_generation,
                delay,
                self.max_waiting(),
                self.plan.pinned.as_deref(),
                &self.plan.requirement,
            )
            .await
            {
                Ok(true) => {
                    self.ids.insert(0, route.provider.id.clone());
                    self.retry_owner = Some(route.provider.id.clone());
                }
                Ok(false) => {}
                Err(Rejected::Stopped) => {
                    self.failover.finish(super::failover::Exit::Stopped);
                    return End::reply(stopped());
                }
                Err(_) => return End::Stop,
            }
        } else if route.client_id == super::ClientId::Dsh && fallback != Action::TryingNext {
            self.failover.finish(super::failover::Exit::ReplayUnsafe);
            return End::reply(error(
                if failure_kind.ends_with("TIMEOUT") {
                    StatusCode::GATEWAY_TIMEOUT
                } else {
                    StatusCode::BAD_GATEWAY
                },
                failure_kind,
                if failure_kind.ends_with("TIMEOUT") {
                    "上游响应超时"
                } else {
                    "上游响应流中断"
                },
            ));
        }
        End::Again
    }
    /// Settles a response whose start is itself a rejection; any other
    /// response goes on to the client.
    async fn initial_rejection(
        &mut self,
        attempt: Attempt,
        opening: &mut Opening,
    ) -> Result<Attempt, End> {
        let moved = attempt.protocol.terminal()
            == Some(super::protocol::Terminal::ModelUnavailable)
            && !attempt.protocol.observation.meter.content_seen
            && !attempt.protocol.observation.search_content
            && !attempt.protocol.observation.opaque_output
            && self.unpin(true);
        if !moved
            && (self.endless(&attempt.route) || attempt.route.provider.buffer_complete_response)
            && attempt.protocol.explicit_failure()
        {
            let mut attempt = attempt;
            let details = attempt.protocol.observation.error.clone();
            let model_unavailable =
                attempt.protocol.terminal() == Some(super::protocol::Terminal::ModelUnavailable);
            let policy = super::failure_policy::decision(Some(opening.status.as_u16()), &details);
            // A listed code carried inside the response is retried whatever
            // else the rejection says, like a listed status.
            let endless = self.endless(&attempt.route);
            let listed = endless
                && super::model::retry_class(
                    attempt.route.risk_codes(),
                    Some(opening.status.as_u16()),
                    &details,
                ) == RetryClass::Risk;
            // Endless mode also waits for a quota that comes back on its own.
            let window =
                endless && details.error_basis == Some(crate::events::ErrorBasis::QuotaExhausted);
            if !model_unavailable
                && (listed
                    || window
                    || (!policy.hard
                        && (policy.temporary
                            || details.error_basis
                                == Some(crate::events::ErrorBasis::ServiceError))))
            {
                let retry = attempt.protocol.observation.retry_after;
                return Err(self
                    .claude_retry(
                        attempt,
                        Some(opening.status.as_u16()),
                        code::UPSTREAM_REJECTED,
                        details,
                        retry,
                        false,
                    )
                    .await);
            }
            observe_protocol(
                &attempt.protocol,
                &mut attempt.admission.permits,
                self.settings,
                self.gateway,
                &attempt.route,
                self.plan.model.as_deref(),
                Some(opening.status.as_u16()),
                self.attempted,
            );
            // Answered here: the general path below would rebuild the queue
            // and leave the provider endless mode fixed.
            return Err(End::reply(
                if attempt.route.client_id == super::ClientId::Claude {
                    claude_rejection_response(
                        Some(opening.status.as_u16()),
                        code::UPSTREAM_REJECTED,
                        &details,
                        model_unavailable,
                    )
                } else {
                    error(
                        StatusCode::BAD_GATEWAY,
                        code::UPSTREAM_REJECTED,
                        "上游业务请求失败，已结束本次重试",
                    )
                },
            ));
        }
        let Attempt {
            mut admission,
            route,
            mut protocol,
            reset_generation,
            rerouted,
            started,
        } = attempt;
        let (status, failed) = (opening.status, opening.failed);
        opening.ended |= failed;
        if opening.ended {
            protocol.finish(
                Some(status.as_u16()),
                if status.is_success() {
                    code::OK
                } else {
                    code::HTTP
                },
            );
            if protocol.terminal_observed() {
                if let Some((_, terminal, business)) = opening.prefix.last_mut() {
                    *terminal = true;
                    *business = true;
                }
            }
        }
        let before_output = !protocol.observation.meter.content_seen
            && !protocol.observation.search_content
            && !protocol.observation.opaque_output;
        let initial_model_error = before_output
            && protocol.terminal() == Some(super::protocol::Terminal::ModelUnavailable);
        let initial_compact_error = protocol.observation.compaction_incompatible
            && self.plan.ownership.as_ref().is_some_and(|lease| {
                lease.handoff && lease.owner.as_deref() != Some(&route.provider.id)
            });
        if initial_compact_error {
            if let Some(owner) = self
                .plan
                .ownership
                .as_ref()
                .and_then(|lease| lease.owner.as_ref())
                .filter(|owner| self.ids.contains(owner))
            {
                self.retry_owner = Some(owner.clone());
            }
        }
        let policy =
            super::failure_policy::decision(Some(status.as_u16()), &protocol.observation.error);
        let initial_hard_rejection = before_output && policy.hard;
        if initial_hard_rejection {
            self.hard_denied
                .insert((route.provider.id.clone(), route.provider.version.clone()));
        }
        let initial_capacity = before_output
            && !initial_hard_rejection
            && !self.websocket
            && !initial_model_error
            && policy.temporary;
        // This whole prefix is still private to the gateway. A body frame
        // can contain both generated deltas and a terminal service error;
        // observing those deltas is not proof that the client received them.
        let initial_service_error = (before_output || protocol.explicit_failure())
            && protocol.observation.error.error_basis
                == Some(crate::events::ErrorBasis::ServiceError);
        let initial_other_error = before_output && !failed && protocol.explicit_failure();
        let wait_same = initial_capacity
            && self.plan.established_capacity_owner.as_deref() == Some(&route.provider.id);
        if initial_model_error
            || initial_capacity
            || initial_compact_error
            || initial_hard_rejection
            || initial_service_error
            || initial_other_error
        {
            self.failover.failure(if initial_hard_rejection {
                super::failover::Failure::AccountRejected
            } else if initial_capacity {
                super::failover::Failure::RateLimit
            } else if initial_model_error {
                super::failover::Failure::ModelUnavailable
            } else if initial_service_error {
                super::failover::Failure::UpstreamService
            } else {
                super::failover::Failure::Protocol
            });
            let mut cooldown = opening
                .parts
                .headers
                .get(header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok())
                .and_then(circuit::retry_after)
                .max(protocol.observation.retry_after);
            if initial_capacity {
                if wait_same {
                    self.capacity_wait_owner = Some(route.provider.id.clone());
                    self.ids.clear();
                }
                self.ordinary_attempts = self.ordinary_attempts.saturating_sub(1);
                self.capacity_protected = true;
                self.wait_budget.protect_capacity();

                self.capacity_pending = true;
                self.capacity_sources.push(CapacitySource {
                    provider_id: route.provider.id.clone(),
                    reset_generation,
                });
                self.capacity_retry_after = self.capacity_retry_after.max(cooldown);
            }
            protocol.finish(Some(status.as_u16()), code::HTTP);
            let retry_same = initial_service_error
                && !self.plan.unknown_affinity
                && self.ordinary_attempts <= self.settings.max_retries
                && (self.plan.pinned.as_deref() == Some(&route.provider.id)
                    || self
                        .plan
                        .ownership
                        .as_ref()
                        .is_some_and(|lease| lease.owns(&route.provider.id))
                    || self.plan.fixed_dsh.as_deref() == Some(&route.provider.id));
            let will_retry = policy.retryable
                && (super::attempt::retained_fallback(
                    self.gateway,
                    &route.provider.id,
                    &self.ids,
                    &self.routes,
                    &self.plan.requirement,
                    self.parts.uri.path(),
                    self.plan.ownership.as_deref(),
                    self.gateway.client_id() == super::ClientId::Codex && !self.plan.auxiliary,
                    super::attempt::may_fail_over(
                        self.mode == "manual",
                        self.plan.pinned.as_deref(),
                        self.plan.unknown_affinity,
                        self.ordinary_attempts,
                        self.settings.max_retries,
                    ),
                ) == Action::TryingNext);
            let mut details = protocol.observation.error.clone();
            details.phase = Some(crate::events::Phase::Response);
            details.counted_failure = Some(!initial_compact_error && policy.counted_failure);
            details.wait_seconds =
                cooldown.map(|d| d.as_secs().saturating_add(u64::from(d.subsec_nanos() > 0)));
            let mut event = admission.permits.prepare_report(
                self.gateway,
                &route,
                self.plan.model.as_deref(),
                if initial_compact_error {
                    Reason::ProtocolError
                } else {
                    policy.reason
                },
                self.after(if wait_same {
                    Action::RetryingSame
                } else if will_retry {
                    Action::TryingNext
                } else if initial_capacity {
                    Action::Waiting
                } else {
                    Action::Returned
                }),
                Some(status.as_u16()),
                self.attempted,
                details,
            );
            if initial_capacity {
                admission.permits.capacity_limited(self.settings, cooldown);
            } else if initial_hard_rejection || initial_service_error {
                admission.permits.failure(self.settings, cooldown);
            } else {
                admission.permits.neutral(self.settings);
            }
            let retry_delay = super::attempt::same_provider_delay(
                retry_same,
                &route.health_for(&self.plan.requirement),
                &mut self.transient_retries,
                cooldown,
            );
            if let Some(delay) = retry_delay {
                event.action = Action::RetryingSame;
                event.details.wait_seconds = Some(delay.as_secs());
            }
            self.last_error = Some(self.failover.report_failure(Some(&protocol), || {
                super::health_settlement::publish(self.gateway, event)
            }));
            if initial_capacity {
                cooldown = cooldown.max(Some(Duration::from_secs(
                    route.health_for(&self.plan.requirement).retry_in,
                )));
                self.capacity_retry_after = self.capacity_retry_after.max(cooldown);
            }
            // Rejected initialization frames are never a successful client response.
            // The upstream payload remains available in the error log.
            self.last = Some(error(
                StatusCode::BAD_GATEWAY,
                code::UPSTREAM_REJECTED,
                "上游业务请求失败，已结束本次重试",
            ));
            if !policy.retryable && !initial_compact_error {
                self.failover
                    .finish(super::failover::Exit::PermanentRejection);
                return Err(End::reply(self.last.take().unwrap()));
            }
            if let Some(delay) = retry_delay {
                self.failover
                    .wait(super::failover::WaitKind::TransientRetry, delay.as_secs());
                // Release the failed attempt before registering a bounded wait.
                // Keep the original spool, model, and response cursor untouched.
                drop(admission);
                self.request_budget
                    .describe(None, self.plan.model.as_deref(), "retry");
                match transient_wait(
                    self.gateway,
                    &route,
                    reset_generation,
                    delay,
                    self.max_waiting(),
                    self.plan.pinned.as_deref(),
                    &self.plan.requirement,
                )
                .await
                {
                    Ok(true) => {
                        self.ids.insert(0, route.provider.id.clone());
                        self.retry_owner = Some(route.provider.id.clone());
                    }
                    Ok(false) => {}
                    Err(Rejected::Stopped) => {
                        self.failover.finish(super::failover::Exit::Stopped);
                        return Err(End::reply(stopped()));
                    }
                    Err(_) => return Err(End::Stop),
                }
            } else if !will_retry && !initial_capacity {
                self.ids.clear();
            }
            self.last_category = if initial_hard_rejection || initial_service_error {
                code::UPSTREAM_REJECTED
            } else if initial_capacity {
                code::CAPACITY
            } else if initial_model_error {
                code::MODEL_UNAVAILABLE
            } else {
                code::UPSTREAM_REJECTED
            };
            return Err(End::Again);
        }
        Ok(Attempt {
            admission,
            route,
            protocol,
            reset_generation,
            rerouted,
            started,
        })
    }
    /// One round of the request: admission to a provider, the upstream
    /// request, and what its response head means for the request.
    async fn round(
        &mut self,
        stream_started: &mut Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<Accepted, End> {
        self.traffic.waiting();
        // An unknown previous_response_id has no safe owner to replay against;
        // preserve the existing one-attempt rule instead of silently
        // repeating the request on an arbitrary provider.
        if self.claude_owner.is_none()
            && ((self.plan.unknown_affinity && !self.capacity_protected)
                || (self.gateway.client_id() == super::ClientId::Claude && self.plan.continuation))
            && self.attempted > 0
        {
            return Err(End::Stop);
        }
        if self.ids.is_empty() {
            if self.ordinary.is_some() {
                // The provider that took the request over has had its turn.
                return Err(End::Stop);
            }
            return Err(self.await_capacity().await);
        }
        let (candidates, new_thread_model_denied) = self.candidates().await?;
        if self.parts.method == hyper::Method::POST
            && is_count_tokens_path(self.parts.uri.path())
            && !candidates.is_empty()
            && candidates.iter().all(|c| c.provider.filter_count_tokens)
        {
            return Err(End::reply(count_tokens_not_found()));
        }
        if !candidates.is_empty()
            && self.plan.stream_hint
            && candidates
                .iter()
                .all(|c| c.provider.buffer_complete_response || self.endless(c))
            && is_messages_path(self.parts.uri.path())
        {
            if let Some(started) = stream_started.take() {
                let _ = started.send(());
            }
        }
        let mut admission = self.admit(&candidates).await?;
        let route = self.endpoint(&mut admission).await?;
        if route.provider.filter_count_tokens
            && self.parts.method == hyper::Method::POST
            && is_count_tokens_path(self.parts.uri.path())
        {
            return Err(End::reply(count_tokens_not_found()));
        }
        if self.plan.stream_hint
            && (route.provider.buffer_complete_response || self.endless(&route))
            && is_messages_path(self.parts.uri.path())
        {
            if let Some(started) = stream_started.take() {
                let _ = started.send(());
            }
        }
        if self.endless(&route) {
            let rest = std::mem::take(&mut self.ids);
            if self.claude_owner.as_deref() != Some(&route.provider.id) {
                self.unpinned = Some((rest, self.ordinary_attempts));
            }
            self.claude_owner = Some(route.provider.id.clone());
            self.set_unbounded(true);
            self.wait_budget.pin_provider();
        }
        let (upstream, upstream_model) = match upstream_request(
            &route,
            self.parts,
            self.replay,
            self.plan.model.as_deref(),
            self.client_model(),
            self.ordinary.as_ref().and_then(|o| o.effort.as_deref()),
            self.plan.operation,
            self.request_budget,
            self.websocket,
        )
        .await
        {
            Ok(built) => built,
            Err(Unbuilt::Skip) => {
                // Nothing was sent: a fixed request moves on as well.
                self.unpin(false);
                return Err(End::Again);
            }
            Err(Unbuilt::Reject(response)) => return Err(End::Reply(response)),
        };
        let mut attempt = self.begin(admission, route, upstream_model, new_thread_model_denied);
        let deadline =
            tokio::time::Instant::now() + Duration::from_secs(self.settings.first_byte_seconds);
        let response = self
            .request_budget
            .response(
                deadline,
                self.first_silence(),
                attempt.route.client.request(upstream),
            )
            .await;
        let response = match response {
            Ok(Ok(response)) => response,
            Ok(Err(e)) => return Err(self.unreached(attempt, &e).await),
            Err(_) => return Err(self.headers_timeout(attempt).await),
        };
        let status = response.status();
        let stream = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| {
                h.split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/event-stream"))
            });
        let encoding = response
            .headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        attempt.protocol.response(status.as_u16(), stream, encoding);
        attempt.protocol.response_headers(response.headers());
        if status == StatusCode::SWITCHING_PROTOCOLS && self.websocket {
            return Ok(Accepted::Tunnel(attempt, Box::new(response)));
        }
        if status.as_u16() >= 400 {
            return Err(self.rejected(attempt, response).await);
        }
        let mut opening = self.opening(&mut attempt, response, stream).await;
        if self.endless(&attempt.route)
            && !attempt.route.provider.buffer_complete_response
            && opening.ended
            && !opening.failed
            && is_messages_path(self.parts.uri.path())
        {
            opening.failed = !attempt.protocol.finish_buffer(status.as_u16(), None, false);
            opening.failure_kind = code::STREAM_INTERRUPTED;
        }
        if opening.failed
            && (attempt.route.client_id == super::ClientId::Dsh
                || ((attempt.route.provider.buffer_complete_response
                    || self.endless(&attempt.route))
                    && !attempt.protocol.explicit_failure())
                || super::attempt::transport_retry(
                    opening.prefix_bytes != 0,
                    attempt.protocol.transport_failure(),
                    opening.failure_kind,
                ))
        {
            return Err(self
                .interrupted(attempt, opening.status, opening.failure_kind, opening.probe)
                .await);
        }
        let attempt = self.initial_rejection(attempt, &mut opening).await?;
        Ok(Accepted::Stream(attempt, Box::new(opening)))
    }
    /// Starts the relay task for an accepted response and returns its head.
    fn hand_over(mut self, mut attempt: Attempt, opening: Opening) -> Response<WireBody> {
        self.request_budget.delivering();
        if attempt.route.provider.buffer_complete_response
            && is_messages_path(self.parts.uri.path())
        {
            attempt.protocol.buffer_output(false);
            attempt.protocol.effective_output();
        }
        let Attempt {
            admission,
            route,
            protocol,
            rerouted,
            ..
        } = attempt;
        let Opening {
            status,
            parts: response_parts,
            body,
            prefix,
            mut ended,
            failed,
            failure_kind,
            preinspected,
            inspected_bytes,
            ..
        } = opening;
        if preinspected {
            ended = false;
        }
        let replay_unsafe = failed && failure_kind == code::FIRST_BYTE_TIMEOUT;
        if replay_unsafe {
            self.failover.failure(super::failover::Failure::Transport);
            protocol.associate_failure(self.failover.failure_generation());
        }
        self.failover.finish(if replay_unsafe {
            super::failover::Exit::ReplayUnsafe
        } else {
            super::failover::Exit::Forwarded
        });
        let Admission { permits, slot, .. } = admission;
        let g = self.gateway.clone();
        let cfg = self.settings.clone();
        let neutral = status.as_u16() >= 400;
        let end_at_stop = ends_at_message_stop(&route, status, &response_parts.headers);
        if rerouted {
            self.gateway.record(
                Some(&route.provider.id),
                self.plan.model.as_deref(),
                Reason::Failover,
                Action::Routed,
                Some(status.as_u16()),
                Some(self.attempted.min(u32::MAX as usize) as u32),
            );
        }
        let delivery = protocol.delivery();
        let output_delivery = delivery.clone();
        let activity = self
            .parts
            .extensions
            .get::<std::sync::Arc<super::wire_progress::Activity>>()
            .cloned()
            .unwrap_or_default();
        let (sender, receiver) = tokio::sync::mpsc::channel::<ForwardFrame>(2);
        let empty = ended && !failed && prefix.is_empty();
        if empty {
            output_delivery.body_complete();
        }
        let shutdown = self
            .gateway
            .request_state()
            .shutdown
            .as_ref()
            .map(watch::Sender::subscribe);
        tokio::spawn(relay(Relay {
            active: self.active,
            traffic: self.traffic,
            slot,
            protocol,
            g,
            route,
            model: self.plan.model,
            permits,
            cfg,
            status,
            attempted: self.attempted,
            prefix,
            request_budget: self.request_budget.clone(),
            sender,
            activity,
            shutdown,
            ended,
            failed,
            delivery,
            lasting: self.ordinary.is_none(),
            body,
            inspected_bytes,
            neutral,
            end_at_stop,
            operation: self.plan.operation,
        }));
        // Hyper may drop a Content-Length body immediately after its last frame,
        // without polling it again for None. Mark that frame before yielding it.
        Response::from_parts(response_parts, delivered(receiver, output_delivery, empty))
    }
    /// No attempt produced a response to relay.
    fn exhausted(mut self) -> Response<WireBody> {
        self.failover
            .finish(if self.ordinary_attempts > self.settings.max_retries {
                super::failover::Exit::AttemptsExhausted
            } else {
                super::failover::Exit::NoEligibleProvider
            });
        self.gateway.record_final(
            self.last_error.as_ref(),
            self.failover.failure_reported(),
            self.previous_provider.as_deref(),
            self.plan.model.as_deref(),
            self.last.as_ref().map(|r| r.status().as_u16()),
            self.attempted,
        );
        self.last.unwrap_or_else(|| {
            if self.last_category == code::RPM_LIMIT {
                let mut response = error(
                    StatusCode::TOO_MANY_REQUESTS,
                    code::RPM_LIMIT,
                    "供应商已达到 RPM 上限，等待后重试",
                );
                response.headers_mut().insert(
                    header::RETRY_AFTER,
                    self.rpm_retry_after
                        .unwrap_or(Duration::from_secs(1))
                        .as_secs()
                        .max(1)
                        .into(),
                );
                response
            } else {
                error(
                    if self.attempted == 0 {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::BAD_GATEWAY
                    },
                    self.last_category,
                    if self.attempted == 0 {
                        "没有可用供应商，请检查队列和熔断状态"
                    } else {
                        "所有可用供应商均请求失败"
                    },
                )
            }
        })
    }
}
async fn forward(gateway: Gateway, request: Request<Incoming>) -> Response<WireBody> {
    let managed = gateway
        .request_state()
        .store
        .providers
        .iter()
        .any(|p| p.buffer_complete_response || p.endless_mode);
    if managed {
        let (started, received) = tokio::sync::oneshot::channel();
        // Keep the unboxed forwarding state out of the waiting future.
        super::claude_stream::waiting(
            Box::pin(forward_inner(gateway, request, Some(started))),
            received,
        )
        .await
    } else {
        forward_inner(gateway, request, None).await
    }
}
async fn forward_inner(
    gateway: Gateway,
    mut request: Request<Incoming>,
    mut stream_started: Option<tokio::sync::oneshot::Sender<()>>,
) -> Response<WireBody> {
    let (settings, mode, mut ids, token, running) = {
        let s = gateway.request_state();
        let ids = if s.store.mode == "auto" {
            s.store
                .providers
                .iter()
                .filter(|p| p.queued)
                .map(|p| p.id.clone())
                .collect::<Vec<_>>()
        } else {
            s.store.selected.iter().cloned().collect()
        };
        (
            s.store.settings.clone(),
            s.store.mode.clone(),
            ids,
            s.store.local_token.clone(),
            s.running,
        )
    };
    if !running {
        return stopped();
    }
    if !super::listener::authorized(request.headers(), &token) {
        return error(
            StatusCode::UNAUTHORIZED,
            code::LOCAL_AUTH,
            "本地网关认证失败",
        );
    }
    if gateway.client_id() == super::ClientId::Agy && super::gemini::telemetry(request.uri().path())
    {
        // Antigravity's own metrics: a gateway answers them and sends nothing on.
        return Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(replay::full("{}"))
            .expect("metric response");
    }
    if request.method() == hyper::Method::GET {
        if let Some(response) =
            super::catalog::codex_catalog(&gateway, &ids, request.uri(), request.headers()).await
        {
            return response;
        }
        if let Some(response) =
            super::catalog::local_list(&gateway, request.uri(), request.headers())
        {
            return response;
        }
        if let Some(response) = super::catalog::gemini_list(&gateway, request.uri()) {
            return response;
        }
    }
    // Capture all route versions before receiving the request body: edits only affect new requests.
    let provider_ids: Vec<_> = gateway
        .request_state()
        .store
        .providers
        .iter()
        .map(|p| p.id.clone())
        .collect();
    let mut routes: std::collections::HashMap<_, _> = provider_ids
        .into_iter()
        .filter_map(|id| gateway.route(&id).map(|r| (id, r)))
        .collect();
    routes.retain(|_, route| route.accepts_protocol(request.uri().path()));
    let active = Active::new(gateway.clone());
    let websocket = request
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    if websocket
        && matches!(
            request.uri().path().trim_end_matches('/'),
            "/responses" | "/v1/responses"
        )
    {
        return super::websocket::accept(
            gateway,
            request,
            settings,
            mode == "manual",
            ids,
            routes,
            active,
        );
    }
    let downstream_upgrade = websocket.then(|| hyper::upgrade::on(&mut request));
    let traffic = gateway
        .0
        .registry
        .freeze
        .request(super::freeze::Scope::Client(gateway.client_id()));
    let (parts, body) = request.into_parts();
    let replay = match Replay::capture_progress(
        body,
        gateway.0.spool.path(),
        Some(Duration::from_secs(settings.idle_seconds)),
    )
    .await
    {
        Ok(body) => body,
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                code::BODY,
                "请求未完整接收、超过 1 GiB 或已取消",
            );
        }
    };
    let request_budget = super::wait_budget::WaitBudget::tracked(
        settings.retry_budget_seconds,
        &gateway.0.pending_requests,
    );
    traffic.waiting();
    request_budget.begin();
    request_budget.attach_traffic(&traffic);
    let forwarding = async {
        let plan = match plan(
            &gateway, &parts, &replay, &mode, websocket, &mut ids, &routes,
        )
        .await
        {
            Ok(plan) => plan,
            Err(response) => return *response,
        };
        let mut failover = super::failover::Tracker::new(plan.usage_trace.clone());
        failover.attach_wait_budget(&request_budget);
        if gateway.client_id() == super::ClientId::Codex && mode == "auto" && plan.responses {
            failover.diagnostics(&gateway);
            if let Some(log) = failover.routing() {
                log.pending(
                    &parts.headers,
                    plan.hints.as_ref().ok(),
                    plan.ownership.as_deref(),
                    plan.continuation,
                );
            }
        }
        request_budget.describe(plan.pinned.as_deref(), plan.model.as_deref(), "admission");
        let wait_budget = admission_budget(&settings, &traffic, &plan);
        let mut cx = Exchange {
            gateway: &gateway,
            settings: &settings,
            mode: &mode,
            parts: &parts,
            replay: &replay,
            request_budget: &request_budget,
            websocket,
            active,
            traffic,
            downstream_upgrade,
            plan,
            ids,
            routes,
            failover,
            wait_budget,
            last: None,
            last_error: None,
            last_category: code::NO_PROVIDER,
            attempted: 0,
            ordinary_attempts: 0,
            capacity_protected: false,
            previous_provider: None,
            capacity_pending: false,
            capacity_wait_owner: None,
            hard_denied: std::collections::HashSet::new(),
            capacity_retry_after: None,
            rpm_retry_after: None,
            capacity_sources: Vec::new(),
            transient_retries: 0,
            retry_owner: None,
            claude_owner: None,
            unpinned: None,
            unbounded: false,
            ordinary: None,
            asked: Vec::new(),
            busy: Vec::new(),
        };
        if let Some(kind) = cx.plan.delegated {
            if !cx.delegate(kind) && cx.queued_for(kind) {
                // The kind's queue cannot serve it, and no other may.
                cx.ids.clear();
            }
        }
        loop {
            cx.refresh_endless();
            if cx.claude_owner.is_none() && cx.ordinary_attempts > cx.settings.max_retries {
                if cx.hand_back(None) {
                    continue;
                }
                break;
            }
            let before = (cx.attempted, cx.ordinary_attempts, cx.ids.clone());
            // A manual interruption drops the round in progress, with its
            // upstream request and slot, and the request is sent again.
            // Boxed: the round's state is too large to keep on the stack
            // beside the rest of the request.
            let round = {
                let mut round = Box::pin(cx.round(&mut stream_started));
                tokio::select! {
                    biased;
                    _ = request_budget.cancelled() => None,
                    round = &mut round => Some(round),
                }
            };
            match round {
                None => cx.resend(before),
                Some(Err(End::Again)) => {}
                // Before any output, a provider that took the request over
                // failing it is not the request failing.
                Some(Err(End::Stop)) => {
                    if !cx.hand_back(None) {
                        break;
                    }
                }
                Some(Err(End::Reply(response))) => {
                    if cx.ordinary.is_none() {
                        return *response;
                    }
                    if !cx.hand_back(Some(*response)) {
                        match cx.last.take() {
                            Some(last) => return last,
                            None => break,
                        }
                    }
                }
                Some(Ok(Accepted::Tunnel(attempt, response))) => {
                    return cx.tunnel(attempt, *response)
                }
                Some(Ok(Accepted::Stream(attempt, opening))) => {
                    return cx.hand_over(attempt, *opening)
                }
            }
        }
        cx.exhausted()
    };
    let mut shutdown = gateway
        .request_state()
        .shutdown
        .as_ref()
        .map(watch::Sender::subscribe);
    tokio::select! {
        biased;
        _ = stopping(&mut shutdown) => stopped(),
        _ = request_budget.wait() => error(StatusCode::GATEWAY_TIMEOUT, code::RETRY_BUDGET_EXHAUSTED, "排队与重试等待预算已耗尽"),
        response = forwarding => response,
    }
}
pub(super) async fn stopping(stop: &mut Option<watch::Receiver<bool>>) {
    if let Some(stop) = stop {
        if !*stop.borrow() {
            let _ = stop.changed().await;
        }
    } else {
        std::future::pending::<()>().await;
    }
}
fn remember(protocol: &super::protocol::Protocol, g: &Gateway, route: &Route, model: Option<&str>) {
    if let Some(id) = &protocol.observation.response_id {
        g.0.endpoints.remember(&route.provider, id);
        g.remember_model(
            id,
            &route.provider.id,
            model.or(protocol.observation.model.as_deref()),
        );
    }
}
