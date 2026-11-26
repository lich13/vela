//! Which Vela the pages show and operate: this machine, or another reached
//! over SSH, a Linux server or a Mac with the app open. Keeps the list of
//! them in `instances.json`, the channel to the selected one and what the
//! windows are told about both.
use crate::{
    errors::code,
    remote::{self, Channel, Dialer, Listener, Program, Target},
    storage::{self, AppError, Result},
    wire::Capabilities,
};
use parking_lot::{Mutex, MutexGuard};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, Weak},
};

/// The instance that is this machine; every other one is a machine id.
pub(crate) const LOCAL: &str = "local";
const FILE: &str = "instances.json";
const NAME_LIMIT: usize = 64;

/// What runs Vela on a machine. A file from before there were Macs lists
/// Linux servers only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    /// The `vela-server` service.
    #[default]
    Linux,
    /// The Vela app, open and set to let itself be controlled.
    Mac,
}

impl Kind {
    /// The app belongs to the user who logs in, so a Mac is never asked for
    /// `sudo`.
    fn runs(self, sudo: bool) -> Program {
        match (self, sudo) {
            (Self::Mac, _) => Program::MacApp,
            (Self::Linux, true) => Program::ServerWithSudo,
            (Self::Linux, false) => Program::Server,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Machine {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: Kind,
    pub ssh_host: String,
    pub fallback_host: Option<String>,
    pub use_sudo: bool,
}

impl Machine {
    fn target(&self) -> Target {
        Target {
            host: self.ssh_host.clone(),
            fallback: self.fallback_host.clone(),
            runs: self.kind.runs(self.use_sudo),
        }
    }
}

/// A machine as the manage dialog saves it; without an id it is a new one.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MachineInput {
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub kind: Kind,
    pub ssh_host: String,
    pub fallback_host: Option<String>,
    pub use_sudo: bool,
}

/// The file. Without one there is only this machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    active: String,
    machines: Vec<Machine>,
}

impl Stored {
    fn machine(&self, id: &str) -> Option<&Machine> {
        self.machines.iter().find(|machine| machine.id == id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    Local,
    Connecting,
    Connected,
    Offline,
}

/// The connection to the selected instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Connection {
    pub status: Status,
    /// The server's version, as last heard.
    pub version: Option<String>,
    pub error: Option<String>,
    /// Shared commands of this app that the server does not answer.
    pub unsupported: Vec<String>,
}

impl Connection {
    fn new(status: Status) -> Self {
        Self {
            status,
            version: None,
            error: None,
            unsupported: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct InstanceState {
    pub revision: String,
    pub active: String,
    pub machines: Vec<Machine>,
    pub connection: Connection,
}

#[derive(Debug, Serialize)]
pub(crate) struct Tested {
    pub version: String,
}

/// A shared event of a server, as the windows receive it.
#[derive(Debug, Serialize)]
pub(crate) struct RemoteEvent<'a> {
    pub instance: &'a str,
    pub event: &'a str,
    pub payload: &'a Value,
}

/// Where the service tells the windows what changed. Called while the service
/// holds its state, so it must not call back into it.
pub(crate) trait Sink: Send + Sync + 'static {
    /// Goes out as `instance-state`.
    fn state(&self, state: &InstanceState);
    /// Goes out as `remote-event`.
    fn remote(&self, event: &RemoteEvent<'_>);
}

impl Sink for tauri::AppHandle {
    fn state(&self, state: &InstanceState) {
        #[cfg(target_os = "macos")]
        crate::quick_macos::instances_changed(state);
        let _ = tauri::Emitter::emit(self, "instance-state", state);
    }
    fn remote(&self, event: &RemoteEvent<'_>) {
        #[cfg(target_os = "macos")]
        crate::quick_macos::server_event(event.instance, event.event, event.payload);
        let _ = tauri::Emitter::emit(self, "remote-event", event);
    }
}

struct Inner {
    /// Whether `stored` is what the file says. A file that cannot be read is
    /// tried again by the next command.
    read: bool,
    stored: Stored,
    revision: String,
    connection: Connection,
    /// The channel to the selected server and the number that tells it from
    /// every earlier one. There is one exactly while a server is selected.
    channel: Option<(u64, Arc<Channel>)>,
    channels: u64,
}

impl Inner {
    fn view(&self) -> InstanceState {
        InstanceState {
            revision: self.revision.clone(),
            active: self.stored.active.clone(),
            machines: self.stored.machines.clone(),
            connection: self.connection.clone(),
        }
    }

    fn check(&self, revision: &str) -> Result<()> {
        if self.revision == revision {
            Ok(())
        } else {
            Err(AppError::new(code::CONFLICT, "实例列表已更新，请重试"))
        }
    }
}

pub(crate) struct Service {
    me: Weak<Service>,
    data: PathBuf,
    /// The shared commands of this app.
    local: &'static [&'static str],
    dialer: Dialer,
    sink: Arc<dyn Sink>,
    inner: Mutex<Inner>,
    /// One change of the list or the selection at a time.
    changing: tokio::sync::Mutex<()>,
}

/// What the selected server's channel reports. A channel that is no longer
/// the selected one is not listened to.
struct Watch {
    service: Weak<Service>,
    machine: String,
    number: u64,
}

impl Listener for Watch {
    fn event(&self, event: String, payload: Value) {
        let Some(service) = self.service.upgrade() else {
            return;
        };
        let inner = service.inner.lock();
        if matches!(&inner.channel, Some((number, _)) if *number == self.number) {
            service.sink.remote(&RemoteEvent {
                instance: &self.machine,
                event: &event,
                payload: &payload,
            });
        }
    }

