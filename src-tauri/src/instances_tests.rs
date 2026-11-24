use super::*;
use crate::{remote::Transport, wire};
use serde_json::json;
use std::{fs, path::Path, time::Duration};
use tempfile::{tempdir, TempDir};
use tokio::{
    io::{DuplexStream, WriteHalf},
    task::AbortHandle,
};

const HOST: &str = "server.example.invalid";
const SPARE: &str = "spare.example.invalid";
const OTHER: &str = "other.example.invalid";
/// The shared commands of the app in these tests.
const COMMANDS: &[&str] = &["get_state", "refuse", "start_gateway"];

struct Line {
    alias: String,
    writer: Arc<tokio::sync::Mutex<WriteHalf<DuplexStream>>>,
    task: AbortHandle,
}

/// The servers behind the aliases, in place of `ssh`. Every connection
/// answers the handshake, refuses `refuse` and echoes anything else.
struct Fleet {
    /// Aliases that cannot be reached.
    down: Mutex<Vec<&'static str>>,
    protocol: Mutex<u32>,
    /// What the servers say they answer.
    commands: Mutex<Vec<&'static str>>,
    dialed: Mutex<Vec<(String, Program)>>,
    /// Every request after the handshake, as `alias command`.
    asked: Mutex<Vec<String>>,
    lines: Mutex<Vec<Line>>,
}

impl Fleet {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            down: Mutex::new(Vec::new()),
            protocol: Mutex::new(wire::PROTOCOL),
            commands: Mutex::new(vec!["get_state", "refuse"]),
            dialed: Mutex::new(Vec::new()),
            asked: Mutex::new(Vec::new()),
            lines: Mutex::new(Vec::new()),
        })
    }

    fn dialer(self: &Arc<Self>) -> Dialer {
        let fleet = self.clone();
        Arc::new(move |alias: &str, runs: Program| {
            fleet.dialed.lock().push((alias.to_string(), runs));
            if fleet.down.lock().contains(&alias) {
                return Err(AppError::new(
                    remote::SSH,
                    &format!("无法连接 {alias}：fixture"),
                ));
            }
            let (near, far) = tokio::io::duplex(64 * 1024);
            let (reader, writer) = tokio::io::split(far);
            let writer = Arc::new(tokio::sync::Mutex::new(writer));
            let task = tokio::spawn(serve(
                fleet.clone(),
                alias.to_string(),
                reader,
                writer.clone(),
            ));
            fleet.lines.lock().push(Line {
                alias: alias.to_string(),
                writer,
                task: task.abort_handle(),
            });
            let (reader, writer) = tokio::io::split(near);
            Ok(Transport {
                reader: Box::new(reader),
                writer: Box::new(writer),
                ended: Box::pin(async { "fixture: closed".to_string() }),
            })
        })
    }

    fn dialed(&self) -> Vec<String> {
        let dialed = self.dialed.lock();
        dialed.iter().map(|(alias, _)| alias.clone()).collect()
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().clone()
    }

    /// Pushes an event on the newest connection.
    async fn event(&self, event: &str, payload: Value) {
        let writer = self.lines.lock().last().unwrap().writer.clone();
        let event = wire::Reply::Event {
            event: event.into(),
            payload,
        };
        wire::write(&mut *writer.lock().await, &event)
            .await
            .unwrap();
    }

    /// Ends every connection from the servers' side.
    fn hang_up(&self) {
        for line in self.lines.lock().drain(..) {
            line.task.abort();
        }
    }

    /// The aliases whose connection the app has not ended.
    async fn connected(&self) -> Vec<String> {
        settle().await;
        let lines = self.lines.lock();
        lines
            .iter()
            .filter(|line| !line.task.is_finished())
            .map(|line| line.alias.clone())
            .collect()
    }
}

async fn serve(
    fleet: Arc<Fleet>,
    alias: String,
    mut reader: tokio::io::ReadHalf<DuplexStream>,
    writer: Arc<tokio::sync::Mutex<WriteHalf<DuplexStream>>>,
) {
    while let Ok(Some(request)) = wire::read::<wire::Request>(&mut reader).await {
        let (result, error) = match request.command.as_str() {
            wire::CAPABILITIES => {
                let capabilities = json!({
                    "protocol": *fleet.protocol.lock(),
                    "version": "0.41.0",
                    "commands": *fleet.commands.lock(),
                });
                (Some(capabilities), None)
            }
            command => {
                fleet.asked.lock().push(format!("{alias} {command}"));
                if command == "refuse" {
                    let refused = AppError {
                        line: Some(3),
                        ..AppError::new(code::CONFLICT, "fixture: refused")
                    };
                    (None, Some(refused))
                } else {
                    (Some(json!({"host": alias, "args": request.args})), None)
                }
            }
        };
        let reply = wire::Reply::Answer {
            id: request.id,
            result,
            error,
        };
        if wire::write(&mut *writer.lock().await, &reply)
            .await
            .is_err()
        {
            return;
        }
    }
}

