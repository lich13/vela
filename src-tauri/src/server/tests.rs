use super::*;
use serde_json::json;

/// A server over a home directory of its own, listening on a socket in it.
struct Fixture {
    server: Arc<Server>,
    socket: PathBuf,
    _home: tempfile::TempDir,
}

/// Every directory the services look into is under the temporary home, and
/// nothing runs in the background: `start` is never called.
async fn fixture() -> Fixture {
    let home = tempfile::tempdir().unwrap();
    let root = home.path();
    let client = |name: &str| root.join(name).to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join(".vela")).unwrap();
    std::fs::write(
        root.join(".vela/accounts.json"),
        json!({
            "schema": 1,
            "profiles": [],
            "preferences": {
                "codexHome": client(".codex"),
                "claudeHome": client(".claude"),
                "grokHome": client(".grok"),
                "dshHome": client(".dsh"),
                "cliPath": "",
                "theme": "system",
            },
        })
        .to_string(),
    )
    .unwrap();
    let hub = Arc::new(Hub::default());
    let runtime = Runtime::at(hub.clone(), root.to_path_buf(), root.join(".codex")).unwrap();
    let server = Arc::new(Server { runtime, hub });
    let socket = root.join("rpc.sock");
    let listener = bind(&socket).unwrap();
    let accepting = server.clone();
    tokio::spawn(async move {
        accepting
            .accept(listener, std::future::pending::<()>())
            .await
    });
    Fixture {
        server,
        socket,
        _home: home,
    }
}

struct Client(UnixStream);
impl Client {
    async fn connect(fixture: &Fixture) -> Self {
        Self(UnixStream::connect(&fixture.socket).await.unwrap())
    }
    async fn ask(&mut self, id: u64, command: &str, args: Value) {
        let request = Request {
            id,
            command: command.into(),
            args,
        };
        wire::write(&mut self.0, &request).await.unwrap();
    }
    /// The next message, or `None` once the server has closed the connection.
    async fn next(&mut self) -> Option<Reply> {
        tokio::time::timeout(Duration::from_secs(20), wire::read(&mut self.0))
            .await
            .expect("the server says something in time")
            .unwrap_or(None)
    }
    /// The next answer and the names of the events that came before it.
    async fn answer(&mut self) -> (u64, std::result::Result<Value, AppError>, Vec<String>) {
        let mut events = Vec::new();
        loop {
            match self.next().await.expect("the connection stays open") {
                Reply::Event { event, .. } => events.push(event),
                Reply::Answer { id, result, error } => {
                    let outcome = error.map_or(Ok(result.unwrap_or(Value::Null)), Err);
                    return (id, outcome, events);
                }
            }
        }
    }
    async fn call(&mut self, command: &str, args: Value) -> std::result::Result<Value, AppError> {
        self.ask(9, command, args).await;
        self.answer().await.1
    }
    async fn shake_hands(&mut self) -> Capabilities {
        let result = self.call(wire::CAPABILITIES, Value::Null).await.unwrap();
        serde_json::from_value(result).unwrap()
    }
}

