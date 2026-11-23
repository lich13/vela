//! Requests a provider takes over from the rest of the queue, end to end
//! through loopback upstreams.
use super::super::model::DelegateEdit;
use super::*;
use crate::usage::model::RoutingDecision;

const ASKED: &[u8] = br#" { "model" : "claude-fixture", "stream": false,
  "max_tokens": 32, "messages":[{"role":"user","content":"fixture"}], "unknown":9007199254740993 } "#;
const TURN: &[u8] = br#" { "model" : "gpt-request-fixture", "stream": false,
  "input":[{"role":"user","content":"fixture"}], "unknown":9007199254740993 } "#;
const AUXILIARY: (&str, &str) = ("x-claude-code-request-class", "auxiliary");

type Seen = Arc<Mutex<Vec<Bytes>>>;

/// An upstream that keeps the bodies it is sent and answers the nth request
/// with `answer(n)`.
async fn upstream(
    answer: impl Fn(usize) -> Response<WireBody> + Send + Sync + 'static,
) -> (LoopbackServer, Seen) {
    let seen = Seen::default();
    let bodies = seen.clone();
    let answer = Arc::new(answer);
    let upstream = server(move |request, _| {
        let (bodies, answer) = (bodies.clone(), answer.clone());
        async move {
            let body = request.into_body().collect().await.unwrap().to_bytes();
            let count = {
                let mut bodies = bodies.lock();
                bodies.push(body);
                bodies.len()
            };
            answer(count)
        }
    })
    .await;
    (upstream, seen)
}
fn answered(status: u16, body: String) -> Response<WireBody> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full(body))
        .unwrap()
}
/// A complete message. Each has its own id, so each is its own usage record.
fn message(id: &str) -> String {
    json!({
        "id": id, "type": "message", "model": "claude-fixture", "stop_reason": "end_turn",
        "content": [{"type": "text", "text": "complete"}],
        "usage": {"input_tokens": 5, "output_tokens": 3}
    })
    .to_string()
}
fn completed_response(id: &str) -> String {
    json!({
        "id": id, "object": "response", "model": "gpt-response-fixture", "status": "completed",
        "usage": {"input_tokens": 5, "output_tokens": 3}
    })
    .to_string()
}
async fn send(
    gateway: &Gateway,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, Bytes) {
    let client: HttpClient =
        Client::builder(TokioExecutor::new()).build(Connector::new(Duration::from_secs(2), 0));
    let token = gateway.0.inner.lock().store.local_token.clone();
    let mut request = Request::builder()
        .method("POST")
        .uri(format!(
            "http://127.0.0.1:{}{path}",
            gateway.view().settings.port
        ))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = tokio::time::timeout(
        WAIT,
        client.request(request.body(full(body.to_vec())).unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    (response.status().as_u16(), response_bytes(response).await)
}
/// What the fixture's provider at `index` takes over. `Some("")` takes a kind
/// on the model the client asked for.
fn takes(f: &Fixture, index: usize, compaction: Option<&str>, auxiliary: Option<&str>) {
    f.edit(taking(f, index, compaction, auxiliary));
}
fn taking(f: &Fixture, index: usize, compaction: Option<&str>, auxiliary: Option<&str>) -> Edit {
    saving(
        f,
        index,
        DelegateEdit {
            take_compaction: Some(compaction.is_some()),
            compaction_model: Some(compaction.unwrap_or_default().to_owned()),
            take_auxiliary: Some(auxiliary.is_some()),
            auxiliary_model: Some(auxiliary.unwrap_or_default().to_owned()),
            ..Default::default()
        },
    )
}
/// The reasoning effort that provider runs each kind on. An empty one leaves
/// the client's as it is.
fn efforts(f: &Fixture, index: usize, compaction: &str, auxiliary: &str) {
    f.edit(saving(
        f,
        index,
        DelegateEdit {
            compaction_effort: Some(compaction.to_owned()),
            auxiliary_effort: Some(auxiliary.to_owned()),
            ..Default::default()
        },
    ));
}
/// The model and reasoning effort that provider runs the web searches it
/// takes on, empty for the client's own. `None` takes no searches.
fn searches(f: &Fixture, index: usize, on: Option<(&str, &str)>) {
    let (model, effort) = on.unwrap_or_default();
    f.edit(saving(
        f,
        index,
        DelegateEdit {
            take_web_search: Some(on.is_some()),
            web_search_model: Some(model.to_owned()),
            web_search_effort: Some(effort.to_owned()),
            ..Default::default()
        },
    ));
}
fn saving(f: &Fixture, index: usize, delegate: DelegateEdit) -> Edit {
    let provider = &f.gateway.view().providers[index];
    Edit::SaveProvider {
        delegate,
        id: Some(provider.id.clone()),
        base_url: provider.base_url.clone(),
        token: String::new(),
        name: None,
        quota_access_token: None,
        end_on_message_stop: None,
        filter_count_tokens: None,
        buffer_complete_response: None,
        endless_mode: None,
        upstream_retry_seconds: None,
        normal_retry_seconds: None,
        risk_codes: None,
        retry_intensity: None,
        limit_retry_seconds: None,
        limit_rest_seconds: None,
    }
}
/// Two providers in automatic mode: the queue's own, and one beside it.
async fn pair(
    client: ClientId,
    main: &LoopbackServer,
    side: &LoopbackServer,
) -> (Fixture, Vec<String>) {
    listed(client, &[main, side]).await
}
/// Providers in automatic mode, in list order.
async fn listed(client: ClientId, upstreams: &[&LoopbackServer]) -> (Fixture, Vec<String>) {
    let base = if client == ClientId::Codex { "/v1" } else { "" };
    let urls: Vec<_> = upstreams
        .iter()
        .map(|upstream| upstream.url(base))
        .collect();
    let f = Fixture::new(client, &urls).await;
    f.edit(Edit::Mode {
        mode: "auto".into(),
    });
    let ids = f
        .gateway
        .view()
        .providers
        .iter()
        .map(|p| p.id.clone())
        .collect();
    (f, ids)
}
/// Takes the only slot of the provider `id`, for as long as the result lives.
async fn occupy(f: &Fixture, id: &str) -> super::super::admission::Admission {
    f.gateway
        .0
        .admission
        .acquire(
            &[f.gateway.route(id).unwrap()],
            false,
            100,
            &mut super::super::admission::Budget::new(1),
        )
        .await
        .unwrap()
}
/// `body` with its model replaced.
fn on_model(body: &[u8], model: &str) -> serde_json::Value {
    let mut value: serde_json::Value = serde_json::from_slice(body).unwrap();
    value["model"] = model.into();
    value
}
fn parsed(body: &Bytes) -> serde_json::Value {
    serde_json::from_slice(body).unwrap()
}

#[tokio::test]
async fn an_auxiliary_request_goes_to_its_taker_on_the_takers_model() {
    let (main, main_seen) = upstream(|n| answered(200, message(&format!("msg_main_{n}")))).await;
    let (side, side_seen) = upstream(|n| answered(200, message(&format!("msg_side_{n}")))).await;
    let (f, ids) = pair(ClientId::Claude, &main, &side).await;
    // The taker need not be in the queue: it serves only what it takes.
    f.edit(Edit::QueueProvider {
        id: ids[1].clone(),
        queued: false,
    });
    takes(&f, 1, None, Some(" aux-model "));
    let view = serde_json::to_value(&f.gateway.view().providers[1]).unwrap();
    assert_eq!(
        (
            &view["takeAuxiliary"],
            &view["auxiliaryModel"],
            &view["takeCompaction"]
        ),
        (&json!(true), &json!("aux-model"), &json!(false))
    );
    f.start().await;

    // The conversation itself, and its agents, stay in the queue.
    for class in [None, Some("main"), Some("subagent")] {
        let headers: Vec<_> = class
            .map(|class| ("x-claude-code-request-class", class))
            .into_iter()
            .collect();
        let (status, _) = send(&f.gateway, "/v1/messages", &headers, ASKED).await;
        assert_eq!(status, 200, "{class:?}");
    }
    assert!(main_seen.lock().iter().all(|body| body.as_ref() == ASKED));
    assert_eq!((main_seen.lock().len(), side_seen.lock().len()), (3, 0));
    settled(|| f.gateway.view().last_successful.as_deref() == Some(ids[0].as_str())).await;

    let (status, body) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
    assert_eq!(
        (status, body.as_ref()),
        (200, message("msg_side_1").as_bytes())
    );
    assert_eq!(main_seen.lock().len(), 3);
    // Whatever model the client asked for, the taker runs its own.
    assert_eq!(parsed(&side_seen.lock()[0]), on_model(ASKED, "aux-model"));

    let rows = recorded(&f.usage, 4).await;
    let taken: Vec<_> = rows
        .iter()
        .flat_map(|row| &row.attempts)
        .filter(|attempt| attempt.provider.as_deref() == Some(ids[1].as_str()))
        .collect();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken[0].requested_model.as_deref(), Some("claude-fixture"));
    assert_eq!(taken[0].upstream_model.as_deref(), Some("aux-model"));
    assert_eq!(
        taken[0].routing_decision,
        Some(RoutingDecision::AuxiliaryTakeover)
    );
    assert_eq!(taken[0].routing_outcome, None);
    // Serving one request does not make the taker the provider the client
    // is left with when the gateway stops.
    super::super::test_support::settled(&f.gateway).await;
    assert_eq!(
        f.gateway.view().last_successful.as_deref(),
        Some(ids[0].as_str())
    );
    f.stop().await;
}

#[tokio::test]
async fn a_taker_without_a_model_runs_what_the_client_asked_for() {
    let (main, main_seen) = upstream(|n| answered(200, message(&format!("msg_main_{n}")))).await;
    let (side, side_seen) = upstream(|n| answered(200, message(&format!("msg_side_{n}")))).await;
    let (f, ids) = pair(ClientId::Claude, &main, &side).await;
    takes(&f, 1, Some(""), None);
    f.start().await;

    // Claude Code marks its compactions; other errands are not this taker's.
    let (status, _) = send(
        &f.gateway,
        "/v1/messages",
        &[("x-claude-code-compaction", "1")],
        ASKED,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(side_seen.lock()[0].as_ref(), ASKED);
    let (status, _) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
    assert_eq!(status, 200);
    assert_eq!((main_seen.lock().len(), side_seen.lock().len()), (1, 1));

    let rows = recorded(&f.usage, 2).await;
    let taken = rows
        .iter()
        .flat_map(|row| &row.attempts)
        .find(|attempt| attempt.provider.as_deref() == Some(ids[1].as_str()))
        .unwrap();
    assert_eq!(taken.upstream_model, None);
    assert_eq!(taken.operation, Operation::Compaction);
    assert_eq!(
        taken.routing_decision,
        Some(RoutingDecision::CompactionTakeover)
    );
    f.stop().await;
}

#[tokio::test]
async fn a_request_its_taker_does_not_serve_is_answered_and_stays_out_of_the_queue() {
    // A failing service, a refused request, a refused key, and no service at all.
    for failure in [Some(500), Some(400), Some(401), None] {
        let (main, main_seen) =
            upstream(|n| answered(200, message(&format!("msg_main_{n}")))).await;
        let (side, side_seen) = upstream(move |_| {
            answered(
                failure.unwrap_or(500),
                r#"{"type":"error","error":{"type":"api_error","message":"fixture failure"}}"#
                    .into(),
            )
        })
        .await;
        let (f, ids) = pair(ClientId::Claude, &main, &side).await;
        if failure.is_none() {
            let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let provider = &f.gateway.view().providers[1];
            f.edit(Edit::SaveProvider {
                delegate: Default::default(),
                id: Some(provider.id.clone()),
                base_url: format!("http://127.0.0.1:{}", closed.local_addr().unwrap().port()),
                token: FIXTURE_KEY.into(),
                name: None,
                quota_access_token: None,
                end_on_message_stop: None,
                filter_count_tokens: None,
                buffer_complete_response: None,
                endless_mode: None,
                upstream_retry_seconds: None,
                normal_retry_seconds: None,
                risk_codes: None,
                retry_intensity: None,
                limit_retry_seconds: None,
                limit_rest_seconds: None,
            });
        }
        takes(&f, 1, None, Some("aux-model"));
        f.edit(Edit::QueueProvider {
            id: ids[1].clone(),
            queued: false,
        });
        f.start().await;

        let (status, _) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
        // The client is answered with the taker's failure.
        assert_eq!(status, failure.unwrap_or(502), "{failure:?}");
        // The ordinary queue, which could serve the request, is not asked.
        assert!(main_seen.lock().is_empty(), "{failure:?}");
        assert_eq!(
            side_seen.lock().is_empty(),
            failure.is_none(),
            "{failure:?}"
        );

        let rows = recorded(&f.usage, 1).await;
        let attempts = &rows[0].attempts;
        assert!(!attempts.is_empty(), "{failure:?}");
        for attempt in attempts {
            assert_eq!(attempt.provider.as_deref(), Some(ids[1].as_str()));
            assert_eq!(attempt.status, failure);
            assert_eq!(attempt.upstream_model.as_deref(), Some("aux-model"));
        }
        f.stop().await;
    }
}

#[tokio::test]
async fn the_takers_of_a_kind_are_asked_in_list_order_and_the_ordinary_queue_never() {
    let failing = || {
        answered(
            500,
            r#"{"type":"error","error":{"type":"api_error","message":"fixture failure"}}"#.into(),
        )
    };
    let (main, main_seen) = upstream(|n| answered(200, message(&format!("msg_main_{n}")))).await;
    let (first, first_seen) = upstream(move |_| failing()).await;
    // The second taker serves its first request only.
    let (second, second_seen) = upstream(move |n| match n {
        1 => answered(200, message("msg_second_1")),
        _ => failing(),
    })
    .await;
    let (f, ids) = listed(ClientId::Claude, &[&main, &first, &second]).await;
    takes(&f, 1, None, Some("first-model"));
    takes(&f, 2, None, Some("second-model"));
    efforts(&f, 2, "", "low");
    f.start().await;

    // The first taker fails the request, the second has it next, each on its
    // own model and effort; the ordinary queue is not asked.
    let (status, body) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
    assert_eq!(
        (status, body.as_ref()),
        (200, message("msg_second_1").as_bytes())
    );
    assert_eq!(
        parsed(&first_seen.lock()[0]),
        on_model(ASKED, "first-model")
    );
    let mut sent = on_model(ASKED, "second-model");
    sent["output_config"] = json!({"effort": "low"});
    assert_eq!(parsed(&second_seen.lock()[0]), sent);
    assert!(main_seen.lock().is_empty());

    // With every taker failing it, the client is answered with the last
    // one's failure; the request is still not the ordinary queue's.
    let (status, body) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
    assert_eq!(status, 500);
    assert!(String::from_utf8_lossy(&body).contains("fixture failure"));
    assert!(main_seen.lock().is_empty());
    assert_eq!((first_seen.lock().len(), second_seen.lock().len()), (2, 2));

    let rows = recorded(&f.usage, 2).await;
    let asked = |row: &Record| -> Vec<_> {
        row.attempts
            .iter()
            .map(|attempt| (attempt.provider.clone().unwrap(), attempt.routing_decision))
            .collect()
    };
    let taken = Some(RoutingDecision::AuxiliaryTakeover);
    let served = rows
        .iter()
        .find(|row| row.attempts.len() == 2)
        .expect("the request the second taker served");
    assert_eq!(
        asked(served),
        [(ids[1].clone(), taken), (ids[2].clone(), taken)]
    );
    let failed = rows
        .iter()
        .find(|row| !std::ptr::eq(*row, served))
        .expect("the request every taker failed");
    assert_eq!(
        asked(failed),
        [(ids[1].clone(), taken), (ids[2].clone(), taken)]
    );
    f.stop().await;
}

#[tokio::test]
async fn a_kind_its_takers_cannot_serve_is_refused_and_not_the_ordinary_queues() {
    let (main, main_seen) = upstream(|n| answered(200, message(&format!("msg_main_{n}")))).await;
    let (side, side_seen) = upstream(|n| answered(200, message(&format!("msg_side_{n}")))).await;
    let (f, ids) = pair(ClientId::Claude, &main, &side).await;
    takes(&f, 1, None, Some(""));
    // The only taker of errands does not serve the model the client asks for.
    f.edit(Edit::ModelsProvider {
        id: ids[1].clone(),
        allowed_models: Some(vec!["other-model".into()]),
    });
    f.start().await;
    let (status, _) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
    assert_eq!(status, 503);
    assert_eq!((main_seen.lock().len(), side_seen.lock().len()), (0, 0));
    // The conversation is still the ordinary queue's.
    let (status, _) = send(&f.gateway, "/v1/messages", &[], ASKED).await;
    assert_eq!(status, 200);
    assert_eq!((main_seen.lock().len(), side_seen.lock().len()), (1, 0));
    f.stop().await;
}

#[tokio::test]
async fn a_model_a_taker_lacks_sends_the_request_to_the_next_taker_alone() {
    let missing = || {
        answered(
            404,
            r#"{"type":"error","error":{"type":"model_not_found","message":"The model claude-fixture does not exist"}}"#.into(),
        )
    };
    // With the first taker in endless mode or not, and a next taker or none.
    for (endless, next) in [(false, true), (true, true), (true, false)] {
        let (main, main_seen) =
            upstream(|n| answered(200, message(&format!("msg_main_{n}")))).await;
        let (first, first_seen) = upstream(move |_| missing()).await;
        let (second, second_seen) =
            upstream(|n| answered(200, message(&format!("msg_second_{n}")))).await;
        let (f, _) = listed(ClientId::Claude, &[&main, &first, &second]).await;
        takes(&f, 1, None, Some(""));
        if next {
            takes(&f, 2, None, Some(""));
        }
        let mut edit = saving(&f, 1, Default::default());
        if let Edit::SaveProvider {
            endless_mode,
            normal_retry_seconds,
            upstream_retry_seconds,
            ..
        } = &mut edit
        {
            *endless_mode = Some(endless);
            *normal_retry_seconds = Some(60);
            *upstream_retry_seconds = Some(60);
        }
        f.edit(edit);
        f.start().await;
        let (status, body) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
        if next {
            assert_eq!(
                (status, body.as_ref()),
                (200, message("msg_second_1").as_bytes()),
                "{endless}"
            );
        } else {
            assert_eq!(status, 404);
        }
        assert_eq!(
            (
                main_seen.lock().len(),
                first_seen.lock().len(),
                second_seen.lock().len()
            ),
            (0, 1, usize::from(next)),
            "{endless} {next}"
        );
        f.stop().await;
    }
}

#[tokio::test]
async fn an_endless_taker_retries_a_rate_limit_where_it_was() {
    let (main, main_seen) = upstream(|n| answered(200, message(&format!("msg_main_{n}")))).await;
    let (first, first_seen) = upstream(|n| match n {
        1 => answered(
            429,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"rate limit"}}"#.into(),
        ),
        _ => answered(200, message(&format!("msg_first_{n}"))),
    })
    .await;
    let (second, second_seen) =
        upstream(|n| answered(200, message(&format!("msg_second_{n}")))).await;
    let (f, _) = listed(ClientId::Claude, &[&main, &first, &second]).await;
    for index in [1, 2] {
        takes(&f, index, None, Some(""));
    }
    let mut edit = saving(&f, 1, Default::default());
    if let Edit::SaveProvider {
        endless_mode,
        limit_retry_seconds,
        limit_rest_seconds,
        retry_intensity,
        ..
    } = &mut edit
    {
        *endless_mode = Some(true);
        *limit_retry_seconds = Some(1);
        *limit_rest_seconds = Some(0);
        *retry_intensity = Some(0);
    }
    f.edit(edit);
    f.start().await;
    let started = std::time::Instant::now();
    let (status, body) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
    assert_eq!(
        (status, body.as_ref()),
        (200, message("msg_first_2").as_bytes())
    );
    // It waited its interval there: neither the next taker nor the ordinary
    // queue was asked.
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert_eq!(
        (
            main_seen.lock().len(),
            first_seen.lock().len(),
            second_seen.lock().len()
        ),
        (0, 2, 0)
    );
    f.stop().await;
}

