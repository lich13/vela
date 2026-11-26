use super::*;
use crate::gateway::test_support::{
    downstream::{attach_observers, usage_records},
    form, hand_out_key, until,
};
use crate::gateway::tests::server;
use http_body_util::BodyExt;
use serde_json::json;
use std::sync::Arc;

const STREAM: &str = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"fixture\"}]},\"finishReason\":\"STOP\"}],\"modelVersion\":\"gemini-fixture\",\"responseId\":\"fixture-response\",\"usageMetadata\":{\"promptTokenCount\":100,\"cachedContentTokenCount\":20,\"candidatesTokenCount\":5,\"thoughtsTokenCount\":7}}\n\n";

struct Fixture {
    temp: tempfile::TempDir,
    root: Gateway,
    agy: Gateway,
    home: PathBuf,
}
impl Fixture {
    fn edit(&self, edit: Edit) -> View {
        self.agy
            .edit(edit, &self.agy.view().revision, &self.home)
            .unwrap()
    }
    fn save(&self, base: &str) -> String {
        self.edit(form(
            json!({"name": "Fixture", "baseUrl": base, "token": "fixture-upstream"}),
        ))
        .providers
        .last()
        .unwrap()
        .id
        .clone()
    }
    async fn start(&self) -> View {
        self.agy
            .start(&self.agy.view().revision, &self.home)
            .await
            .unwrap()
    }
    fn environment(&self) -> PathBuf {
        self.temp.path().join("data/antigravity/agy.env")
    }
    fn token(&self) -> String {
        self.agy.0.inner.lock().store.local_token.clone()
    }
    /// A request the way `agy` sends one: its gateway key as a bearer token.
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .request(method, format!("{}{path}", self.agy.view().address))
            .bearer_auth(self.token())
    }
}

async fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let root = Gateway::new(temp.path().join("data")).unwrap();
    let agy = root
        .agy_companion(temp.path().join("data/antigravity"))
        .unwrap();
    let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    root.set_listener_port(port, &root.listener_view().revision)
        .unwrap();
    let fixture = Fixture {
        temp,
        root,
        agy,
        home,
    };
    fixture.edit(Edit::Mode {
        mode: "auto".into(),
    });
    fixture
}

type Seen = Arc<parking_lot::Mutex<Vec<(String, hyper::HeaderMap, bytes::Bytes)>>>;

/// An upstream that keeps what it was sent and answers every call alike, but
/// for the response id, which is numbered as a real one is unique.
async fn upstream(content_type: &'static str, body: String) -> (u16, Seen) {
    let seen = Seen::default();
    let port = server({
        let seen = seen.clone();
        move |request| {
            let (seen, body) = (seen.clone(), body.clone());
            async move {
                let (parts, sent) = request.into_parts();
                let sent = sent.collect().await.unwrap().to_bytes();
                let body = {
                    let mut seen = seen.lock();
                    seen.push((parts.uri.to_string(), parts.headers, sent));
                    body.replace(
                        "fixture-response",
                        &format!("fixture-response-{}", seen.len()),
                    )
                };
                hyper::Response::builder()
                    .header("content-type", content_type)
                    .body(replay::full(body))
                    .unwrap()
            }
        }
    })
    .await;
    (port, seen)
}

