//! Usage under the keys a gateway hands out, through loopback gateways and
//! upstreams; every key, name and agent here is a fixture.
use super::*;
use crate::gateway::test_support::hand_out_key;
use std::collections::HashSet;

const CREATE: &str = r#"{"type":"response.create","model":"gpt-ws-fixture","input":"fixture"}"#;
const MESSAGES: &[u8] = br#"{"model":"claude-request-fixture","stream":true,"max_tokens":32,
    "messages":[{"role":"user","content":"fixture"}]}"#;

fn own(gateway: &Gateway) -> String {
    gateway.0.inner.lock().store.local_token.clone()
}

/// A request with `credential`, naming `agent` as what sent it.
async fn request_as(
    gateway: &Gateway,
    path: &str,
    body: &[u8],
    credential: &str,
    agent: Option<&str>,
) -> Response<Incoming> {
    let client: HttpClient =
        Client::builder(TokioExecutor::new()).build(Connector::new(Duration::from_secs(2), 0));
    let mut request = Request::builder()
        .method("POST")
        .uri(format!(
            "http://127.0.0.1:{}{path}",
            gateway.view().settings.port
        ))
        .header("authorization", format!("Bearer {credential}"))
        .header("content-type", "application/json");
    if let Some(agent) = agent {
        request = request.header("user-agent", agent);
    }
    tokio::time::timeout(
        WAIT,
        client.request(request.body(full(body.to_vec())).unwrap()),
    )
    .await
    .unwrap()
    .unwrap()
}

/// A Responses WebSocket opened with `credential`, or `None` when the
/// gateway does not let it in.
async fn websocket_as(
    gateway: &Gateway,
    credential: &str,
    agent: Option<&str>,
) -> Option<yawc::TcpWebSocket> {
    let mut request =
        yawc::HttpRequest::builder().header("authorization", format!("Bearer {credential}"));
    if let Some(agent) = agent {
        request = request.header("user-agent", agent);
    }
    tokio::time::timeout(
        WAIT,
        yawc::WebSocket::connect(
            format!(
                "ws://127.0.0.1:{}/v1/responses",
                gateway.view().settings.port
            )
            .parse()
            .unwrap(),
        )
        .with_request(request),
    )
    .await
    .expect("fixture gateway did not answer the handshake")
    .ok()
}

/// The one record written since `known` last took stock.
async fn newest(fixture: &Fixture, known: &mut HashSet<String>) -> Record {
    let rows = recorded(&fixture.usage, known.len() + 1).await;
    let mut fresh = rows.into_iter().filter(|row| !known.contains(&row.id));
    let row = fresh.next().expect("fixture request left no record");
    assert!(fresh.next().is_none());
    known.insert(row.id.clone());
    assert_eq!(row.source, "proxy");
    assert!(row.completed);
    row
}

fn responses_json(n: usize) -> Bytes {
    json!({"id": format!("fixture-keyed-response-{n}"), "object": "response",
        "model": "gpt-response-fixture", "status": "completed",
        "usage": {"input_tokens": 100, "output_tokens": 20}})
    .to_string()
    .into()
}

fn messages_sse(n: usize) -> Bytes {
    format!(
        concat!(
            "event: message_start\r\ndata: {{\"type\":\"message_start\",\"message\":{{\"id\":\"fixture-keyed-message-{}\",\"type\":\"message\",\"model\":\"claude-response-fixture\",\"usage\":{{\"input_tokens\":11,\"output_tokens\":0}}}}}}\r\n\r\n",
            "event: content_block_delta\r\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"fixture\"}}}}\r\n\r\n",
            "event: message_delta\r\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":7}}}}\r\n\r\n",
            "event: message_stop\r\ndata: {{\"type\":\"message_stop\"}}\r\n\r\n"
        ),
        n
    )
    .into()
}

