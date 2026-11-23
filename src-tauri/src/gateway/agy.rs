//! Antigravity CLI on the gateway. Nothing of Antigravity's own is changed:
//! `agy` takes a gateway from its environment, so a terminal reaches this one
//! through the function in `agy_shell`, and the desktop app keeps its Google
//! account. Here `home` is the user's home folder, where startup files live.
use super::{agy_shell, *};

/// How a terminal's `agy` reaches the gateway while it runs.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ShellView {
    /// Startup files that hold the function, as the user writes them.
    pub profiles: Vec<String>,
    /// The same by hand, for a shell none of those files serves.
    pub command: Option<String>,
}

pub(super) fn current(running: bool) -> current::Current {
    let mut value = current::Current::new(if running {
        current::Kind::Gateway
    } else {
        current::Kind::Official
    });
    value.protocol = Some(provider_protocol::Protocol::GeminiNative);
    value
}

/// The address and local token of the running gateway, as `agy` reads them.
/// The file exists exactly while the gateway runs.
fn environment_file(data: &Path) -> PathBuf {
    data.join("agy.env")
}

fn forget_environment(data: &Path) -> Result<()> {
    match std::fs::remove_file(environment_file(data)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(storage::io_error(error)),
        _ => Ok(()),
    }
}

impl Gateway {
    pub fn agy_companion(&self, data: PathBuf) -> Result<Self> {
        // A file left by a run that never stopped names a gateway that is gone.
        forget_environment(&data)?;
        Self::new_client(data, ClientId::Agy, Some(self.0.registry.clone()))
    }

    pub(super) fn agy_shell(&self, running: bool) -> Option<ShellView> {
        (self.0.client == ClientId::Agy && running).then(|| ShellView {
            profiles: self.0.agy_profiles.lock().clone(),
            command: agy_shell::command(&environment_file(&self.0.data)),
        })
    }

    pub(super) async fn start_agy(&self, home: &Path, expected: &str) -> Result<View> {
        let _listener = self.0.registry.lifecycle.lock().await;
        let _lifecycle = self.0.lifecycle.lock().await;
        let token = {
            let s = self.0.inner.lock();
            if self.view_revision(&s)? != expected {
                return Err(AppError::new(code::CONFLICT, "网关设置已变化，请重试"));
            }
            if s.running {
                drop(s);
                return self.try_view();
            }
            if let Some(error) = &s.recovery_error {
                return Err(error.clone());
            }
            s.store.local_token.clone()
        };
        let listener = self.0.registry.listener.prepare().await?;
        self.0.registry.update_ports()?;
        self.0.registry.listener.check_revision()?;
        let token = self.0.registry.unique_local_token(ClientId::Agy, &token);
        let file = environment_file(&self.0.data);
        let environment = agy_shell::environment(&ClientId::Agy.address(listener.port), &token)
            .ok_or_else(|| AppError::new(code::STATE, "本地令牌无法写入 agy 环境文件"))?;
        {
            let mut s = self.0.inner.lock();
            if self.view_revision_with_listener(&s, &listener.revision) != expected {
                return Err(AppError::new(
                    code::CONFLICT,
                    "绑定端口期间设置已变化，请重试",
                ));
            }
            let mut next = s.store.clone();
            next.local_token = token;
            next.resume = Some(model::Resume {
                home: home.to_owned(),
                desired: true,
                pair_hash: String::new(),
            });
            self.0.registry.listener.activate_with(
                ClientId::Agy,
                listener.socket,
                self.0.registry.clone(),
                || {
                    storage::atomic_write(&file, environment.as_bytes(), None)?;
                    match next.persist(&self.0.data.join("gateway.json"), &s.revision) {
                        Ok(revision) => s.revision = revision,
                        Err(error) => {
                            let _ = forget_environment(&self.0.data);
                            return Err(error);
                        }
                    }
                    self.replace_store(&mut s, next);
                    {
                        let editable = s.editable_mut();
                        editable.home = Some(home.to_owned());
                        editable.running = true;
                    }
                    s.last_successful = None;
                    s.shutdown = Some(watch::channel(false).0);
                    s.error = None;
                    self.0.admission.configure(&s.store.providers, true);
                    self.0.compaction.provider_names(&s.store.providers);
                    self.wake_endpoint_prober();
                    Ok(())
                },
            )?;
        }
        // The gateway runs whether or not a startup file took the function:
        // the command shown beside it reaches the gateway all the same.
        let installed = agy_shell::install(&agy_shell::Shells::detect(home), &file);
        if !installed.failures.is_empty() {
            self.0.inner.lock().error = Some(format!(
                "未能写入 agy 函数：{}",
                installed.failures.join("；")
            ));
        }
        *self.0.agy_profiles.lock() = installed.profiles;
        self.0.registry.changed();
        self.try_view()
    }

    /// Quitting Vela keeps the function for the next start; turning the
    /// gateway off takes it out of the startup files again.
    pub(super) async fn stop_agy(&self, preserve: bool) -> Result<View> {
        let _listener = self.0.registry.lifecycle.lock().await;
        let _lifecycle = self.0.lifecycle.lock().await;
        let home = {
            let mut s = self.0.inner.lock();
            if !preserve {
                let mut next = s.store.clone();
                if let Some(resume) = next.resume.as_mut().filter(|r| r.desired) {
                    resume.desired = false;
                    s.revision = next.persist(&self.0.data.join("gateway.json"), &s.revision)?;
                    self.replace_store(&mut s, next);
                }
            }
            if let Some(shutdown) = s.shutdown.take() {
                let _ = shutdown.send(true);
            }
            s.editable_mut().running = false;
            s.error = None;
            self.0.admission.configure(&s.store.providers, false);
            self.wake_endpoint_prober();
            s.home
                .clone()
                .or_else(|| s.store.resume.as_ref().map(|r| r.home.clone()))
        };
        // Without the file every `agy` is the plain one, in open terminals too.
        let mut failures: Vec<String> = forget_environment(&self.0.data)
            .err()
            .map(|error| error.message)
            .into_iter()
            .collect();
        if !preserve {
            if let Some(home) = &home {
                failures.extend(agy_shell::remove(&agy_shell::Shells::detect(home)));
            }
            self.0.agy_profiles.lock().clear();
        }
        if !failures.is_empty() {
            self.0.inner.lock().error = Some(format!("未能撤回 agy 函数：{}", failures.join("；")));
        }
        self.0.registry.listener.deactivate(ClientId::Agy).await?;
        self.0.clients.lock().clear();
        self.0.registry.changed();
        self.try_view()
    }
}

#[cfg(test)]
#[path = "agy_tests.rs"]
mod tests;
