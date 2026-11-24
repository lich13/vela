//! One SSH channel to another machine's Vela. The system `ssh` runs
//! `rpc-stdio` there, of the Linux server or of the Mac app, which joins its
//! input and output to that Vela's socket, and the messages of `wire.rs`
//! cross it.
use crate::{
    errors::code,
    storage::{AppError, Result},
    wire::{self, Capabilities},
};
use parking_lot::Mutex;
use serde_json::Value;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Weak,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    sync::{mpsc, oneshot, Notify},
    task::AbortHandle,
};

/// The code of every failure of the channel itself, as opposed to an error
/// the server answered.
pub(crate) const SSH: &str = "SSH";
const SERVER: &str = "/usr/local/bin/vela-server rpc-stdio";
/// The app where the macOS installer puts it.
const MAC_APP: &str = "/Applications/Vela.app/Contents/MacOS/vela rpc-stdio";
#[cfg(windows)]
const PROGRAM: &str = "ssh.exe";
#[cfg(not(windows))]
const PROGRAM: &str = "/usr/bin/ssh";
const HANDSHAKE: Duration = Duration::from_secs(15);
const CALL: Duration = Duration::from_secs(60);
/// How long a closed channel waits for the transport to say why.
const LAST_WORDS: Duration = Duration::from_secs(1);
const ALIAS_LIMIT: usize = 255;
const SAID_BYTES: usize = 4096;
const SAID_LINES: usize = 3;
const SAID_CHARS: usize = 200;

fn failure(message: &str) -> AppError {
    AppError::new(SSH, message)
}

/// The machine a channel reaches, by the aliases of the user's SSH
/// configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub host: String,
    /// Tried once whenever `host` cannot be reached.
    pub fallback: Option<String>,
    pub runs: Program,
}

/// What an alias is asked to run. Each is a fixed command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Program {
    /// The Linux server's bridge, by a user who may open its socket.
    Server,
    /// The same through `sudo -n`.
    ServerWithSudo,
    /// The bridge of the Vela app of the user who logs in to a Mac.
    MacApp,
}

/// An alias goes to `ssh` as an argument, so it is held to what a `Host` name
/// needs and can never read as an option or reach a shell.
pub(crate) fn check_alias(alias: &str) -> Result<()> {
    let plain = alias
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if alias.is_empty() || alias.len() > ALIAS_LIMIT || alias.starts_with('-') || !plain {
        return Err(failure(
            "SSH 别名只能包含字母、数字、点、下划线和连字符，不能以连字符开头，最长 255 个字符",
        ));
    }
    Ok(())
}

/// Everything after the program name.
pub(crate) fn arguments(alias: &str, runs: Program) -> Result<Vec<String>> {
    check_alias(alias)?;
    let mut arguments = vec!["-T".to_string()];
    for option in [
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "ConnectTimeout=10",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=2",
        "ClearAllForwardings=yes",
        "RequestTTY=no",
        "RemoteCommand=none",
        "ControlMaster=no",
        "ControlPath=none",
    ] {
        arguments.extend(["-o".to_string(), option.to_string()]);
    }
    arguments.extend([
        "--".to_string(),
        alias.to_string(),
        match runs {
            Program::Server => SERVER.to_string(),
            Program::ServerWithSudo => format!("sudo -n {SERVER}"),
            Program::MacApp => MAC_APP.to_string(),
        },
    ]);
    Ok(arguments)
}

/// The two directions of one connection. Dropping all three parts ends it.
pub(crate) struct Transport {
    pub reader: Box<dyn AsyncRead + Send + Unpin>,
    pub writer: Box<dyn AsyncWrite + Send + Unpin>,
    /// Settles once the connection is over, with what it said about why.
    pub ended: Pin<Box<dyn Future<Output = String> + Send>>,
}

/// Starts a transport to an alias that runs a program there.
pub(crate) type Dialer = Arc<dyn Fn(&str, Program) -> Result<Transport> + Send + Sync>;

/// The dialer of the app: the system's own `ssh`.
pub(crate) fn system() -> Dialer {
    Arc::new(ssh)
}

fn ssh(alias: &str, runs: Program) -> Result<Transport> {
    let mut command = tokio::process::Command::new(PROGRAM);
    command
        .args(arguments(alias, runs)?)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // No console window.
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let missing = || failure("无法启动 ssh，请确认系统已安装 OpenSSH 客户端");
    let mut child = command.spawn().map_err(|_| missing())?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(missing());
    };
    let said = tokio::spawn(collect(stderr));
    Ok(Transport {
        reader: Box::new(stdout),
        writer: Box::new(stdin),
        // The process lives as long as this is held and is killed with it.
        ended: Box::pin(async move {
            let said = said.await.unwrap_or_default();
            drop(child);
            said
        }),
    })
}