#[tokio::test]
async fn the_environment_file_lives_exactly_while_the_gateway_runs() {
    let f = fixture().await;
    f.save("https://example.invalid/api");
    assert_eq!(f.agy.view().agy_shell.map(|shell| shell.profiles), None);
    let view = f.start().await;
    assert_eq!(view.listener.running_clients, [ClientId::Agy]);
    assert!(!f.root.view().running);
    assert_eq!(
        view.current.protocol,
        Some(provider_protocol::Protocol::GeminiNative)
    );
    let port = view.listener.port;
    let written = std::fs::read_to_string(f.environment()).unwrap();
    assert_eq!(
        written,
        format!(
            "AGY_LLM_GATEWAY_URL=http://127.0.0.1:{port}\nAGY_LLM_GATEWAY_API_KEY={}\nAGY_LLM_GATEWAY_WIRE_PROTOCOL=genai\n",
            f.token()
        )
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = f.environment().metadata().unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let shell = view.agy_shell.unwrap();
    assert_eq!(shell.command.is_some(), cfg!(unix));
    // Neither the local token nor the address reaches a view or a dotfile.
    assert!(!serde_json::to_string(&f.agy.view())
        .unwrap()
        .contains(&f.token()));
    // Quitting keeps the wish to run; the file goes, since nothing listens.
    f.agy.stop_for_exit().await.unwrap();
    assert!(!f.environment().exists());
    assert!(f.agy.0.inner.lock().store.resume.as_ref().unwrap().desired);
    f.agy.resume(&f.home).await.unwrap();
    assert!(f.agy.view().running && f.environment().exists());
    // Turned off by the user: it stays off at the next start.
    f.agy.stop().await.unwrap();
    assert!(!f.environment().exists());
    assert!(!f.agy.0.inner.lock().store.resume.as_ref().unwrap().desired);
    f.agy.resume(&f.home).await.unwrap();
    assert!(!f.agy.view().running);
    assert!(f.agy.view().agy_shell.is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn the_function_stays_across_a_quit_and_leaves_with_the_gateway() {
    let f = fixture().await;
    f.save("https://example.invalid/api");
    // bash's file is where it is whatever this process's own shell is.
    let rc = f.home.join(if cfg!(target_os = "macos") {
        ".bash_profile"
    } else {
        ".bashrc"
    });
    std::fs::write(&rc, "export KEPT=1\n").unwrap();
    let shown = format!("~/{}", rc.file_name().unwrap().to_str().unwrap());
    let view = f.start().await;
    assert_eq!(view.error, None);
    assert!(view.agy_shell.unwrap().profiles.contains(&shown));
    let block = std::fs::read_to_string(&rc).unwrap();
    assert!(block.starts_with("export KEPT=1\n\n# >>> vela agy >>>\n"));
    assert!(block.contains(&f.environment().display().to_string()));
    // The function is fixed text: the token is only ever in the private file.
    assert!(!block.contains(&f.token()));
    f.agy.stop_for_exit().await.unwrap();
    assert_eq!(std::fs::read_to_string(&rc).unwrap(), block);
    f.agy.resume(&f.home).await.unwrap();
    assert_eq!(std::fs::read_to_string(&rc).unwrap(), block);
    f.agy.stop().await.unwrap();
    assert_eq!(std::fs::read_to_string(&rc).unwrap(), "export KEPT=1\n");
    // A file left by a run that ended without stopping names a dead gateway.
    let reopened = Gateway::new(f.temp.path().join("reopened")).unwrap();
    std::fs::create_dir_all(f.temp.path().join("reopened/antigravity")).unwrap();
    std::fs::write(
        f.temp.path().join("reopened/antigravity/agy.env"),
        "AGY_LLM_GATEWAY_URL=stale\n",
    )
    .unwrap();
    reopened
        .agy_companion(f.temp.path().join("reopened/antigravity"))
        .unwrap();
    assert!(!f.temp.path().join("reopened/antigravity/agy.env").exists());
}

#[tokio::test]
async fn a_stream_is_sent_on_natively_and_counted_under_antigravity() {
    let f = fixture().await;
    let (usage, _events) = attach_observers(&f.temp, &f.agy);
    let (port, seen) = upstream("text/event-stream", STREAM.into()).await;
    f.save(&format!("http://127.0.0.1:{port}/prefix"));
    f.start().await;
    let path = "/v1beta/models/gemini-fixture:streamGenerateContent?alt=sse&key=fixture-local";
    // Sent as it came when nothing in it is refused upstream.
    let plain = r#" {"contents":[{"role":"user","parts":[{"text":"fixture"}]}],"unknown":9007199254740993} "#;
    let response = f
        .request(reqwest::Method::POST, path)
        .header("content-type", "application/json")
        .body(plain)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.text().await.unwrap(),
        STREAM.replace("fixture-response", "fixture-response-1")
    );
    // Antigravity's billing labels are for Vertex AI alone.
    let labelled = json!({"contents":[{"role":"user","parts":[{"text":"fixture"}]}],"labels":{"client":"fixture"},"generationConfig":{"temperature":0}});
    let response = f
        .request(reqwest::Method::POST, path)
        .json(&labelled)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.text().await.unwrap(),
        STREAM.replace("fixture-response", "fixture-response-2")
    );
    {
        let seen = seen.lock();
        assert_eq!(seen.len(), 2);
        for (uri, headers, _) in seen.iter() {
            assert_eq!(
                uri,
                "/prefix/v1beta/models/gemini-fixture:streamGenerateContent?alt=sse"
            );
            assert_eq!(headers["x-goog-api-key"], "fixture-upstream");
            assert!(!headers.contains_key("authorization"));
            assert!(!headers.contains_key("x-api-key"));
        }
        assert_eq!(seen[0].2.as_ref(), plain.as_bytes());
        let sent: serde_json::Value = serde_json::from_slice(&seen[1].2).unwrap();
        assert_eq!(
            sent,
            json!({"contents":labelled["contents"],"generationConfig":{"temperature":0}})
        );
        assert_eq!(
            seen[1].1["content-length"].to_str().unwrap(),
            seen[1].2.len().to_string()
        );
    }
    until(|| usage_records(&usage).iter().filter(|r| r.completed).count() == 2).await;
    // What a CLI session row is matched to, so one call is counted once.
    let mut ids: Vec<_> = usage_records(&usage)
        .iter()
        .map(|r| r.final_attempt().unwrap().response_id.clone())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        ["fixture-response-1", "fixture-response-2"]
            .map(|id| Some(crate::usage::model::safe_id(id)))
    );
    for record in usage_records(&usage) {
        assert_eq!(
            (record.client.as_str(), record.source.as_str()),
            ("antigravity", "proxy")
        );
        let attempt = record.final_attempt().unwrap();
        assert_eq!(attempt.requested_model.as_deref(), Some("gemini-fixture"));
        assert!(attempt.stream);
        let tokens = &attempt.tokens;
        assert_eq!(
            (
                tokens.input,
                tokens.cache_read,
                tokens.output,
                tokens.reasoning
            ),
            (Some(80), Some(20), Some(12), Some(7))
        );
    }
    // Another client's key opens no Gemini path, and this one no other protocol.
    let wrong = reqwest::Client::builder().no_proxy().build().unwrap();
    let address = f.agy.view().address;
    assert_eq!(
        wrong
            .post(format!("{address}{path}"))
            .bearer_auth("fixture-wrong")
            .body(plain)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        f.request(reqwest::Method::POST, "/v1/responses")
            .body("{}")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(seen.lock().len(), 2);
}

#[tokio::test]
async fn a_vertex_address_keeps_its_own_path_and_the_labels() {
    let f = fixture().await;
    let (port, seen) = upstream("text/event-stream", STREAM.into()).await;
    f.save(&format!(
        "http://127.0.0.1:{port}/v1/projects/fixture/locations/global/publishers/google"
    ));
    f.start().await;
    let labelled = r#"{"contents":[],"labels":{"client":"fixture"}}"#;
    let response = f
        .request(
            reqwest::Method::POST,
            "/v1beta/models/gemini-fixture:streamGenerateContent?alt=sse",
        )
        .header("content-type", "application/json")
        .body(labelled)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let seen = seen.lock();
    assert_eq!(
        seen[0].0,
        "/v1/projects/fixture/locations/global/publishers/google/models/gemini-fixture:streamGenerateContent?alt=sse"
    );
    assert_eq!(seen[0].1["x-goog-api-key"], "fixture-upstream");
    assert_eq!(seen[0].2.as_ref(), labelled.as_bytes());
}

#[tokio::test]
async fn the_model_list_is_answered_here_with_the_default_first() {
    let f = fixture().await;
    let (port, seen) = upstream(
        "application/json",
        json!({"models":[{"name":"models/gemini-upstream"}]}).to_string(),
    )
    .await;
    let id = f.save(&format!("http://127.0.0.1:{port}"));
    f.start().await;
    let list = || async {
        f.request(reqwest::Method::GET, "/v1beta/models?pageSize=1000")
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    };
    let names = |value: &serde_json::Value| -> Vec<String> {
        value["models"]
            .as_array()
            .unwrap()
            .iter()
            .map(|model| model["name"].as_str().unwrap().to_owned())
            .collect()
    };
    // Nothing known yet: the provider's own list goes through.
    assert_eq!(names(&list().await), ["models/gemini-upstream"]);
    assert_eq!(seen.lock()[0].0, "/v1beta/models?pageSize=1000");
    f.edit(Edit::ModelsProvider {
        id,
        allowed_models: Some(vec!["gemini-a".into(), "gemini-b".into()]),
    });
    f.agy
        .edit_model_list(
            model_list::Operation::Add,
            "gemini-c",
            None,
            Default::default(),
        )
        .unwrap();
    // A name that cannot stand in a request's address is not offered.
    f.agy
        .edit_model_list(
            model_list::Operation::Add,
            "maker/gemini-d",
            None,
            Default::default(),
        )
        .unwrap();
    let listed = list().await;
    assert_eq!(
        names(&listed),
        ["models/gemini-a", "models/gemini-b", "models/gemini-c"]
    );
    assert_eq!(
        listed["models"][0]["supportedGenerationMethods"],
        json!(["generateContent", "countTokens"])
    );
    let view = f
        .agy
        .edit_model_list(
            model_list::Operation::First,
            "gemini-c",
            None,
            Default::default(),
        )
        .unwrap();
    assert_eq!(
        view.models
            .iter()
            .map(|m| (m.id.as_str(), m.first))
            .collect::<Vec<_>>(),
        [
            ("gemini-c", true),
            ("gemini-a", false),
            ("gemini-b", false),
            ("maker/gemini-d", false)
        ]
    );
    assert_eq!(
        names(&list().await),
        ["models/gemini-c", "models/gemini-a", "models/gemini-b"]
    );
    // Chosen again, it goes back to its own place.
    f.agy
        .edit_model_list(
            model_list::Operation::First,
            "gemini-c",
            None,
            Default::default(),
        )
        .unwrap();
    assert_eq!(names(&list().await)[0], "models/gemini-a");
    assert_eq!(seen.lock().len(), 1);
    // Only Antigravity starts on the head of its list.
    assert!(f
        .root
        .edit_model_list(
            model_list::Operation::First,
            "gemini-c",
            None,
            Default::default()
        )
        .is_err());
}

#[tokio::test]
async fn metric_calls_are_answered_and_never_sent_on() {
    let f = fixture().await;
    let (port, seen) = upstream("application/json", "{}".into()).await;
    f.save(&format!("http://127.0.0.1:{port}"));
    f.start().await;
    let response = f
        .request(reqwest::Method::POST, "/v1beta:recordClientEvents")
        .json(&json!({"events":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "{}");
    let unknown = reqwest::Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        unknown
            .post(format!(
                "{}/v1beta:recordClientEvents",
                f.agy.view().address
            ))
            .bearer_auth("fixture-wrong")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(seen.lock().is_empty());
}

#[tokio::test]
async fn the_catalog_follows_page_tokens_and_keeps_chat_models_only() {
    let f = fixture().await;
    let seen = Seen::default();
    let port = server({
        let seen = seen.clone();
        move |request| {
            let seen = seen.clone();
            async move {
                let uri = request.uri().to_string();
                seen.lock()
                    .push((uri.clone(), request.headers().clone(), Default::default()));
                let body = if uri.contains("pageToken=") {
                    json!({"models":[
                        {"name":"models/gemini-b","supportedGenerationMethods":["generateContent"]},
                        {"name":"models/embedding-fixture","supportedGenerationMethods":["embedContent"]}
                    ]})
                } else {
                    json!({"models":[{"name":"models/gemini-a"}],"nextPageToken":"next/fixture+="})
                };
                hyper::Response::builder()
                    .header("content-type", "application/json")
                    .body(replay::full(body.to_string()))
                    .unwrap()
            }
        }
    })
    .await;
    let id = f.save(&format!("http://127.0.0.1:{port}/v1beta"));
    let models = f.agy.list_models(&id, true).await.unwrap();
    assert_eq!(models.error, None);
    assert_eq!(models.models, ["gemini-a", "gemini-b"]);
    {
        let seen = seen.lock();
        assert_eq!(
            seen.iter()
                .map(|(uri, ..)| uri.as_str())
                .collect::<Vec<_>>(),
            [
                "/v1beta/models?pageSize=1000",
                "/v1beta/models?pageSize=1000&pageToken=next%2Ffixture%2B%3D"
            ]
        );
        assert_eq!(seen[0].1["x-goog-api-key"], "fixture-upstream");
        assert!(!seen[0].1.contains_key("authorization"));
    }
    // Vertex AI has no list to ask for with a key.
    let vertex = f.save("https://example.invalid/v1/publishers/google");
    let models = f.agy.list_models(&vertex, true).await.unwrap();
    assert!(models.models.is_empty());
    assert_eq!(
        models.error.as_deref(),
        Some("Vertex AI 没有模型列表接口，请在白名单填写模型 ID")
    );
}

#[tokio::test]
async fn it_runs_with_no_provider_and_answers_that_there_is_none() {
    let f = fixture().await;
    assert!(f.agy.view().providers.is_empty());
    for _ in 0..2 {
        let view = f.start().await;
        assert!(view.running && f.environment().exists());
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            f.request(
                reqwest::Method::POST,
                "/v1beta/models/gemini-fixture:streamGenerateContent?alt=sse",
            )
            .header("content-type", "application/json")
            .body(r#"{"contents":[{"role":"user","parts":[{"text":"fixture"}]}]}"#)
            .send(),
        )
        .await
        .expect("a request no provider can take is answered at once")
        .unwrap();
        assert_eq!(response.status(), 503);
        assert!(response.text().await.unwrap().contains("没有可用供应商"));
        f.agy.stop_for_exit().await.unwrap();
        assert!(!f.environment().exists());
        f.agy.resume(&f.home).await.unwrap();
        assert!(f.agy.view().running);
        f.agy.stop().await.unwrap();
        assert!(!f.environment().exists() && f.agy.view().error.is_none());
    }
    // The last provider goes while it runs, in either mode.
    let id = f.save("https://example.invalid/api");
    f.start().await;
    f.edit(Edit::Select { id: id.clone() });
    assert!(f.edit(Edit::DeleteProvider { id }).running);
    f.edit(Edit::Mode {
        mode: "auto".into(),
    });
    f.agy.stop().await.unwrap();
}

#[tokio::test]
async fn providers_come_as_gemini_addresses_and_from_nowhere_else() {
    let f = fixture().await;
    let link = |protocol| Edit::ImportLink {
        base_url: "https://example.invalid/api".into(),
        token: "fixture-key".into(),
        display_name: "Fixture".into(),
        protocol,
    };
    let revision = f.agy.view().revision;
    assert!(f
        .agy
        .edit(
            link(provider_protocol::Protocol::Responses),
            &revision,
            &f.home
        )
        .is_err());
    assert!(f.agy.edit(Edit::Import, &revision, &f.home).is_err());
    let view = f.edit(link(provider_protocol::Protocol::GeminiNative));
    assert_eq!(view.providers.len(), 1);
    assert_eq!(
        view.providers[0].protocol,
        Some(provider_protocol::Protocol::GeminiNative)
    );
    // Antigravity has no file of its own for the gateway page to edit.
    assert!(f.agy.read_config(&f.home).is_err());
    assert!(std::fs::read_dir(&f.home).unwrap().next().is_none());
}

/// An upstream that gives `answers` in turn and the last of them from then
/// on, and how often it was asked.
async fn answering(
    content_type: &'static str,
    answers: &'static [&'static str],
) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let port = server({
        let hits = hits.clone();
        move |_| {
            let turn = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                hyper::Response::builder()
                    .header("content-type", content_type)
                    .body(replay::full(answers[turn.min(answers.len() - 1)]))
                    .unwrap()
            }
        }
    })
    .await;
    (port, hits)
}

