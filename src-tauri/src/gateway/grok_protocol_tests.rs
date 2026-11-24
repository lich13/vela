// 覆盖 Grok 三种原生协议与用量透传。
//! Grok gateway transport integration tests use synthetic credentials and loopback upstreams.
use super::{
    connector::Connector,
    grok_config::{self, Protocol},
    model::Edit,
    replay, ClientId, Gateway, HttpClient,
};
pub(super) use crate::gateway::test_support::scoped::{server, LoopbackServer};
use crate::gateway::test_support::{downstream::usage_records, UsageFixture as Fixture};
use crate::usage::{model::Record, Service};
use http_body_util::BodyExt;
use hyper::{body::Incoming, Request, Response, StatusCode};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use parking_lot::Mutex;
use std::{path::PathBuf, sync::Arc, time::Duration};

const WAIT: Duration = Duration::from_secs(5);
const ORIGINAL_TOKEN: &str = "fixture-original-grok-token";
const NEW_RESPONSES_TOKEN: &str = "fixture-new-responses-token";
const CHAT_TOKEN: &str = "fixture-chat-token";
const MESSAGES_TOKEN: &str = "fixture-messages-token";
const RESPONSES_RESPONSE_BODY: &[u8] =
    br#" { "id":"fixture-responses-response", "object":"response",
  "model":"fixture-grok-responses-upstream-model", "status":"completed", "output":[],
  "usage":{"input_tokens":100,"output_tokens":30,
    "input_tokens_details":{"cached_tokens":40},
    "output_tokens_details":{"reasoning_tokens":8}} } "#;
const CHAT_RESPONSE_BODY: &[u8] = br#" { "id":"fixture-chat-response", "object":"chat.completion",
  "model":"fixture-grok-chat-upstream-model",
  "choices":[{"index":0,"message":{"role":"assistant","content":"fixture-chat-output"},"finish_reason":"stop"}],
  "usage":{"prompt_tokens":100,"completion_tokens":30,
    "prompt_tokens_details":{"cached_tokens":40},
    "completion_tokens_details":{"reasoning_tokens":8}} } "#;
const MESSAGES_RESPONSE_BODY: &[u8] = br#" { "id":"fixture-messages-response", "type":"message",
  "model":"fixture-grok-messages-upstream-model", "role":"assistant", "content":[],
  "stop_reason":"end_turn",
  "usage":{"input_tokens":60,"output_tokens":30,"cache_read_input_tokens":40,
    "cache_creation_input_tokens":10} } "#;
const RESPONSES_REQUEST: &[u8] = br#" { "model" : "fixture-grok-responses-model", "input" : [{"role":"user","content":"fixture-responses"}], "future" : { "integer" : 9007199254740993 } } "#;
const CHAT_REQUEST: &[u8] = br#" { "model" : "fixture-grok-chat-model", "messages" : [{"role":"user","content":"fixture-chat"}], "future" : true } "#;
const MESSAGES_REQUEST: &[u8] = br#" { "model" : "fixture-grok-messages-model", "max_tokens" : 16, "messages" : [{"role":"user","content":"fixture-messages"}], "future" : true } "#;

#[derive(Clone, Debug)]
struct ObservedRequest {
    path_and_query: String,
    body: Vec<u8>,
    authorization: Option<String>,
    api_key: Option<String>,
    anthropic_version: Option<String>,
    model_private: Option<String>,
    provider_private: Option<String>,
}

async fn capturing_server(
    seen: Arc<Mutex<Vec<ObservedRequest>>>,
    response_body: &'static [u8],
) -> LoopbackServer {
    server(move |request| {
        let seen = seen.clone();
        let response_body = response_body.to_vec();
        async move {
            let (parts, body) = request.into_parts();
            let body = body.collect().await.unwrap().to_bytes();
            let header = |name: &str| {
                parts
                    .headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            };
            seen.lock().push(ObservedRequest {
                path_and_query: parts
                    .uri
                    .path_and_query()
                    .map(|value| value.as_str().to_owned())
                    .unwrap_or_default(),
                body: body.to_vec(),
                authorization: header("authorization"),
                api_key: header("x-api-key"),
                anthropic_version: header("anthropic-version"),
                model_private: header("x-model-private"),
                provider_private: header("x-provider-private"),
            });
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(replay::full(response_body))
                .unwrap()
        }
    })
    .await
}