    fn connected(&self, capabilities: &Capabilities) {
        let Some(service) = self.service.upgrade() else {
            return;
        };
        let mut inner = service.inner.lock();
        if matches!(&inner.channel, Some((number, _)) if *number == self.number) {
            let connection = service.connected(capabilities);
            service.show(&mut inner, connection);
        }
    }

    fn offline(&self, error: &AppError) {
        if let Some(service) = self.service.upgrade() {
            service.lost(self.number, error);
        }
    }
}

impl Service {
    /// Reads nothing and starts nothing until `start` or the first command.
    pub(crate) fn new(
        data: PathBuf,
        local: &'static [&'static str],
        dialer: Dialer,
        sink: Arc<dyn Sink>,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            data,
            local,
            dialer,
            sink,
            inner: Mutex::new(Inner {
                read: false,
                stored: Stored {
                    active: LOCAL.into(),
                    machines: Vec::new(),
                },
                revision: storage::revision(None),
                connection: Connection::new(Status::Local),
                channel: None,
                channels: 0,
            }),
            changing: tokio::sync::Mutex::new(()),
        })
    }

    /// Reads the file and, when it selects a server, connects to it in the
    /// background. Needs the tokio runtime. A file that cannot be read is
    /// reported by the commands, which read it again.
    pub(crate) fn start(&self) {
        drop(self.loaded());
    }

    pub(crate) fn state(&self) -> Result<InstanceState> {
        Ok(self.loaded()?.view())
    }

    /// Adds a machine or changes one. A new address for the selected server
    /// ends its connection; the next call or a retry connects again.
    pub(crate) async fn save(&self, expected: &str, input: MachineInput) -> Result<InstanceState> {
        let _changing = self.changing.lock().await;
        let mut inner = self.loaded()?;
        inner.check(expected)?;
        let name = input.name.trim();
        if name.is_empty() || name.chars().count() > NAME_LIMIT {
            return Err(AppError::new(code::NAME, "名称需为 1–64 个字符"));
        }
        remote::check_alias(&input.ssh_host)?;
        if let Some(fallback) = &input.fallback_host {
            remote::check_alias(fallback)?;
        }
        let machine = Machine {
            id: input
                .id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            name: name.to_string(),
            kind: input.kind,
            ssh_host: input.ssh_host,
            fallback_host: input.fallback_host,
            use_sudo: input.use_sudo && input.kind == Kind::Linux,
        };
        let mut stored = inner.stored.clone();
        let moved = match &input.id {
            None => {
                stored.machines.push(machine.clone());
                false
            }
            Some(id) => {
                let known = stored
                    .machines
                    .iter_mut()
                    .find(|known| known.id == *id)
                    .ok_or_else(missing)?;
                let moved = known.target() != machine.target();
                *known = machine.clone();
                moved && stored.active == *id
            }
        };
        self.write(&mut inner, stored)?;
        if moved {
            self.open(&mut inner, &machine);
            inner.connection = Connection {
                error: Some("连接设置已修改，请重新连接".into()),
                ..Connection::new(Status::Offline)
            };
        }
        self.sink.state(&inner.view());
        Ok(inner.view())
    }

    /// Removes a machine. Without the selected one, this machine is selected.
    pub(crate) async fn delete(&self, expected: &str, id: &str) -> Result<InstanceState> {
        let _changing = self.changing.lock().await;
        let mut inner = self.loaded()?;
        inner.check(expected)?;
        let mut stored = inner.stored.clone();
        if stored.machine(id).is_none() {
            return Err(missing());
        }
        stored.machines.retain(|machine| machine.id != id);
        let selected = stored.active == id;
        if selected {
            stored.active = LOCAL.into();
        }
        self.write(&mut inner, stored)?;
        if selected {
            self.leave(&mut inner);
        }
        self.sink.state(&inner.view());
        Ok(inner.view())
    }

