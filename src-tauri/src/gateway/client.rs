use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_SHARED_PORT: u16 = 43721;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientId {
    #[default]
    Codex,
    Claude,
    /// Antigravity CLI (`agy`). The wire name stays `agy`: `antigravity` in
    /// the journal already means the app's own logs.
    Agy,
    Grok,
    Dsh,
}
impl ClientId {
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
            Self::Agy => "Antigravity",
            Self::Grok => "Grok Build",
            Self::Dsh => "DeepSeek Harness",
        }
    }
    /// The platform a client's calls are filed under in the usage ledger.
    /// Antigravity is one platform there, whichever way a call was seen: by
    /// this gateway, from the app's own server or from a CLI session.
    pub fn usage_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Agy => "antigravity",
            Self::Grok => "grok",
            Self::Dsh => "dsh",
        }
    }
    pub fn address(self, port: u16) -> String {
        format!(
            "http://127.0.0.1:{port}{}",
            match self {
                Self::Codex | Self::Grok | Self::Dsh => "/v1",
                Self::Claude | Self::Agy => "",
            }
        )
    }
    /// The file a gateway manages for its client. Antigravity has none and
    /// never asks: its gateway comes from the shell that starts it.
    pub fn config(self, home: &Path) -> PathBuf {
        if self == Self::Dsh {
            return home.join("profiles");
        }

        home.join(if self == Self::Claude {
            "settings.json"
        } else {
            "config.toml"
        })
    }
    pub fn default_protocol(self) -> super::provider_protocol::Protocol {
        use super::provider_protocol::Protocol;
        match self {
            Self::Codex | Self::Grok => Protocol::Responses,
            Self::Claude => Protocol::Messages,
            Self::Agy => Protocol::GeminiNative,
            Self::Dsh => Protocol::ChatCompletions,
        }
    }
    /// Upstream statuses endless mode treats as risk control unless the
    /// provider lists its own. A 429 is a rate limit, which has its own wait.
    pub fn default_risk_codes(self) -> &'static [u16] {
        match self {
            Self::Claude => &[520],
            Self::Dsh => &[524],
            Self::Codex | Self::Agy | Self::Grok => &[],
        }
    }
    pub fn accepts_protocol(self, protocol: super::provider_protocol::Protocol) -> bool {
        self.default_protocol() == protocol
            || (self == Self::Grok && protocol != super::provider_protocol::Protocol::GeminiNative)
    }
}

pub fn loopback_address(base: &str) -> bool {
    url::Url::parse(base)
        .ok()
        .is_some_and(|url| match url.host() {
            Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(host)) => host.is_loopback(),
            Some(url::Host::Ipv6(host)) => host.is_loopback(),
            None => false,
        })
}

pub fn claude_home() -> String {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude"))
        .to_string_lossy()
        .into_owned()
}

pub fn grok_home() -> String {
    std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".grok"))
        .to_string_lossy()
        .into_owned()
}
pub fn dsh_home() -> String {
    std::env::var_os("DSH_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".dsh"))
        .to_string_lossy()
        .into_owned()
}
