//! A gateway with no provider to route to: every switch still works, a request
//! is answered at once, and stopping leaves no transaction behind.
use super::*;
use crate::gateway::test_support::loopback::{next_frame, ws_client};

const TURN: &[u8] = br#"{"model":"fixture-model","input":"fixture","stream":true}"#;
const COMPACT: &[u8] = br#"{"model":"fixture-model","input":[{"role":"user","content":"fixture"},{"type":"compaction_trigger"}]}"#;
const SEARCH: &[u8] = br#"{"model":"fixture-search-model","query":"fixture search"}"#;
const BEFORE: &str = "# fixture\nmodel_provider='custom'\n[model_providers.custom]\nbase_url = \"https://before.example.invalid/v1\"\nexperimental_bearer_token = \"before-fixture-token\"\nwire_api='responses'\nsupports_websockets=false\n";

fn journal(data: &std::path::Path) -> bool {
    data.join("gateway-recovery.json").exists()
}

/// The answer a request gets when nothing can take it, without any waiting.
async fn unanswerable(g: &Gateway, path: &str, body: &[u8], headers: Vec<(&str, &str)>) {
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        request(g, path, body.to_vec(), headers),
    )
    .await
    .expect("a request no provider can take is answered at once");
    assert_eq!(response.status(), 503, "{path}");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(
        String::from_utf8_lossy(&body).contains("没有可用供应商"),
        "{path}"
    );
}

/// One request of each kind the four queues take, and a WebSocket turn.
async fn every_request_is_unanswerable(g: &Gateway) {
    unanswerable(g, "/v1/responses", TURN, vec![]).await;
    unanswerable(g, "/v1/responses/compact", COMPACT, vec![]).await;
    unanswerable(
        g,
        "/v1/responses",
        TURN,
        vec![("x-openai-subagent", "guardian")],
    )
    .await;
    unanswerable(g, "/v1/alpha/search", SEARCH, vec![]).await;
    let mut ws = ws_client(g, "fixture-providerless").await;
    ws.send(yawc::Frame::text(
        r#"{"type":"response.create","model":"fixture-model","input":"fixture"}"#,
    ))
    .await
    .unwrap();
    let frame = next_frame(&mut ws).await;
    assert_eq!(frame.opcode(), yawc::OpCode::Close);
    // 1013: try again later.
    assert_eq!(frame.payload()[..2], 1013u16.to_be_bytes());
}

#[tokio::test]
async fn codex_without_a_provider_starts_answers_stops_and_starts_again() {
    let (t, g) = fixture(vec![]).await;
    let path = t.path().join("config.toml");
    std::fs::write(&path, BEFORE).unwrap();
    assert!(g.view().providers.is_empty() && g.view().selected.is_none());

    for mode in ["manual", "auto"] {
        update(&g, &t, Edit::Mode { mode: mode.into() });
        start(&g, &t).await;
        assert!(g.view().running);
        let live = std::fs::read_to_string(&path).unwrap();
        assert!(live.contains("http://127.0.0.1:") && !live.contains("before.example.invalid"));
        every_request_is_unanswerable(&g).await;
        // Both switches keep working with nothing to route to.
        for other in ["auto", "manual", mode] {
            update(&g, &t, Edit::Mode { mode: other.into() });
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), live);
        g.stop().await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), BEFORE);
        assert!(!journal(t.path()));
        assert_eq!(g.view().error, None);
    }

    // Quitting and coming back is the same round trip.
    start(&g, &t).await;
    g.stop_for_exit().await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), BEFORE);
    assert!(!journal(t.path()));
    g.resume(t.path()).await.unwrap();
    assert!(g.view().running);
    g.stop().await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), BEFORE);
    assert_eq!(
        std::fs::read_to_string(t.path().join("auth.json")).unwrap(),
        "unchanged-auth"
    );
}

#[tokio::test]
async fn codex_with_no_connection_to_put_back_keeps_the_gateway_one() {
    let (t, g) = fixture(vec![]).await;
    let path = t.path().join("config.toml");
    // A custom provider that never had an address or a token of its own.
    let bare = "model_provider='custom'\n[model_providers.custom]\nwire_api='responses'\n";
    std::fs::write(&path, bare).unwrap();
    start(&g, &t).await;
    let live = std::fs::read_to_string(&path).unwrap();
    unanswerable(&g, "/v1/responses", TURN, vec![]).await;
    g.stop().await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), live);
    assert!(!journal(t.path()));

    // What is left is the gateway's own address: it is never taken for a provider.
    let error = g
        .edit(Edit::Import, &g.view().revision, t.path())
        .err()
        .unwrap();
    assert_eq!(error.code, code::MANAGED);
    // Neither by this store at its next launch nor by one seeing the file first.
    let reopened = Gateway::new(t.path().to_path_buf()).unwrap();
    reopened.import_initial(t.path()).unwrap();
    assert!(reopened.view().providers.is_empty());
    drop(reopened);
    let fresh = tempfile::tempdir().unwrap();
    let first = Gateway::new(fresh.path().to_path_buf()).unwrap();
    assert_eq!(
        first.import_initial(t.path()).err().unwrap().code,
        code::MANAGED
    );
    assert!(first.view().providers.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), live);

    start(&g, &t).await;
    assert!(g.view().running);
    g.stop_for_exit().await.unwrap();
    g.resume(t.path()).await.unwrap();
    assert!(g.view().running);
    g.stop().await.unwrap();
    assert!(!journal(t.path()));
}