#[tokio::test]
async fn a_provider_answering_whole_never_hands_on_a_cut_gemini_response() {
    const PARTIAL: &str = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"discard-partial\"}]}}],\"responseId\":\"fixture-response\"}\n\n";
    const ARRAY: &str = r#"[{"candidates":[{"content":{"parts":[{"text":"fixture"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":5}}]"#;
    const WHOLE: &str = r#"{"candidates":[{"content":{"parts":[{"text":"fixture"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":5}}"#;
    let body = r#"{"contents":[{"role":"user","parts":[{"text":"fixture"}]}]}"#;
    let events = "/v1beta/models/gemini-fixture:streamGenerateContent?alt=sse";
    for (content_type, answers, path, status, hits, expected) in [
        // A stream cut before its finish reason is asked for again.
        (
            "text/event-stream",
            &[PARTIAL, STREAM][..],
            events,
            200,
            2,
            Some(STREAM),
        ),
        // Never whole: the stream already opened ends with Google's error.
        ("text/event-stream", &[PARTIAL][..], events, 200, 2, None),
        // A stream of a JSON array is sent on as before; a plain answer is held.
        (
            "application/json",
            &[ARRAY][..],
            "/v1beta/models/gemini-fixture:streamGenerateContent",
            200,
            1,
            Some(ARRAY),
        ),
        (
            "application/json",
            &[r#"{"candidates":[{"content""#, WHOLE][..],
            "/v1beta/models/gemini-fixture:generateContent",
            200,
            2,
            Some(WHOLE),
        ),
        (
            "application/json",
            &[r#"{"candidates":[{"content""#][..],
            "/v1beta/models/gemini-fixture:generateContent",
            502,
            2,
            None,
        ),
    ] {
        let f = fixture().await;
        let (port, asked) = answering(content_type, answers).await;
        let id = f.save(&format!("http://127.0.0.1:{port}"));
        f.edit(form(json!({
            "id": id,
            "baseUrl": format!("http://127.0.0.1:{port}"),
            "token": "",
            "bufferCompleteResponse": true,
        })));
        let mut settings = f.agy.view().settings;
        settings.max_retries = 1;
        settings.rate_limit_seconds = 1;
        settings.network_probe = false;
        f.edit(Edit::Settings { settings });
        f.start().await;
        let response = f
            .request(reqwest::Method::POST, path)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{path}");
        let text = response.text().await.unwrap();
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::SeqCst),
            hits,
            "{path} {text}"
        );
        assert!(!text.contains("discard-partial"), "{text}");
        match expected {
            Some(expected) => assert!(text.ends_with(expected), "{path} {text}"),
            None if status == 200 => {
                let failed = text
                    .split("\n\n")
                    .find_map(|event| event.trim().strip_prefix("data: "))
                    .expect("an error object");
                let failed: serde_json::Value = serde_json::from_str(failed).unwrap();
                assert_eq!(failed["error"]["code"], 502);
                assert_eq!(failed["error"]["status"], "UNAVAILABLE");
                assert!(failed["error"]["message"].is_string());
            }
            None => assert!(text.contains("vela_gateway"), "{text}"),
        }
        f.agy.stop().await.unwrap();
    }
}