    /// Selects this machine at once and a server only after it answered the
    /// handshake; until then, and when it does not, the selection stays what
    /// it was. Selecting the selected server again connects to it again.
    pub(crate) async fn select(&self, expected: &str, id: &str) -> Result<InstanceState> {
        let _changing = self.changing.lock().await;
        let (number, channel, selected) = {
            let mut inner = self.loaded()?;
            inner.check(expected)?;
            if id == LOCAL {
                if inner.stored.active != LOCAL {
                    let stored = Stored {
                        active: LOCAL.into(),
                        machines: inner.stored.machines.clone(),
                    };
                    self.write(&mut inner, stored)?;
                    self.leave(&mut inner);
                    self.sink.state(&inner.view());
                }
                return Ok(inner.view());
            }
            let machine = inner.stored.machine(id).cloned().ok_or_else(missing)?;
            if inner.stored.active != id {
                // Not the selected channel yet: the one in use stays in use.
                let (number, channel) = self.channel(&mut inner, &machine);
                (number, channel, false)
            } else {
                let (number, channel) = match inner.channel.clone() {
                    Some(current) => current,
                    None => self.open(&mut inner, &machine),
                };
                if channel.is_open() {
                    return Ok(inner.view());
                }
                let connection = Connection {
                    status: Status::Connecting,
                    error: None,
                    ..inner.connection.clone()
                };
                self.show(&mut inner, connection);
                (number, channel, true)
            }
        };
        let capabilities = match channel.connect().await {
            Ok(capabilities) => capabilities,
            Err(error) => {
                if selected {
                    self.lost(number, &error);
                }
                return Err(error);
            }
        };
        let mut inner = self.inner.lock();
        if !selected {
            let stored = Stored {
                active: id.into(),
                machines: inner.stored.machines.clone(),
            };
            if let Err(error) = self.write(&mut inner, stored) {
                channel.close();
                return Err(error);
            }
            if let Some((_, former)) = inner.channel.replace((number, channel.clone())) {
                former.close();
            }
            inner.connection = if channel.is_open() {
                self.connected(&capabilities)
            } else {
                // It ended before anyone listened to it.
                Connection {
                    error: Some("连接已断开，请重试".into()),
                    ..Connection::new(Status::Offline)
                }
            };
            self.sink.state(&inner.view());
        }
        Ok(inner.view())
    }

    /// Whether a server answers at an address, on a channel of its own.
    pub(crate) async fn test(&self, target: Target) -> Result<Tested> {
        let channel = Channel::new(target, self.dialer.clone(), Arc::new(remote::Silent));
        let capabilities = channel.connect().await;
        channel.close();
        Ok(Tested {
            version: capabilities?.version,
        })
    }

    /// Runs a shared command on the selected server and answers what the
    /// server answered, its errors included. `instance` is the one the caller
    /// believes is selected. This is the way of whatever a window asked for:
    /// the request does not say it is internal, and what only this app's
    /// Rust may ask is refused before anything is sent.
    pub(crate) async fn call(&self, instance: &str, name: &str, args: Value) -> Result<Value> {
        if crate::rpc::internal(name) {
            return Err(crate::rpc::unsupported());
        }
        self.selected(instance, name, args, false).await
    }

    async fn selected(
        &self,
        instance: &str,
        name: &str,
        args: Value,
        internal: bool,
    ) -> Result<Value> {
        let (number, channel) = {
            let inner = self.loaded()?;
            let switched = || AppError::new(code::STALE, "实例已切换，请重试");
            if instance == LOCAL || inner.stored.active != instance {
                return Err(switched());
            }
            if inner
                .connection
                .unsupported
                .iter()
                .any(|known| known == name)
            {
                return Err(unsupported());
            }
            inner.channel.clone().ok_or_else(switched)?
        };
        // An offline channel connects here, once, before anything is sent.
        let capabilities = match channel.connect().await {
            Ok(capabilities) => capabilities,
            Err(error) => {
                self.lost(number, &error);
                return Err(error);
            }
        };
        if self.local.contains(&name) && !capabilities.commands.iter().any(|known| known == name) {
            return Err(unsupported());
        }
        channel.call(name, args, internal).await
    }