#[tokio::test]
async fn a_request_waits_in_line_once_no_taker_of_its_kind_has_room() {
    let (main, main_seen) = upstream(|n| answered(200, message(&format!("msg_main_{n}")))).await;
    let (first, first_seen) = upstream(|n| answered(200, message(&format!("msg_first_{n}")))).await;
    let (second, second_seen) =
        upstream(|n| answered(200, message(&format!("msg_second_{n}")))).await;
    let (f, ids) = listed(ClientId::Claude, &[&main, &first, &second]).await;
    for index in [1, 2] {
        takes(&f, index, None, Some(""));
        f.edit(Edit::ConcurrencyProvider {
            id: ids[index].clone(),
            max_concurrency: 1,
        });
    }
    // The wait for a taker is cut short of the request's whole waiting
    // budget, which would otherwise answer the client first.
    let mut settings = f.gateway.view().settings.clone();
    settings.queue_seconds = 2;
    settings.retry_budget_seconds = 2;
    f.edit(Edit::Settings { settings });
    f.start().await;
    let seen = || {
        (
            main_seen.lock().len(),
            first_seen.lock().len(),
            second_seen.lock().len(),
        )
    };

    // A taker with no room is passed over for the next one that has some.
    let first_held = occupy(&f, &ids[1]).await;
    let started = std::time::Instant::now();
    let (status, _) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
    assert_eq!(status, 200);
    assert!(started.elapsed() < Duration::from_millis(900));
    assert_eq!(seen(), (0, 0, 1));

    // With none of them free the request waits for the first, rather than
    // going to the ordinary queue, which has room.
    let second_held = occupy(&f, &ids[2]).await;
    let gateway = f.gateway.clone();
    let waiting =
        tokio::spawn(async move { send(&gateway, "/v1/messages", &[AUXILIARY], ASKED).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!waiting.is_finished());
    assert_eq!(seen(), (0, 0, 1));
    drop(first_held);
    let (status, body) = waiting.await.unwrap();
    assert_eq!(
        (status, body.as_ref()),
        (200, message("msg_first_1").as_bytes())
    );
    assert_eq!(seen(), (0, 1, 1));

    // A wait that outlasts the queue's patience is answered as a full
    // queue is, and not by the ordinary queue.
    let first_held = occupy(&f, &ids[1]).await;
    let started = std::time::Instant::now();
    let (status, _) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
    assert_eq!(status, 429);
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert_eq!(seen(), (0, 1, 1));
    drop((first_held, second_held));
    f.stop().await;
}

#[tokio::test]
async fn a_taker_sets_the_reasoning_effort_it_names_in_place_of_the_clients() {
    const THOUGHT: &[u8] = br#"{"model":"claude-fixture","stream":false,"max_tokens":32,
      "thinking":{"type":"adaptive"},"output_config":{"effort":"xhigh","format":{"type":"json_schema","schema":{"type":"object"}}},
      "messages":[{"role":"user","content":"fixture"}]}"#;
    let (main, main_seen) = upstream(|n| answered(200, message(&format!("msg_main_{n}")))).await;
    let (side, side_seen) = upstream(|n| answered(200, message(&format!("msg_side_{n}")))).await;
    let (f, _) = pair(ClientId::Claude, &main, &side).await;
    takes(&f, 1, Some(""), Some("aux-model"));
    efforts(&f, 1, " max ", "low");
    let view = serde_json::to_value(&f.gateway.view().providers[1]).unwrap();
    assert_eq!(
        (&view["compactionEffort"], &view["auxiliaryEffort"]),
        (&json!("max"), &json!("low"))
    );
    f.start().await;

    // The conversation's effort is replaced and the settings beside it kept.
    let (status, _) = send(&f.gateway, "/v1/messages", &[AUXILIARY], THOUGHT).await;
    assert_eq!(status, 200);
    let mut sent = on_model(THOUGHT, "aux-model");
    sent["output_config"]["effort"] = json!("low");
    assert_eq!(parsed(&side_seen.lock()[0]), sent);
    // A request that names none is given the taker's.
    let (status, _) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
    assert_eq!(status, 200);
    let mut sent = on_model(ASKED, "aux-model");
    sent["output_config"] = json!({"effort": "low"});
    assert_eq!(parsed(&side_seen.lock()[1]), sent);
    // Each kind has its own, and the effort is set without a model as well.
    let compaction = [("x-claude-code-compaction", "1")];
    let (status, _) = send(&f.gateway, "/v1/messages", &compaction, THOUGHT).await;
    assert_eq!(status, 200);
    let mut sent = parsed(&Bytes::from_static(THOUGHT));
    sent["output_config"]["effort"] = json!("max");
    assert_eq!(parsed(&side_seen.lock()[2]), sent);
    // The conversation itself is sent as the client wrote it.
    let (status, _) = send(&f.gateway, "/v1/messages", &[], THOUGHT).await;
    assert_eq!(status, 200);
    assert_eq!(main_seen.lock().as_slice(), [Bytes::from_static(THOUGHT)]);
    f.stop().await;
}

