//! A provider that answers whole, for the OpenAI protocols: the client gets
//! one complete response or the protocol's own failure, never a cut one.
use super::*;
use crate::gateway::grok_config::Protocol;
use crate::gateway::test_support::{form, until};

const RESPONSES_REQUEST: &[u8] =
    br#"{"model":"gpt-fixture","stream":true,"input":[{"role":"user","content":"fixture"}]}"#;
const RESPONSES_JSON_REQUEST: &[u8] =
    br#"{"model":"gpt-fixture","stream":false,"input":[{"role":"user","content":"fixture"}]}"#;
const RESPONSES_PARTIAL: &[u8] = b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_fixture\",\"status\":\"in_progress\"}}\n\nevent: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"discard-partial\"}\n\n";
const RESPONSES_COMPLETE: &[u8] = b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_fixture\",\"status\":\"in_progress\"}}\n\nevent: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"complete\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_fixture\",\"status\":\"completed\",\"model\":\"gpt-fixture\",\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\n";
const RESPONSES_CUT_SHORT: &[u8] = b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"as-far-as-it-went\"}\n\nevent: response.incomplete\ndata: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_fixture\",\"status\":\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"},\"usage\":{\"input_tokens\":5,\"output_tokens\":3}}}\n\n";
const RESPONSES_JSON: &[u8] = br#"{"id":"resp_fixture","object":"response","status":"completed","model":"gpt-fixture","output":[{"type":"message","content":[{"type":"output_text","text":"complete"}]}],"usage":{"input_tokens":5,"output_tokens":3}}"#;
const CHAT_REQUEST: &[u8] =
    br#"{"model":"chat-fixture","stream":true,"messages":[{"role":"user","content":"fixture"}]}"#;
const CHAT_PARTIAL: &[u8] = b"data: {\"id\":\"chat_fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"discard-partial\"},\"finish_reason\":null}]}\n\n";
const CHAT_COMPLETE: &[u8] = b"data: {\"id\":\"chat_fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"complete\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"chat_fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":3}}\n\ndata: [DONE]\n\n";
const CHAT_CUT_SHORT: &[u8] = b"data: {\"id\":\"chat_fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"as-far-as-it-went\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";

/// Sets the fixture's first provider to answer whole, with one retry. A Grok
/// provider is saved the way its own form saves it, speaking `protocol`.
fn held(f: &Fixture, protocol: Protocol) {
    let provider = &f.gateway.view().providers[0];
    f.edit(if f.gateway.client_id() == ClientId::Grok {
        Edit::SaveGrokProvider {
            buffer_complete_response: Some(true),
            delegate: Default::default(),
            id: Some(provider.id.clone()),
            base_url: provider.base_url.clone(),
            token: String::new(),
            quota_access_token: None,
            name: None,
            protocol,
        }
    } else {
        form(serde_json::json!({
            "id": provider.id,
            "baseUrl": provider.base_url,
            "token": "",
            "bufferCompleteResponse": true,
        }))
    });
    assert!(f.gateway.view().providers[0].buffer_complete_response);
    let mut settings = f.gateway.view().settings;
    settings.max_retries = 1;
    settings.rate_limit_seconds = 1;
    settings.retry_budget_seconds = 10;
    settings.network_probe = false;
    f.edit(Edit::Settings { settings });
}
fn answer(content_type: &'static str, bytes: &'static [u8]) -> Response<WireBody> {
    Response::builder()
        .header("content-type", content_type)
        .body(full(bytes))
        .unwrap()
}
/// An upstream that gives `answers` in turn and the last of them from then on.
async fn upstream(
    content_type: &'static str,
    answers: &'static [&'static [u8]],
) -> (LoopbackServer, Arc<AtomicUsize>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let upstream = server(move |_, _| {
        let seen = seen.clone();
        async move {
            let turn = seen.fetch_add(1, Ordering::SeqCst).min(answers.len() - 1);
            answer(content_type, answers[turn])
        }
    })
    .await;
    (upstream, hits)
}
/// A gateway for `client` whose one provider answers whole, in front of an
/// upstream that gives `answers`.
async fn fixture(
    client: ClientId,
    protocol: Protocol,
    content_type: &'static str,
    answers: &'static [&'static [u8]],
) -> (Fixture, Arc<AtomicUsize>, LoopbackServer) {
    let (upstream, hits) = upstream(content_type, answers).await;
    let f = Fixture::new(client, &[upstream.url("/v1")]).await;
    held(&f, protocol);
    f.start().await;
    (f, hits, upstream)
}
async fn text(f: &Fixture, path: &str, body: &[u8]) -> (u16, String) {
    let response = request(&f.gateway, path, body).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8(response_bytes(response).await.to_vec()).unwrap(),
    )
}