#[tokio::test]
async fn the_handshake_names_the_protocol_the_version_and_every_command() {
    let fixture = fixture().await;
    let mut client = Client::connect(&fixture).await;
    let capabilities = client.shake_hands().await;
    assert_eq!(capabilities.protocol, wire::PROTOCOL);
    assert_eq!(capabilities.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(capabilities.commands, rpc::names());
    assert!(fixture.server.hub.attentive());
    drop(client);
    for _ in 0..200 {
        if !fixture.server.hub.attentive() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("a closed connection still counts as someone watching");
}

#[tokio::test]
async fn a_change_is_answered_and_told_to_every_connection_that_shook_hands() {
    let fixture = fixture().await;
    let mut writer = Client::connect(&fixture).await;
    let mut watcher = Client::connect(&fixture).await;
    let mut stranger = Client::connect(&fixture).await;
    writer.shake_hands().await;
    watcher.shake_hands().await;
    // A connection is known to the server once it has been answered.
    let frozen = stranger.call("get_gateway_freeze", json!({})).await;
    assert!(frozen.is_ok(), "{frozen:?}");

    let gateway = writer
        .call("get_gateway", json!({"clientId": "claude"}))
        .await
        .unwrap();
    assert_eq!(gateway["running"], false, "{gateway}");

    writer
        .ask(
            2,
            "set_quota_refresh",
            json!({"seconds": 120, "expectedSeconds": 60}),
        )
        .await;
    // The event and the answer come in either order.
    let (mut answered, mut told) = (false, false);
    while !(answered && told) {
        match writer.next().await.unwrap() {
            Reply::Answer { id, result, error } => {
                assert!(error.is_none(), "{error:?}");
                assert_eq!(id, 2);
                assert_eq!(result.unwrap()["preferences"]["quotaRefreshSeconds"], 120);
                answered = true;
            }
            Reply::Event { event, payload } => {
                assert_eq!(event, "switch-state");
                assert_eq!(payload["preferences"]["quotaRefreshSeconds"], 120);
                told = true;
            }
        }
    }
    let Some(Reply::Event { event, payload }) = watcher.next().await else {
        panic!("the other connection is told too");
    };
    assert_eq!(event, "switch-state");
    assert_eq!(payload["preferences"]["quotaRefreshSeconds"], 120);
    assert_eq!(
        fixture
            .server
            .runtime
            .state()
            .unwrap()
            .preferences
            .quota_refresh_seconds,
        120
    );

    // One that never shook hands was told nothing: its next message is the
    // answer to its handshake.
    stranger.ask(3, wire::CAPABILITIES, Value::Null).await;
    assert!(matches!(
        stranger.next().await,
        Some(Reply::Answer { id: 3, .. })
    ));
}

#[tokio::test]
async fn an_unknown_command_and_the_agy_gateway_are_unsupported() {
    let fixture = fixture().await;
    let mut client = Client::connect(&fixture).await;
    for (command, args) in [
        ("no_such_command", json!({})),
        (
            "start_gateway",
            json!({"clientId": "agy", "expectedRevision": ""}),
        ),
        ("stop_gateway", json!({"clientId": "agy"})),
    ] {
        let error = client.call(command, args).await.unwrap_err();
        assert_eq!(error.code, code::UNSUPPORTED, "{command}");
    }
    // The rest of what concerns `agy` is answered as for any client.
    let gateway = client.call("get_gateway", json!({"clientId": "agy"})).await;
    assert!(gateway.is_ok(), "{gateway:?}");
    let missing = client.call("get_gateway", json!({})).await.unwrap_err();
    assert_eq!(missing.message, "缺少参数 clientId");
}

#[tokio::test]
async fn a_request_that_waits_does_not_hold_up_the_next() {
    let fixture = fixture().await;
    let mut client = Client::connect(&fixture).await;
    let held = fixture.server.runtime.official_tx.lock().await;
    client.ask(1, "disable_official_account", json!({})).await;
    client.ask(2, "get_gateway_freeze", json!({})).await;
    let (first, outcome, _) = client.answer().await;
    assert_eq!(first, 2, "{outcome:?}");
    drop(held);
    assert_eq!(client.answer().await.0, 1);
}

#[tokio::test]
async fn a_connection_that_stops_reading_is_closed_and_the_others_miss_nothing() {
    let fixture = fixture().await;
    let mut stalled = Client::connect(&fixture).await;
    let mut reading = Client::connect(&fixture).await;
    stalled.shake_hands().await;
    reading.shake_hands().await;
    let listeners = || fixture.server.hub.listeners.lock().len();
    assert_eq!(listeners(), 2);

    let notice = "x".repeat(4096);
    let mut sent = 0;
    while listeners() == 2 {
        assert!(sent < 4096, "the stalled connection is never given up on");
        fixture.server.runtime.notice(&notice);
        sent += 1;
        // The other connection keeps up, one event at a time.
        let Some(Reply::Event { event, payload }) = reading.next().await else {
            panic!("the reading connection was closed");
        };
        assert_eq!(
            (event.as_str(), payload.as_str()),
            ("switch-notice", Some(&*notice))
        );
    }
    assert!(sent > EVENT_BACKLOG, "given up on after {sent} events");

    // What the stalled one had been sent is still there to read, then the end.
    let mut received = 0;
    while stalled.next().await.is_some() {
        received += 1;
    }
    assert!(received < sent, "{received} of {sent}");
    let frozen = reading.call("get_gateway_freeze", json!({})).await;
    assert!(frozen.is_ok(), "{frozen:?}");
}

#[tokio::test]
async fn the_socket_is_private_and_only_a_dead_one_is_replaced() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("made/rpc.sock");
    let listener = bind(&path).unwrap();
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);

    let taken = bind(&path).unwrap_err();
    assert!(taken.contains("已有 Vela 服务器"), "{taken}");
    drop(listener);
    assert!(path.exists(), "a listener leaves its socket behind");
    drop(bind(&path).unwrap());

    let file = directory.path().join("notes.txt");
    std::fs::write(&file, "kept").unwrap();
    assert!(bind(&file).is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "kept");
}

#[tokio::test]
async fn health_is_the_handshake_answered() {
    let fixture = fixture().await;
    assert_eq!(health(fixture.socket.clone()).await, Ok(()));
    let nowhere = fixture.socket.with_file_name("absent.sock");
    let reason = health(nowhere).await.unwrap_err();
    assert!(reason.starts_with("无法连接"), "{reason}");

    // Something that listens and never answers the handshake properly.
    let mute = fixture.socket.with_file_name("mute.sock");
    let listener = bind(&mute).unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _: Option<Request> = wire::read(&mut stream).await.unwrap();
        let reply = Reply::Answer {
            id: 1,
            result: None,
            error: Some(rpc::unsupported()),
        };
        wire::write(&mut stream, &reply).await.unwrap();
    });
    assert_eq!(health(mute).await.unwrap_err(), "服务器没有按约定应答");
}

#[test]
fn the_relay_carries_bytes_both_ways_until_one_way_ends() {
    let (near, mut far) = BlockingStream::pair().unwrap();
    let (input, mut typed) = BlockingStream::pair().unwrap();
    let (output, mut shown) = BlockingStream::pair().unwrap();
    let relaying = std::thread::spawn(move || relay(near, input, output));

    // Bytes that are no message at all pass as they are.
    let outward = [0, 0, 0, 9, b'{', 0xff, 0, b'\n', 7];
    typed.write_all(&outward).unwrap();
    let mut arrived = [0; 9];
    far.read_exact(&mut arrived).unwrap();
    assert_eq!(arrived, outward);

    far.write_all(b"back\0again").unwrap();
    let mut returned = [0; 10];
    shown.read_exact(&mut returned).unwrap();
    assert_eq!(&returned, b"back\0again");

    drop(typed);
    relaying.join().unwrap().unwrap();
}