#[tokio::test]
async fn a_codex_taker_sets_the_effort_among_the_reasoning_settings() {
    const REASONED: &[u8] = br#"{"model":"gpt-request-fixture","stream":false,
      "reasoning":{"effort":"xhigh","summary":"auto"},"input":[{"role":"user","content":"fixture"}]}"#;
    const UNREASONED: &[u8] = br#"{"model":"gpt-request-fixture","stream":false,"reasoning":null,
      "input":[{"role":"user","content":"fixture"}]}"#;
    const COMPACT: &[u8] = br#"{"model":"gpt-request-fixture","reasoning":{"effort":"xhigh"},
      "input":[{"role":"user","content":"fixture"},{"type":"compaction_trigger"}]}"#;
    let (main, main_seen) =
        upstream(|n| answered(200, completed_response(&format!("resp_main_{n}")))).await;
    let (side, side_seen) =
        upstream(|n| answered(200, completed_response(&format!("resp_side_{n}")))).await;
    let (f, _) = pair(ClientId::Codex, &main, &side).await;
    takes(&f, 1, Some("compact-model"), Some(""));
    efforts(&f, 1, "low", "minimal");
    f.start().await;

    let guardian = [("x-openai-subagent", "guardian")];
    let (status, _) = send(&f.gateway, "/v1/responses", &guardian, REASONED).await;
    assert_eq!(status, 200);
    let mut sent = parsed(&Bytes::from_static(REASONED));
    sent["reasoning"]["effort"] = json!("minimal");
    assert_eq!(parsed(&side_seen.lock()[0]), sent);
    // Codex sends null for a model it takes not to reason.
    let (status, _) = send(&f.gateway, "/v1/responses", &guardian, UNREASONED).await;
    assert_eq!(status, 200);
    let mut sent = parsed(&Bytes::from_static(UNREASONED));
    sent["reasoning"] = json!({"effort": "minimal"});
    assert_eq!(parsed(&side_seen.lock()[1]), sent);
    let (status, _) = send(&f.gateway, "/v1/responses", &[], COMPACT).await;
    assert_eq!(status, 200);
    let mut sent = on_model(COMPACT, "compact-model");
    sent["reasoning"]["effort"] = json!("low");
    assert_eq!(parsed(&side_seen.lock()[2]), sent);
    // The turns of the conversation keep the effort they were sent at.
    let (status, _) = send(&f.gateway, "/v1/responses", &[], REASONED).await;
    assert_eq!(status, 200);
    assert_eq!(main_seen.lock().as_slice(), [Bytes::from_static(REASONED)]);
    f.stop().await;
}