impl Fixture {
    async fn native(upstream: &str) -> Self {
        Self::new_with_config(native_config(upstream)).await
    }

    async fn new_with_config(config: String) -> Self {
        Self::build(config, true).await
    }

    async fn new_manual_provider(upstream: &str) -> Self {
        let fixture = Self::build(native_config(upstream), false).await;
        add_provider(
            &fixture,
            upstream.to_owned(),
            "fixture-original-grok-token",
            Protocol::Responses,
        );
        fixture
    }

    async fn build(config: String, import_native_profile: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("grok-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.toml"), &config).unwrap();

        let primary = Gateway::new(dir.path().join("primary-data")).unwrap();
        let gateway = primary
            .grok_companion(dir.path().join("grok-gateway-data"))
            .unwrap();
        assert_eq!(gateway.client_id(), ClientId::Grok);
        let fixture = Self::over(dir, home, gateway);
        let (gateway, home) = (&fixture.gateway, &fixture.home);
        let reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = reservation.local_addr().unwrap().port();
        gateway
            .set_listener_port(port, &gateway.listener_view().revision)
            .unwrap();
        let mut settings = gateway.view().settings;
        settings.max_retries = 1;
        settings.connect_seconds = 2;
        settings.first_byte_seconds = 5;
        settings.idle_seconds = 5;
        settings.queue_seconds = 2;
        gateway
            .edit(Edit::Settings { settings }, &gateway.view().revision, home)
            .unwrap();
        drop(reservation);

        // Grok discovery is read-only; import is an explicit preview/commit action.
        gateway.import_initial(home).unwrap();
        assert!(gateway.view().providers.is_empty());
        assert_eq!(
            std::fs::read(home.join("config.toml")).unwrap().as_slice(),
            config.as_bytes()
        );
        if import_native_profile {
            let preview = gateway
                .preview_grok_import_with_env(home, None, false, &|_| None)
                .unwrap();
            let token = preview
                .token
                .as_deref()
                .expect("fixture Grok config should be importable");
            gateway
                .commit_grok_import(home, token, &preview.revision)
                .unwrap();
        }

        fixture
    }

    fn config_path(&self) -> PathBuf {
        self.home.join("config.toml")
    }
}

fn native_config(upstream: &str) -> String {
    format!(
        r#"
[models]
default = "fixture-selected"
extra_headers = {{ "x-model-private" = "fixture-model-private" }}

[model."fixture-selected"]
model_provider = "fixture-provider"
model = "fixture-grok-model"

[model_providers."fixture-provider"]
base_url = {}
api_key = "{ORIGINAL_TOKEN}"
api_backend = "responses"
extra_headers = {{ "authorization" = "Bearer fixture-stale-auth", "x-api-key" = "fixture-stale-api-key", "x-provider-private" = "fixture-provider-private" }}
query_params = {{ "profile" = "fixture-profile-query", "replace" = "fixture-profile-value" }}
"#,
        serde_json::to_string(upstream).unwrap()
    )
}

fn native_config_with_official_model(upstream: &str) -> String {
    let mut config = native_config(upstream);
    config.push_str(
        r#"

[model."fixture-official-native"]
model = "fixture-official-native-model"
base_url = "https://fixture-official-native.example.invalid/v1"
api_key = "fixture-official-native-token"
api_backend = "responses"
"#,
    );
    config
}

fn add_provider(fixture: &Fixture, base_url: String, token: &str, protocol: Protocol) {
    fixture.edit(Edit::SaveGrokProvider {
        delegate: Default::default(),
        quota_access_token: None,
        id: None,
        base_url,
        token: token.to_owned(),
        name: Some(format!("fixture-{}", protocol.name())),
        protocol,
    });
}

async fn gateway_request(gateway: &Gateway, path: &str, body: &[u8]) -> Response<Incoming> {
    let client: HttpClient =
        Client::builder(TokioExecutor::new()).build(Connector::new(Duration::from_secs(2), 0));
    let token = {
        let state = gateway.0.inner.lock();
        state.store.local_token.clone()
    };
    let port = gateway.listener_view().port;
    let request = Request::builder()
        .method("POST")
        .uri(format!("http://127.0.0.1:{port}{path}"))
        .header("authorization", format!("Bearer {token}"))
        .header("x-api-key", token)
        .header("content-type", "application/json")
        .body(replay::full(body.to_vec()))
        .unwrap();
    tokio::time::timeout(WAIT, client.request(request))
        .await
        .unwrap()
        .unwrap()
}