#[tokio::test]
async fn a_count_of_tokens_asked_with_a_handed_out_key_is_a_request_of_that_key() {
    let f = fixture().await;
    let (usage, _events) = attach_observers(&f.temp, &f.agy);
    let counted = r#"{"totalTokens":31}"#;
    let (port, seen) = upstream("application/json", counted.into()).await;
    f.save(&format!("http://127.0.0.1:{port}"));
    f.start().await;
    let (id, key) = hand_out_key(&f.agy, &f.home, "fixture-partner");

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let address = f.agy.view().address;
    let path = "/v1beta/models/gemini-fixture:countTokens";
    let body = r#"{"contents":[{"role":"user","parts":[{"text":"fixture"}]}]}"#;
    // `agy`'s own count is no usage of its, as before. One asked with a key
    // that was handed out is a request its holder made, and it used no tokens.
    for credential in [f.token(), key.clone()] {
        let response = client
            .post(format!("{address}{path}"))
            .header("x-goog-api-key", credential)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.text().await.unwrap(), counted);
    }
    until(|| usage_records(&usage).iter().any(|record| record.completed)).await;
    let records = usage_records(&usage);
    assert_eq!(records.len(), 1);
    assert_eq!(
        (records[0].client.as_str(), records[0].source.as_str()),
        ("antigravity", "proxy")
    );
    assert_eq!(records[0].entry_key.as_ref(), Some(&id));
    let attempt = records[0].final_attempt().unwrap();
    assert_eq!(attempt.status, Some(200));
    assert_eq!(attempt.tokens.total(), None);
    assert_eq!(seen.lock().len(), 2);
}