#[tokio::test]
async fn a_cut_responses_stream_is_dropped_and_the_whole_one_delivered_once() {
    for client in [ClientId::Codex, ClientId::Grok] {
        let (f, hits, _upstream) = fixture(
            client,
            Protocol::Responses,
            "text/event-stream",
            &[RESPONSES_PARTIAL, RESPONSES_COMPLETE],
        )
        .await;
        let (status, body) = text(&f, "/v1/responses", RESPONSES_REQUEST).await;
        assert_eq!(status, 200);
        assert!(!body.contains("discard-partial"), "{client:?} {body}");
        assert_eq!(body.matches("event: response.completed").count(), 1);
        assert!(body.contains("complete"));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        f.stop().await;
    }
}

#[tokio::test]
async fn a_responses_stream_never_whole_ends_as_a_failed_response_without_its_parts() {
    let (f, hits, _upstream) = fixture(
        ClientId::Codex,
        Protocol::Responses,
        "text/event-stream",
        &[RESPONSES_PARTIAL],
    )
    .await;
    let (status, body) = text(&f, "/v1/responses", RESPONSES_REQUEST).await;
    // The stream was opened before the provider answered.
    assert_eq!(status, 200);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert!(!body.contains("discard-partial"), "{body}");
    let failed = body
        .split("\n\n")
        .find_map(|event| event.trim().strip_prefix("event: response.failed\ndata: "))
        .expect("a response.failed event");
    let failed: serde_json::Value = serde_json::from_str(failed).unwrap();
    assert_eq!(failed["type"], "response.failed");
    // What Codex reads the failure from: a code and a message, both text.
    assert!(failed["response"]["error"]["code"].is_string());
    assert!(failed["response"]["error"]["message"].is_string());
    f.stop().await;
}