async fn response_bytes(response: Response<Incoming>) -> Vec<u8> {
    tokio::time::timeout(WAIT, response.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes()
        .to_vec()
}

async fn recorded(service: &Service, count: usize) -> Vec<Record> {
    let mut events = service.subscribe();
    tokio::time::timeout(WAIT, async {
        loop {
            let rows = usage_records(service);
            if rows.len() >= count {
                assert_eq!(rows.len(), count);
                assert!(service.state().error.is_none());
                return rows;
            }
            events.recv().await.expect("fixture usage writer stopped");
        }
    })
    .await
    .expect("fixture usage record was not written")
}

fn record_for_model<'a>(records: &'a [Record], model: &str) -> &'a Record {
    records
        .iter()
        .find(|record| {
            record
                .final_attempt()
                .and_then(|attempt| attempt.requested_model.as_deref())
                == Some(model)
        })
        .unwrap_or_else(|| panic!("fixture usage record for {model} was not found"))
}

struct ExpectedUsage<'a> {
    requested_model: &'a str,
    response_model: &'a str,
    input: u64,
    inclusive_input: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    output: u64,
}

fn assert_usage_record(record: &Record, expected: ExpectedUsage<'_>) {
    assert_eq!(record.client, "grok");
    assert_eq!(record.source, "proxy");
    assert!(record.completed);
    assert_eq!(record.attempts.len(), 1);
    let attempt = record.final_attempt().unwrap();
    assert_eq!(attempt.status, Some(200));
    assert_eq!(attempt.outcome, "success");
    assert_eq!(attempt.transport, "http");
    assert_eq!(
        attempt.requested_model.as_deref(),
        Some(expected.requested_model)
    );
    assert_eq!(
        attempt.response_model.as_deref(),
        Some(expected.response_model)
    );
    assert_eq!(attempt.inclusive_input_tokens, expected.inclusive_input);
    assert_eq!(attempt.tokens.input, Some(expected.input));
    assert_eq!(attempt.tokens.cache_read, expected.cache_read);
    assert_eq!(attempt.tokens.cache_write, expected.cache_write);
    assert_eq!(attempt.tokens.output, Some(expected.output));
}

fn grok_connection(gateway: &Gateway, provider_id: &str) -> grok_config::Connection {
    gateway
        .0
        .inner
        .lock()
        .store
        .grok_connections
        .get(provider_id)
        .cloned()
        .unwrap()
}

fn requests(seen: &Arc<Mutex<Vec<ObservedRequest>>>) -> Vec<ObservedRequest> {
    seen.lock().clone()
}