#[tokio::test]
async fn a_handed_out_key_is_counted_under_antigravity_and_stays_out_of_the_environment_file() {
    let f = fixture().await;
    let (usage, _events) = attach_observers(&f.temp, &f.agy);
    let (port, seen) = upstream("text/event-stream", STREAM.into()).await;
    let provider = f.save(&format!("http://127.0.0.1:{port}"));
    f.edit(Edit::ModelsProvider {
        id: provider,
        allowed_models: Some(vec!["gemini-fixture".into()]),
    });
    f.start().await;
    let written = std::fs::read_to_string(f.environment()).unwrap();
    let (id, key) = hand_out_key(&f.agy, &f.home, "fixture-partner");
    // The file is what `agy` itself starts with: it names the gateway's own
    // key and is not written again for one that is handed to someone else.
    assert_eq!(std::fs::read_to_string(f.environment()).unwrap(), written);
    assert!(written.contains(&f.token()) && !written.contains(&key));

    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let address = f.agy.view().address;
    let path = "/v1beta/models/gemini-fixture:streamGenerateContent?alt=sse";
    let body = r#"{"contents":[{"role":"user","parts":[{"text":"fixture"}]}]}"#;
    // The key in the header a Gemini client sends one in, `agy`'s own the way
    // `agy` sends it, and the key the way `agy` would.
    for (n, (header, value, expected)) in [
        ("x-goog-api-key", key.clone(), Some(&id)),
        ("authorization", format!("Bearer {}", f.token()), None),
        ("authorization", format!("Bearer {key}"), Some(&id)),
    ]
    .into_iter()
    .enumerate()
    {
        let response = client
            .post(format!("{address}{path}"))
            .header(header, value)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let answered = format!("fixture-response-{}", n + 1);
        assert_eq!(
            response.text().await.unwrap(),
            STREAM.replace("fixture-response", &answered)
        );
        until(|| usage_records(&usage).iter().filter(|r| r.completed).count() == n + 1).await;
        let records = usage_records(&usage);
        let record = records
            .iter()
            .find(|record| {
                record.final_attempt().unwrap().response_id
                    == Some(crate::usage::model::safe_id(&answered))
            })
            .expect("the call that was just answered");
        assert_eq!(
            (record.client.as_str(), record.source.as_str()),
            ("antigravity", "proxy")
        );
        assert_eq!(record.entry_key.as_ref(), expected);
        assert_eq!(record.final_attempt().unwrap().tokens.input, Some(80));
    }
    {
        // Whichever key let a call in, the provider is shown its own alone.
        let seen = seen.lock();
        assert_eq!(seen.len(), 3);
        for (uri, headers, sent) in seen.iter() {
            assert_eq!(uri, path);
            assert_eq!(headers["x-goog-api-key"], "fixture-upstream");
            assert!(!headers.contains_key("authorization"));
            assert!(!headers.contains_key("x-api-key"));
            assert_eq!(sent.as_ref(), body.as_bytes());
        }
    }

    // `agy`'s metric calls are answered for whoever is let in and go nowhere,
    // and the list of models is the one `agy` is given.
    let response = client
        .post(format!("{address}/v1beta:recordClientEvents"))
        .header("x-goog-api-key", &key)
        .json(&json!({"events":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "{}");
    let response = client
        .get(format!("{address}/v1beta/models?pageSize=1000"))
        .header("x-goog-api-key", &key)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let listed: serde_json::Value = response.json().await.unwrap();
    assert_eq!(listed["models"][0]["name"], "models/gemini-fixture");
    // Gemini's generation and nothing else is spoken here: what a provider
    // keeps for its account is no path of this gateway's, whoever asks.
    for (method, path) in [
        (reqwest::Method::GET, "/v1beta/files"),
        (reqwest::Method::POST, "/v1beta/cachedContents"),
        (
            reqwest::Method::POST,
            "/v1beta/models/gemini-fixture:embedContent",
        ),
        (reqwest::Method::GET, "/v1/files"),
        (reqwest::Method::POST, "/v1/responses"),
    ] {
        for credential in [&key, &f.token()] {
            let response = client
                .request(method.clone(), format!("{address}{path}"))
                .header("x-goog-api-key", credential)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 404, "{path}");
        }
    }
    assert_eq!(seen.lock().len(), 3);
    assert_eq!(usage_records(&usage).len(), 3);

    // Taken back, the key lets nothing in, and `agy` goes on as it was.
    f.edit(Edit::DeleteEntryKey { id });
    for path in [path, "/v1beta:recordClientEvents"] {
        let response = client
            .post(format!("{address}{path}"))
            .header("x-goog-api-key", &key)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401, "{path}");
    }
    assert_eq!(
        f.request(reqwest::Method::POST, path)
            .body(body)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(seen.lock().len(), 4);
    assert_eq!(std::fs::read_to_string(f.environment()).unwrap(), written);
}