/// Lets everything that can run without the clock run.
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

#[derive(Default)]
struct Seen {
    states: Mutex<Vec<InstanceState>>,
    events: Mutex<Vec<Value>>,
}

impl Seen {
    /// The states sent since the last look, as `active status`.
    fn take(&self) -> Vec<String> {
        let states = std::mem::take(&mut *self.states.lock());
        states.iter().map(brief).collect()
    }
}

impl Sink for Seen {
    fn state(&self, state: &InstanceState) {
        self.states.lock().push(state.clone());
    }
    fn remote(&self, event: &RemoteEvent<'_>) {
        self.events
            .lock()
            .push(serde_json::to_value(event).unwrap());
    }
}

fn brief(state: &InstanceState) -> String {
    let status = serde_json::to_value(state.connection.status).unwrap();
    format!("{} {}", state.active, status.as_str().unwrap())
}

struct Bench {
    folder: TempDir,
    fleet: Arc<Fleet>,
    seen: Arc<Seen>,
    service: Arc<Service>,
}

impl Bench {
    fn new() -> Self {
        Self::over(tempdir().unwrap())
    }

    /// The app as it starts over a data folder.
    fn over(folder: TempDir) -> Self {
        let fleet = Fleet::new();
        let seen = Arc::new(Seen::default());
        let service = Service::new(
            folder.path().to_path_buf(),
            COMMANDS,
            fleet.dialer(),
            seen.clone(),
        );
        Self {
            folder,
            fleet,
            seen,
            service,
        }
    }

    fn file(&self) -> PathBuf {
        self.folder.path().join(FILE)
    }

    fn on_disk(&self) -> Value {
        serde_json::from_slice(&fs::read(self.file()).unwrap()).unwrap()
    }

    fn state(&self) -> InstanceState {
        self.service.state().unwrap()
    }

    /// Saves a new machine and answers its id.
    async fn add(&self, name: &str, host: &str) -> String {
        let state = self
            .service
            .save(&self.state().revision, input(None, name, host))
            .await
            .unwrap();
        state.machines.last().unwrap().id.clone()
    }

    async fn select(&self, id: &str) -> Result<InstanceState> {
        self.service.select(&self.state().revision, id).await
    }
}

fn input(id: Option<&str>, name: &str, host: &str) -> MachineInput {
    MachineInput {
        id: id.map(Into::into),
        name: name.into(),
        kind: Kind::Linux,
        ssh_host: host.into(),
        fallback_host: None,
        use_sudo: false,
    }
}

fn stored(folder: &Path, content: &Value) {
    fs::write(folder.join(FILE), serde_json::to_vec(content).unwrap()).unwrap();
}

fn machine(id: &str, host: &str) -> Value {
    json!({"id": id, "name": id, "sshHost": host, "fallbackHost": null, "useSudo": false})
}