fn query_pairs(uri: &str) -> std::collections::BTreeMap<String, String> {
    url::Url::parse(&format!("http://fixture.invalid{uri}"))
        .unwrap()
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn assert_body_and_model(request: &ObservedRequest, original: &[u8], model: &str) {
    assert_eq!(request.body.as_slice(), original);
    let value: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(
        value.get("model").and_then(serde_json::Value::as_str),
        Some(model)
    );
}

#[tokio::test]
async fn grok_gateway_routes_each_native_protocol_with_private_connection_data() {
    let responses_seen = Arc::new(Mutex::new(Vec::new()));
    let chat_seen = Arc::new(Mutex::new(Vec::new()));
    let messages_seen = Arc::new(Mutex::new(Vec::new()));
    let responses_upstream =
        capturing_server(responses_seen.clone(), RESPONSES_RESPONSE_BODY).await;
    let chat_upstream = capturing_server(chat_seen.clone(), CHAT_RESPONSE_BODY).await;
    let messages_upstream = capturing_server(messages_seen.clone(), MESSAGES_RESPONSE_BODY).await;

    let fixture = Fixture::native(&responses_upstream.url("/v1")).await;
    let original_config = std::fs::read(fixture.config_path()).unwrap();
    let responses_id = fixture.gateway.view().providers[0].id.clone();
    fixture.edit(Edit::SaveGrokProvider {
        delegate: Default::default(),
        quota_access_token: None,
        id: Some(responses_id),
        base_url: responses_upstream.url("/v1"),
        token: NEW_RESPONSES_TOKEN.into(),
        name: None,
        protocol: Protocol::Responses,
    });
    add_provider(
        &fixture,
        chat_upstream.url("/v1"),
        CHAT_TOKEN,
        Protocol::ChatCompletions,
    );
    add_provider(
        &fixture,
        messages_upstream.url("/v1"),
        MESSAGES_TOKEN,
        Protocol::Messages,
    );
    fixture.edit(Edit::Mode {
        mode: "auto".into(),
    });
    fixture.start().await;

    let response = gateway_request(
        &fixture.gateway,
        "/v1/responses?replace=fixture-client-value&keep=fixture-client-query",
        RESPONSES_REQUEST,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_bytes(response).await.as_slice(),
        RESPONSES_RESPONSE_BODY
    );
    let rows = recorded(&fixture.usage, 1).await;
    assert_usage_record(
        record_for_model(&rows, "fixture-grok-responses-model"),
        ExpectedUsage {
            requested_model: "fixture-grok-responses-model",
            response_model: "fixture-grok-responses-upstream-model",
            input: 60,
            inclusive_input: Some(100),
            cache_read: Some(40),
            cache_write: None,
            output: 30,
        },
    );
    let responses = requests(&responses_seen);
    assert_eq!(responses.len(), 1);
    let request = &responses[0];
    assert_eq!(
        request.path_and_query.split('?').next(),
        Some("/v1/responses")
    );
    assert_body_and_model(request, RESPONSES_REQUEST, "fixture-grok-responses-model");
    assert_eq!(
        request.authorization.as_deref(),
        Some("Bearer fixture-new-responses-token")
    );
    assert_eq!(request.api_key, None);
    assert_eq!(
        request.model_private.as_deref(),
        Some("fixture-model-private")
    );
    assert_eq!(
        request.provider_private.as_deref(),
        Some("fixture-provider-private")
    );
    let query = query_pairs(&request.path_and_query);
    assert_eq!(query.len(), 3);
    assert_eq!(
        query.get("keep").map(String::as_str),
        Some("fixture-client-query")
    );
    assert_eq!(
        query.get("profile").map(String::as_str),
        Some("fixture-profile-query")
    );
    assert_eq!(
        query.get("replace").map(String::as_str),
        Some("fixture-profile-value")
    );
    assert!(requests(&chat_seen).is_empty());
    assert!(requests(&messages_seen).is_empty());

    let response = gateway_request(
        &fixture.gateway,
        "/v1/chat/completions?client=fixture-chat-query",
        CHAT_REQUEST,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_bytes(response).await.as_slice(),
        CHAT_RESPONSE_BODY
    );
    let rows = recorded(&fixture.usage, 2).await;
    assert_usage_record(
        record_for_model(&rows, "fixture-grok-chat-model"),
        ExpectedUsage {
            requested_model: "fixture-grok-chat-model",
            response_model: "fixture-grok-chat-upstream-model",
            input: 60,
            inclusive_input: Some(100),
            cache_read: Some(40),
            cache_write: None,
            output: 30,
        },
    );
    let chat = requests(&chat_seen);
    assert_eq!(chat.len(), 1);
    assert_eq!(
        chat[0].path_and_query,
        "/v1/chat/completions?client=fixture-chat-query"
    );
    assert_body_and_model(&chat[0], CHAT_REQUEST, "fixture-grok-chat-model");
    assert_eq!(
        chat[0].authorization.as_deref(),
        Some("Bearer fixture-chat-token")
    );
    assert_eq!(chat[0].api_key, None);
    assert_eq!(chat[0].anthropic_version, None);
    assert_eq!(requests(&responses_seen).len(), 1);
    assert!(requests(&messages_seen).is_empty());

    let response = gateway_request(
        &fixture.gateway,
        "/v1/messages?client=fixture-messages-query",
        MESSAGES_REQUEST,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_bytes(response).await.as_slice(),
        MESSAGES_RESPONSE_BODY
    );
    let rows = recorded(&fixture.usage, 3).await;
    assert_usage_record(
        record_for_model(&rows, "fixture-grok-messages-model"),
        ExpectedUsage {
            requested_model: "fixture-grok-messages-model",
            response_model: "fixture-grok-messages-upstream-model",
            input: 60,
            inclusive_input: None,
            cache_read: Some(40),
            cache_write: Some(10),
            output: 30,
        },
    );
    let messages = requests(&messages_seen);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].path_and_query,
        "/v1/messages?client=fixture-messages-query"
    );
    assert_body_and_model(
        &messages[0],
        MESSAGES_REQUEST,
        "fixture-grok-messages-model",
    );
    assert_eq!(messages[0].authorization, None);
    assert_eq!(
        messages[0].api_key.as_deref(),
        Some("fixture-messages-token")
    );
    assert_eq!(messages[0].anthropic_version.as_deref(), Some("2023-06-01"));
    assert_eq!(requests(&responses_seen).len(), 1);
    assert_eq!(requests(&chat_seen).len(), 1);

    fixture.stop().await;
    assert!(!fixture.gateway.view().running);
    assert_eq!(
        std::fs::read(fixture.config_path()).unwrap(),
        original_config
    );
}

