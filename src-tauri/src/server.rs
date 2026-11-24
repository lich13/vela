//! Vela with no window: this machine's services behind a Unix socket, in the
//! frames of `wire`. `serve` is the server; `rpc-stdio` and `health` are brief
//! clients of it, the first being what an SSH session runs to reach the socket.
//! A Mac's app serves the same connections on a socket of its own while it is
//! set to (`Control`), and its binary is the bridge to that one.
use crate::{
    errors::code,
    rpc,
    runtime::{Event, Runtime, Sink},
    storage::{self, AppError, Result},
    wire::{self, Capabilities, Reply, Request},
};
use parking_lot::Mutex;
use serde_json::Value;
use std::{
    future::Future,
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::UnixStream as BlockingStream,
    },
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{UnixListener, UnixStream},
    signal::unix::{signal, SignalKind},
    sync::{mpsc, Notify, Semaphore},
    task::JoinSet,
};

const DEFAULT_SOCKET: &str = "/run/vela/rpc.sock";
const SOCKET_VARIABLE: &str = "VELA_SERVER_SOCKET";
const USAGE: &str = "用法：vela-server <serve | rpc-stdio | health | --version>";
/// The events that may wait for one connection. One more and it is closed.
const EVENT_BACKLOG: usize = 256;
/// The requests of one connection that run at once. The next is read when one
/// of them is answered.
const REQUESTS_AT_ONCE: usize = 64;
const HEALTH_PATIENCE: Duration = Duration::from_secs(5);

/// What a subcommand ends with: nothing, or the reason it failed, for stderr.
type Outcome = std::result::Result<(), String>;

/// Runs the subcommand the arguments name and exits with its status.
pub fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let outcome = match arguments[..] {
        ["--version"] => {
            println!("vela-server {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["serve"] => on_tokio(serve(socket())),
        ["rpc-stdio"] => rpc_stdio(&socket()),
        ["health"] => on_tokio(health(socket())).map(|()| println!("ok")),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(reason) = &outcome {
        eprintln!("{reason}");
    }
    std::process::exit(outcome.is_err().into());
}

fn socket() -> PathBuf {
    std::env::var_os(SOCKET_VARIABLE)
        .filter(|path| !path.is_empty())
        .map_or_else(|| DEFAULT_SOCKET.into(), PathBuf::from)
}

fn on_tokio(work: impl Future<Output = Outcome>) -> Outcome {
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("无法启动运行时：{error}"))?;
    let outcome = tokio.block_on(work);
    // Work still on the blocking pool is not waited for.
    tokio.shutdown_background();
    outcome
}

fn unreachable(path: &Path, error: &std::io::Error) -> String {
    format!("无法连接 {}：{error}", path.display())
}

fn described(error: AppError) -> String {
    format!("{}：{}", error.code, error.message)
}

/// Serves until SIGTERM or SIGINT, then stops the gateways, which puts the
/// clients' files back, and removes the socket.
async fn serve(path: PathBuf) -> Outcome {
    let signals = |error| format!("无法接收终止信号：{error}");
    let mut terminate = signal(SignalKind::terminate()).map_err(signals)?;
    let mut interrupt = signal(SignalKind::interrupt()).map_err(signals)?;
    // Holding the socket is also what makes this the only server on the data.
    let listener = bind(&path)?;
    let served = async {
        let hub = Arc::new(Hub::default());
        let runtime = Runtime::new(hub.clone()).map_err(described)?;
        if let Some(error) = runtime.startup_error.lock().clone() {
            eprintln!("{}", described(error));
        }
        runtime.start().map_err(described)?;
        let server = Arc::new(Server { runtime, hub });
        eprintln!(
            "vela-server {} 正在监听 {}",
            env!("CARGO_PKG_VERSION"),
            path.display()
        );
        let asked_to_stop = async {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = interrupt.recv() => {}
            }
        };
        // Held to the end: what is asked while the gateways stop is still
        // answered.
        let _connections = server.accept(listener, asked_to_stop).await;
        let runtime = &server.runtime;
        runtime.quit_pending.store(true, Ordering::Relaxed);
        let stopped = runtime.stop_gateways().await;
        runtime.quitting.store(true, Ordering::Relaxed);
        stopped.map_err(described)
    }
    .await;
    let _ = std::fs::remove_file(&path);
    served
}