/// What the upstream of a WebSocket fixture saw and is allowed to do next.
#[derive(Clone, Default)]
struct Turns {
    connections: Arc<AtomicUsize>,
    started: Arc<AtomicUsize>,
    agents: Arc<Mutex<Vec<Option<String>>>>,
}

/// An upstream of both transports. Over HTTP every request is answered at
/// once with a response of its own id. Over a WebSocket every turn gets its
/// delta at once and its end when `finish` has a permit for it; the ends are
/// numbered across connections.
async fn upstream(finish: Arc<tokio::sync::Semaphore>) -> (LoopbackServer, Turns) {
    let turns = Turns::default();
    let seen = turns.clone();
    let answered = Arc::new(AtomicUsize::new(0));
    let server = server(move |mut request, tasks| {
        let (seen, finish, answered) = (seen.clone(), finish.clone(), answered.clone());
        async move {
            seen.agents.lock().push(
                request
                    .headers()
                    .get("user-agent")
                    .and_then(|agent| agent.to_str().ok())
                    .map(str::to_owned),
            );
            if !request.headers().contains_key("upgrade") {
                let _ = request.into_body().collect().await;
                return Response::builder()
                    .header("content-type", "application/json")
                    .body(full(responses_json(
                        answered.fetch_add(1, Ordering::SeqCst),
                    )))
                    .unwrap();
            }
            seen.connections.fetch_add(1, Ordering::SeqCst);
            let (response, upgrade) = yawc::WebSocket::upgrade(&mut request).unwrap();
            tasks.spawn(async move {
                let mut socket = upgrade.await.unwrap();
                while let Some(frame) = socket.next().await {
                    if frame.opcode() == yawc::OpCode::Close {
                        break;
                    }
                    if frame.opcode() == yawc::OpCode::Ping {
                        let pong = yawc::Frame::pong(frame.payload().to_vec());
                        if socket.send(pong).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    if frame.opcode().is_control() {
                        continue;
                    }
                    let turn = seen.started.fetch_add(1, Ordering::SeqCst) as u64 + 1;
                    if socket.send(yawc::Frame::text(WS_DELTA)).await.is_err() {
                        break;
                    }
                    finish.acquire().await.unwrap().forget();
                    if socket
                        .send(yawc::Frame::text(completed(turn)))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
            response.map(|_| replay::empty())
        }
    })
    .await;
    (server, turns)
}

/// Sends a turn and reads it to its end, which is the `turn`th the upstream
/// gave out.
async fn finished_turn(socket: &mut yawc::TcpWebSocket, turn: u64) {
    socket.send(yawc::Frame::text(CREATE)).await.unwrap();
    assert_eq!(
        next_text_frame(socket).await.payload().as_ref(),
        WS_DELTA.as_bytes()
    );
    assert_eq!(
        next_text_frame(socket).await.payload().as_ref(),
        completed(turn).as_bytes()
    );
}

/// The close a WebSocket ends with once its key was taken back.
async fn closed_for_its_key(socket: &mut yawc::TcpWebSocket) {
    let closed = tokio::time::timeout(WAIT, async {
        loop {
            let frame = socket
                .next()
                .await
                .expect("fixture websocket ended unclosed");
            match frame.opcode() {
                yawc::OpCode::Ping | yawc::OpCode::Pong => continue,
                _ => return frame,
            }
        }
    })
    .await
    .expect("fixture websocket was left open");
    assert_eq!(closed.opcode(), yawc::OpCode::Close);
    assert_eq!(closed.close_code().map(u16::from), Some(1008));
    assert_eq!(
        closed.close_reason().unwrap(),
        Some(crate::errors::code::LOCAL_AUTH)
    );
}

/// Waits until no request is held by a provider or for one.
async fn idle(fixture: &Fixture) {
    settled(|| {
        let view = fixture.gateway.view();
        view.waiting_requests == 0
            && view.pending_requests.is_empty()
            && view.providers[0].active_requests == 0
    })
    .await;
}

/// The same, and then that the gateway writes nothing beyond the `count`
/// records it has: time enough for whatever a request leaves to settle.
async fn quiet(fixture: &Fixture, count: usize) {
    idle(fixture).await;
    no_new_records(&fixture.usage, count).await;
}

#[tokio::test]
async fn an_http_request_is_recorded_under_the_key_it_came_in_with() {
    let (upstream, _) = upstream(Arc::new(tokio::sync::Semaphore::new(0))).await;
    let fixture = Fixture::new(ClientId::Codex, &[upstream.url("/v1")]).await;
    fixture.start().await;
    let own = own(&fixture.gateway);
    let (first, first_key) = hand_out_key(&fixture.gateway, &fixture.home, "fixture-first");
    let (second, second_key) = hand_out_key(&fixture.gateway, &fixture.home, "fixture-second");
    let mut known = HashSet::new();
    for (credential, expected) in [
        (&own, None),
        (&first_key, Some(&first)),
        (&second_key, Some(&second)),
        (&first_key, Some(&first)),
        (&own, None),
    ] {
        let response = request_as(
            &fixture.gateway,
            "/v1/responses",
            HTTP_REQUEST,
            credential,
            None,
        )
        .await;
        assert_eq!(response.status(), 200);
        response_bytes(response).await;
        let row = newest(&fixture, &mut known).await;
        assert_eq!(row.client, "codex");
        assert_eq!(row.entry_key.as_ref(), expected);
        let attempt = row.final_attempt().unwrap();
        assert_eq!(attempt.transport, "http");
        assert_eq!(attempt.outcome, "success");
        assert_eq!(attempt.tokens.total(), Some(120));
    }
    // A rename leaves what a key is counted under alone.
    fixture.edit(Edit::SaveEntryKey {
        id: Some(first.clone()),
        name: "fixture-renamed".into(),
    });
    let response = request_as(
        &fixture.gateway,
        "/v1/responses",
        HTTP_REQUEST,
        &first_key,
        None,
    )
    .await;
    assert_eq!(response.status(), 200);
    response_bytes(response).await;
    assert_eq!(
        newest(&fixture, &mut known).await.entry_key,
        Some(first.clone())
    );
    // Neither a key nor its name is in a record, only the id.
    let rows = serde_json::to_string(&records(&fixture.usage)).unwrap();
    for absent in [&own, &first_key, &second_key] {
        assert!(!rows.contains(absent.as_str()));
    }
    assert!(!rows.contains("fixture-renamed") && !rows.contains("fixture-second"));
    // What is refused for the key it came with is not recorded under any.
    fixture.edit(Edit::DeleteEntryKey { id: second });
    let refused = request_as(
        &fixture.gateway,
        "/v1/responses",
        HTTP_REQUEST,
        &second_key,
        None,
    )
    .await;
    assert_eq!(refused.status(), 401);
    no_new_records(&fixture.usage, known.len()).await;
    fixture.stop().await;
}

#[tokio::test]
async fn an_sse_response_is_recorded_under_the_key_it_was_asked_with() {
    let answered = Arc::new(AtomicUsize::new(0));
    let upstream = server(move |request, _| {
        let answered = answered.clone();
        async move {
            let _ = request.into_body().collect().await;
            Response::builder()
                .header("content-type", "text/event-stream; charset=utf-8")
                .body(fragmented(&messages_sse(
                    answered.fetch_add(1, Ordering::SeqCst),
                )))
                .unwrap()
        }
    })
    .await;
    let fixture = Fixture::new(ClientId::Claude, &[upstream.url("/deployment")]).await;
    fixture.start().await;
    let own = own(&fixture.gateway);
    let (id, key) = hand_out_key(&fixture.gateway, &fixture.home, "fixture-partner");
    let mut known = HashSet::new();
    for (n, (credential, expected)) in [(&key, Some(&id)), (&own, None), (&key, Some(&id))]
        .into_iter()
        .enumerate()
    {
        let response =
            request_as(&fixture.gateway, "/v1/messages", MESSAGES, credential, None).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response_bytes(response).await, messages_sse(n));
        let row = newest(&fixture, &mut known).await;
        assert_eq!(row.client, "claude");
        assert_eq!(row.entry_key.as_ref(), expected);
        let attempt = row.final_attempt().unwrap();
        assert!(attempt.stream);
        assert_eq!(attempt.outcome, "success");
        assert_eq!(
            attempt.response_id,
            Some(safe_id(&format!("fixture-keyed-message-{n}")))
        );
        assert_eq!(attempt.tokens.input, Some(11));
        assert_eq!(attempt.tokens.output, Some(7));
    }
    fixture.stop().await;
}

#[tokio::test]
async fn a_request_with_a_handed_out_key_is_not_taken_for_the_client_itself() {
    let (upstream, turns) = upstream(Arc::new(tokio::sync::Semaphore::new(0))).await;
    let fixture = Fixture::new(ClientId::Codex, &[upstream.url("/v1")]).await;
    fixture.start().await;
    let gateway = &fixture.gateway;
    let own = own(gateway);
    let (id, key) = hand_out_key(gateway, &fixture.home, "fixture-partner");
    let mut known = HashSet::new();
    let agent = || {
        gateway
            .successful_user_agent()
            .ok()
            .map(|agent| agent.to_str().unwrap().to_owned())
    };
    // Sends one request as `agent` and waits until nothing of it is left in
    // the gateway, what settles after its response included.
    let post = |credential: &str, agent: &'static str| {
        let credential = credential.to_owned();
        async move {
            let response = request_as(
                gateway,
                "/v1/responses",
                HTTP_REQUEST,
                &credential,
                Some(agent),
            )
            .await;
            assert_eq!(response.status(), 200);
            response_bytes(response).await;
        }
    };

    // Whoever holds a key is not the client the provider tests speak for.
    post(&key, "fixture-partner-agent/1.0").await;
    assert_eq!(
        newest(&fixture, &mut known).await.entry_key,
        Some(id.clone())
    );
    quiet(&fixture, known.len()).await;
    assert_eq!(agent(), None);
    post(&own, "fixture-client-agent/1.0").await;
    assert_eq!(newest(&fixture, &mut known).await.entry_key, None);
    settled(|| agent().as_deref() == Some("fixture-client-agent/1.0")).await;
    post(&key, "fixture-partner-agent/2.0").await;
    assert_eq!(newest(&fixture, &mut known).await.entry_key, Some(id));
    quiet(&fixture, known.len()).await;
    assert_eq!(agent().as_deref(), Some("fixture-client-agent/1.0"));

    // Each of the three did say who it was: a key changes who is let in,
    // not what is passed on.
    assert_eq!(
        *turns.agents.lock(),
        [
            "fixture-partner-agent/1.0",
            "fixture-client-agent/1.0",
            "fixture-partner-agent/2.0",
        ]
        .map(|agent| Some(agent.to_owned()))
    );
    fixture.stop().await;
    assert_eq!(agent().as_deref(), Some("fixture-client-agent/1.0"));
}

#[tokio::test]
async fn a_websocket_records_its_turns_under_its_key_and_ends_when_the_key_is_taken_back() {
    let finish = Arc::new(tokio::sync::Semaphore::new(0));
    let (upstream, turns) = upstream(finish.clone()).await;
    let fixture = Fixture::new(ClientId::Codex, &[upstream.url("/v1")]).await;
    fixture.start().await;
    let gateway = &fixture.gateway;
    let own = own(gateway);
    let (id, key) = hand_out_key(gateway, &fixture.home, "fixture-partner");
    let (kept, kept_key) = hand_out_key(gateway, &fixture.home, "fixture-kept");
    // The key each turn was counted under, by the number of the turn.
    let counted = |rows: &[Record], turn: u64| {
        let response = safe_id(&format!("fixture-ws-response-{turn}"));
        let row = rows
            .iter()
            .find(|row| row.final_attempt().unwrap().response_id.as_ref() == Some(&response))
            .unwrap_or_else(|| panic!("turn {turn} left no record"));
        assert!(row.completed);
        let attempt = row.final_attempt().unwrap();
        assert_eq!(attempt.transport, "websocket");
        assert_eq!(attempt.outcome, "success");
        assert_eq!(attempt.tokens.output, Some(10 * turn));
        row.entry_key.clone()
    };

    // Every turn of a connection is counted under the key it was made with.
    finish.add_permits(4);
    let mut keyed = websocket_as(gateway, &key, None).await.unwrap();
    finished_turn(&mut keyed, 1).await;
    let mut direct = websocket_as(gateway, &own, None).await.unwrap();
    finished_turn(&mut direct, 2).await;
    finished_turn(&mut keyed, 3).await;
    let mut other = websocket_as(gateway, &kept_key, None).await.unwrap();
    finished_turn(&mut other, 4).await;

    // The key is taken back while a turn is on its way: that turn is left to
    // end and is counted as the others were.
    keyed.send(yawc::Frame::text(CREATE)).await.unwrap();
    assert_eq!(
        next_text_frame(&mut keyed).await.payload().as_ref(),
        WS_DELTA.as_bytes()
    );
    fixture.edit(Edit::DeleteEntryKey { id: id.clone() });
    finish.add_permits(1);
    assert_eq!(
        next_text_frame(&mut keyed).await.payload().as_ref(),
        completed(5).as_bytes()
    );
    let rows = recorded(&fixture.usage, 5).await;
    for (turn, expected) in [
        (1, Some(&id)),
        (2, None),
        (3, Some(&id)),
        (4, Some(&kept)),
        (5, Some(&id)),
    ] {
        assert_eq!(counted(&rows, turn).as_ref(), expected, "turn {turn}");
    }
    idle(&fixture).await;

    // No turn starts on it after that, and none reaches the provider.
    keyed.send(yawc::Frame::text(CREATE)).await.unwrap();
    closed_for_its_key(&mut keyed).await;
    quiet(&fixture, 5).await;
    assert_eq!(turns.started.load(Ordering::SeqCst), 5);
    assert!(websocket_as(gateway, &key, None).await.is_none());

    // The connections made with what is still held go on.
    finish.add_permits(2);
    finished_turn(&mut other, 6).await;
    finished_turn(&mut direct, 7).await;
    let rows = recorded(&fixture.usage, 7).await;
    assert_eq!(counted(&rows, 6), Some(kept));
    assert_eq!(counted(&rows, 7), None);
    assert_eq!(turns.connections.load(Ordering::SeqCst), 3);
    drop((keyed, other, direct));
    fixture.stop().await;
    no_new_records(&fixture.usage, 7).await;
}

#[tokio::test]
async fn a_key_taken_back_before_the_first_turn_ends_the_websocket_without_a_provider() {
    let (upstream, turns) = upstream(Arc::new(tokio::sync::Semaphore::new(0))).await;
    let fixture = Fixture::new(ClientId::Codex, &[upstream.url("/v1")]).await;
    fixture.start().await;
    let (id, key) = hand_out_key(&fixture.gateway, &fixture.home, "fixture-partner");
    let mut socket = websocket_as(&fixture.gateway, &key, None).await.unwrap();
    socket
        .send(yawc::Frame::ping(b"fixture-idle".to_vec()))
        .await
        .unwrap();
    assert_eq!(next_frame(&mut socket).await.opcode(), yawc::OpCode::Pong);

    fixture.edit(Edit::DeleteEntryKey { id });
    socket.send(yawc::Frame::text(CREATE)).await.unwrap();
    closed_for_its_key(&mut socket).await;
    quiet(&fixture, 0).await;
    assert_eq!(turns.connections.load(Ordering::SeqCst), 0);
    assert_eq!(turns.started.load(Ordering::SeqCst), 0);
    drop(socket);
    fixture.stop().await;
}