#[tokio::test]
async fn grok_gateway_stop_restores_current_api_without_overwriting_conflicts() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let upstream = capturing_server(seen, RESPONSES_RESPONSE_BODY).await;
    let fixture = Fixture::new_manual_provider(&upstream.url("/v1")).await;
    let config_path = fixture.config_path();
    let original_config = std::fs::read(&config_path).unwrap();
    let provider_id = fixture.gateway.view().providers[0].id.clone();

    fixture.edit(Edit::Select {
        id: provider_id.clone(),
    });
    assert!(fixture.gateway.view().grok_managed);
    let direct_config = std::fs::read_to_string(&config_path).unwrap();
    assert_ne!(direct_config.as_bytes(), original_config.as_slice());
    let direct_doc = direct_config.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(
        direct_doc
            .get("models")
            .and_then(|models| models.get("default"))
            .and_then(toml_edit::Item::as_str),
        Some(grok_config::MANAGED)
    );

    fixture.start().await;
    let active_config = std::fs::read_to_string(&config_path).unwrap();
    let active_doc = active_config.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(
        active_doc
            .get("models")
            .and_then(|models| models.get("default"))
            .and_then(toml_edit::Item::as_str),
        Some(grok_config::MANAGED)
    );

    let mut external_doc = active_config.parse::<toml_edit::DocumentMut>().unwrap();
    external_doc["model"][grok_config::MANAGED]["api_key"] =
        toml_edit::value("fixture-external-grok-token");
    let external_config = external_doc.to_string();
    std::fs::write(&config_path, &external_config).unwrap();
    let stop = tokio::time::timeout(WAIT, fixture.gateway.stop())
        .await
        .unwrap();
    let conflict = match stop {
        Ok(_) => panic!("stop must preserve an externally changed managed credential"),
        Err(error) => error,
    };
    assert_eq!(conflict.code, "CONFLICT");
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        external_config
    );
    assert!(fixture.gateway.view().running);

    std::fs::write(&config_path, &active_config).unwrap();
    fixture.stop().await;
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        direct_config
    );
    assert!(!fixture.gateway.view().running);
}

#[tokio::test]
async fn grok_managed_api_ignores_native_default_changes_and_preserves_them_on_stop() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let upstream = capturing_server(seen.clone(), RESPONSES_RESPONSE_BODY).await;
    let config = native_config_with_official_model(&upstream.url("/v1"));
    let fixture = Fixture::new_with_config(config).await;
    fixture.edit(Edit::Mode {
        mode: "auto".into(),
    });

    fixture.start().await;
    let config_path = fixture.config_path();
    let active_config = std::fs::read_to_string(&config_path).unwrap();
    let mut external_doc = active_config.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(
        external_doc
            .get("models")
            .and_then(|models| models.get("default"))
            .and_then(toml_edit::Item::as_str),
        Some(grok_config::MANAGED)
    );
    external_doc["models"]["default"] = toml_edit::value("fixture-official-native");
    let external_config = external_doc.to_string();
    std::fs::write(&config_path, &external_config).unwrap();

    let response = gateway_request(&fixture.gateway, "/v1/responses", RESPONSES_REQUEST).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_bytes(response).await.as_slice(),
        RESPONSES_RESPONSE_BODY
    );
    let routed = requests(&seen);
    assert_eq!(routed.len(), 1);
    assert_body_and_model(
        &routed[0],
        RESPONSES_REQUEST,
        "fixture-grok-responses-model",
    );
    // Imported native custom auth headers remain part of the managed connection.
    assert_eq!(
        routed[0].authorization.as_deref(),
        Some("Bearer fixture-stale-auth")
    );
    assert!(routed[0].path_and_query.starts_with("/v1/responses"));

    fixture.stop().await;
    let stopped_config = std::fs::read_to_string(&config_path).unwrap();
    let stopped_doc = stopped_config.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(
        stopped_doc
            .get("models")
            .and_then(|models| models.get("default"))
            .and_then(toml_edit::Item::as_str),
        Some("fixture-official-native")
    );
    assert!(!fixture.gateway.view().running);
}