#[tokio::test]
async fn a_response_its_provider_cut_short_is_whole_and_goes_out_as_said() {
    let (f, hits, _upstream) = fixture(
        ClientId::Codex,
        Protocol::Responses,
        "text/event-stream",
        &[RESPONSES_CUT_SHORT],
    )
    .await;
    let (status, body) = text(&f, "/v1/responses", RESPONSES_REQUEST).await;
    assert_eq!(status, 200);
    assert!(body.contains("as-far-as-it-went"));
    assert!(body.contains("event: response.incomplete"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    f.stop().await;
}

#[tokio::test]
async fn an_unstreamed_response_is_held_too_and_fails_with_a_status() {
    let (f, hits, _upstream) = fixture(
        ClientId::Codex,
        Protocol::Responses,
        "application/json",
        &[br#"{"id":"resp_fixture","status":"in_prog"#, RESPONSES_JSON],
    )
    .await;
    let (status, body) = text(&f, "/v1/responses", RESPONSES_JSON_REQUEST).await;
    assert_eq!(status, 200);
    assert_eq!(body.as_bytes(), RESPONSES_JSON);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    f.stop().await;

    let (f, hits, _upstream) = fixture(
        ClientId::Codex,
        Protocol::Responses,
        "application/json",
        &[br#"{"id":"resp_fixture","status":"in_prog"#],
    )
    .await;
    let (status, body) = text(&f, "/v1/responses", RESPONSES_JSON_REQUEST).await;
    assert_eq!(status, 502);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["error"]["type"], "vela_gateway");
    f.stop().await;
}

#[tokio::test]
async fn a_cut_chat_stream_is_dropped_and_a_failure_is_an_error_object() {
    let (f, hits, _upstream) = fixture(
        ClientId::Grok,
        Protocol::ChatCompletions,
        "text/event-stream",
        &[CHAT_PARTIAL, CHAT_COMPLETE],
    )
    .await;
    let (status, body) = text(&f, "/v1/chat/completions", CHAT_REQUEST).await;
    assert_eq!(status, 200);
    assert!(!body.contains("discard-partial"), "{body}");
    assert!(body.contains("complete"));
    assert_eq!(body.matches("data: [DONE]").count(), 1);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    f.stop().await;

    let (f, hits, _upstream) = fixture(
        ClientId::Grok,
        Protocol::ChatCompletions,
        "text/event-stream",
        &[CHAT_PARTIAL],
    )
    .await;
    let (status, body) = text(&f, "/v1/chat/completions", CHAT_REQUEST).await;
    assert_eq!(status, 200);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    assert!(!body.contains("discard-partial"), "{body}");
    let failed = body
        .split("\n\n")
        .find_map(|event| event.trim().strip_prefix("data: "))
        .expect("an error object");
    let failed: serde_json::Value = serde_json::from_str(failed).unwrap();
    assert!(failed["error"]["message"].is_string());
    assert!(!body.contains("event:"));
    f.stop().await;

    let (f, hits, _upstream) = fixture(
        ClientId::Grok,
        Protocol::ChatCompletions,
        "text/event-stream",
        &[CHAT_CUT_SHORT],
    )
    .await;
    let (status, body) = text(&f, "/v1/chat/completions", CHAT_REQUEST).await;
    assert_eq!(status, 200);
    assert!(body.contains("as-far-as-it-went"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    f.stop().await;
}

#[tokio::test]
async fn codex_is_kept_waiting_with_events_its_parser_passes_over() {
    let released = Arc::new(tokio::sync::Notify::new());
    let wait = released.clone();
    let upstream = server(move |_, _| {
        let wait = wait.clone();
        async move {
            let stream = async_stream::stream! {
                yield Ok::<_, BoxError>(Frame::data(Bytes::from_static(RESPONSES_PARTIAL)));
                wait.notified().await;
                yield Ok(Frame::data(Bytes::from_static(RESPONSES_COMPLETE)));
            };
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(StreamBody::new(stream).boxed_unsync())
                .unwrap()
        }
    })
    .await;
    let f = Fixture::new(ClientId::Codex, &[upstream.url("/v1")]).await;
    held(&f, Protocol::Responses);
    let mut settings = f.gateway.view().settings;
    settings.idle_seconds = 10;
    settings.first_byte_seconds = 10;
    settings.retry_budget_seconds = 20;
    f.edit(Edit::Settings { settings });
    f.start().await;
    let mut response = request(&f.gateway, "/v1/responses", RESPONSES_REQUEST).await;
    let frame = tokio::time::timeout(Duration::from_secs(6), response.body_mut().frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        frame.data_ref().unwrap().as_ref(),
        b"event: response.in_progress\ndata: {\"type\":\"response.in_progress\"}\n\n"
    );
    released.notify_one();
    let body = String::from_utf8(response_bytes(response).await.to_vec()).unwrap();
    assert!(body.contains("discard-partial") && body.contains("event: response.completed"));
    f.stop().await;
}

/// A gateway in automatic mode over two providers: the first answers whole
/// and only ever cuts its stream, the second completes it.
async fn pair() -> (Fixture, [Arc<AtomicUsize>; 2], [LoopbackServer; 2]) {
    let (first, first_hits) = upstream("text/event-stream", &[RESPONSES_PARTIAL]).await;
    let (second, second_hits) = upstream("text/event-stream", &[RESPONSES_COMPLETE]).await;
    let f = Fixture::new(ClientId::Codex, &[first.url("/v1"), second.url("/v1")]).await;
    held(&f, Protocol::Responses);
    f.edit(Edit::Mode {
        mode: "auto".into(),
    });
    (f, [first_hits, second_hits], [first, second])
}
/// Sends the streamed request apart from the test and gives its body.
fn sent(f: &Fixture) -> tokio::task::JoinHandle<String> {
    let gateway = f.gateway.clone();
    tokio::spawn(async move {
        let response = request(&gateway, "/v1/responses", RESPONSES_REQUEST).await;
        String::from_utf8(response_bytes(response).await.to_vec()).unwrap()
    })
}
/// The second provider's whole response reached the client, once, after the
/// first provider's one cut stream.
fn moved_once(body: &str, hits: &[Arc<AtomicUsize>; 2]) {
    assert!(!body.contains("discard-partial"), "{body}");
    assert_eq!(body.matches("event: response.completed").count(), 1);
    assert_eq!(hits[0].load(Ordering::SeqCst), 1);
    assert_eq!(hits[1].load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_provider_taken_out_of_the_queue_before_its_retry_leaves_it_to_the_queue() {
    let (f, hits, _upstreams) = pair().await;
    f.start().await;
    let pending = sent(&f);
    until(|| {
        hits[0].load(Ordering::SeqCst) == 1 && !f.gateway.view().transient_retries.is_empty()
    })
    .await;
    dequeue(&f);
    moved_once(&pending.await.unwrap(), &hits);
    f.stop().await;
}

#[tokio::test]
async fn a_provider_taken_out_while_its_retry_waits_for_a_slot_leaves_it_to_the_queue() {
    let (f, hits, _upstreams) = pair().await;
    let id = f.gateway.view().providers[0].id.clone();
    f.edit(Edit::ConcurrencyProvider {
        id: id.clone(),
        max_concurrency: 1,
    });
    f.start().await;
    let pending = sent(&f);
    until(|| {
        let view = f.gateway.view();
        hits[0].load(Ordering::SeqCst) == 1
            && view.providers[0].active_requests == 0
            && !view.transient_retries.is_empty()
    })
    .await;
    // The provider's one slot is taken before the retry comes for it.
    let slot = f
        .gateway
        .0
        .admission
        .acquire(
            &[f.gateway.route(&id).unwrap()],
            false,
            100,
            &mut super::super::admission::Budget::new(1),
        )
        .await
        .unwrap();
    until(|| {
        let view = f.gateway.view();
        view.transient_retries.is_empty() && view.waiting_requests == 1
    })
    .await;
    dequeue(&f);
    moved_once(&pending.await.unwrap(), &hits);
    drop(slot);
    f.stop().await;
}