/// Reads a process's error output to its end and keeps the last of it.
async fn collect(mut from: impl AsyncRead + Unpin) -> String {
    let mut kept = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        match from.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                kept.extend_from_slice(&chunk[..read]);
                if kept.len() > SAID_BYTES {
                    kept.drain(..kept.len() - SAID_BYTES);
                }
            }
        }
    }
    last_lines(&kept)
}

/// The last lines of an error output as one line of an error message.
fn last_lines(said: &[u8]) -> String {
    let text = String::from_utf8_lossy(said);
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            line.chars()
                .map(|c| if c == '\t' { ' ' } else { c })
                .filter(|c| !c.is_control())
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|line| !line.is_empty())
        .collect();
    let kept = lines[lines.len().saturating_sub(SAID_LINES)..].join(" ");
    let length = kept.chars().count();
    if length > SAID_CHARS {
        let end: String = kept.chars().skip(length - SAID_CHARS).collect();
        format!("…{end}")
    } else {
        kept
    }
}

/// What a channel tells whoever opened it. Called from the channel's own
/// tasks, never while the channel holds a lock.
pub(crate) trait Listener: Send + Sync + 'static {
    /// The server sent an event nobody asked for.
    fn event(&self, _event: String, _payload: Value) {}
    /// A connection finished its handshake.
    fn connected(&self, _capabilities: &Capabilities) {}
    /// A connection that had finished its handshake is gone.
    fn offline(&self, _error: &AppError) {}
}

/// For a channel nobody watches.
pub(crate) struct Silent;
impl Listener for Silent {}

enum Waiting {
    Open(HashMap<u64, oneshot::Sender<Result<Value>>>),
    /// Why every later request fails.
    Closed(AppError),
}

/// One connection, from its start to its first transport error.
struct Link {
    alias: String,
    requests: mpsc::UnboundedSender<wire::Request>,
    waiting: Mutex<Waiting>,
    next: AtomicU64,
    tasks: Mutex<Vec<AbortHandle>>,
    /// Set after the handshake: from then on the listener hears of the end.
    online: AtomicBool,
    listener: Arc<dyn Listener>,
}

impl Link {
    fn start(alias: &str, transport: Transport, listener: Arc<dyn Listener>) -> Arc<Self> {
        let (requests, queue) = mpsc::unbounded_channel();
        let link = Arc::new(Self {
            alias: alias.to_string(),
            requests,
            waiting: Mutex::new(Waiting::Open(HashMap::new())),
            next: AtomicU64::new(1),
            tasks: Mutex::new(Vec::new()),
            online: AtomicBool::new(false),
            listener,
        });
        let broken = Arc::new(Notify::new());
        let reader = tokio::spawn(receive(
            Arc::downgrade(&link),
            transport.reader,
            transport.ended,
            broken.clone(),
        ));
        let writer = tokio::spawn(transmit(transport.writer, queue, broken));
        *link.tasks.lock() = vec![reader.abort_handle(), writer.abort_handle()];
        link
    }

    fn is_open(&self) -> bool {
        matches!(*self.waiting.lock(), Waiting::Open(_))
    }