#[tokio::test(start_paused = true)]
async fn without_a_file_there_is_only_this_machine() {
    let bench = Bench::new();
    let state = bench.state();
    assert_eq!(
        serde_json::to_value(&state).unwrap(),
        json!({
            "revision": "missing",
            "active": "local",
            "machines": [],
            "connection": {"status": "local", "version": null, "error": null, "unsupported": []},
        })
    );
    assert!(!bench.file().exists());
    assert!(bench.fleet.dialed().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_machine_is_added_changed_and_removed_in_the_file() {
    let bench = Bench::new();
    let added = bench
        .service
        .save(
            "missing",
            MachineInput {
                fallback_host: Some(SPARE.into()),
                use_sudo: true,
                ..input(None, "  Tokyo  ", HOST)
            },
        )
        .await
        .unwrap();
    let id = added.machines[0].id.clone();
    assert!(uuid::Uuid::parse_str(&id).is_ok());
    let saved = json!({
        "id": id, "name": "Tokyo", "kind": "linux", "sshHost": HOST,
        "fallbackHost": SPARE, "useSudo": true,
    });
    assert_eq!(
        bench.on_disk(),
        json!({"active": "local", "machines": [saved]})
    );
    assert_eq!(
        added.revision,
        storage::digest(&fs::read(bench.file()).unwrap())
    );
    assert_eq!(added.active, LOCAL);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(bench.file()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let second = bench.add("Osaka", OTHER).await;
    assert_ne!(second, id);
    let changed = bench
        .service
        .save(
            &bench.state().revision,
            input(Some(&id), "Tokyo 2", "new.example.invalid"),
        )
        .await
        .unwrap();
    assert_ne!(changed.revision, added.revision);
    assert_eq!(
        bench.on_disk()["machines"],
        json!([
            {"id": id, "name": "Tokyo 2", "kind": "linux", "sshHost": "new.example.invalid", "fallbackHost": null, "useSudo": false},
            {"id": second, "name": "Osaka", "kind": "linux", "sshHost": OTHER, "fallbackHost": null, "useSudo": false},
        ])
    );

    let removed = bench.service.delete(&changed.revision, &id).await.unwrap();
    assert_eq!(removed.machines.len(), 1);
    assert_eq!(bench.on_disk()["machines"][0]["id"], json!(second));
    // The first read and every change went to the windows, and nothing was
    // dialed for any of it.
    assert_eq!(bench.seen.take(), ["local local"; 5]);
    assert!(bench.fleet.dialed().is_empty());

    // The next start reads the same list.
    let again = Bench::over(bench.folder);
    assert_eq!(again.state(), removed);
}

#[tokio::test(start_paused = true)]
async fn a_stale_revision_an_unknown_id_or_a_bad_field_changes_nothing() {
    let bench = Bench::new();
    let id = bench.add("Tokyo", HOST).await;
    let before = fs::read(bench.file()).unwrap();
    let revision = bench.state().revision;
    bench.seen.take();

    let stale = [
        bench
            .service
            .save("missing", input(None, "Osaka", OTHER))
            .await,
        bench.service.delete("missing", &id).await,
        bench.service.select("missing", &id).await,
        bench.service.select("missing", LOCAL).await,
    ];
    for outcome in stale {
        let error = outcome.unwrap_err();
        assert_eq!(error.code, code::CONFLICT);
        assert_eq!(error.message, "实例列表已更新，请重试");
    }

    let unknown = [
        bench
            .service
            .save(&revision, input(Some("nobody"), "Osaka", OTHER))
            .await,
        bench.service.delete(&revision, "nobody").await,
        bench.service.delete(&revision, LOCAL).await,
        bench.service.select(&revision, "nobody").await,
    ];
    for outcome in unknown {
        assert_eq!(outcome.unwrap_err().code, code::MISSING);
    }

    for (name, host, fallback, expected) in [
        ("", HOST, None, code::NAME),
        ("   ", HOST, None, code::NAME),
        (&*"名".repeat(65), HOST, None, code::NAME),
        ("Osaka", "", None, remote::SSH),
        ("Osaka", "-oProxyCommand=id", None, remote::SSH),
        ("Osaka", "two words", None, remote::SSH),
        ("Osaka", HOST, Some("host;id"), remote::SSH),
        ("Osaka", HOST, Some(""), remote::SSH),
    ] {
        let machine = MachineInput {
            fallback_host: fallback.map(Into::into),
            ..input(None, name, host)
        };
        let error = bench.service.save(&revision, machine).await.unwrap_err();
        assert_eq!(error.code, expected, "{name:?} {host:?} {fallback:?}");
    }

    assert_eq!(fs::read(bench.file()).unwrap(), before);
    assert_eq!(bench.state().revision, revision);
    assert!(bench.seen.take().is_empty());
    assert!(bench.fleet.dialed().is_empty());

    let longest = "名".repeat(64);
    let state = bench
        .service
        .save(&revision, input(None, &longest, OTHER))
        .await
        .unwrap();
    assert_eq!(state.machines[1].name, longest);
}

#[tokio::test(start_paused = true)]
async fn a_machine_that_is_not_selected_is_asked_over_a_channel_of_its_own() {
    let bench = Bench::new();
    let tokyo = bench.add("Tokyo", HOST).await;
    let osaka = bench.add("Osaka", OTHER).await;
    bench.select(&tokyo).await.unwrap();
    bench.seen.take();

    let answer = bench
        .service
        .ask(&osaka, "get_state", json!({"n": 1}))
        .await
        .unwrap();
    assert_eq!(answer, json!({"host": OTHER, "args": {"n": 1}}));
    // It was closed with the answer, and the selection never moved.
    assert_eq!(bench.fleet.connected().await, [HOST]);
    assert_eq!(bench.state().active, tokyo);
    assert!(bench.seen.take().is_empty());

    // The selected one is asked over the channel it has.
    let dialed = bench.fleet.dialed().len();
    let answer = bench.service.ask(&tokyo, "get_state", json!({})).await;
    assert_eq!(answer.unwrap()["host"], HOST);
    assert_eq!(bench.fleet.dialed().len(), dialed);

    // What the machine does not list is not sent to it.
    let error = bench
        .service
        .ask(&osaka, "start_gateway", json!({}))
        .await
        .unwrap_err();
    assert_eq!(error.code, code::UNSUPPORTED);
    assert!(!bench
        .fleet
        .asked
        .lock()
        .iter()
        .any(|asked| asked.ends_with("start_gateway")));
    assert_eq!(bench.fleet.connected().await, [HOST]);
    let unknown = bench.service.ask("nobody", "get_state", json!({})).await;
    assert_eq!(unknown.unwrap_err().code, code::MISSING);
}

#[tokio::test(start_paused = true)]
async fn a_mac_is_asked_for_its_app_and_never_for_sudo() {
    let bench = Bench::new();
    let state = bench
        .service
        .save(
            "missing",
            MachineInput {
                kind: Kind::Mac,
                use_sudo: true,
                ..input(None, "Studio", HOST)
            },
        )
        .await
        .unwrap();
    assert_eq!(bench.on_disk()["machines"][0]["kind"], "mac");
    assert_eq!(bench.on_disk()["machines"][0]["useSudo"], false);
    let id = state.machines[0].id.clone();
    bench.service.select(&state.revision, &id).await.unwrap();
    assert_eq!(
        *bench.fleet.dialed.lock(),
        [(HOST.to_string(), Program::MacApp)]
    );

    // A list written before there were Macs is of Linux servers.
    let folder = tempdir().unwrap();
    stored(
        folder.path(),
        &json!({
            "active": LOCAL,
            "machines": [{
                "id": "m1",
                "name": "Tokyo",
                "sshHost": HOST,
                "fallbackHost": null,
                "useSudo": true,
            }],
        }),
    );
    let older = Bench::over(folder);
    assert_eq!(older.state().machines[0].kind, Kind::Linux);
}

#[tokio::test(start_paused = true)]
async fn a_server_is_selected_once_it_answered_the_handshake() {
    let bench = Bench::new();
    let state = bench
        .service
        .save(
            "missing",
            MachineInput {
                use_sudo: true,
                ..input(None, "Tokyo", HOST)
            },
        )
        .await
        .unwrap();
    let id = state.machines[0].id.clone();
    bench.seen.take();

    let selected = bench.service.select(&state.revision, &id).await.unwrap();
    assert_eq!(selected.active, id);
    assert_ne!(selected.revision, state.revision);
    assert_eq!(
        serde_json::to_value(&selected.connection).unwrap(),
        json!({
            "status": "connected",
            "version": "0.41.0",
            "error": null,
            "unsupported": ["start_gateway"],
        })
    );
    assert_eq!(bench.on_disk()["active"], json!(id));
    assert_eq!(
        *bench.fleet.dialed.lock(),
        [(HOST.to_string(), Program::ServerWithSudo)]
    );
    assert_eq!(bench.seen.take(), [format!("{id} connected")]);
    assert_eq!(bench.state(), selected);

    // Selecting it again while it is connected changes nothing.
    assert_eq!(bench.select(&id).await.unwrap(), selected);
    assert!(bench.seen.take().is_empty());
    assert_eq!(bench.fleet.dialed().len(), 1);

    // This machine is selected at once, and the connection ends.
    let local = bench.select(LOCAL).await.unwrap();
    assert_eq!(local.active, LOCAL);
    assert_eq!(local.connection, Connection::new(Status::Local));
    assert_eq!(bench.on_disk()["active"], json!(LOCAL));
    assert_eq!(bench.seen.take(), ["local local"]);
    assert!(bench.fleet.connected().await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_server_that_does_not_answer_is_not_selected() {
    let bench = Bench::new();
    let tokyo = bench.add("Tokyo", HOST).await;
    let osaka = bench.add("Osaka", OTHER).await;
    *bench.fleet.down.lock() = vec![OTHER];

    // From this machine.
    let before = fs::read(bench.file()).unwrap();
    bench.seen.take();
    let error = bench.select(&osaka).await.unwrap_err();
    assert_eq!(error.code, remote::SSH);
    assert!(error.message.contains(OTHER), "{error}");
    assert_eq!(bench.state().active, LOCAL);
    assert_eq!(fs::read(bench.file()).unwrap(), before);
    assert!(bench.seen.take().is_empty());

    // From another server, which stays selected, connected and in use.
    let selected = bench.select(&tokyo).await.unwrap();
    let before = fs::read(bench.file()).unwrap();
    bench.seen.take();
    assert_eq!(bench.select(&osaka).await.unwrap_err().code, remote::SSH);
    assert_eq!(bench.state(), selected);
    assert_eq!(fs::read(bench.file()).unwrap(), before);
    assert!(bench.seen.take().is_empty());
    assert_eq!(bench.fleet.connected().await, [HOST]);
    let answer = bench.service.call(&tokyo, "get_state", json!({})).await;
    assert_eq!(answer.unwrap()["host"], json!(HOST));

    // A server of another protocol is not selected either.
    *bench.fleet.down.lock() = Vec::new();
    *bench.fleet.protocol.lock() = wire::PROTOCOL + 1;
    let error = bench.select(&osaka).await.unwrap_err();
    assert_eq!(error.code, code::UNSUPPORTED);
    assert!(error.message.contains("服务器版本不兼容"), "{error}");
    assert_eq!(bench.state(), selected);
    assert_eq!(bench.fleet.connected().await, [HOST]);

    // Once it answers, the former server's connection ends.
    *bench.fleet.protocol.lock() = wire::PROTOCOL;
    let moved = bench.select(&osaka).await.unwrap();
    assert_eq!(moved.active, osaka);
    assert_eq!(bench.on_disk()["active"], json!(osaka));
    assert_eq!(bench.seen.take(), [format!("{osaka} connected")]);
    assert_eq!(bench.fleet.connected().await, [OTHER]);
}

#[tokio::test(start_paused = true)]
async fn the_fallback_alias_reaches_a_server_whose_first_alias_is_down() {
    let bench = Bench::new();
    let state = bench
        .service
        .save(
            "missing",
            MachineInput {
                fallback_host: Some(SPARE.into()),
                ..input(None, "Tokyo", HOST)
            },
        )
        .await
        .unwrap();
    *bench.fleet.down.lock() = vec![HOST];
    let id = state.machines[0].id.clone();
    let selected = bench.service.select(&state.revision, &id).await.unwrap();
    assert_eq!(selected.connection.status, Status::Connected);
    assert_eq!(bench.fleet.dialed(), [HOST, SPARE]);
}

#[tokio::test(start_paused = true)]
async fn a_lost_connection_is_reported_once_and_selecting_again_retries() {
    let bench = Bench::new();
    let id = bench.add("Tokyo", HOST).await;
    let selected = bench.select(&id).await.unwrap();
    bench.seen.take();

    bench.fleet.hang_up();
    settle().await;
    let states = std::mem::take(&mut *bench.seen.states.lock());
    assert_eq!(states.len(), 1);
    let offline = &states[0];
    assert_eq!(
        (offline.active.as_str(), offline.revision.as_str()),
        (&*id, &*selected.revision)
    );
    assert_eq!(
        serde_json::to_value(&offline.connection).unwrap(),
        json!({
            "status": "offline",
            "version": "0.41.0",
            "error": format!("与 {HOST} 的连接已断开：fixture: closed"),
            "unsupported": ["start_gateway"],
        })
    );
    assert_eq!(&bench.state(), offline);

    // The retry that fails says why and leaves the server selected.
    *bench.fleet.down.lock() = vec![HOST];
    let error = bench.select(&id).await.unwrap_err();
    assert_eq!(error.code, remote::SSH);
    assert_eq!(
        bench.seen.take(),
        [format!("{id} connecting"), format!("{id} offline")]
    );
    let state = bench.state();
    assert_eq!(state.active, id);
    assert_eq!(state.connection.error, Some(error.message));
    assert_eq!(bench.on_disk()["active"], json!(id));

    *bench.fleet.down.lock() = Vec::new();
    let again = bench.select(&id).await.unwrap();
    assert_eq!(again, selected);
    assert_eq!(
        bench.seen.take(),
        [format!("{id} connecting"), format!("{id} connected")]
    );
    assert_eq!(bench.fleet.dialed().len(), 3);
}

#[tokio::test(start_paused = true)]
async fn removing_the_selected_server_selects_this_machine() {
    let bench = Bench::new();
    let tokyo = bench.add("Tokyo", HOST).await;
    let osaka = bench.add("Osaka", OTHER).await;
    let selected = bench.select(&tokyo).await.unwrap();

    // Another machine goes without touching the connection.
    let state = bench
        .service
        .delete(&selected.revision, &osaka)
        .await
        .unwrap();
    assert_eq!(state.active, tokyo);
    assert_eq!(state.connection, selected.connection);
    assert_eq!(bench.fleet.connected().await, [HOST]);
    bench.seen.take();

    let state = bench.service.delete(&state.revision, &tokyo).await.unwrap();
    assert_eq!(state.active, LOCAL);
    assert!(state.machines.is_empty());
    assert_eq!(state.connection, Connection::new(Status::Local));
    assert_eq!(bench.on_disk(), json!({"active": "local", "machines": []}));
    assert_eq!(bench.seen.take(), ["local local"]);
    assert!(bench.fleet.connected().await.is_empty());
    let error = bench.service.call(&tokyo, "get_state", json!({})).await;
    assert_eq!(error.unwrap_err().code, code::STALE);
}

#[tokio::test(start_paused = true)]
async fn a_new_address_for_the_selected_server_ends_its_connection() {
    let bench = Bench::new();
    let id = bench.add("Tokyo", HOST).await;
    let selected = bench.select(&id).await.unwrap();
    bench.seen.take();

    // A new name is not a new address.
    let renamed = bench
        .service
        .save(&selected.revision, input(Some(&id), "Tokyo 2", HOST))
        .await
        .unwrap();
    assert_eq!(renamed.connection, selected.connection);
    assert_eq!(bench.fleet.connected().await, [HOST]);
    assert_eq!(bench.seen.take(), [format!("{id} connected")]);

    let moved = bench
        .service
        .save(&renamed.revision, input(Some(&id), "Tokyo 2", OTHER))
        .await
        .unwrap();
    assert_eq!(moved.active, id);
    assert_eq!(
        serde_json::to_value(&moved.connection).unwrap(),
        json!({
            "status": "offline",
            "version": null,
            "error": "连接设置已修改，请重新连接",
            "unsupported": [],
        })
    );
    assert_eq!(bench.seen.take(), [format!("{id} offline")]);
    assert!(bench.fleet.connected().await.is_empty());
    // The ended connection reports nothing of its own.
    assert!(bench.seen.take().is_empty());

    // The next call connects to the new address.
    let answer = bench.service.call(&id, "get_state", json!({})).await;
    assert_eq!(answer.unwrap()["host"], json!(OTHER));
    assert_eq!(bench.seen.take(), [format!("{id} connected")]);
    assert_eq!(bench.fleet.dialed(), [HOST, OTHER]);
}

#[tokio::test(start_paused = true)]
async fn a_call_goes_to_the_selected_server_and_nowhere_else() {
    let bench = Bench::new();
    let tokyo = bench.add("Tokyo", HOST).await;
    let osaka = bench.add("Osaka", OTHER).await;

    // Nothing is selected yet.
    for instance in [LOCAL, &*tokyo] {
        let error = bench
            .service
            .call(instance, "get_state", json!({}))
            .await
            .unwrap_err();
        assert_eq!(error.code, code::STALE);
        assert_eq!(error.message, "实例已切换，请重试");
    }
    assert!(bench.fleet.dialed().is_empty());

    bench.select(&tokyo).await.unwrap();
    let args = json!({"clientId": "codex", "cost": 0.1});
    let answer = bench.service.call(&tokyo, "get_state", args.clone()).await;
    assert_eq!(answer.unwrap(), json!({"host": HOST, "args": args}));

    // The server's own error comes back as it was.
    let error = bench
        .service
        .call(&tokyo, "refuse", json!({}))
        .await
        .unwrap_err();
    assert_eq!(
        serde_json::to_value(&error).unwrap(),
        json!({"code": "CONFLICT", "message": "fixture: refused", "line": 3, "column": null})
    );

    // What the server does not answer is not sent.
    let error = bench
        .service
        .call(&tokyo, "start_gateway", json!({}))
        .await
        .unwrap_err();
    assert_eq!(error.code, code::UNSUPPORTED);

    for instance in [LOCAL, &*osaka, "nobody"] {
        let error = bench.service.call(instance, "get_state", json!({})).await;
        assert_eq!(error.unwrap_err().code, code::STALE);
    }
    assert_eq!(
        bench.fleet.asked(),
        [format!("{HOST} get_state"), format!("{HOST} refuse")]
    );

    // After the switch the former instance is refused.
    bench.select(&osaka).await.unwrap();
    let error = bench.service.call(&tokyo, "get_state", json!({})).await;
    assert_eq!(error.unwrap_err().code, code::STALE);
    let answer = bench.service.call(&osaka, "get_state", json!({})).await;
    assert_eq!(answer.unwrap()["host"], json!(OTHER));
}

#[tokio::test(start_paused = true)]
async fn a_call_reconnects_an_offline_server_and_learns_its_commands_again() {
    let bench = Bench::new();
    let id = bench.add("Tokyo", HOST).await;
    bench.select(&id).await.unwrap();
    bench.fleet.hang_up();
    settle().await;
    bench.seen.take();

    // Still down: the call fails, and the state says why.
    *bench.fleet.down.lock() = vec![HOST];
    let error = bench
        .service
        .call(&id, "get_state", json!({}))
        .await
        .unwrap_err();
    assert_eq!(error.code, remote::SSH);
    assert_eq!(bench.seen.take(), [format!("{id} offline")]);
    assert_eq!(bench.state().connection.error, Some(error.message));
    // The same failure again is not news.
    assert!(bench
        .service
        .call(&id, "get_state", json!({}))
        .await
        .is_err());
    assert!(bench.seen.take().is_empty());

    // Back, and upgraded meanwhile.
    *bench.fleet.down.lock() = Vec::new();
    *bench.fleet.commands.lock() = vec!["refuse", "start_gateway"];
    // Known as unsupported from the last connection, so not even tried.
    let error = bench.service.call(&id, "start_gateway", json!({})).await;
    assert_eq!(error.unwrap_err().code, code::UNSUPPORTED);
    assert_eq!(bench.fleet.dialed().len(), 3);
    // Unsupported since this connection, so connected but not sent.
    let error = bench.service.call(&id, "get_state", json!({})).await;
    assert_eq!(error.unwrap_err().code, code::UNSUPPORTED);
    assert_eq!(bench.seen.take(), [format!("{id} connected")]);
    assert_eq!(bench.state().connection.unsupported, ["get_state"]);
    let answer = bench.service.call(&id, "start_gateway", json!({})).await;
    assert_eq!(answer.unwrap()["host"], json!(HOST));
    assert_eq!(bench.fleet.asked(), [format!("{HOST} start_gateway")]);
    assert_eq!(bench.fleet.dialed().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn a_servers_event_goes_out_under_its_machine() {
    let bench = Bench::new();
    let tokyo = bench.add("Tokyo", HOST).await;
    let osaka = bench.add("Osaka", OTHER).await;
    bench.select(&tokyo).await.unwrap();
    bench
        .fleet
        .event("gateway-state", json!({"running": true}))
        .await;
    settle().await;
    assert_eq!(
        *bench.seen.events.lock(),
        [json!({"instance": tokyo, "event": "gateway-state", "payload": {"running": true}})]
    );

    bench.select(&osaka).await.unwrap();
    bench.fleet.event("switch-state", json!(null)).await;
    settle().await;
    assert_eq!(bench.seen.events.lock().len(), 2);
    assert_eq!(
        bench.seen.events.lock()[1],
        json!({"instance": osaka, "event": "switch-state", "payload": null})
    );
}

#[tokio::test(start_paused = true)]
async fn testing_an_address_leaves_the_selection_alone() {
    let bench = Bench::new();
    let id = bench.add("Tokyo", HOST).await;
    let selected = bench.select(&id).await.unwrap();
    bench.seen.take();

    let target = |host: &str, fallback: Option<&str>| Target {
        host: host.into(),
        fallback: fallback.map(Into::into),
        runs: Program::ServerWithSudo,
    };
    let tested = bench.service.test(target(OTHER, None)).await.unwrap();
    assert_eq!(
        serde_json::to_value(&tested).unwrap(),
        json!({"version": "0.41.0"})
    );
    assert_eq!(
        bench.fleet.dialed.lock()[1],
        (OTHER.to_string(), Program::ServerWithSudo)
    );
    // Only the selected server's connection is left.
    assert_eq!(bench.fleet.connected().await, [HOST]);

    *bench.fleet.down.lock() = vec![OTHER];
    let error = bench.service.test(target(OTHER, None)).await.unwrap_err();
    assert_eq!(error.code, remote::SSH);
    bench
        .service
        .test(target(OTHER, Some(SPARE)))
        .await
        .unwrap();
    let error = bench.service.test(target("host;id", None)).await;
    assert_eq!(error.unwrap_err().code, remote::SSH);

    assert_eq!(bench.state(), selected);
    assert!(bench.seen.take().is_empty());
    assert_eq!(bench.fleet.connected().await, [HOST]);
}

#[tokio::test(start_paused = true)]
async fn a_start_with_a_server_selected_connects_in_the_background() {
    let folder = tempdir().unwrap();
    let content = json!({"active": "m1", "machines": [machine("m1", HOST)]});
    stored(folder.path(), &content);
    let bench = Bench::over(folder);
    bench.service.start();
    let state = bench.state();
    assert_eq!(brief(&state), "m1 connecting");
    assert_eq!(state.connection, Connection::new(Status::Connecting));
    assert_eq!(
        state.revision,
        storage::digest(&fs::read(bench.file()).unwrap())
    );

    settle().await;
    assert_eq!(bench.seen.take(), ["m1 connecting", "m1 connected"]);
    assert_eq!(bench.state().connection.version.as_deref(), Some("0.41.0"));
    let answer = bench.service.call("m1", "get_state", json!({})).await;
    assert_eq!(answer.unwrap()["host"], json!(HOST));
    assert_eq!(bench.fleet.dialed(), [HOST]);
    assert_eq!(bench.on_disk(), content);
}

#[tokio::test(start_paused = true)]
async fn a_start_that_cannot_reach_the_selected_server_keeps_it_selected() {
    let folder = tempdir().unwrap();
    let content = json!({"active": "m1", "machines": [machine("m1", HOST)]});
    stored(folder.path(), &content);
    let bench = Bench::over(folder);
    *bench.fleet.down.lock() = vec![HOST];
    bench.service.start();
    settle().await;
    assert_eq!(bench.seen.take(), ["m1 connecting", "m1 offline"]);
    let state = bench.state();
    assert_eq!(state.active, "m1");
    assert_eq!(
        state.connection.error.as_deref(),
        Some(&*format!("无法连接 {HOST}：fixture"))
    );
    assert_eq!(bench.on_disk(), content);

    // The retry is the same selection again.
    *bench.fleet.down.lock() = Vec::new();
    let retried = bench.select("m1").await.unwrap();
    assert_eq!(retried.connection.status, Status::Connected);
    assert_eq!(bench.seen.take(), ["m1 connecting", "m1 connected"]);
}

#[tokio::test(start_paused = true)]
async fn a_silent_server_at_start_is_offline_after_the_handshake_wait() {
    let folder = tempdir().unwrap();
    stored(
        folder.path(),
        &json!({"active": "m1", "machines": [machine("m1", HOST)]}),
    );
    let bench = Bench::over(folder);
    // A connection nobody serves.
    let held = Arc::new(Mutex::new(Vec::new()));
    let service = Service::new(
        bench.folder.path().to_path_buf(),
        COMMANDS,
        {
            let held = held.clone();
            Arc::new(move |_: &str, _: Program| {
                let (near, far) = tokio::io::duplex(1024);
                held.lock().push(far);
                let (reader, writer) = tokio::io::split(near);
                Ok(Transport {
                    reader: Box::new(reader),
                    writer: Box::new(writer),
                    ended: Box::pin(std::future::pending()),
                })
            })
        },
        bench.seen.clone(),
    );
    service.start();
    settle().await;
    assert_eq!(bench.seen.take(), ["m1 connecting"]);
    tokio::time::sleep(Duration::from_secs(16)).await;
    assert_eq!(bench.seen.take(), ["m1 offline"]);
    assert_eq!(service.state().unwrap().active, "m1");
}

#[tokio::test(start_paused = true)]
async fn a_damaged_file_is_an_error_until_it_is_repaired() {
    for content in [
        "{not json",
        "[]",
        r#"{"active": "local"}"#,
        r#"{"active": "local", "machines": [{"id": "m1"}]}"#,
        // Selects a machine the list does not have.
        r#"{"active": "m1", "machines": []}"#,
    ] {
        let folder = tempdir().unwrap();
        fs::write(folder.path().join(FILE), content).unwrap();
        let bench = Bench::over(folder);
        bench.service.start();
        let outcomes = [
            bench.service.state().map(|_| ()),
            bench
                .service
                .save("missing", input(None, "Tokyo", HOST))
                .await
                .map(|_| ()),
            bench.service.delete("missing", "m1").await.map(|_| ()),
            bench.service.select("missing", LOCAL).await.map(|_| ()),
            bench
                .service
                .call("m1", "get_state", json!({}))
                .await
                .map(|_| ()),
        ];
        for outcome in outcomes {
            let error = outcome.unwrap_err();
            assert_eq!(error.code, code::SETTINGS, "{content}");
            assert!(error.message.contains("instances.json"), "{error}");
        }
        assert_eq!(fs::read_to_string(bench.file()).unwrap(), content);
        assert!(bench.seen.take().is_empty());
        assert!(bench.fleet.dialed().is_empty());

        // Repaired by hand, it is read by the next command.
        stored(
            bench.folder.path(),
            &json!({"active": "local", "machines": [machine("m1", HOST)]}),
        );
        assert_eq!(bench.state().machines[0].ssh_host, HOST);
        assert_eq!(bench.seen.take(), ["local local"]);
    }
}