    /// Runs a command this app's Rust asks for itself on a machine of the
    /// list: the selected one over its channel, any other over a channel of
    /// its own that is closed with the answer. The request says it is
    /// internal, which no request a window asked for does.
    pub(crate) async fn ask(&self, machine: &str, name: &str, args: Value) -> Result<Value> {
        let target = {
            let inner = self.loaded()?;
            if inner.stored.active == machine {
                None
            } else {
                Some(inner.stored.machine(machine).ok_or_else(missing)?.target())
            }
        };
        let Some(target) = target else {
            return self.selected(machine, name, args, true).await;
        };
        let channel = Channel::new(target, self.dialer.clone(), Arc::new(remote::Silent));
        let answer = async {
            let capabilities = channel.connect().await?;
            if !capabilities.commands.iter().any(|known| known == name) {
                return Err(unsupported());
            }
            channel.call(name, args, true).await
        }
        .await;
        channel.close();
        answer
    }

    /// The state, read from the file the first time. A file that selects a
    /// server starts the connection to it.
    fn loaded(&self) -> Result<MutexGuard<'_, Inner>> {
        let mut inner = self.inner.lock();
        if !inner.read {
            let bytes = storage::read_optional(&self.data.join(FILE))?;
            if let Some(bytes) = &bytes {
                let damaged = || {
                    AppError::new(
                        code::SETTINGS,
                        "实例配置 instances.json 已损坏，请修复或删除该文件后重试",
                    )
                };
                let stored: Stored = serde_json::from_slice(bytes).map_err(|_| damaged())?;
                if stored.active != LOCAL && stored.machine(&stored.active).is_none() {
                    return Err(damaged());
                }
                inner.stored = stored;
            }
            inner.revision = storage::revision(bytes.as_deref());
            inner.read = true;
            let active = inner.stored.active.clone();
            if let Some(machine) = inner.stored.machine(&active).cloned() {
                let (number, channel) = self.open(&mut inner, &machine);
                inner.connection = Connection::new(Status::Connecting);
                let service = self.me.clone();
                tokio::spawn(async move {
                    if let Err(error) = channel.connect().await {
                        if let Some(service) = service.upgrade() {
                            service.lost(number, &error);
                        }
                    }
                });
            }
            self.sink.state(&inner.view());
        }
        Ok(inner)
    }

    /// Writes the list and only then takes it for the current one.
    fn write(&self, inner: &mut Inner, stored: Stored) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&stored)
            .map_err(|_| AppError::new(code::JSON, "实例配置无法保存"))?;
        storage::atomic_write(&self.data.join(FILE), &bytes, None)?;
        inner.revision = storage::digest(&bytes);
        inner.stored = stored;
        Ok(())
    }

    /// A channel to `machine` that nothing listens to until it is selected.
    fn channel(&self, inner: &mut Inner, machine: &Machine) -> (u64, Arc<Channel>) {
        inner.channels += 1;
        let watch = Watch {
            service: self.me.clone(),
            machine: machine.id.clone(),
            number: inner.channels,
        };
        let channel = Channel::new(machine.target(), self.dialer.clone(), Arc::new(watch));
        (inner.channels, Arc::new(channel))
    }

    /// Replaces the selected channel with a new one to `machine`, not yet
    /// connected.
    fn open(&self, inner: &mut Inner, machine: &Machine) -> (u64, Arc<Channel>) {
        let opened = self.channel(inner, machine);
        if let Some((_, former)) = inner.channel.replace(opened.clone()) {
            former.close();
        }
        opened
    }

    /// Back to this machine.
    fn leave(&self, inner: &mut Inner) {
        if let Some((_, former)) = inner.channel.take() {
            former.close();
        }
        inner.connection = Connection::new(Status::Local);
    }

    fn connected(&self, capabilities: &Capabilities) -> Connection {
        Connection {
            status: Status::Connected,
            version: Some(capabilities.version.clone()),
            error: None,
            unsupported: self
                .local
                .iter()
                .filter(|name| !capabilities.commands.iter().any(|known| known == *name))
                .map(|name| name.to_string())
                .collect(),
        }
    }

    /// The selected channel `number` has no connection, because of `error`.
    fn lost(&self, number: u64, error: &AppError) {
        let mut inner = self.inner.lock();
        // A later connection of the same channel may already be up.
        if !matches!(&inner.channel, Some((current, channel)) if *current == number && !channel.is_open())
        {
            return;
        }
        let connection = Connection {
            status: Status::Offline,
            error: Some(error.message.clone()),
            ..inner.connection.clone()
        };
        self.show(&mut inner, connection);
    }

    /// Takes a connection state and tells the windows when it is a new one.
    fn show(&self, inner: &mut Inner, connection: Connection) {
        if inner.connection != connection {
            inner.connection = connection;
            self.sink.state(&inner.view());
        }
    }
}

