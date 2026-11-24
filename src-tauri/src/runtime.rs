//! The services behind every client, with no window in sight. Whoever hosts
//! them, the desktop shell or a headless server, builds one `Runtime` and hands
//! it a `Sink` for what the services have to say.
use crate::{
    antigravity, auth_cache, claude_profile, cleanup_spool,
    core::{Core, ViewState},
    errors::code,
    events, gateway, migrations, official, provider_testing,
    storage::{self, AppError, Result},
    usage,
};
use parking_lot::Mutex;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

/// Something the runtime tells its host. It serializes as the payload that
/// goes out under `name`.
pub(crate) enum Event<'a> {
    State(&'a ViewState),
    /// An error nobody is waiting for.
    Error(&'a AppError),
    /// A startup failure that the main window has yet to collect.
    StartupError(&'a AppError),
    Gateway(&'a gateway::View),
    Freeze(&'a gateway::freeze::View),
    Quota {
        client_id: gateway::ClientId,
        quota: &'a gateway::QuotaView,
    },
    /// A client's configuration file changed. Its `clientId` names the
    /// client, so a host that holds events back keeps the latest for each.
    Config(&'a serde_json::Value),
    AntigravityAuth(&'a antigravity::auth::AntigravityAuthState),
    /// A sentence telling the user what a change they just made means.
    Notice(&'a str),
    ClaudeProfile(&'a claude_profile::View),
    /// The journal gained a record or failed to keep one.
    Journal(&'a events::Change),
    /// Provider test results, usage or prices changed. Nothing rides along:
    /// whoever shows them reads them again.
    ProviderTests,
    Usage,
    Pricing,
}
impl Event<'_> {
    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::State(_) => "switch-state",
            Self::Error(_) | Self::StartupError(_) => "switch-error",
            Self::Gateway(_) => "gateway-state",
            Self::Freeze(_) => "gateway-freeze",
            Self::Quota { .. } => "provider-quota",
            Self::Config(_) => "config-state",
            Self::AntigravityAuth(_) => "antigravity-auth-state",
            Self::Notice(_) => "switch-notice",
            Self::ClaudeProfile(_) => "claude-profile-state",
            Self::Journal(_) => "app-event",
            Self::ProviderTests => "provider-tests",
            Self::Usage => "usage-state",
            Self::Pricing => "pricing-state",
        }
    }
}
impl serde::Serialize for Event<'_> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::State(state) => state.serialize(serializer),
            Self::Error(error) | Self::StartupError(error) => error.serialize(serializer),
            Self::Gateway(view) => view.serialize(serializer),
            Self::Freeze(view) => view.serialize(serializer),
            Self::Quota { client_id, quota } => {
                serde_json::json!({"clientId": client_id, "quota": quota}).serialize(serializer)
            }
            Self::Config(state) => state.serialize(serializer),
            Self::AntigravityAuth(state) => state.serialize(serializer),
            Self::Notice(message) => message.serialize(serializer),
            Self::ClaudeProfile(view) => view.serialize(serializer),
            Self::Journal(change) => change.serialize(serializer),
            Self::ProviderTests | Self::Usage | Self::Pricing => serializer.serialize_unit(),
        }
    }
}

/// Where the runtime's events go, and the little it asks of its host.
pub(crate) trait Sink: Send + Sync + 'static {
    /// Delivers `event` to whoever is watching.
    fn emit(&self, event: Event<'_>);
    /// Whether someone is watching; background polling slows down when nobody is.
    fn attentive(&self) -> bool;
    /// The sign-ins the host is running. A client's configuration files are
    /// polled while there is one, and a change that would cut across one is
    /// refused.
    fn sign_ins(&self) -> SignIns {
        SignIns::default()
    }
    /// Runs once per polling round, for upkeep the host does at that pace.
    fn tick(&self) {}
}

/// The sink of a run nobody watches.
#[cfg(any(feature = "desktop", test))]
pub(crate) struct Silent;
#[cfg(any(feature = "desktop", test))]
impl Sink for Silent {
    fn emit(&self, _event: Event<'_>) {}
    fn attentive(&self) -> bool {
        false
    }
}

#[derive(Default)]
pub(crate) struct SignIns {
    /// A Codex sign-in is starting, waiting or being cancelled.
    pub(crate) codex: bool,
    /// The saved account that sign-in refreshes.
    pub(crate) codex_account: Option<String>,
    /// A Claude sign-in is waiting or being cancelled.
    pub(crate) claude: bool,
}

pub(crate) struct Runtime {
    pub(crate) diagnostics: events::Service,
    pub(crate) usage: usage::Service,
    pub(crate) core: Mutex<Core>,
    pub(crate) data: PathBuf,
    /// The user's own home directory, where the clients keep theirs.
    pub(crate) user_home: PathBuf,
    pub(crate) provider_tests: provider_testing::Service,
    pub(crate) gateway: gateway::Gateway,
    pub(crate) claude: gateway::Gateway,
    pub(crate) agy: gateway::Gateway,
    pub(crate) grok: gateway::Gateway,
    pub(crate) dsh: gateway::Gateway,
    /// The gateways are stopped and the host is leaving.
    pub(crate) quitting: AtomicBool,
    /// The host asked to leave and the gateways are being stopped.
    pub(crate) quit_pending: AtomicBool,
    pub(crate) startup_error: Mutex<Option<AppError>>,
    pub(crate) startup_error_consumed: AtomicBool,
    pub(crate) official_tx: tokio::sync::Mutex<()>,
    /// Ends the wait between two polling rounds early.
    pub(crate) poll_wake: tokio::sync::Notify,
    sink: Arc<dyn Sink>,
    #[cfg(feature = "desktop")]
    pub(crate) shell: crate::Shell,
}

impl Runtime {
    /// Builds every service over `~/.vela` and the clients' own directories,
    /// settling what an earlier run left unfinished. The host must already be
    /// the only instance. Nothing runs in the background until `start`.
    pub(crate) fn new(sink: Arc<dyn Sink>) -> Result<Arc<Self>> {
        let user_home =
            dirs::home_dir().ok_or_else(|| AppError::new(code::PATH, "无法定位用户主目录"))?;
        let home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home.join(".codex"));
        Self::at(sink, user_home, home)
    }

    /// The same over a home directory the caller names, with Codex in `home`
    /// until the preferences say otherwise. Tests keep both in a temporary
    /// directory.
    pub(crate) fn at(sink: Arc<dyn Sink>, user_home: PathBuf, home: PathBuf) -> Result<Arc<Self>> {
        let data = user_home.join(".vela");
        storage::private_dir(&data)?;
        // No other instance is running, so no current spool exists yet.
        cleanup_spool::clean(&data);
        official::recover_interrupted(&data);
        let core = Core::new(data.clone(), home)?;
        let gateway = gateway::Gateway::new(data.clone())?;
        let claude = gateway.companion(data.join("claude"))?;
        let agy = gateway.agy_companion(data.join("antigravity"))?;
        let grok = gateway.grok_companion(data.join("grok"))?;
        let dsh = gateway.dsh_companion(data.join("dsh"))?;
        let antigravity_paths = antigravity::auth::Paths::from_roots(&user_home, &data);
        let antigravity_migration_error =
            migrations::antigravity_accounts(&antigravity_paths).err();
        // The first look at the Antigravity login may wait on the system
        // credential store, so it runs off the calling thread.
        std::thread::scope(|scope| {
            let _ = scope
                .spawn(|| antigravity::auth::poll_state(&antigravity_paths, &mut None))
                .join();
        });
        let usage = usage::Service::new(&data);
        gateway.set_usage(usage.clone());
        claude.set_usage(usage.clone());
        agy.set_usage(usage.clone());
        grok.set_usage(usage.clone());
        dsh.set_usage(usage.clone());
        let codex_import_error = gateway.import_initial(&core.home()).err();
        let claude_import_error = {
            let home = PathBuf::from(core.preferences().claude_home);
            let recovery = if claude_profile::recovery_pending(&data)
                && claude_profile::login::running_sessions()?
            {
                Err(AppError::new(
                    code::CLAUDE_RUNNING,
                    "请先退出 Claude Code，再恢复配置切换事务",
                ))
            } else {
                claude_profile::recover(&data, &home, &user_home)
            };
            match recovery {
                Err(e) => Some(e),
                Ok(()) if !claude_profile::blocks(&data) => claude.import_initial(&home).err(),
                Ok(()) => None,
            }
        };
        let grok_import_error = grok
            .import_initial(&PathBuf::from(core.preferences().grok_home))
            .err();
        gateway.set_quota_interval(core.preferences().quota_refresh_seconds);
        agy.set_quota_interval(core.preferences().quota_refresh_seconds);
        grok.set_quota_interval(core.preferences().quota_refresh_seconds);
        dsh.set_quota_interval(core.preferences().quota_refresh_seconds);
        claude.set_quota_interval(core.preferences().quota_refresh_seconds);
        let diagnostics = events::Service::new(&data);
        gateway.set_diagnostics(diagnostics.clone());
        claude.set_diagnostics(diagnostics.clone());
        agy.set_diagnostics(diagnostics.clone());
        grok.set_diagnostics(diagnostics.clone());
        dsh.set_diagnostics(diagnostics.clone());
        let provider_tests = provider_testing::Service::new(
            &data,
            [
                gateway.clone(),
                claude.clone(),
                agy.clone(),
                grok.clone(),
                dsh.clone(),
            ],
        )?;
        let runtime = Arc::new(Self {
            usage,
            diagnostics,
            core: Mutex::new(core),
            data,
            user_home,
            provider_tests,
            gateway,
            claude,
            agy,
            grok,
            dsh,
            quitting: AtomicBool::new(false),
            quit_pending: AtomicBool::new(false),
            startup_error: Mutex::new(
                codex_import_error
                    .or(claude_import_error)
                    .or(grok_import_error)
                    .or(antigravity_migration_error),
            ),
            startup_error_consumed: AtomicBool::new(false),
            official_tx: tokio::sync::Mutex::new(()),
            poll_wake: tokio::sync::Notify::new(),
            sink,
            #[cfg(feature = "desktop")]
            shell: crate::Shell::new(),
        });
        runtime.gateway.observe_home(&runtime.core.lock().home());
        runtime
            .claude
            .observe_home(&runtime.home(gateway::ClientId::Claude)?);
        Ok(runtime)
    }

    /// Starts the background work: resuming the gateways that were on,
    /// keeping model lists and endpoints fresh, and reporting every change to
    /// the sink. Call once, on a thread that has entered the tokio runtime.
    pub(crate) fn start(self: &Arc<Self>) -> Result<()> {
        let state = self.state()?;
        self.agy.observe_home(&self.home(gateway::ClientId::Agy)?);
        self.grok.observe_home(&self.home(gateway::ClientId::Grok)?);
        self.dsh.observe_home(&self.home(gateway::ClientId::Dsh)?);
        self.report_services();
        // Told, round after round of the polling below, that someone is
        // watching: whatever rests for want of a watcher goes on.
        let watched = Arc::new(tokio::sync::Notify::new());
        self.keep_usage(watched.clone());
        {
            let r = self.clone();
            tokio::spawn(async move {
                let _official_guard = r.official_tx.lock().await;
                for client in [
                    gateway::ClientId::Codex,
                    gateway::ClientId::Claude,
                    gateway::ClientId::Agy,
                    gateway::ClientId::Grok,
                    gateway::ClientId::Dsh,
                ] {
                    if r.quit_pending.load(Ordering::Relaxed) || r.quitting.load(Ordering::Relaxed)
                    {
                        break;
                    }
                    if (client == gateway::ClientId::Codex && official::blocks_gateway(&r.data))
                        || (client == gateway::ClientId::Claude && claude_profile::blocks(&r.data))
                    {
                        continue;
                    }
                    let outcome = match r.home(client) {
                        Ok(home) => r.gateway(client).resume(&home).await,
                        Err(e) => Err(e),
                    };
                    if let Err(e) = outcome {
                        let e =
                            AppError::new(&e.code, &format!("{}：{}", client.name(), e.message));
                        if let Some(record) =
                            events::Record::configuration_failure(client, &e, true)
                        {
                            r.diagnostics.emit(record);
                        }
                        r.record_startup_error(e, true);
                    }
                    r.publish_gateway(r.gateway(client));
                }
            });
        }
        for g in self.gateways() {
            tokio::spawn(async move {
                if let Err(e) = g.backfill_models().await {
                    g.report_model_error(e);
                }
            });
        }
        {
            // An hourly look; the catalog is fetched once a day, and only when chosen.
            let codex = self.gateway.clone();
            tokio::spawn(async move {
                let mut hourly = tokio::time::interval(std::time::Duration::from_secs(3600));
                hourly.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    hourly.tick().await;
                    let _ = codex.refresh_official_catalog(false).await;
                }
            });
        }
        self.gateway.report_diagnostics();
        self.agy.report_diagnostics();
        self.grok.report_diagnostics();
        self.dsh.report_diagnostics();
        self.claude.report_diagnostics();
        {
            let freeze = self.gateway.freeze_gate();
            let mut freeze_events = freeze.subscribe();
            let sink = self.sink.clone();
            tokio::spawn(async move {
                loop {
                    let next = freeze.next_resume();
                    tokio::select! {
                        changed = freeze_events.changed() => if changed.is_err() { break; },
                        _ = async {
                            if let Some(at) = next {
                                let delay = at.saturating_sub(usage::model::now()).max(0) as u64;
                                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                            } else { std::future::pending::<()>().await; }
                        } => {},
                    }
                    freeze.resume_due(usage::model::now());
                    sink.emit(Event::Freeze(&freeze.view()));
                }
            });
        }
        for probe_gateway in self.gateways() {
            let mut probe_freeze = probe_gateway.freeze_gate().subscribe_control();
            tokio::spawn(async move {
                loop {
                    if probe_gateway
                        .next_endpoint_probe_wait()
                        .is_some_and(|w| w.is_zero())
                    {
                        probe_gateway.probe_due_endpoints().await;
                    }
                    let sleep_for = probe_gateway
                        .next_endpoint_probe_wait()
                        .map(|w| w.max(std::time::Duration::from_secs(1)));
                    tokio::select! {
                        _ = probe_gateway.wait_endpoint_probe_wake() => {}
                        res = probe_freeze.changed() => if res.is_err() { break; },
                        _ = async {
                            if let Some(d) = sleep_for {
                                tokio::time::sleep(d).await;
                            } else {
                                std::future::pending::<()>().await;
                            }
                        } => {}
                    }
                }
            });
        }
        for g in self.gateways() {
            let mut quota_events = g.quota_events();
            let sink = self.sink.clone();
            let client_id = g.client_id();
            tokio::spawn(async move {
                loop {
                    match quota_events.recv().await {
                        Ok(view) => sink.emit(Event::Quota {
                            client_id,
                            quota: &view,
                        }),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
            let mut events = g.subscribe();
            let r = self.clone();
            tokio::spawn(async move {
                // The view moves by the second, with no event to announce
                // it, while a circuit counts down or a request is
                // receiving its response. Only a web panel needs the
                // latter: the native one refreshes itself.
                let ticking = || {
                    g.has_health_wait()
                        || (cfg!(not(target_os = "macos"))
                            && g.try_view().is_ok_and(|view| {
                                view.pending_requests
                                    .iter()
                                    .any(|request| request.received_bytes.is_some())
                            }))
                };
                let mut waiting = ticking();
                loop {
                    tokio::select! {
                        event = events.recv() => {
                            if matches!(event, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                                break;
                            }
                            coalesce_gateway_events(&mut events);
                            waiting = ticking();
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(1)), if waiting => {
                            let current = ticking();
                            let refresh = waiting || current;
                            waiting = current;
                            // Include the final zero so an idle open circuit
                            // becomes visibly ready for a request probe.
                            if !refresh { continue; }
                        }
                    }
                    g.report_diagnostics();
                    r.publish_gateway(&g);
                }
            });
        }
        let runtime = self.clone();
        tokio::spawn(async move {
            let mut last = Some(state);
            let mut config_versions = std::collections::HashMap::new();
            let mut config_cache = std::collections::HashMap::new();
            let mut agy_config_version: Option<String> = None;
            let mut agy_config_cache = auth_cache::Cache::default();
            let mut agy_auth_stamp: Option<String> = None;
            let mut agy_auth_revision: Option<String> = None;
            let mut agy_auth_error: Option<String> = None;
            loop {
                let attentive = runtime.sink.attentive();
                wait_next_background_poll(attentive, &runtime.poll_wake).await;
                if runtime.quitting.load(Ordering::Relaxed) {
                    break;
                }
                if runtime.sink.attentive() {
                    watched.notify_waiters();
                }
                runtime
                    .gateway
                    .freeze_gate()
                    .resume_due(usage::model::now());
                match poll_account_state_step(&runtime.core, &runtime.gateway, &mut last) {
                    PollStateUpdate::StateChanged(s) => {
                        runtime.publish_gateway(&runtime.gateway);
                        runtime.publish(s);
                    }
                    PollStateUpdate::ErrorChanged(s) => {
                        runtime.publish(s);
                    }
                    PollStateUpdate::Unchanged => {}
                }
                if should_poll_editor_configs(
                    runtime.sink.attentive(),
                    [
                        &runtime.gateway,
                        &runtime.claude,
                        &runtime.agy,
                        &runtime.grok,
                        &runtime.dsh,
                    ]
                    .into_iter()
                    .any(gateway::Gateway::is_running)
                        || {
                            let sign_ins = runtime.sink.sign_ins();
                            sign_ins.codex || sign_ins.claude
                        },
                ) {
                    for client in [
                        gateway::ClientId::Codex,
                        gateway::ClientId::Claude,
                        gateway::ClientId::Grok,
                        gateway::ClientId::Dsh,
                    ] {
                        if let Ok(stamp) = runtime.home(client).and_then(|home| {
                            runtime
                                .gateway(client)
                                .config_stamp(&home, config_cache.entry(client).or_default())
                        }) {
                            if config_versions.get(&client) != Some(&stamp) {
                                runtime.publish_config(client, &stamp.1, stamp.2);
                                config_versions.insert(client, stamp);
                                if client != gateway::ClientId::Codex {
                                    runtime.publish_gateway(runtime.gateway(client));
                                }
                            }
                        }
                    }
                    if let Ok(rev) = antigravity::config_stamp(
                        &runtime.antigravity_home(),
                        &mut agy_config_cache,
                    ) {
                        if agy_config_version.as_ref() != Some(&rev) {
                            runtime.sink.emit(Event::Config(&serde_json::json!({"clientId":"antigravity","revision":rev,"guarded":false})));
                            agy_config_version = Some(rev);
                        }
                    }
                }
                // Like the Codex account, a new Antigravity login is picked
                // up whether or not anyone is watching.
                let paths = runtime.antigravity_paths();
                let mut stamp = agy_auth_stamp.take();
                let polled = tokio::task::spawn_blocking(move || {
                    let result = antigravity::auth::poll_state(&paths, &mut stamp);
                    (stamp, result)
                })
                .await;
                let result = match polled {
                    Ok((stamp, result)) => {
                        agy_auth_stamp = stamp;
                        result
                    }
                    Err(_) => Err(AppError::new(
                        code::PROCESS,
                        "读取 Antigravity 账号任务异常中断",
                    )),
                };
                match result {
                    Ok(state) => {
                        agy_auth_error = None;
                        if let Some(auth_state) = state {
                            if agy_auth_revision.as_ref() != Some(&auth_state.revision) {
                                agy_auth_revision = Some(auth_state.revision.clone());
                                runtime.sink.emit(Event::AntigravityAuth(&auth_state));
                            }
                        }
                    }
                    Err(error) => {
                        let signature = format!("{}:{}", error.code, error.message);
                        if agy_auth_error.as_ref() != Some(&signature) {
                            agy_auth_error = Some(signature);
                            let mut record = events::Record::new(
                                None,
                                None,
                                None,
                                events::Reason::AccountSync,
                                events::Action::Stopped,
                                None,
                                None,
                            );
                            record.client_id = Some(events::JournalClient::External(
                                events::ExternalClient::Antigravity,
                            ));
                            record.details.local_code = Some(error.code);
                            record.details.message = Some(error.message);
                            runtime.diagnostics.emit(record);
                        }
                    }
                }
                runtime.sink.tick();
            }
        });
        Ok(())
    }

    /// Passes on what the journal, the provider tests and the usage store
    /// announce, and starts the scheduled provider tests.
    fn report_services(self: &Arc<Self>) {
        use tokio::sync::broadcast::error::RecvError;
        let mut journal = self.diagnostics.subscribe();
        let sink = self.sink.clone();
        tokio::spawn(async move {
            loop {
                match journal.recv().await {
                    Ok(change) => sink.emit(Event::Journal(&change)),
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        });
        let mut tests = self.provider_tests.subscribe();
        let sink = self.sink.clone();
        tokio::spawn(async move {
            while let Ok(()) | Err(RecvError::Lagged(_)) = tests.recv().await {
                sink.emit(Event::ProviderTests);
            }
        });
        self.provider_tests.run_scheduler();
        let mut usage = self.usage.subscribe();
        let sink = self.sink.clone();
        tokio::spawn(async move {
            while let Ok(()) | Err(RecvError::Lagged(_)) = usage.recv().await {
                coalesce_gateway_events(&mut usage);
                sink.emit(Event::Usage);
            }
        });
    }

    /// Keeps prices current and, when the user asked for it, reads the
    /// clients' sessions every minute, or as rarely as the host lets them be
    /// read while nobody is watching. `watched` ends that rest.
    fn keep_usage(self: &Arc<Self>, watched: Arc<tokio::sync::Notify>) {
        // Price checks must not wait behind a session scan or historical compaction.
        self.usage.schedule_price_backfill();
        let runtime = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut scheduled = tokio::time::interval(std::time::Duration::from_secs(600));
            scheduled.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            scheduled.tick().await;
            // An unpriced row asks models.dev for newer prices sooner than the timer.
            let mut hinted = false;
            loop {
                let Some(runtime) = runtime.upgrade() else {
                    break;
                };
                let usage = runtime.usage.clone();
                if let Ok(prices) = usage.prices() {
                    let before = prices.sync_stamp();
                    let _ = usage.update_prices(false).await;
                    let after = prices.sync_stamp();
                    let index = prices.models_dev.view().version;
                    let max_age =
                        std::time::Duration::from_secs(if hinted { 3_600 } else { 21_600 });
                    let refreshed = prices
                        .models_dev
                        .refresh_stale(max_age)
                        .await
                        .is_ok_and(|view| view.version != index);
                    if refreshed {
                        usage.schedule_price_backfill();
                    }
                    if before.1 != after.1 || refreshed {
                        runtime.emit(Event::Pricing);
                    }
                }
                drop(runtime);
                // Coalesce hints from unpriced requests, including high-volume streams.
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                hinted = tokio::select! {
                    _ = scheduled.tick() => false,
                    _ = usage.wait_price_hint() => true,
                };
            }
        });
        let runtime = Arc::downgrade(self);
        let mut announced = self.usage.subscribe();
        tokio::spawn(async move {
            let mut maintained: Option<tokio::time::Instant> = None;
            loop {
                let Some(runtime) = runtime.upgrade() else {
                    break;
                };
                // What a gateway recorded since the last round, or the user
                // had done. A failed tidying is announced too, and so tried
                // again.
                let mut written = take_announced(&mut announced);
                let reading = runtime.usage.settings().auto_sync;
                if reading {
                    let roots = runtime.usage_roots();
                    let usage = runtime.usage.clone();
                    let _ =
                        tokio::task::spawn_blocking(move || usage.sync_roots(roots, None)).await;
                    // A reading announces itself whether or not it found
                    // anything, so its reports are asked instead.
                    take_announced(&mut announced);
                }
                let usage = runtime.usage.state();
                written |= reading && usage.reports.values().any(|report| report.imported > 0);
                let attentive = runtime.sink.attentive();
                if maintenance_due(
                    attentive || written || usage.maintenance.is_some(),
                    maintained.map(|at| at.elapsed()),
                ) {
                    maintained = Some(tokio::time::Instant::now());
                    let maintenance = runtime.usage.clone();
                    let _ = tokio::task::spawn_blocking(move || maintenance.maintain()).await;
                }
                drop(runtime);
                wait_next_usage_scan(attentive, UNWATCHED_USAGE_SCAN_INTERVAL, &watched).await;
            }
        });
    }

    fn gateways(&self) -> [gateway::Gateway; 5] {
        [
            self.gateway.clone(),
            self.claude.clone(),
            self.agy.clone(),
            self.grok.clone(),
            self.dsh.clone(),
        ]
    }

    pub(crate) fn gateway(&self, client: gateway::ClientId) -> &gateway::Gateway {
        match client {
            gateway::ClientId::Codex => &self.gateway,
            gateway::ClientId::Claude => &self.claude,
            gateway::ClientId::Agy => &self.agy,
            gateway::ClientId::Grok => &self.grok,
            gateway::ClientId::Dsh => &self.dsh,
        }
    }
    /// Where a client keeps what the gateway changes for it. For Antigravity
    /// that is the user's own home: its `agy` function lives in the shell
    /// startup files there.
    pub(crate) fn home(&self, client: gateway::ClientId) -> Result<PathBuf> {
        let core = self.core.lock();
        Ok(match client {
            gateway::ClientId::Codex => core.home(),
            gateway::ClientId::Claude => PathBuf::from(core.preferences().claude_home),
            gateway::ClientId::Agy => self.user_home.clone(),
            gateway::ClientId::Grok => PathBuf::from(core.preferences().grok_home),
            gateway::ClientId::Dsh => PathBuf::from(core.preferences().dsh_home),
        })
    }
    /// Where Antigravity keeps its settings and, off the system credential
    /// store, its login.
    pub(crate) fn antigravity_home(&self) -> PathBuf {
        self.user_home.join(".gemini")
    }
    pub(crate) fn antigravity_paths(&self) -> antigravity::auth::Paths {
        antigravity::auth::Paths::from_roots(&self.user_home, &self.data)
    }
    /// The session directories usage is read from, by source.
    pub(crate) fn usage_roots(&self) -> Vec<(String, Result<PathBuf>)> {
        vec![
            ("codex".into(), self.home(gateway::ClientId::Codex)),
            ("claude".into(), self.home(gateway::ClientId::Claude)),
            ("grok".into(), self.home(gateway::ClientId::Grok)),
            ("antigravity".into(), Ok(self.antigravity_home())),
        ]
    }
    /// Hands every client back for good, as turning each gateway off does.
    /// Building the runtime has already settled a run that was killed, so
    /// nothing is started: each gateway gives up coming back with the next
    /// run, and the agy function leaves the shell profiles. An error means a
    /// client may still point at the gateway.
    #[cfg(any(feature = "desktop", test))]
    pub(crate) async fn release_clients(&self) -> Result<()> {
        let _official_guard = self.official_tx.lock().await;
        let mut outcome = Ok(());
        for gateway in self.gateways() {
            let released = gateway.stop_checked(None).await.and_then(|view| {
                if view.running || view.recovery_pending {
                    Err(AppError::new(code::RECOVERY, "客户端配置尚未还原"))
                } else {
                    Ok(())
                }
            });
            outcome = outcome.and(released);
        }
        outcome
    }
    pub(crate) fn emit(&self, event: Event<'_>) {
        self.sink.emit(event);
    }
    pub(crate) fn notice(&self, message: &str) {
        self.sink.emit(Event::Notice(message));
    }
    pub(crate) fn sign_ins(&self) -> SignIns {
        self.sink.sign_ins()
    }
    /// Announces a client's configuration file as it now stands.
    pub(crate) fn publish_config(&self, client: gateway::ClientId, revision: &str, guarded: bool) {
        self.sink.emit(Event::Config(
            &serde_json::json!({"clientId":client,"revision":revision,"guarded":guarded}),
        ));
    }
    pub(crate) fn set_quota_interval(&self, seconds: u64) {
        for gateway in [
            &self.gateway,
            &self.claude,
            &self.agy,
            &self.grok,
            &self.dsh,
        ] {
            gateway.set_quota_interval(seconds);
        }
    }
    pub(crate) fn config_failure(&self, client: gateway::ClientId, error: AppError) -> AppError {
        if let Some(record) = events::Record::configuration_failure(client, &error, false) {
            self.diagnostics.emit(record);
        }
        error
    }
    pub(crate) async fn stop_gateways(&self) -> Result<()> {
        let _official_guard = self.official_tx.lock().await;
        let a = self.gateway.stop_for_exit().await;
        let b = self.claude.stop_for_exit().await;
        let c = self.agy.stop_for_exit().await;
        let d = self.grok.stop_for_exit().await;
        let e = self.dsh.stop_for_exit().await;
        a.and(b).and(c).and(d).and(e).map(|_| ())
    }

    pub(crate) fn state(&self) -> Result<ViewState> {
        self.core.lock().state()
    }
    pub(crate) fn publish(&self, state: ViewState) {
        emit_account_sync_diagnostic(&self.diagnostics, &state);
        self.sink.emit(Event::State(&state));
    }
    /// Reads the account state afresh and publishes it.
    pub(crate) fn refresh(&self) -> Result<ViewState> {
        let state = self.state()?;
        let home = self.core.lock().home();
        self.gateway.observe_home(&home);
        self.publish(state.clone());
        Ok(state)
    }
    /// Publishes a gateway's view, which is only read when someone is watching.
    pub(crate) fn publish_gateway(&self, gateway: &gateway::Gateway) {
        if !self.sink.attentive() {
            return;
        }
        match gateway.try_view() {
            Ok(view) => self.sink.emit(Event::Gateway(&view)),
            Err(error) => self.sink.emit(Event::Error(&error)),
        }
    }
    /// Keeps a startup failure for the main window to collect, or reports it
    /// at once when the window already collected what there was.
    pub(crate) fn record_startup_error(&self, error: AppError, overwrite: bool) {
        let already_consumed = {
            let mut slot = self.startup_error.lock();
            if self.startup_error_consumed.load(Ordering::Acquire) {
                true
            } else {
                if overwrite || slot.is_none() {
                    *slot = Some(error.clone());
                }
                false
            }
        };
        if already_consumed {
            self.sink.emit(Event::Error(&error));
        } else {
            self.sink.emit(Event::StartupError(&error));
        }
    }
}

pub(crate) const VISIBLE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
pub(crate) const HIDDEN_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

pub(crate) async fn wait_next_background_poll(
    has_visible_windows: bool,
    wake: &tokio::sync::Notify,
) {
    if has_visible_windows {
        tokio::time::sleep(VISIBLE_POLL_INTERVAL).await;
    } else {
        tokio::select! {
            _ = tokio::time::sleep(HIDDEN_POLL_INTERVAL) => {}
            _ = wake.notified() => {}
        }
    }
}

pub(crate) const USAGE_SCAN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
/// How long the clients' sessions go unread while nobody is watching. The
/// desktop does not let them: the Antigravity app answers for a conversation
/// only while it runs, so a session read late may be read without its details.
/// A server has no such app, and what it reads are files that stay.
pub(crate) const UNWATCHED_USAGE_SCAN_INTERVAL: std::time::Duration = if cfg!(feature = "desktop") {
    USAGE_SCAN_INTERVAL
} else {
    std::time::Duration::from_secs(600)
};

/// Waits for the next reading of the clients' sessions. The wait of a run
/// nobody watches is `unwatched` long, and when `wake` says someone has come
/// it ends where it would have, had they been there from its start.
pub(crate) async fn wait_next_usage_scan(
    watched: bool,
    unwatched: std::time::Duration,
    wake: &tokio::sync::Notify,
) {
    let watched_next = tokio::time::Instant::now() + USAGE_SCAN_INTERVAL;
    if !watched {
        tokio::select! {
            _ = tokio::time::sleep(unwatched) => return,
            _ = wake.notified() => {}
        }
    }
    tokio::time::sleep_until(watched_next).await;
}

/// How long the usage store goes untidied while nothing calls for it. Records
/// grow old with nothing written: after a week their details are put away,
/// and after a month they join the totals of their day.
pub(crate) const UNCALLED_MAINTENANCE_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(3600);

/// Whether the usage store is tidied this round. Something calls for it when
/// someone is watching, when records were written, or when the last tidying
/// left work it could not finish; `since_last` is how long ago that was.
pub(crate) fn maintenance_due(called_for: bool, since_last: Option<std::time::Duration>) -> bool {
    called_for || since_last.is_none_or(|rested| rested >= UNCALLED_MAINTENANCE_INTERVAL)
}

/// Forgets what `events` announced so far and tells whether there was any of
/// it.
pub(crate) fn take_announced(events: &mut tokio::sync::broadcast::Receiver<()>) -> bool {
    let any = !events.is_empty();
    coalesce_gateway_events(events);
    any
}

pub(crate) const fn should_poll_editor_configs(
    has_visible_windows: bool,
    background_active: bool,
) -> bool {
    has_visible_windows || background_active
}

pub(crate) fn emit_account_sync_diagnostic(
    diagnostics: &events::Service,
    state: &ViewState,
) -> bool {
    let should_emit = state.error.is_some()
        || state
            .auth_sync
            .as_ref()
            .is_some_and(|s| matches!(s.state.as_str(), "invalid" | "missing" | "error"));
    if should_emit {
        let mut record = events::Record::new(
            Some(gateway::ClientId::Codex),
            None,
            None,
            events::Reason::AccountSync,
            events::Action::Stopped,
            None,
            None,
        );
        record.details.local_code = Some(code::AUTH.into());
        record.details.message = state
            .error
            .clone()
            .or_else(|| state.auth_sync.as_ref().map(|s| s.message.clone()));
        diagnostics.emit(record);
    }
    should_emit
}

pub(crate) enum PollStateUpdate {
    Unchanged,
    StateChanged(ViewState),
    ErrorChanged(ViewState),
}

pub(crate) fn poll_account_state_step(
    core: &Mutex<Core>,
    gateway: &gateway::Gateway,
    last: &mut Option<ViewState>,
) -> PollStateUpdate {
    let result = {
        let mut c = core.lock();
        c.state().map(|s| (s, c.home()))
    };
    match result {
        Ok((s, home)) => {
            if last.as_ref() != Some(&s) {
                gateway.observe_home(&home);
                *last = Some(s.clone());
                return PollStateUpdate::StateChanged(s);
            }
        }
        Err(e) => {
            if let Some(mut s) = last.clone() {
                if s.error.as_ref() != Some(&e.message) {
                    s.error = Some(e.message);
                    *last = Some(s.clone());
                    return PollStateUpdate::ErrorChanged(s);
                }
            }
        }
    }
    PollStateUpdate::Unchanged
}

pub(crate) fn coalesce_gateway_events(events: &mut tokio::sync::broadcast::Receiver<()>) {
    // Drain only the backlog observed now. A busy producer cannot postpone
    // publishing the latest view or the periodic final zero indefinitely.
    let pending = events.len();
    for _ in 0..pending {
        match events.try_recv() {
            Ok(()) | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => (),
            Err(_) => break,
        }
    }
}
#[cfg(test)]
mod gateway_broadcast_tests {
    #[tokio::test]
    async fn coalesced_burst_publishes_latest_and_leaves_future_notification_live() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(8);
        let mut final_state = 0;
        for n in 1..=32 {
            final_state = n;
            tx.send(()).unwrap();
        }
        let _ = rx.recv().await;
        super::coalesce_gateway_events(&mut rx);
        assert_eq!(final_state, 32);
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        tx.send(()).unwrap();
        rx.recv().await.unwrap();
        drop(tx);
        super::coalesce_gateway_events(&mut rx);
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Closed)
        ));
    }
}

#[cfg(test)]
mod idle_tests {
    use super::*;
    use std::time::Duration;

    const REST: Duration = Duration::from_secs(600);
    const MOMENT: Duration = Duration::from_millis(1);

    /// Lets `wait` run as far as it can and tells whether it has ended.
    async fn ended(wait: &tokio::task::JoinHandle<()>) -> bool {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        wait.is_finished()
    }

    /// A wait for the next reading that has begun.
    async fn waiting(
        watched: bool,
        wake: &Arc<tokio::sync::Notify>,
    ) -> tokio::task::JoinHandle<()> {
        let wake = wake.clone();
        let wait = tokio::spawn(async move { wait_next_usage_scan(watched, REST, &wake).await });
        assert!(!ended(&wait).await);
        wait
    }

    #[tokio::test(start_paused = true)]
    async fn sessions_are_read_every_minute_while_someone_watches() {
        let wake = Arc::new(tokio::sync::Notify::new());
        let wait = waiting(true, &wake).await;
        tokio::time::advance(USAGE_SCAN_INTERVAL - MOMENT).await;
        assert!(!ended(&wait).await);
        tokio::time::advance(MOMENT).await;
        assert!(ended(&wait).await);
    }

    #[tokio::test(start_paused = true)]
    async fn unwatched_sessions_rest_until_someone_comes() {
        let wake = Arc::new(tokio::sync::Notify::new());

        // Nobody comes: the whole rest, and no longer.
        let wait = waiting(false, &wake).await;
        tokio::time::advance(REST - MOMENT).await;
        assert!(!ended(&wait).await);
        tokio::time::advance(MOMENT).await;
        assert!(ended(&wait).await);

        // Someone comes early: the minute they would have waited is kept.
        let wait = waiting(false, &wake).await;
        tokio::time::advance(Duration::from_secs(20)).await;
        wake.notify_waiters();
        tokio::time::advance(Duration::from_secs(40) - MOMENT).await;
        assert!(!ended(&wait).await);
        tokio::time::advance(MOMENT).await;
        assert!(ended(&wait).await);

        // Someone comes after that minute: the sessions are read at once.
        let wait = waiting(false, &wake).await;
        tokio::time::advance(Duration::from_secs(200)).await;
        assert!(!ended(&wait).await);
        wake.notify_waiters();
        assert!(ended(&wait).await);

        // Having been told while nothing waited leaves nothing behind.
        wake.notify_waiters();
        let wait = waiting(false, &wake).await;
        tokio::time::advance(REST - MOMENT).await;
        assert!(!ended(&wait).await);
        wait.abort();
    }

    #[test]
    fn the_usage_store_is_tidied_when_called_for_and_otherwise_once_an_hour() {
        let hour = UNCALLED_MAINTENANCE_INTERVAL;
        // A run begins with it.
        assert!(maintenance_due(false, None));
        assert!(!maintenance_due(false, Some(Duration::ZERO)));
        assert!(!maintenance_due(false, Some(hour - MOMENT)));
        assert!(maintenance_due(false, Some(hour)));
        assert!(maintenance_due(true, Some(Duration::ZERO)));
    }

    #[test]
    fn what_was_announced_is_taken_once() {
        let (announce, mut announced) = tokio::sync::broadcast::channel(4);
        assert!(!take_announced(&mut announced));
        announce.send(()).unwrap();
        announce.send(()).unwrap();
        assert!(take_announced(&mut announced));
        assert!(!take_announced(&mut announced));

        // More than the channel holds is still something, and is all taken.
        for _ in 0..16 {
            announce.send(()).unwrap();
        }
        assert!(take_announced(&mut announced));
        assert!(!take_announced(&mut announced));
        announce.send(()).unwrap();
        assert!(take_announced(&mut announced));
    }
}

#[cfg(test)]
mod release_tests {
    use super::*;
    use crate::gateway::{form, Edit, Settings};
    use serde_json::json;

    /// A runtime whose every directory is under `root`.
    fn runtime(root: &std::path::Path) -> Arc<Runtime> {
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
        Runtime::at(Arc::new(Silent), root.to_path_buf(), root.join(".codex")).unwrap()
    }

    #[tokio::test]
    async fn a_run_that_was_killed_is_handed_back_and_does_not_come_back() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join(".codex");
        std::fs::create_dir_all(&home).unwrap();
        let config = home.join("config.toml");
        std::fs::write(
            &config,
            "model_provider='custom'\n[model_providers.custom]\nbase_url = \"https://a.example.invalid\"\nexperimental_bearer_token = \"upstream-fixture-token\"\nwire_api='responses'\n",
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let first = runtime(root.path());
        let codex = &first.gateway;
        for edit in [
            Edit::Settings {
                settings: Settings {
                    port,
                    network_probe: false,
                    ..Default::default()
                },
            },
            form(
                json!({"baseUrl": "https://a.example.invalid", "token": "upstream-fixture-token"}),
            ),
        ] {
            codex.edit(edit, &codex.view().revision, &home).unwrap();
        }
        codex.start(&codex.view().revision, &home).await.unwrap();
        let taken = std::fs::read_to_string(&config).unwrap();
        assert!(taken.contains(&format!("127.0.0.1:{port}")), "{taken}");
        // The run ends without its exit handler: `first` is simply forgotten.

        let cleanup = runtime(root.path());
        cleanup.release_clients().await.unwrap();
        let released = std::fs::read_to_string(&config).unwrap();
        assert!(released.contains("https://a.example.invalid"), "{released}");
        assert!(!released.contains("127.0.0.1"), "{released}");

        let next = runtime(root.path());
        next.gateway.resume(&home).await.unwrap();
        assert!(!next.gateway.view().running);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), released);
        drop(first);
    }
}