    /// Sends one request and waits for its answer. The request is written
    /// once: when the connection goes after that, the answer is the error.
    async fn request(&self, command: &str, args: Value, limit: Duration) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        match &mut *self.waiting.lock() {
            Waiting::Open(waiting) => waiting.insert(id, answer),
            Waiting::Closed(error) => return Err(error.clone()),
        };
        // The writer task only stops with the link, which answers `answered`.
        let _ = self.requests.send(wire::Request {
            id,
            command: command.to_string(),
            args,
        });
        match tokio::time::timeout(limit, answered).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(self.lost("")),
            Err(_) => {
                if let Waiting::Open(waiting) = &mut *self.waiting.lock() {
                    waiting.remove(&id);
                }
                let waited = format!("{} 在 {} 秒内没有应答", self.alias, limit.as_secs());
                Err(failure(&if self.online.load(Ordering::SeqCst) {
                    // The request was sent and may still be carried out.
                    format!("{waited}，操作可能仍在进行，请稍后刷新确认")
                } else {
                    waited
                }))
            }
        }
    }

    fn deliver(&self, reply: wire::Reply) {
        match reply {
            wire::Reply::Answer { id, result, error } => {
                let waiter = match &mut *self.waiting.lock() {
                    Waiting::Open(waiting) => waiting.remove(&id),
                    Waiting::Closed(_) => None,
                };
                if let Some(waiter) = waiter {
                    let _ = waiter.send(match error {
                        Some(error) => Err(error),
                        None => Ok(result.unwrap_or(Value::Null)),
                    });
                }
            }
            wire::Reply::Event { event, payload } => self.listener.event(event, payload),
        }
    }

    /// The error of a connection that ended, with what the transport said.
    fn lost(&self, said: &str) -> AppError {
        let what = if self.online.load(Ordering::SeqCst) {
            format!("与 {} 的连接已断开", self.alias)
        } else {
            format!("无法连接 {}", self.alias)
        };
        failure(&if said.is_empty() {
            what
        } else {
            format!("{what}：{said}")
        })
    }

    /// Ends the connection: every waiting request gets `error`, and so does
    /// the listener of a connection that was online. Only the first call acts.
    fn fail(&self, error: AppError) {
        let waiting = {
            let mut waiting = self.waiting.lock();
            if matches!(*waiting, Waiting::Closed(_)) {
                return;
            }
            std::mem::replace(&mut *waiting, Waiting::Closed(error.clone()))
        };
        if let Waiting::Open(waiting) = waiting {
            for waiter in waiting.into_values() {
                let _ = waiter.send(Err(error.clone()));
            }
        }
        for task in self.tasks.lock().drain(..) {
            task.abort();
        }
        if self.online.swap(false, Ordering::SeqCst) {
            self.listener.offline(&error);
        }
    }

    /// Ends the connection without telling the listener.
    fn discard(&self) {
        self.online.store(false, Ordering::SeqCst);
        self.fail(failure("连接已关闭"));
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        for task in self.tasks.lock().drain(..) {
            task.abort();
        }
    }
}

/// Hands answers to their requests and events to the listener until the
/// transport ends, then fails the link with the reason.
async fn receive(
    link: Weak<Link>,
    mut reader: Box<dyn AsyncRead + Send + Unpin>,
    ended: Pin<Box<dyn Future<Output = String> + Send>>,
    broken: Arc<Notify>,
) {
    let malformed = loop {
        let reply = tokio::select! {
            reply = wire::read::<wire::Reply>(&mut reader) => reply,
            _ = broken.notified() => break false,
        };
        match reply {
            Ok(Some(reply)) => {
                let Some(link) = link.upgrade() else { return };
                link.deliver(reply);
            }
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => break true,
            Ok(None) | Err(_) => break false,
        }
    };
    let said = if malformed {
        "服务器的应答无法识别".to_string()
    } else {
        tokio::time::timeout(LAST_WORDS, ended)
            .await
            .unwrap_or_default()
    };
    if let Some(link) = link.upgrade() {
        link.fail(link.lost(&said));
    }
}

/// The only writer: requests go out whole, one after another.
async fn transmit(
    mut writer: Box<dyn AsyncWrite + Send + Unpin>,
    mut queue: mpsc::UnboundedReceiver<wire::Request>,
    broken: Arc<Notify>,
) {
    while let Some(request) = queue.recv().await {
        if wire::write(&mut writer, &request).await.is_err() {
            // The reader ends the link, so one place words the reason.
            broken.notify_one();
            return;
        }
    }
}

/// The channel to one target. It is offline until a connection finished its
/// handshake and again after any transport error; `call` reconnects.
pub(crate) struct Channel {
    target: Target,
    dialer: Dialer,
    listener: Arc<dyn Listener>,
    current: Mutex<Option<(Arc<Link>, Capabilities)>>,
    /// Held by the one caller that connects; keeps the error of the last
    /// attempt for the callers that waited behind it.
    connecting: tokio::sync::Mutex<Option<AppError>>,
    closed: AtomicBool,
}

impl Channel {
    /// Nothing is started until `connect` or `call`.
    pub(crate) fn new(target: Target, dialer: Dialer, listener: Arc<dyn Listener>) -> Self {
        Self {
            target,
            dialer,
            listener,
            current: Mutex::new(None),
            connecting: tokio::sync::Mutex::new(None),
            closed: AtomicBool::new(false),
        }
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open().is_some()
    }

