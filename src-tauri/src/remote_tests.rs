use super::*;
use serde_json::json;
use tokio::io::DuplexStream;

const HOST: &str = "primary.example.invalid";
const SPARE: &str = "spare.example.invalid";

/// The server's end of one dialed transport.
struct Far {
    alias: String,
    runs: Program,
    stream: DuplexStream,
}

impl Far {
    async fn next(&mut self) -> Option<wire::Request> {
        wire::read(&mut self.stream).await.unwrap()
    }

    async fn answer(&mut self, id: u64, result: Value) {
        let reply = wire::Reply::Answer {
            id,
            result: Some(result),
            error: None,
        };
        wire::write(&mut self.stream, &reply).await.unwrap();
    }

    async fn capabilities(&mut self, protocol: u32) {
        let request = self.next().await.unwrap();
        assert_eq!(request.command, wire::CAPABILITIES);
        self.answer(
            request.id,
            json!({"protocol": protocol, "version": "0.41.0", "commands": ["get_state"]}),
        )
        .await;
    }

    async fn greet(&mut self) {
        self.capabilities(wire::PROTOCOL).await;
    }
}

/// How the pipes of a test behave in place of `ssh`.
#[derive(Default)]
struct Plan {
    /// Aliases whose transport does not start.
    refused: Vec<&'static str>,
    /// What every transport says once it has ended.
    said: &'static str,
}

fn pipes(plan: Plan) -> (Dialer, mpsc::UnboundedReceiver<Far>) {
    let (dialed, far) = mpsc::unbounded_channel();
    let dialer: Dialer = Arc::new(move |alias: &str, runs: Program| {
        let (near, stream) = tokio::io::duplex(64 * 1024);
        // A refused alias is still seen by the test, with an end nobody holds.
        let refused = plan.refused.contains(&alias);
        let _ = dialed.send(Far {
            alias: alias.to_string(),
            runs,
            stream,
        });
        if refused {
            return Err(failure("fixture: transport refused"));
        }
        let (reader, writer) = tokio::io::split(near);
        let said = plan.said.to_string();
        Ok(Transport {
            reader: Box::new(reader),
            writer: Box::new(writer),
            ended: Box::pin(async move { said }),
        })
    });
    (dialer, far)
}

#[derive(Default)]
struct Heard(Mutex<Vec<String>>);

impl Heard {
    fn all(&self) -> Vec<String> {
        self.0.lock().clone()
    }
}

impl Listener for Heard {
    fn event(&self, event: String, payload: Value) {
        self.0.lock().push(format!("event {event} {payload}"));
    }
    fn connected(&self, capabilities: &Capabilities) {
        self.0
            .lock()
            .push(format!("connected {}", capabilities.version));
    }
    fn offline(&self, error: &AppError) {
        self.0.lock().push(format!("offline {}", error.message));
    }
}

fn target(fallback: Option<&str>) -> Target {
    Target {
        host: HOST.into(),
        fallback: fallback.map(Into::into),
        runs: Program::Server,
    }
}

fn channel(plan: Plan) -> (Channel, mpsc::UnboundedReceiver<Far>, Arc<Heard>) {
    let (dialer, dialed) = pipes(plan);
    let heard = Arc::new(Heard::default());
    (
        Channel::new(target(None), dialer, heard.clone()),
        dialed,
        heard,
    )
}

/// Connects `channel` to a server that greets it.
async fn online(channel: &Channel, dialed: &mut mpsc::UnboundedReceiver<Far>) -> Far {
    let (capabilities, far) = tokio::join!(channel.connect(), async {
        let mut far = dialed.recv().await.unwrap();
        far.greet().await;
        far
    });
    capabilities.unwrap();
    far
}