/// Listens at `path`, readable and writable by this user alone. A socket
/// nobody answers on is what an earlier run left behind and is replaced.
fn bind(path: &Path) -> std::result::Result<UnixListener, String> {
    let failed = |error: std::io::Error| format!("无法监听 {}：{error}", path.display());
    if let Some(parent) = path.parent().filter(|parent| !parent.exists()) {
        storage::private_dir(parent).map_err(described)?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(found) if !found.file_type().is_socket() => {
            return Err(format!("{} 已存在且不是套接字", path.display()));
        }
        Ok(_) if BlockingStream::connect(path).is_ok() => {
            return Err(format!("已有 Vela 服务器在监听 {}", path.display()));
        }
        Ok(_) => std::fs::remove_file(path).map_err(failed)?,
        Err(_) => {}
    }
    let listener = UnixListener::bind(path).map_err(failed)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(failed)?;
    Ok(listener)
}

/// Carries bytes between stdin, stdout and the server's socket, reading none
/// of them.
fn rpc_stdio(path: &Path) -> Outcome {
    let socket = BlockingStream::connect(path).map_err(|error| unreachable(path, &error))?;
    relay(socket, std::io::stdin(), std::io::stdout()).map_err(|error| unreachable(path, &error))
}

/// Copies `input` to the socket and the socket to `output` until either way
/// ends.
fn relay(
    socket: BlockingStream,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
) -> std::io::Result<()> {
    let outward = socket.try_clone()?;
    let (ended, either) = std::sync::mpsc::channel();
    let inward_ended = ended.clone();
    std::thread::spawn(move || {
        pump(input, outward);
        let _ = ended.send(());
    });
    std::thread::spawn(move || {
        pump(socket, output);
        let _ = inward_ended.send(());
    });
    let _ = either.recv();
    Ok(())
}