#[tokio::test]
async fn in_manual_mode_only_the_selected_provider_takes_requests_over() {
    let (main, main_seen) = upstream(|n| answered(200, message(&format!("msg_main_{n}")))).await;
    let (side, side_seen) = upstream(|n| answered(200, message(&format!("msg_side_{n}")))).await;
    let (f, ids) = pair(ClientId::Claude, &main, &side).await;
    f.edit(Edit::Mode {
        mode: "manual".into(),
    });
    f.edit(Edit::Select { id: ids[0].clone() });
    takes(&f, 1, None, Some("other-model"));
    f.start().await;

    let (status, _) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
    assert_eq!(status, 200);
    assert_eq!(main_seen.lock()[0].as_ref(), ASKED);

    // The selected provider's own setting still chooses the model.
    takes(&f, 0, None, Some("own-model"));
    let (status, _) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
    assert_eq!(status, 200);
    assert_eq!(parsed(&main_seen.lock()[1]), on_model(ASKED, "own-model"));
    let (status, _) = send(&f.gateway, "/v1/messages", &[], ASKED).await;
    assert_eq!(status, 200);
    assert_eq!(main_seen.lock()[2].as_ref(), ASKED);
    assert!(side_seen.lock().is_empty());
    f.stop().await;
}

#[tokio::test]
async fn a_codex_compaction_is_taken_over_and_the_thread_stays_with_its_owner() {
    const COMPACT: &[u8] = br#"{"model":"gpt-request-fixture","input":[{"role":"user","content":"fixture"},{"type":"compaction_trigger"}]}"#;
    const COMPACTED: &[u8] = br#"{"model":"gpt-request-fixture","stream":false,"input":[{"type":"compaction","encrypted_content":"fixture-window"},{"role":"user","content":"fixture"}]}"#;
    let (main, main_seen) =
        upstream(|n| answered(200, completed_response(&format!("resp_main_{n}")))).await;
    let (side, side_seen) = upstream(|n| {
        answered(
            200,
            json!({
                "id": format!("resp_side_{n}"), "object": "response", "status": "completed",
                "output": [{"type": "compaction", "encrypted_content": "fixture-window"}],
                "usage": {"input_tokens": 3, "output_tokens": 1}
            })
            .to_string(),
        )
    })
    .await;
    let (f, ids) = pair(ClientId::Codex, &main, &side).await;
    takes(&f, 1, Some("compact-model"), None);
    f.start().await;
    let thread = [("session_id", "fixture-thread")];
    let key = super::super::test_support::digest("fixture-thread");
    let ledger = f.gateway.0.compaction.clone();

    // The thread settles on the first provider in the queue.
    let (status, _) = send(&f.gateway, "/v1/responses", &thread, TURN).await;
    assert_eq!(status, 200);
    settled(|| ledger.owner(&key).as_deref() == Some(ids[0].as_str())).await;

    // Its compaction is the taker's, on the taker's model.
    let (status, body) = send(&f.gateway, "/v1/responses", &thread, COMPACT).await;
    assert_eq!(status, 200);
    assert!(String::from_utf8_lossy(&body).contains("fixture-window"));
    assert_eq!(
        parsed(&side_seen.lock()[0]),
        on_model(COMPACT, "compact-model")
    );
    assert_eq!(main_seen.lock().len(), 1);
    // What the taker wrote is on record; the thread has not moved.
    let window = super::super::test_support::digest("fixture-window");
    settled(|| ledger.boundary_matches(&key, Some(&window))).await;
    assert_eq!(ledger.owner(&key).as_deref(), Some(ids[0].as_str()));

    let (status, _) = send(&f.gateway, "/v1/responses", &thread, COMPACTED).await;
    assert_eq!(status, 200);
    assert_eq!((main_seen.lock().len(), side_seen.lock().len()), (2, 1));
    assert_eq!(main_seen.lock()[1].as_ref(), COMPACTED);
    assert_eq!(ledger.owner(&key).as_deref(), Some(ids[0].as_str()));

    let rows = recorded(&f.usage, 3).await;
    let taken = rows
        .iter()
        .flat_map(|row| &row.attempts)
        .find(|attempt| attempt.provider.as_deref() == Some(ids[1].as_str()))
        .unwrap();
    assert_eq!(taken.operation, Operation::Compaction);
    assert_eq!(taken.upstream_model.as_deref(), Some("compact-model"));
    assert_eq!(
        taken.routing_decision,
        Some(RoutingDecision::CompactionTakeover)
    );
    f.stop().await;
}