#[test]
fn an_alias_is_a_plain_host_name_and_nothing_else() {
    for alias in ["vela", "host.example.invalid", "a_b-1", &"a".repeat(255)] {
        assert!(check_alias(alias).is_ok(), "{alias}");
    }
    for alias in [
        "",
        "-oProxyCommand=touch${IFS}/tmp/x",
        "-v",
        "two words",
        " padded",
        "host;id",
        "host\nid",
        "host\tid",
        "user@host",
        "host:22",
        "$(id)",
        "`id`",
        "a/b",
        "a\\b",
        "a'b",
        "a=b",
        "主机",
        &"a".repeat(256),
    ] {
        let error = check_alias(alias).unwrap_err();
        assert_eq!(error.code, SSH, "{alias:?}");
        assert!(arguments(alias, Program::Server).is_err(), "{alias:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_bad_alias_starts_nothing() {
    for (host, fallback) in [
        ("-oProxyCommand=id", None),
        (HOST, Some("-oProxyCommand=id")),
    ] {
        let (dialer, mut dialed) = pipes(Plan::default());
        let channel = Channel::new(
            Target {
                host: host.into(),
                fallback: fallback.map(Into::into),
                runs: Program::Server,
            },
            dialer,
            Arc::new(Silent),
        );
        assert_eq!(channel.connect().await.unwrap_err().code, SSH);
        assert_eq!(
            channel.call("get_state", json!({}), false).await.unwrap_err().code,
            SSH
        );
        assert!(dialed.try_recv().is_err());
    }
}

#[test]
fn ssh_gets_fixed_options_then_the_alias_and_one_remote_command() {
    let expected = [
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=2",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "RequestTTY=no",
        "-o",
        "RemoteCommand=none",
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPath=none",
        "--",
        HOST,
        "/usr/local/bin/vela-server rpc-stdio",
    ];
    assert_eq!(arguments(HOST, Program::Server).unwrap(), expected);

    // Only the command differs, and each is a fixed one.
    for (runs, command) in [
        (
            Program::ServerWithSudo,
            "sudo -n /usr/local/bin/vela-server rpc-stdio",
        ),
        (
            Program::MacApp,
            "/Applications/Vela.app/Contents/MacOS/vela rpc-stdio",
        ),
    ] {
        let other = arguments(HOST, runs).unwrap();
        let end = other.len() - 3;
        assert_eq!(other[..end], expected[..end]);
        assert_eq!(other[end..], ["--", HOST, command]);
    }
}

#[test]
fn the_last_words_of_ssh_fit_one_line() {
    assert_eq!(last_lines(b""), "");
    assert_eq!(
        last_lines(b"one\r\ntwo\n\n\x1b[31mthree\x07\n\tfour: denied\n"),
        "two [31mthree four: denied"
    );
    let long = last_lines("长".repeat(500).as_bytes());
    assert_eq!(long.chars().count(), 201);
    assert!(long.starts_with('…'));
}

#[tokio::test(start_paused = true)]
async fn the_handshake_asks_for_capabilities_and_reports_the_connection() {
    let (dialer, mut dialed) = pipes(Plan::default());
    let heard = Arc::new(Heard::default());
    let channel = Channel::new(
        Target {
            runs: Program::ServerWithSudo,
            ..target(None)
        },
        dialer,
        heard.clone(),
    );
    assert!(!channel.is_open());
    let (capabilities, far) = tokio::join!(channel.connect(), async {
        let mut far = dialed.recv().await.unwrap();
        far.greet().await;
        far
    });
    let capabilities = capabilities.unwrap();
    assert_eq!(capabilities.version, "0.41.0");
    assert_eq!(capabilities.commands, ["get_state"]);
    assert_eq!(
        (far.alias.as_str(), far.runs),
        (HOST, Program::ServerWithSudo)
    );
    assert!(channel.is_open());
    assert_eq!(heard.all(), ["connected 0.41.0"]);

    // An open channel answers from what it has.
    assert_eq!(channel.connect().await.unwrap().version, "0.41.0");
    assert!(dialed.try_recv().is_err());
    assert_eq!(heard.all(), ["connected 0.41.0"]);
}

#[tokio::test(start_paused = true)]
async fn another_protocol_is_refused_and_the_fallback_is_not_tried() {
    for (protocol, advice) in [
        (wire::PROTOCOL + 1, "请升级本机的 Vela"),
        (wire::PROTOCOL - 1, "请升级服务器上的 Vela"),
    ] {
        let (dialer, mut dialed) = pipes(Plan::default());
        let heard = Arc::new(Heard::default());
        let channel = Channel::new(target(Some(SPARE)), dialer, heard.clone());
        let (answer, mut far) = tokio::join!(channel.connect(), async {
            let mut far = dialed.recv().await.unwrap();
            far.capabilities(protocol).await;
            far
        });
        let error = answer.unwrap_err();
        assert_eq!(error.code, code::UNSUPPORTED);
        assert!(error.message.contains("服务器版本不兼容"), "{error}");
        assert!(error.message.contains(advice), "{error}");
        assert!(!channel.is_open());
        assert!(heard.all().is_empty());
        // The connection was dropped and no other one started.
        assert!(far.next().await.is_none());
        assert!(dialed.try_recv().is_err());
    }
}

#[tokio::test(start_paused = true)]
async fn a_silent_server_fails_the_handshake_after_fifteen_seconds() {
    let (channel, mut dialed, heard) = channel(Plan::default());
    let started = tokio::time::Instant::now();
    // Two callers wait on the one attempt and share its error.
    let (first, second, mut far) = tokio::join!(
        channel.call("get_state", json!({}), false),
        channel.call("get_state", json!({}), false),
        async { dialed.recv().await.unwrap() }
    );
    assert_eq!(started.elapsed(), HANDSHAKE);
    for error in [first.unwrap_err(), second.unwrap_err()] {
        assert_eq!(error.code, SSH);
        assert_eq!(error.message, format!("{HOST} 在 15 秒内没有应答"));
    }
    assert!(heard.all().is_empty());
    assert_eq!(far.next().await.unwrap().command, wire::CAPABILITIES);
    assert!(far.next().await.is_none());
    assert!(dialed.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn two_calls_at_once_each_get_their_own_answer() {
    let (channel, mut dialed, _) = channel(Plan::default());
    let mut far = online(&channel, &mut dialed).await;
    let (first, second, ()) = tokio::join!(
        channel.call("first", json!({"n": 1}), false),
        channel.call("second", json!({"n": 2}), false),
        async {
            let one = far.next().await.unwrap();
            let two = far.next().await.unwrap();
            assert_ne!(one.id, two.id);
            // The later request is answered first.
            for request in [two, one] {
                far.answer(
                    request.id,
                    json!({"command": request.command, "args": request.args}),
                )
                .await;
            }
        }
    );
    assert_eq!(
        first.unwrap(),
        json!({"command": "first", "args": {"n": 1}})
    );
    assert_eq!(
        second.unwrap(),
        json!({"command": "second", "args": {"n": 2}})
    );
}

#[tokio::test(start_paused = true)]
async fn only_a_call_that_says_so_is_sent_as_internal() {
    let (channel, mut dialed, _) = channel(Plan::default());
    // The messages as a server from before the word reads them: any JSON.
    let (capabilities, mut far) = tokio::join!(channel.connect(), async {
        let mut far = dialed.recv().await.unwrap();
        let handshake: Value = wire::read(&mut far.stream).await.unwrap().unwrap();
        assert_eq!(handshake["command"], wire::CAPABILITIES);
        assert!(handshake.get("internal").is_none(), "{handshake}");
        far.answer(
            handshake["id"].as_u64().unwrap(),
            json!({"protocol": wire::PROTOCOL, "version": "0.41.0", "commands": []}),
        )
        .await;
        far
    });
    capabilities.unwrap();
    for (internal, said) in [(false, None), (true, Some(json!(true)))] {
        let asked = channel.call("export_provider_copy", json!({"n": 1}), internal);
        let (answer, ()) = tokio::join!(asked, async {
            let message: Value = wire::read(&mut far.stream).await.unwrap().unwrap();
            assert_eq!(message["command"], "export_provider_copy");
            assert_eq!(message["args"], json!({"n": 1}));
            assert_eq!(message.get("internal"), said.as_ref(), "{message}");
            far.answer(message["id"].as_u64().unwrap(), json!(null))
                .await;
        });
        answer.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn answers_keep_their_numbers_null_and_the_servers_own_error() {
    let (channel, mut dialed, heard) = channel(Plan::default());
    let mut far = online(&channel, &mut dialed).await;
    let numbers = json!({"cost": 0.1, "tokens": 18446744073709551615u64, "list": [1, -2]});
    let (kept, nothing, refused, ()) = tokio::join!(
        channel.call("numbers", json!({}), false),
        channel.call("nothing", json!({}), false),
        async {
            let outcome = channel.call("refused", json!({}), false).await;
            // An error the server answered leaves the connection alone.
            assert!(channel.is_open());
            outcome
        },
        async {
            for _ in 0..3 {
                let request = far.next().await.unwrap();
                let (result, error) = match request.command.as_str() {
                    "numbers" => (Some(numbers.clone()), None),
                    "nothing" => (None, None),
                    _ => (
                        None,
                        Some(AppError::new(code::CONFLICT, "fixture conflict")),
                    ),
                };
                let reply = wire::Reply::Answer {
                    id: request.id,
                    result,
                    error,
                };
                wire::write(&mut far.stream, &reply).await.unwrap();
            }
        }
    );
    assert_eq!(kept.unwrap(), numbers);
    assert_eq!(nothing.unwrap(), Value::Null);
    let error = refused.unwrap_err();
    assert_eq!(
        (error.code.as_str(), error.message.as_str()),
        (code::CONFLICT, "fixture conflict")
    );
    assert_eq!(heard.all(), ["connected 0.41.0"]);
    assert!(dialed.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn an_event_reaches_the_listener() {
    let (channel, mut dialed, heard) = channel(Plan::default());
    let mut far = online(&channel, &mut dialed).await;
    let (answer, ()) = tokio::join!(channel.call("get_state", json!({}), false), async {
        let request = far.next().await.unwrap();
        let event = wire::Reply::Event {
            event: "gateway-state".into(),
            payload: json!({"running": true}),
        };
        wire::write(&mut far.stream, &event).await.unwrap();
        far.answer(request.id, json!(null)).await;
    });
    answer.unwrap();
    assert_eq!(
        heard.all(),
        [
            "connected 0.41.0",
            r#"event gateway-state {"running":true}"#
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn the_end_fails_every_waiting_call_and_the_next_call_reconnects() {
    let (channel, mut dialed, heard) = channel(Plan {
        said: "Connection to primary closed by remote host.",
        ..Plan::default()
    });
    let mut far = online(&channel, &mut dialed).await;
    let (first, second, ()) = tokio::join!(
        channel.call("first", json!({}), false),
        channel.call("second", json!({}), false),
        async {
            far.next().await.unwrap();
            far.next().await.unwrap();
            drop(far);
        }
    );
    let lost = format!("与 {HOST} 的连接已断开：Connection to primary closed by remote host.");
    for error in [first.unwrap_err(), second.unwrap_err()] {
        assert_eq!((error.code.as_str(), error.message.as_str()), (SSH, &*lost));
    }
    assert!(!channel.is_open());
    assert_eq!(
        heard.all(),
        ["connected 0.41.0".to_string(), format!("offline {lost}")]
    );

    let (answer, _far) = tokio::join!(channel.call("third", json!({}), false), async {
        let mut far = dialed.recv().await.unwrap();
        far.greet().await;
        let request = far.next().await.unwrap();
        assert_eq!(request.command, "third");
        far.answer(request.id, json!("again")).await;
        far
    });
    assert_eq!(answer.unwrap(), json!("again"));
    assert!(channel.is_open());
    assert_eq!(heard.all().len(), 3);
    assert_eq!(heard.all()[2], "connected 0.41.0");
}

#[tokio::test(start_paused = true)]
async fn a_request_that_was_sent_is_not_sent_again() {
    let (channel, mut dialed, _) = channel(Plan::default());
    let mut far = online(&channel, &mut dialed).await;
    let (answer, ()) = tokio::join!(channel.call("save_provider", json!({}), false), async {
        assert_eq!(far.next().await.unwrap().command, "save_provider");
        drop(far);
    });
    assert_eq!(answer.unwrap_err().code, SSH);
    // The failed call started no new connection.
    assert!(dialed.try_recv().is_err());

    let (answer, ()) = tokio::join!(channel.call("get_state", json!({}), false), async {
        let mut far = dialed.recv().await.unwrap();
        far.greet().await;
        // The new connection carries only the new request.
        let request = far.next().await.unwrap();
        assert_eq!(request.command, "get_state");
        far.answer(request.id, json!({})).await;
    });
    answer.unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_message_ends_the_connection() {
    let (channel, mut dialed, heard) = channel(Plan::default());
    let mut far = online(&channel, &mut dialed).await;
    let (answer, ()) = tokio::join!(channel.call("get_state", json!({}), false), async {
        far.next().await.unwrap();
        wire::write(&mut far.stream, &json!("neither an answer nor an event"))
            .await
            .unwrap();
    });
    let error = answer.unwrap_err();
    assert_eq!(error.code, SSH);
    assert!(error.message.ends_with("服务器的应答无法识别"), "{error}");
    assert!(!channel.is_open());
    assert_eq!(heard.all().len(), 2);
    assert!(far.next().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn a_call_gives_up_after_a_minute_and_the_connection_stays() {
    let (channel, mut dialed, heard) = channel(Plan::default());
    let mut far = online(&channel, &mut dialed).await;
    let started = tokio::time::Instant::now();
    let (answer, late) = tokio::join!(channel.call("slow", json!({}), false), async {
        far.next().await.unwrap().id
    });
    assert_eq!(started.elapsed(), CALL);
    let error = answer.unwrap_err();
    assert_eq!(error.code, SSH);
    assert_eq!(
        error.message,
        format!("{HOST} 在 60 秒内没有应答，操作可能仍在进行，请稍后刷新确认")
    );

    let (answer, ()) = tokio::join!(channel.call("quick", json!({}), false), async {
        // The late answer has nobody waiting and is dropped.
        far.answer(late, json!("late")).await;
        let request = far.next().await.unwrap();
        far.answer(request.id, json!("quick")).await;
    });
    assert_eq!(answer.unwrap(), json!("quick"));
    assert_eq!(heard.all(), ["connected 0.41.0"]);
    assert!(dialed.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn the_fallback_alias_is_tried_once_when_the_first_cannot_be_reached() {
    // The transport does not start.
    let (dialer, mut dialed) = pipes(Plan {
        refused: vec![HOST],
        ..Plan::default()
    });
    let channel = Channel::new(target(Some(SPARE)), dialer, Arc::new(Silent));
    let (capabilities, _far) = tokio::join!(channel.connect(), async {
        assert_eq!(dialed.recv().await.unwrap().alias, HOST);
        let mut far = dialed.recv().await.unwrap();
        assert_eq!(far.alias, SPARE);
        far.greet().await;
        far
    });
    capabilities.unwrap();

    // `ssh` ends before it answers.
    let (dialer, mut dialed) = pipes(Plan {
        said: "ssh: connect to host primary port 22: Operation timed out",
        ..Plan::default()
    });
    let channel = Channel::new(target(Some(SPARE)), dialer, Arc::new(Silent));
    let (capabilities, _far) = tokio::join!(channel.connect(), async {
        drop(dialed.recv().await.unwrap());
        let mut far = dialed.recv().await.unwrap();
        assert_eq!(far.alias, SPARE);
        far.greet().await;
        far
    });
    capabilities.unwrap();

    // The handshake times out.
    let (dialer, mut dialed) = pipes(Plan::default());
    let channel = Channel::new(target(Some(SPARE)), dialer, Arc::new(Silent));
    let (capabilities, _far) = tokio::join!(channel.connect(), async {
        let silent = dialed.recv().await.unwrap();
        let mut far = dialed.recv().await.unwrap();
        assert_eq!((silent.alias.as_str(), far.alias.as_str()), (HOST, SPARE));
        far.greet().await;
        far
    });
    capabilities.unwrap();
}

#[tokio::test(start_paused = true)]
async fn both_aliases_failing_reports_both_and_tries_no_third_time() {
    let (dialer, mut dialed) = pipes(Plan {
        said: "Permission denied (publickey).",
        ..Plan::default()
    });
    let heard = Arc::new(Heard::default());
    let channel = Channel::new(target(Some(SPARE)), dialer, heard.clone());
    let (capabilities, ()) = tokio::join!(channel.connect(), async {
        drop(dialed.recv().await.unwrap());
        drop(dialed.recv().await.unwrap());
    });
    let error = capabilities.unwrap_err();
    assert_eq!(error.code, SSH);
    assert_eq!(
        error.message,
        format!(
            "无法连接 {HOST}：Permission denied (publickey).；无法连接 {SPARE}：Permission denied (publickey)."
        )
    );
    assert!(heard.all().is_empty());
    assert!(dialed.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn closing_fails_waiting_calls_quietly_and_for_good() {
    let (channel, mut dialed, heard) = channel(Plan::default());
    let mut far = online(&channel, &mut dialed).await;
    let (answer, ()) = tokio::join!(channel.call("get_state", json!({}), false), async {
        far.next().await.unwrap();
        channel.close();
    });
    assert_eq!(answer.unwrap_err().code, SSH);
    assert!(far.next().await.is_none());
    assert_eq!(heard.all(), ["connected 0.41.0"]);
    assert_eq!(
        channel.call("get_state", json!({}), false).await.unwrap_err().code,
        SSH
    );
    assert!(dialed.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn dropping_the_channel_ends_the_connection() {
    let (channel, mut dialed, _) = channel(Plan::default());
    let mut far = online(&channel, &mut dialed).await;
    drop(channel);
    assert!(far.next().await.is_none());
}