#[tokio::test]
async fn grok_initial_discovery_is_read_only_and_manual_connection_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("grok-home");
    std::fs::create_dir_all(&home).unwrap();
    let official_config = native_config("https://fixture-native.example.invalid/v1");
    let official_connection = grok_config::test_support::profile(&official_config)
        .unwrap()
        .connection;
    assert_eq!(official_connection.protocol, Protocol::Responses);
    assert!(!official_connection.headers.is_empty());
    assert!(!official_connection.query.is_empty());
    std::fs::write(home.join("config.toml"), official_config).unwrap();

    let primary_data = dir.path().join("primary-data");
    let grok_data = dir.path().join("grok-gateway-data");
    let primary = Gateway::new(primary_data.clone()).unwrap();
    let gateway = primary.grok_companion(grok_data.clone()).unwrap();
    assert!(!gateway.0.inner.lock().store.initialized);
    let native_before = std::fs::read(home.join("config.toml")).unwrap();
    gateway.import_initial(&home).unwrap();
    assert!(gateway.view().providers.is_empty());
    assert_eq!(
        std::fs::read(home.join("config.toml")).unwrap(),
        native_before
    );
    gateway
        .edit(
            Edit::SaveGrokProvider {
                delegate: Default::default(),
                quota_access_token: None,
                id: None,
                base_url: "https://fixture-manual.example.invalid/v1".into(),
                token: "fixture-manual-messages-token".into(),
                name: Some("fixture-manual-messages".into()),
                protocol: Protocol::Messages,
            },
            &gateway.view().revision,
            &home,
        )
        .unwrap();
    let provider_id = gateway.view().providers[0].id.clone();
    let manual_connection = grok_connection(&gateway, &provider_id);
    assert_eq!(manual_connection.protocol, Protocol::Messages);
    assert!(manual_connection.headers.is_empty());
    assert!(manual_connection.query.is_empty());

    gateway.import_initial(&home).unwrap();
    assert_eq!(gateway.view().providers.len(), 1);
    let imported_connection = grok_connection(&gateway, &provider_id);
    assert_eq!(imported_connection.protocol, Protocol::Messages);
    assert!(imported_connection.headers.is_empty());
    assert!(imported_connection.query.is_empty());
    drop(gateway);
    drop(primary);

    let reopened_primary = Gateway::new(primary_data).unwrap();
    let reopened = reopened_primary.grok_companion(grok_data).unwrap();
    reopened.import_initial(&home).unwrap();
    assert_eq!(reopened.view().providers.len(), 1);
    let reopened_provider_id = reopened.view().providers[0].id.clone();
    assert_eq!(reopened_provider_id, provider_id);
    let reopened_connection = grok_connection(&reopened, &reopened_provider_id);
    assert_eq!(reopened_connection.protocol, Protocol::Messages);
    assert!(reopened_connection.headers.is_empty());
    assert!(reopened_connection.query.is_empty());
}

