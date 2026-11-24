use super::*;
use crate::gateway::test_support::form;
use protocol::Protocol;
use serde_json::json;

fn observed(gateway: &Gateway, ua: &'static str, success: bool, websocket: bool, business: bool) {
    let mut headers = hyper::HeaderMap::new();
    headers.insert(
        hyper::header::USER_AGENT,
        hyper::header::HeaderValue::from_static(ua),
    );
    observed_headers(gateway, &headers, success, websocket, business);
}
fn observed_headers(
    gateway: &Gateway,
    headers: &hyper::HeaderMap,
    success: bool,
    websocket: bool,
    business: bool,
) {
    let mut protocol = Protocol::new(websocket);
    if business {
        protocol.business_request(gateway, headers);
    }
    protocol.request_headers(headers);
    if websocket {
        protocol.websocket_status(101);
    } else {
        protocol.response(200, false, "identity");
    }
    let payload = json!({"type": if success {"response.completed"} else {"response.failed"}, "response": {
        "id":"fixture-response", "status":if success {"completed"} else {"failed"}, "model":"fixture-model",
        "usage":{"input_tokens":2,"output_tokens":1}
    }});
    if websocket {
        protocol.value(&payload);
    } else {
        protocol.feed(payload.to_string().as_bytes());
    }
    protocol.delivery().handed(1, true);
    protocol.finish(Some(if websocket { 101 } else { 200 }), code::OK);
}

#[test]
fn codex_identity_is_atomic_success_only_private_and_bound_to_home() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(
        home.join("installation_id"),
        "11111111-2222-4333-8444-555555555555",
    )
    .unwrap();
    let data = dir.path().join("data");
    let gateway = Gateway::new(data.clone()).unwrap();
    gateway
        .edit(
            form(json!({"baseUrl": "https://example.invalid/v1", "token": "fixture-provider-key"})),
            &gateway.view().revision,
            &home,
        )
        .unwrap();
    let id = gateway.view().providers[0].id.clone();
    // A legacy UA-only store remains readable, but cannot start a Codex test.
    observed(&gateway, "codex_cli_rs/0.1.2 (fixture)", true, false, true);
    assert!(gateway
        .test_target(&id, None)
        .err()
        .unwrap()
        .message
        .contains("完整"));
    let mut headers = hyper::HeaderMap::new();
    headers.insert(
        "user-agent",
        "codex_cli_rs/0.1.2 (fixture)".parse().unwrap(),
    );
    headers.insert("originator", "fixture-exact-originator".parse().unwrap());
    headers.insert("session_id", "fixture-private-session-a".parse().unwrap());
    headers.insert("thread_id", "fixture-private-thread-a".parse().unwrap());
    observed_headers(&gateway, &headers, true, false, true);
    let first = gateway.test_target(&id, None).unwrap();
    assert_eq!(first.headers["session-id"], "fixture-private-session-a");
    assert_eq!(first.headers["originator"], "fixture-exact-originator");
    headers.remove("session_id");
    headers.remove("thread_id");
    headers.insert("session-id", "fixture-private-session-b".parse().unwrap());
    headers.insert("thread-id", "fixture-private-thread-b".parse().unwrap());
    headers.insert(
        "user-agent",
        "Codex Desktop/0.1.3 (fixture)".parse().unwrap(),
    );
    headers.insert("originator", "Codex Desktop".parse().unwrap());
    for (success, business) in [(false, true), (true, false)] {
        observed_headers(&gateway, &headers, success, true, business);
        assert_eq!(
            gateway.test_target(&id, None).unwrap().headers,
            first.headers
        );
    }
    let mut incomplete = headers.clone();
    incomplete.remove("session-id");
    observed_headers(&gateway, &incomplete, true, true, true);
    assert_eq!(
        gateway.test_target(&id, None).unwrap().headers,
        first.headers
    );
    observed_headers(&gateway, &headers, true, true, true);
    let second = gateway.test_target(&id, None).unwrap();
    assert_eq!(second.headers["session-id"], "fixture-private-session-b");
    assert_eq!(second.headers["thread-id"], "fixture-private-thread-b");
    assert_eq!(first.headers["session-id"], "fixture-private-session-a");
    assert!(!serde_json::to_string(&gateway.view())
        .unwrap()
        .contains("fixture-private"));
    drop(gateway);
    let gateway = Gateway::new(data).unwrap();
    gateway.observe_home(&home);
    assert_eq!(
        gateway.test_target(&id, None).unwrap().headers,
        second.headers
    );
    gateway.observe_home(&dir.path().join("other-home"));
    assert!(gateway.test_target(&id, None).is_err());
    gateway.observe_home(&home);
    assert!(gateway.test_target(&id, None).is_err());
}

#[test]
fn v02523_business_user_agent_is_platform_scoped_success_only_and_survives_reload() {
    let dir = tempfile::tempdir().unwrap();
    for client in [
        ClientId::Codex,
        ClientId::Claude,
        ClientId::Grok,
        ClientId::Dsh,
    ] {
        let path = dir.path().join(format!("{client:?}"));
        let gateway = Gateway::new_client(path.clone(), client, None).unwrap();
        assert!(gateway.successful_user_agent().is_err());
        observed(&gateway, "fixture-failed", false, false, true);
        observed(&gateway, "fixture-probe", true, false, false);
        assert!(gateway.successful_user_agent().is_err());
        observed(
            &gateway,
            "fixture-http/1.0 exact  spaces",
            true,
            false,
            true,
        );
        assert_eq!(
            gateway.successful_user_agent().unwrap(),
            "fixture-http/1.0 exact  spaces"
        );
        observed(&gateway, "fixture-websocket/2.0", true, true, true);
        assert_eq!(
            gateway.successful_user_agent().unwrap(),
            "fixture-websocket/2.0"
        );
        observed(
            &gateway,
            "fixture-failure-must-not-replace",
            false,
            true,
            true,
        );
        let view = serde_json::to_string(&gateway.view()).unwrap();
        assert!(!view.contains("fixture-websocket"));
        drop(gateway);
        let gateway = Gateway::new_client(path, client, None).unwrap();
        assert_eq!(
            gateway.successful_user_agent().unwrap(),
            "fixture-websocket/2.0"
        );
    }
}

#[test]
fn a_provider_that_takes_searches_stays_in_model_union_exit_and_generation_routes() {
    let dir = tempfile::tempdir().unwrap();
    let gateway = Gateway::new(dir.path().join("data")).unwrap();
    {
        let mut state = gateway.0.inner.lock();
        let store = &mut state.editable_mut().store;
        for (name, searching) in [("search", true), ("ordinary", false)] {
            store
                .edit(
                    form(json!({
                        "name": name,
                        "baseUrl": format!("https://{name}.example.invalid/v1"),
                        "token": "fixture-token",
                        "takeWebSearch": searching,
                    })),
                    false,
                )
                .unwrap();
            let provider = store.providers.last_mut().unwrap();
            provider.queued = true;
            provider.allowed_models = Some(vec![format!("fixture-{name}")]);
        }
        store.mode = "auto".into();
        assert_eq!(
            Gateway::exit_provider(&state.store, None).unwrap().name,
            "search"
        );
    }
    let routes = gateway.routing_ids(None);
    assert_eq!(routes.len(), 2);
    assert_eq!(
        gateway.model_union(None).unwrap(),
        vec!["fixture-search", "fixture-ordinary"]
    );
    assert_eq!(gateway.gateway_models().unwrap().providers.len(), 2);
    let store = &gateway.0.inner.lock().store;
    assert_eq!(routes[0], store.providers[0].id);
}