#[tokio::test]
async fn codex_errands_are_auxiliary_and_its_agents_are_not() {
    let (main, main_seen) =
        upstream(|n| answered(200, completed_response(&format!("resp_main_{n}")))).await;
    let (side, side_seen) =
        upstream(|n| answered(200, completed_response(&format!("resp_side_{n}")))).await;
    let (f, _) = pair(ClientId::Codex, &main, &side).await;
    takes(&f, 1, None, Some("aux-model"));
    f.start().await;

    for (n, headers) in [
        [("x-openai-subagent", "guardian")],
        [(
            "x-codex-turn-metadata",
            r#"{"thread_source":"thread_title","request_kind":"turn"}"#,
        )],
    ]
    .iter()
    .enumerate()
    {
        let (status, _) = send(&f.gateway, "/v1/responses", headers, TURN).await;
        assert_eq!(status, 200, "{headers:?}");
        assert_eq!(parsed(&side_seen.lock()[n]), on_model(TURN, "aux-model"));
    }
    for headers in [
        &[("x-openai-subagent", "collab_spawn")][..],
        &[(
            "x-codex-turn-metadata",
            r#"{"thread_source":"user","request_kind":"turn"}"#,
        )],
        &[],
    ] {
        let (status, _) = send(&f.gateway, "/v1/responses", headers, TURN).await;
        assert_eq!(status, 200, "{headers:?}");
    }
    assert!(main_seen.lock().iter().all(|body| body.as_ref() == TURN));
    assert_eq!((main_seen.lock().len(), side_seen.lock().len()), (3, 2));
    f.stop().await;
}