#[tokio::test]
async fn clearing_endpoints_of_running_messages_provider_keeps_imported_auth_and_query() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let upstream = capturing_server(seen.clone(), MESSAGES_RESPONSE_BODY).await;
    let config = native_config(&upstream.url("/v1"))
        .replace("api_backend = \"responses\"", "api_backend = \"messages\"");
    let fixture = Fixture::new_with_config(config).await;
    let id = fixture.gateway.view().providers[0].id.clone();
    let before = serde_json::to_value(grok_connection(&fixture.gateway, &id)).unwrap();
    fixture.start().await;
    fixture.edit(Edit::EndpointsProvider {
        id: id.clone(),
        base_urls: vec!["https://backup.example.invalid/v1".into()],
    });
    fixture.edit(Edit::EndpointsProvider {
        id: id.clone(),
        base_urls: vec![],
    });
    assert_eq!(
        serde_json::to_value(grok_connection(&fixture.gateway, &id)).unwrap(),
        before
    );
    let response = gateway_request(&fixture.gateway, "/v1/messages", MESSAGES_REQUEST).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response_bytes(response).await, MESSAGES_RESPONSE_BODY);
    let observed = requests(&seen);
    assert_eq!(
        observed[0].api_key.as_deref(),
        Some("fixture-stale-api-key")
    );
    assert_eq!(
        query_pairs(&observed[0].path_and_query)["profile"],
        "fixture-profile-query"
    );
    fixture.stop().await;
    fixture.edit(Edit::DeleteProvider { id });
    assert!(fixture
        .gateway
        .0
        .inner
        .lock()
        .store
        .grok_connections
        .is_empty());
}

async fn unanswerable(gateway: &Gateway) {
    let response = gateway_request(gateway, "/v1/responses", RESPONSES_REQUEST).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(String::from_utf8_lossy(&response_bytes(response).await).contains("没有可用供应商"));
}

#[tokio::test]
async fn grok_without_a_provider_starts_answers_and_puts_its_config_back() {
    let fixture = Fixture::build(
        native_config("https://fixture-native.example.invalid/v1"),
        false,
    )
    .await;
    let config_path = fixture.config_path();
    let original = std::fs::read_to_string(&config_path).unwrap();
    for mode in ["manual", "auto"] {
        fixture.edit(Edit::Mode { mode: mode.into() });
        fixture.start().await;
        assert!(fixture.gateway.view().running);
        assert_ne!(std::fs::read_to_string(&config_path).unwrap(), original);
        unanswerable(&fixture.gateway).await;
        fixture.stop().await;
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);
        assert!(!fixture
            .gateway
            .0
            .data
            .join("gateway-recovery.json")
            .exists());
    }
    fixture.start().await;
    fixture.gateway.stop_for_exit().await.unwrap();
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);
    fixture.gateway.resume(&fixture.home).await.unwrap();
    assert!(fixture.gateway.view().running);
    fixture.stop().await;
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);
}

#[tokio::test]
async fn the_last_grok_provider_goes_while_running_whatever_protocol_is_left() {
    let fixture = Fixture::new_manual_provider("https://fixture-first.example.invalid/v1").await;
    add_provider(
        &fixture,
        "https://fixture-chat.example.invalid/v1".into(),
        CHAT_TOKEN,
        Protocol::ChatCompletions,
    );
    let config_path = fixture.config_path();
    let ids: Vec<_> = fixture
        .gateway
        .view()
        .providers
        .iter()
        .map(|p| p.id.clone())
        .collect();
    fixture.edit(Edit::Select { id: ids[0].clone() });
    let direct = std::fs::read_to_string(&config_path).unwrap();
    fixture.start().await;
    let live = std::fs::read_to_string(&config_path).unwrap();

    // Choosing the other protocol on purpose still asks for a stop first.
    let revision = fixture.gateway.view().revision;
    let refused = fixture
        .gateway
        .edit(
            Edit::Select { id: ids[1].clone() },
            &revision,
            &fixture.home,
        )
        .err()
        .unwrap();
    assert_eq!(refused.code, crate::errors::code::GROK_PROTOCOL);
    // Being left with it is not a choice: the provider in use can go.
    let view = fixture.edit(Edit::DeleteProvider { id: ids[0].clone() });
    assert_eq!(view.selected.as_deref(), Some(ids[1].as_str()));
    unanswerable(&fixture.gateway).await;
    for mode in ["auto", "manual"] {
        fixture.edit(Edit::Mode { mode: mode.into() });
    }
    fixture.edit(Edit::QueueProvider {
        id: ids[1].clone(),
        queued: false,
    });
    let view = fixture.edit(Edit::DeleteProvider { id: ids[1].clone() });
    assert!(view.running && view.providers.is_empty());
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), live);
    unanswerable(&fixture.gateway).await;
    fixture.stop().await;
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), direct);
    assert!(!fixture
        .gateway
        .0
        .data
        .join("gateway-recovery.json")
        .exists());
    fixture.start().await;
    fixture.stop().await;
}