#[tokio::test]
async fn deleting_the_last_provider_while_running_puts_the_first_connection_back() {
    let (t, g) = fixture(vec!["https://a.example.invalid/v1".into()]).await;
    let path = t.path().join("config.toml");
    std::fs::write(&path, BEFORE).unwrap();
    let a = g.view().providers[0].id.clone();
    start(&g, &t).await;
    let live = std::fs::read_to_string(&path).unwrap();

    update(&g, &t, Edit::DeleteProvider { id: a });
    let view = g.view();
    assert!(view.running && view.providers.is_empty() && view.selected.is_none());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), live);
    unanswerable(&g, "/v1/responses", TURN, vec![]).await;

    // A crash now ends the same way a stop does.
    takeover::recover(t.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), BEFORE);
    g.stop().await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), BEFORE);
    assert!(!journal(t.path()));
    start(&g, &t).await;
    g.stop().await.unwrap();
}

#[tokio::test]
async fn emptying_the_queue_while_running_is_allowed_and_can_be_undone() {
    let ok = server(|_| async { Response::new(full("ok")) }).await;
    let (t, g) = fixture(vec![
        format!("http://127.0.0.1:{ok}"),
        "https://b.example.invalid/v1".into(),
    ])
    .await;
    let path = t.path().join("config.toml");
    std::fs::write(&path, BEFORE).unwrap();
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    start(&g, &t).await;
    let ids: Vec<_> = g.view().providers.iter().map(|p| p.id.clone()).collect();
    for id in &ids {
        update(
            &g,
            &t,
            Edit::QueueProvider {
                id: id.clone(),
                queued: false,
            },
        );
    }
    unanswerable(&g, "/v1/responses", TURN, vec![]).await;

    // The selected provider can go as well; the choice moves to the one left.
    update(&g, &t, Edit::Select { id: ids[0].clone() });
    update(&g, &t, Edit::DeleteProvider { id: ids[0].clone() });
    assert_eq!(g.view().selected.as_deref(), Some(ids[1].as_str()));
    update(
        &g,
        &t,
        Edit::Mode {
            mode: "auto".into(),
        },
    );
    unanswerable(&g, "/v1/responses", TURN, vec![]).await;
    g.stop().await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), BEFORE);
    assert!(!journal(t.path()));
}

async fn claude_request(claude: &Gateway) -> reqwest::Response {
    let token = claude.0.inner.lock().store.local_token.clone();
    tokio::time::timeout(
        Duration::from_secs(3),
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .post(format!(
                "http://127.0.0.1:{}/v1/messages",
                claude.listener_view().port
            ))
            .header("x-api-key", token)
            .header("anthropic-version", "2023-06-01")
            .body(r#"{"model":"claude-fixture","max_tokens":8,"messages":[{"role":"user","content":"fixture"}]}"#)
            .send(),
    )
    .await
    .expect("a request no provider can take is answered at once")
    .unwrap()
}

fn settings(home: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(home.join("settings.json")).unwrap()).unwrap()
}

#[tokio::test]
async fn claude_without_a_provider_puts_the_settings_back_as_they_were() {
    let (_temp, _codex, claude, home) =
        claude_tests::claude_fixture("https://before.example.invalid", "before-fixture-token")
            .await;
    let id = claude.view().providers[0].id.clone();
    claude
        .edit(Edit::DeleteProvider { id }, &claude.view().revision, &home)
        .unwrap();
    assert!(claude.view().providers.is_empty());
    let before = settings(&home);

    claude.start(&claude.view().revision, &home).await.unwrap();
    let live = settings(&home);
    assert!(live["env"]["ANTHROPIC_BASE_URL"]
        .as_str()
        .unwrap()
        .starts_with("http://127.0.0.1:"));
    assert_eq!(live["env"][super::super::claude_config::HINT_HEADERS], "1");
    let response = claude_request(&claude).await;
    assert_eq!(response.status(), 503);
    assert!(response.text().await.unwrap().contains("没有可用供应商"));
    claude.stop().await.unwrap();
    assert_eq!(settings(&home), before);
    assert!(!journal(&claude.0.data));

    claude.start(&claude.view().revision, &home).await.unwrap();
    claude.stop_for_exit().await.unwrap();
    assert_eq!(settings(&home), before);
    claude.resume(&home).await.unwrap();
    assert!(claude.view().running);
    claude.stop().await.unwrap();
    assert_eq!(settings(&home), before);
}

#[tokio::test]
async fn claude_with_no_settings_takes_its_two_fields_out_again() {
    let temp = tempfile::tempdir().unwrap();
    let codex = Gateway::new(temp.path().join("data")).unwrap();
    let claude = codex.companion(temp.path().join("data/claude")).unwrap();
    let home = temp.path().join("claude-home");
    let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    claude
        .set_listener_port(
            free.local_addr().unwrap().port(),
            &claude.listener_view().revision,
        )
        .unwrap();
    drop(free);

    claude.start(&claude.view().revision, &home).await.unwrap();
    assert!(settings(&home)["env"]["ANTHROPIC_AUTH_TOKEN"].is_string());
    claude.stop().await.unwrap();
    assert_eq!(settings(&home), serde_json::json!({"env": {}}));
    assert!(!journal(&claude.0.data));
    // Nothing of the gateway is left to be taken for a provider.
    claude.import_initial(&home).unwrap();
    assert!(claude.view().providers.is_empty());
    claude.start(&claude.view().revision, &home).await.unwrap();
    claude.stop().await.unwrap();
}