#[tokio::test]
async fn a_claude_search_goes_to_the_provider_that_takes_searches() {
    // What Claude Code's search tool sends: the one tool is the provider's
    // own, and the request is classed as an errand.
    const SEARCH: &[u8] = br#"{"model":"claude-fixture","stream":false,"max_tokens":32,
      "tools":[{"type":"web_search_20250305","name":"web_search","max_uses":8}],
      "tool_choice":{"type":"tool","name":"web_search"},
      "messages":[{"role":"user","content":"fixture"}]}"#;
    // The conversation declares the tools the client runs, its search among them.
    const CONVERSATION: &[u8] = br#"{"model":"claude-fixture","stream":false,"max_tokens":32,
      "tools":[{"name":"WebSearch","description":"fixture","input_schema":{"type":"object"}}],
      "messages":[{"role":"user","content":"fixture"}]}"#;
    let failing = || {
        answered(
            500,
            r#"{"type":"error","error":{"type":"api_error","message":"fixture failure"}}"#.into(),
        )
    };
    let (main, main_seen) = upstream(|n| answered(200, message(&format!("msg_main_{n}")))).await;
    let (errand, errand_seen) =
        upstream(|n| answered(200, message(&format!("msg_errand_{n}")))).await;
    // The provider that searches serves its first two requests only.
    let (search, search_seen) = upstream(move |n| match n {
        1 | 2 => answered(200, message(&format!("msg_search_{n}"))),
        _ => failing(),
    })
    .await;
    let (f, ids) = listed(ClientId::Claude, &[&main, &errand, &search]).await;
    takes(&f, 1, None, Some("aux-model"));
    f.start().await;
    let seen = || {
        (
            main_seen.lock().len(),
            errand_seen.lock().len(),
            search_seen.lock().len(),
        )
    };

    // With no provider set to take searches, a search is the errand it is
    // sent as.
    let (status, _) = send(&f.gateway, "/v1/messages", &[AUXILIARY], SEARCH).await;
    assert_eq!(status, 200);
    assert_eq!(seen(), (0, 1, 0));
    assert_eq!(
        parsed(&errand_seen.lock()[0]),
        on_model(SEARCH, "aux-model")
    );

    searches(&f, 2, Some((" search-model ", " low ")));
    let view = serde_json::to_value(&f.gateway.view().providers[2]).unwrap();
    assert_eq!(
        (
            &view["takeWebSearch"],
            &view["webSearchModel"],
            &view["webSearchEffort"],
            &view["takeAuxiliary"]
        ),
        (
            &json!(true),
            &json!("search-model"),
            &json!("low"),
            &json!(false)
        )
    );

    // With one, a search is that provider's before it is an errand, on the
    // model and effort the provider names.
    for (n, headers) in [&[AUXILIARY][..], &[]].into_iter().enumerate() {
        let (status, body) = send(&f.gateway, "/v1/messages", headers, SEARCH).await;
        assert_eq!(
            (status, body.as_ref()),
            (200, message(&format!("msg_search_{}", n + 1)).as_bytes())
        );
        let mut sent = on_model(SEARCH, "search-model");
        sent["output_config"] = json!({"effort": "low"});
        assert_eq!(parsed(&search_seen.lock()[n]), sent);
    }
    assert_eq!(seen(), (0, 1, 2));
    // Other errands and the conversation are whose they were.
    let (status, _) = send(&f.gateway, "/v1/messages", &[AUXILIARY], ASKED).await;
    assert_eq!(status, 200);
    let (status, _) = send(&f.gateway, "/v1/messages", &[], CONVERSATION).await;
    assert_eq!(status, 200);
    assert_eq!(seen(), (1, 2, 2));
    assert_eq!(main_seen.lock()[0].as_ref(), CONVERSATION);

    // A search its provider does not serve is answered with that failure:
    // it goes neither to the ordinary queue nor on to the providers that
    // take errands.
    let (status, _) = send(&f.gateway, "/v1/messages", &[AUXILIARY], SEARCH).await;
    assert_ne!(status, 200);
    assert_eq!(seen(), (1, 2, 3));

    let rows = recorded(&f.usage, 6).await;
    let taken: Vec<_> = rows
        .iter()
        .flat_map(|row| &row.attempts)
        .filter(|attempt| attempt.provider.as_deref() == Some(ids[2].as_str()))
        .collect();
    assert_eq!(taken.len(), 3);
    for attempt in taken {
        assert_eq!(attempt.requested_model.as_deref(), Some("claude-fixture"));
        assert_eq!(attempt.upstream_model.as_deref(), Some("search-model"));
        assert_eq!(
            attempt.routing_decision,
            Some(RoutingDecision::WebSearchTakeover)
        );
    }
    assert!(rows
        .iter()
        .flat_map(|row| &row.attempts)
        .all(|attempt| attempt.routing_decision.is_some()
            || attempt.provider.as_deref() != Some(ids[2].as_str())));
    f.stop().await;
}