fn pump(mut from: impl Read, mut to: impl Write) {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => break,
            // Stdout holds back what has no line end, so each piece is flushed.
            Ok(read) => {
                if to
                    .write_all(&buffer[..read])
                    .and_then(|()| to.flush())
                    .is_err()
                {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

/// Succeeds when the server at `path` answers the handshake in time.
async fn health(path: PathBuf) -> Outcome {
    let asked = async {
        let mut stream = UnixStream::connect(&path)
            .await
            .map_err(|error| unreachable(&path, &error))?;
        let handshake = Request {
            id: 1,
            command: wire::CAPABILITIES.into(),
            args: Value::Null,
        };
        wire::write(&mut stream, &handshake)
            .await
            .map_err(|error| unreachable(&path, &error))?;
        match wire::read(&mut stream).await {
            Ok(Some(Reply::Answer {
                id: 1,
                result: Some(result),
                error: None,
            })) if serde_json::from_value::<Capabilities>(result.clone()).is_ok() => Ok(()),
            _ => Err("服务器没有按约定应答".to_owned()),
        }
    };
    tokio::time::timeout(HEALTH_PATIENCE, asked)
        .await
        .unwrap_or_else(|_| Err("服务器没有在 5 秒内应答".to_owned()))
}

/// A connection that has shaken hands and is told every event.
struct Listener {
    events: mpsc::Sender<Arc<Reply>>,
    /// Told when the connection is given up on.
    dropped: Arc<Notify>,
}

/// Hands every event of the runtime to the connections that listen.
#[derive(Default)]
struct Hub {
    listeners: Mutex<Vec<Listener>>,
}
impl Hub {
    fn tell(&self, event: &Event<'_>) {
        let mut listeners = self.listeners.lock();
        if listeners.is_empty() {
            return;
        }
        let Ok(payload) = serde_json::to_value(event) else {
            return;
        };
        let message = Arc::new(Reply::Event {
            event: event.name().into(),
            payload,
        });
        // A connection that reads too slowly is closed rather than told less:
        // what it shows would be stale with nothing to say so, and one that
        // connects again reads everything afresh.
        listeners.retain(|listener| {
            let kept = listener.events.try_send(message.clone()).is_ok();
            if !kept {
                listener.dropped.notify_one();
            }
            kept
        });
    }

    fn listened_to(&self) -> bool {
        let mut listeners = self.listeners.lock();
        listeners.retain(|listener| !listener.events.is_closed());
        !listeners.is_empty()
    }
}
impl Sink for Hub {
    fn emit(&self, event: Event<'_>) {
        if let Event::StartupError(error) = &event {
            eprintln!("{}", described((*error).clone()));
        }
        self.tell(&event);
    }
    fn attentive(&self) -> bool {
        self.listened_to()
    }
}

/// The socket of the app of the user whose data is in `data`.
#[cfg(all(feature = "desktop", target_os = "macos"))]
fn app_socket(data: &Path) -> PathBuf {
    data.join("rpc.sock")
}

/// What `ssh` runs on a Mac: joins its input and output to the socket of the
/// open app and answers the exit status. Opens no app.
#[cfg(all(feature = "desktop", target_os = "macos"))]
pub fn app_rpc_stdio() -> i32 {
    let socket = dirs::home_dir()
        .map(|home| app_socket(&home.join(".vela")))
        .and_then(|path| BlockingStream::connect(path).ok());
    let Some(socket) = socket else {
        eprintln!("这台 Mac 上的 Vela 没有运行，或没有开启“允许其他设备经 SSH 控制这台 Mac”");
        return 1;
    };
    relay(socket, std::io::stdin(), std::io::stdout())
        .is_err()
        .into()
}

/// The open app as the far end of another machine's channel: the server's
/// connections over the app's own runtime, while the user lets it.
#[cfg(all(feature = "desktop", target_os = "macos"))]
#[derive(Default)]
pub(crate) struct Control {
    hub: Arc<Hub>,
    /// Dropped to stop listening and to end the connections.
    listening: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}
#[cfg(all(feature = "desktop", target_os = "macos"))]
impl Control {
    /// Hands an event of the app's runtime to the connections that listen.
    pub(crate) fn tell(&self, event: &Event<'_>) {
        self.hub.tell(event);
    }

    /// Whether another machine is showing this one.
    pub(crate) fn listened_to(&self) -> bool {
        self.hub.listened_to()
    }

    /// Listens at the app's socket, or stops, which ends every connection and
    /// removes the socket. Listening needs the tokio runtime.
    pub(crate) fn set(&self, runtime: &Arc<Runtime>, on: bool) -> Result<()> {
        let path = app_socket(&runtime.data);
        let mut listening = self.listening.lock();
        if !on {
            if listening.take().is_some() {
                let _ = std::fs::remove_file(&path);
            }
            return Ok(());
        }
        if listening.is_some() {
            return Ok(());
        }
        let listener = bind(&path).map_err(|reason| AppError::new(code::IO, &reason))?;
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = Arc::new(Server {
            runtime: runtime.clone(),
            hub: self.hub.clone(),
        });
        tokio::spawn(async move {
            let asked_to_stop = async {
                let _ = stopped.await;
            };
            drop(server.accept(listener, asked_to_stop).await);
        });
        *listening = Some(stop);
        Ok(())
    }
}

struct Server {
    runtime: Arc<Runtime>,
    hub: Arc<Hub>,
}
impl Server {
    /// Takes connections until `stop` completes, then closes the socket to
    /// new ones and answers those still open, which end when dropped.
    async fn accept(
        self: &Arc<Self>,
        listener: UnixListener,
        stop: impl Future<Output = ()>,
    ) -> JoinSet<()> {
        let mut connections = JoinSet::new();
        tokio::pin!(stop);
        loop {
            tokio::select! {
                _ = &mut stop => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        connections.spawn(self.clone().connection(stream));
                    }
                    // Most likely out of descriptors, which the open
                    // connections give back as they end.
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                },
                Some(_) = connections.join_next() => {}
            }
        }
        connections
    }

    /// Answers one connection's requests, each on its own task so that none
    /// waits for another, and tells it the events from its handshake on.
    async fn connection(self: Arc<Self>, stream: impl AsyncRead + AsyncWrite + Send + 'static) {
        let (mut from, mut to) = tokio::io::split(stream);
        let (answers, mut answered) = mpsc::channel::<Reply>(REQUESTS_AT_ONCE);
        let (events, mut told) = mpsc::channel(EVENT_BACKLOG);
        let dropped = Arc::new(Notify::new());
        let mut listener = Some(Listener {
            events,
            dropped: dropped.clone(),
        });
        let writing = async move {
            loop {
                // Answers go first, so the one to the handshake precedes
                // every event.
                let written = tokio::select! {
                    biased;
                    answer = answered.recv() => match answer {
                        Some(answer) => wire::write(&mut to, &answer).await,
                        None => break,
                    },
                    Some(event) = told.recv() => wire::write(&mut to, &*event).await,
                };
                if written.is_err() {
                    break;
                }
            }
        };
        let reading = async move {
            let running = Arc::new(Semaphore::new(REQUESTS_AT_ONCE));
            while let Ok(Some(request)) = wire::read::<Request>(&mut from).await {
                if request.command == wire::CAPABILITIES {
                    let reply = self.reply(request).await;
                    let Ok(slot) = answers.reserve().await else {
                        break;
                    };
                    // No event can be handed over while the list is held, so
                    // the answer is in line ahead of the first of them.
                    let mut listeners = self.hub.listeners.lock();
                    slot.send(reply);
                    listeners.extend(listener.take());
                    continue;
                }
                let Ok(permit) = running.clone().acquire_owned().await else {
                    break;
                };
                let server = self.clone();
                let answers = answers.clone();
                tokio::spawn(async move {
                    let _ = answers.send(server.reply(request).await).await;
                    drop(permit);
                });
            }
        };
        tokio::pin!(writing);
        tokio::select! {
            _ = &mut writing => return,
            _ = dropped.notified() => return,
            _ = reading => {}
        }
        // The other side has asked its last; what is still being worked out
        // is answered before the connection closes.
        tokio::select! {
            _ = writing => {}
            _ = dropped.notified() => {}
        }
    }

    async fn reply(&self, request: Request) -> Reply {
        let (result, error) = match self.answer(&request.command, request.args).await {
            Ok(result) => (Some(result), None),
            Err(error) => (None, Some(error)),
        };
        Reply::Answer {
            id: request.id,
            result,
            error,
        }
    }

    async fn answer(&self, command: &str, args: Value) -> Result<Value> {
        if command == wire::CAPABILITIES {
            return serde_json::to_value(capabilities())
                .map_err(|_| AppError::new(code::JSON, "结果无法编码"));
        }
        if self.runtime.quit_pending.load(Ordering::Relaxed) {
            return Err(AppError::new(code::BUSY, "服务器正在停止"));
        }
        // `agy` is taken over through the shell startup files of whoever
        // sits at the machine, which a server has no business rewriting.
        if matches!(command, "start_gateway" | "stop_gateway")
            && args.get("clientId").and_then(Value::as_str) == Some("agy")
        {
            return Err(rpc::unsupported());
        }
        rpc::call(&self.runtime, command, args).await
    }
}

fn capabilities() -> Capabilities {
    Capabilities {
        protocol: wire::PROTOCOL,
        version: env!("CARGO_PKG_VERSION").into(),
        commands: rpc::names().iter().map(|name| (*name).into()).collect(),
    }
}

#[cfg(test)]
mod tests;