fn unsupported() -> AppError {
    AppError::new(
        code::UNSUPPORTED,
        "服务器版本不支持此操作，请升级服务器上的 Vela",
    )
}

fn missing() -> AppError {
    AppError::new(code::MISSING, "实例不存在，请刷新后重试")
}

/// Runs a command on an instance for this app's own Rust: in this machine's
/// table, or on another machine as a request that says it is internal. What
/// carries credentials between instances goes this way and through no window.
pub(crate) async fn on(
    service: &Service,
    runtime: &Arc<crate::runtime::Runtime>,
    instance: &str,
    name: &str,
    args: Value,
) -> Result<Value> {
    if instance == LOCAL {
        crate::rpc::call(runtime, name, args).await
    } else {
        service.ask(instance, name, args).await
    }
}

#[tauri::command]
pub(crate) async fn get_instances(
    service: tauri::State<'_, Arc<Service>>,
) -> Result<InstanceState> {
    service.state()
}

#[tauri::command]
pub(crate) async fn save_instance(
    service: tauri::State<'_, Arc<Service>>,
    expected_revision: String,
    machine: MachineInput,
) -> Result<InstanceState> {
    service.save(&expected_revision, machine).await
}

#[tauri::command]
pub(crate) async fn delete_instance(
    service: tauri::State<'_, Arc<Service>>,
    expected_revision: String,
    id: String,
) -> Result<InstanceState> {
    service.delete(&expected_revision, &id).await
}

#[tauri::command]
pub(crate) async fn select_instance(
    service: tauri::State<'_, Arc<Service>>,
    expected_revision: String,
    id: String,
) -> Result<InstanceState> {
    service.select(&expected_revision, &id).await
}

#[tauri::command]
pub(crate) async fn test_instance(
    service: tauri::State<'_, Arc<Service>>,
    kind: Option<Kind>,
    ssh_host: String,
    fallback_host: Option<String>,
    use_sudo: bool,
) -> Result<Tested> {
    let target = Target {
        host: ssh_host,
        fallback: fallback_host,
        runs: kind.unwrap_or_default().runs(use_sudo),
    };
    service.test(target).await
}

#[tauri::command]
pub(crate) async fn remote_call(
    service: tauri::State<'_, Arc<Service>>,
    instance: String,
    name: String,
    args: Value,
) -> Result<Value> {
    service.call(&instance, &name, args).await
}

/// Adds a copy of a provider of one instance to another. The copy holds the
/// provider's tokens: it goes from one Rust to the other, over SSH where
/// either is a server, and the window hears only whether it arrived. It is
/// sent once; after a failure the user looks before trying again.
#[tauri::command]
pub(crate) async fn copy_provider(
    service: tauri::State<'_, Arc<Service>>,
    runtime: tauri::State<'_, Arc<crate::runtime::Runtime>>,
    instance: String,
    client_id: crate::gateway::ClientId,
    provider_id: String,
    target: String,
) -> Result<()> {
    if instance == target {
        return Err(AppError::new(code::STALE, "请选择另一个实例"));
    }
    let named = serde_json::json!({"clientId": client_id, "providerId": provider_id});
    let copy = on(&service, &runtime, &instance, "export_provider_copy", named).await?;
    let handed = serde_json::json!({"clientId": client_id, "copy": copy});
    on(&service, &runtime, &target, "import_provider_copy", handed).await?;
    Ok(())
}

/// Puts one of the keys an instance handed out on this machine's clipboard.
/// The key goes from that instance's Rust to this one, over SSH where it is a
/// server, and the window hears only whether it was copied.
#[tauri::command]
pub(crate) async fn copy_gateway_key(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    service: tauri::State<'_, Arc<Service>>,
    runtime: tauri::State<'_, Arc<crate::runtime::Runtime>>,
    instance: String,
    client_id: crate::gateway::ClientId,
    key_id: String,
) -> Result<()> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    if window.label() != "main" {
        return Err(AppError::new(code::FORBIDDEN, "仅主窗口可复制 Key"));
    }
    let named = serde_json::json!({"clientId": client_id, "keyId": key_id});
    let key = on(&service, &runtime, &instance, "export_gateway_key", named).await?;
    let key = key.as_str().ok_or_else(crate::rpc::unsupported)?;
    app.clipboard()
        .write_text(key)
        .map_err(|_| AppError::new(code::CLIPBOARD, "无法写入剪贴板，请重试"))
}

#[cfg(test)]
#[path = "instances_tests.rs"]
mod tests;