#[tokio::test]
async fn a_grok_search_goes_to_the_provider_that_takes_searches() {
    // What Grok Build's search tool sends: a response of its own, whose only
    // tool is the provider's search.
    const SEARCH: &[u8] = br#"{"model":"grok-fixture","input":"fixture","max_output_tokens":64,
      "store":false,"tools":[{"type":"web_search","filters":{"allowed_domains":["example.invalid"]}}]}"#;
    // A turn of a conversation that leaves its searches to the provider.
    const CONVERSATION: &[u8] = br#"{"model":"grok-fixture","stream":false,
      "input":[{"role":"user","content":"fixture"}],
      "tools":[{"type":"web_search"},{"type":"x_search"},
        {"type":"function","name":"fixture","parameters":{"type":"object"}}]}"#;
    let (main, main_seen) =
        upstream(|n| answered(200, completed_response(&format!("resp_main_{n}")))).await;
    let (side, side_seen) =
        upstream(|n| answered(200, completed_response(&format!("resp_side_{n}")))).await;
    let (f, ids) = pair(ClientId::Grok, &main, &side).await;
    f.start().await;

    // Nothing is taken over until a provider is set to.
    let (status, _) = send(&f.gateway, "/v1/responses", &[], SEARCH).await;
    assert_eq!(status, 200);
    assert_eq!(main_seen.lock().as_slice(), [Bytes::from_static(SEARCH)]);

    searches(&f, 1, Some(("search-model", "low")));
    let (status, body) = send(&f.gateway, "/v1/responses", &[], SEARCH).await;
    assert_eq!(
        (status, body.as_ref()),
        (200, completed_response("resp_side_1").as_bytes())
    );
    let mut sent = on_model(SEARCH, "search-model");
    sent["reasoning"] = json!({"effort": "low"});
    assert_eq!(parsed(&side_seen.lock()[0]), sent);
    // The conversation is the queue's, whatever it has the provider search.
    let (status, _) = send(&f.gateway, "/v1/responses", &[], CONVERSATION).await;
    assert_eq!(status, 200);
    assert_eq!(main_seen.lock()[1].as_ref(), CONVERSATION);
    assert_eq!((main_seen.lock().len(), side_seen.lock().len()), (2, 1));

    let rows = recorded(&f.usage, 3).await;
    let taken = rows
        .iter()
        .flat_map(|row| &row.attempts)
        .find(|attempt| attempt.provider.as_deref() == Some(ids[1].as_str()))
        .unwrap();
    assert_eq!(taken.upstream_model.as_deref(), Some("search-model"));
    assert_eq!(
        taken.routing_decision,
        Some(RoutingDecision::WebSearchTakeover)
    );
    f.stop().await;
}