    /// What the server answered in the handshake, connecting first when the
    /// channel is offline.
    pub(crate) async fn connect(&self) -> Result<Capabilities> {
        Ok(self.link().await?.1)
    }

    /// Runs a command on the server. An offline channel reconnects once
    /// before it sends; a request that was sent is never sent again.
    pub(crate) async fn call(&self, name: &str, args: Value) -> Result<Value> {
        let (link, _) = self.link().await?;
        link.request(name, args, CALL).await
    }

    /// Ends the connection for good and without telling the listener.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some((link, _)) = self.current.lock().take() {
            link.discard();
        }
    }

    fn open(&self) -> Option<(Arc<Link>, Capabilities)> {
        self.current
            .lock()
            .as_ref()
            .filter(|(link, _)| link.is_open())
            .cloned()
    }

    async fn link(&self) -> Result<(Arc<Link>, Capabilities)> {
        let (waited, mut last) = match self.connecting.try_lock() {
            Ok(last) => (false, last),
            Err(_) => (true, self.connecting.lock().await),
        };
        if let Some(open) = self.open() {
            return Ok(open);
        }
        let closed = || failure("连接已关闭");
        if self.closed.load(Ordering::SeqCst) {
            return Err(closed());
        }
        // Whoever waited behind a failed attempt takes its error instead of
        // starting another one.
        if let (true, Some(error)) = (waited, last.as_ref()) {
            return Err(error.clone());
        }
        *last = None;
        let (link, capabilities) = match self.dial().await {
            Ok(opened) => opened,
            Err(error) => {
                *last = Some(error.clone());
                return Err(error);
            }
        };
        {
            let mut current = self.current.lock();
            if self.closed.load(Ordering::SeqCst) {
                drop(current);
                link.discard();
                return Err(closed());
            }
            *current = Some((link.clone(), capabilities.clone()));
        }
        // `connected` comes first, so an end that falls anywhere around these
        // lines is still reported after it, and exactly once.
        self.listener.connected(&capabilities);
        link.online.store(true, Ordering::SeqCst);
        let ended = match &*link.waiting.lock() {
            Waiting::Closed(error) => Some(error.clone()),
            Waiting::Open(_) => None,
        };
        if let Some(error) = ended {
            if link.online.swap(false, Ordering::SeqCst) {
                self.listener.offline(&error);
            }
            return Err(error);
        }
        Ok((link, capabilities))
    }

    async fn dial(&self) -> Result<(Arc<Link>, Capabilities)> {
        check_alias(&self.target.host)?;
        if let Some(fallback) = &self.target.fallback {
            check_alias(fallback)?;
        }
        let first = match self.handshake(&self.target.host).await {
            Err(error) if error.code == SSH => error,
            settled => return settled,
        };
        let Some(fallback) = &self.target.fallback else {
            return Err(first);
        };
        self.handshake(fallback).await.map_err(|second| {
            if second.code == SSH {
                failure(&format!("{}；{}", first.message, second.message))
            } else {
                second
            }
        })
    }

    async fn handshake(&self, alias: &str) -> Result<(Arc<Link>, Capabilities)> {
        let transport = (self.dialer)(alias, self.target.runs)?;
        let link = Link::start(alias, transport, self.listener.clone());
        let answer = link
            .request(
                wire::CAPABILITIES,
                Value::Object(Default::default()),
                HANDSHAKE,
            )
            .await;
        let capabilities = answer.and_then(|answer| {
            serde_json::from_value::<Capabilities>(answer).map_err(|_| incompatible(None))
        });
        match capabilities {
            Ok(capabilities) if capabilities.protocol == wire::PROTOCOL => Ok((link, capabilities)),
            Ok(capabilities) => {
                link.discard();
                Err(incompatible(Some(&capabilities)))
            }
            Err(error) => {
                link.discard();
                Err(error)
            }
        }
    }
}

fn incompatible(capabilities: Option<&Capabilities>) -> AppError {
    let message = match capabilities {
        Some(server) if server.protocol > wire::PROTOCOL => format!(
            "服务器版本不兼容（服务器为 {}），请升级本机的 Vela",
            server.version
        ),
        Some(server) => format!(
            "服务器版本不兼容（服务器为 {}），请升级服务器上的 Vela",
            server.version
        ),
        None => "服务器版本不兼容，请升级服务器上的 Vela".to_string(),
    };
    AppError::new(code::UNSUPPORTED, &message)
}

#[cfg(test)]
#[path = "remote_tests.rs"]
mod tests;