#[tokio::test]
async fn a_provider_takes_over_only_the_kinds_its_client_hands_over() {
    let f = Fixture::new(ClientId::Grok, &["http://127.0.0.1:9/v1".to_owned()]).await;
    let revision = f.gateway.view().revision;
    for (compaction, auxiliary) in [(Some(""), None), (None, Some("small-fixture"))] {
        let refused = f
            .gateway
            .edit(taking(&f, 0, compaction, auxiliary), &revision, &f.home);
        assert!(refused.is_err());
    }
    // A form that switches nothing on is an ordinary save.
    takes(&f, 0, None, None);
    // Grok Build's own form carries the same settings, and of them a Grok
    // provider takes web searches only.
    let form = |field: &str| -> Edit {
        let provider = &f.gateway.view().providers[0];
        serde_json::from_value(json!({
            "op": "saveGrokProvider", "id": provider.id, "baseUrl": provider.base_url,
            "token": "", "protocol": "responses", field: true, "webSearchModel": "search-fixture"
        }))
        .unwrap()
    };
    let revision = f.gateway.view().revision;
    for field in ["takeCompaction", "takeAuxiliary"] {
        assert!(f.gateway.edit(form(field), &revision, &f.home).is_err());
    }
    f.edit(form("takeWebSearch"));
    let view = serde_json::to_value(&f.gateway.view().providers[0]).unwrap();
    assert_eq!(
        (&view["takeWebSearch"], &view["webSearchModel"]),
        (&json!(true), &json!("search-fixture"))
    );

    // Codex hands over all three.
    let f = Fixture::new(ClientId::Codex, &["http://127.0.0.1:9/v1".to_owned()]).await;
    searches(&f, 0, Some(("search-fixture", "low")));
    takes(&f, 0, Some(""), Some(""));
    let view = serde_json::to_value(&f.gateway.view().providers[0]).unwrap();
    assert_eq!(
        (
            &view["takeWebSearch"],
            &view["webSearchModel"],
            &view["takeCompaction"],
            &view["takeAuxiliary"]
        ),
        (
            &json!(true),
            &json!("search-fixture"),
            &json!(true),
            &json!(true)
        )
    );
    assert!(view.get("searchOnly").is_none());
}
