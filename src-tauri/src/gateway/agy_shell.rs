//! The `agy` function Vela keeps in the user's shell startup files.
//!
//! Antigravity CLI takes a gateway from its environment alone, so the way to
//! put a terminal's `agy` on this gateway is a function of that name. Its
//! text is fixed: it reads the address and the local token from a private
//! file in Vela's data directory. The token therefore never enters a dotfile,
//! and a terminal that is already open follows the gateway as it starts and
//! stops. Vela writes and removes only the lines between its two markers.
use crate::{
    errors::code,
    storage::{self, AppError, Result},
};
use std::path::{Path, PathBuf};

pub const OPEN: &str = "# >>> vela agy >>>";
pub const CLOSE: &str = "# <<< vela agy <<<";
const BOM: &[u8] = b"\xef\xbb\xbf";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Posix,
    Fish,
    PowerShell,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    pub kind: Kind,
    pub path: PathBuf,
    /// Made when missing: the startup file of the shell a terminal opens.
    pub create: bool,
}

/// What decides where a user's shells look for their startup files.
#[derive(Clone, Debug, Default)]
pub struct Shells {
    pub home: PathBuf,
    pub login: Option<String>,
    pub zdotdir: Option<PathBuf>,
    pub xdg_config: Option<PathBuf>,
    pub documents: Option<PathBuf>,
}

impl Shells {
    /// A home other than the user's own keeps to itself: its startup files
    /// are the ones inside it, whatever this process's environment names.
    pub fn detect(home: &Path) -> Self {
        let path = |name: &str| std::env::var_os(name).map(PathBuf::from);
        Self {
            home: home.to_owned(),
            login: std::env::var("SHELL").ok(),
            zdotdir: path("ZDOTDIR"),
            xdg_config: path("XDG_CONFIG_HOME"),
            documents: if dirs::home_dir().as_deref() == Some(home) {
                dirs::document_dir()
            } else {
                Some(home.join("Documents"))
            },
        }
    }

    pub fn profiles(&self) -> Vec<Profile> {
        if cfg!(windows) {
            // Only a profile that is already there: one made here could be
            // refused by the execution policy and put an error in every new
            // terminal.
            return self
                .documents
                .iter()
                .flat_map(|documents| {
                    ["PowerShell", "WindowsPowerShell"].map(|folder| Profile {
                        kind: Kind::PowerShell,
                        path: documents
                            .join(folder)
                            .join("Microsoft.PowerShell_profile.ps1"),
                        create: false,
                    })
                })
                .collect();
        }
        let login = self
            .login
            .as_deref()
            .and_then(|shell| Path::new(shell).file_name())
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let absolute = |dir: &Option<PathBuf>| dir.clone().filter(|dir| dir.is_absolute());
        let zsh = absolute(&self.zdotdir)
            .unwrap_or_else(|| self.home.clone())
            .join(".zshrc");
        let bash = if cfg!(target_os = "macos") {
            // A Mac terminal opens a login bash, which reads the first of these.
            [".bash_profile", ".bash_login", ".profile"]
                .into_iter()
                .map(|name| self.home.join(name))
                .find(|path| path.symlink_metadata().is_ok())
                .unwrap_or_else(|| self.home.join(".bash_profile"))
        } else {
            self.home.join(".bashrc")
        };
        let fish = absolute(&self.xdg_config)
            .unwrap_or_else(|| self.home.join(".config"))
            .join("fish")
            .join("config.fish");
        // fish makes its directory on first start and reads config.fish there.
        let fish_used = fish.parent().is_some_and(Path::is_dir);
        [
            (Kind::Posix, zsh, login == "zsh"),
            (Kind::Posix, bash, login == "bash"),
            (Kind::Fish, fish, login == "fish" || fish_used),
        ]
        .into_iter()
        // A startup file outside the home is not this user's to change.
        .filter(|(_, path, _)| path.starts_with(&self.home))
        .map(|(kind, path, create)| Profile { kind, path, create })
        .collect()
    }

    /// `path` as the user writes it.
    pub fn display(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) => Path::new("~").join(rest).to_string_lossy().into_owned(),
            Err(_) => path.to_string_lossy().into_owned(),
        }
    }
}

/// The three variables Antigravity CLI reads an enterprise gateway from, one
/// per line. The function hands the lines to `env` unquoted, so each value
/// must be free of blanks and shell patterns.
pub fn environment(address: &str, token: &str) -> Option<String> {
    let plain = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.:/".contains(&c))
    };
    (plain(address) && plain(token)).then(|| {
        format!(
            "AGY_LLM_GATEWAY_URL={address}\nAGY_LLM_GATEWAY_API_KEY={token}\nAGY_LLM_GATEWAY_WIRE_PROTOCOL=genai\n"
        )
    })
}

/// The function for one kind of shell, without the marker lines. Remote
/// control needs a Google sign-in, which a gateway session never has, so
/// those invocations run the plain `agy`; so does everything while the
/// environment file is absent, which is whenever the gateway is off.
pub fn function(kind: Kind, environment: &Path) -> Option<String> {
    let path = environment
        .to_str()
        .filter(|path| !path.chars().any(char::is_control))?;
    Some(match kind {
        Kind::Posix => {
            let file = format!("'{}'", path.replace('\'', r"'\''"));
            // `agy() {` rather than bash's `function agy {`, which dash
            // refuses when ~/.profile is read by sh. An alias of the same
            // name would expand in the definition, so it goes first.
            format!(
                r#"unalias agy 2>/dev/null || true
agy() {{
  if [ -r {file} ]; then
    for vela_agy_arg in "$@"; do
      case "$vela_agy_arg" in
        remote-control|--remote-control|--remote-control=*|-remote-control|-remote-control=*)
          unset vela_agy_arg
          command agy "$@"
          return ;;
      esac
    done
    unset vela_agy_arg
    env $(cat {file}) agy "$@"
  else
    command agy "$@"
  fi
}}"#
            )
        }
        Kind::Fish => {
            let file = format!("'{}'", path.replace('\\', r"\\").replace('\'', r"\'"));
            format!(
                r#"function agy
    if test -r {file}
        for vela_agy_arg in $argv
            switch $vela_agy_arg
                case remote-control --remote-control '--remote-control=*' -remote-control '-remote-control=*'
                    command agy $argv
                    return
            end
        end
        env (cat {file}) agy $argv
    else
        command agy $argv
    end
end"#
            )
        }
        Kind::PowerShell => {
            let file = format!("'{}'", path.replace('\'', "''"));
            format!(
                r#"function agy {{
    $real = Get-Command agy -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $real) {{ Write-Error 'agy is not installed'; return }}
    $file = {file}
    if (-not (Test-Path -LiteralPath $file) -or @($args | Where-Object {{ "$_" -match '^--?remote-control(=|$)|^remote-control$' }}).Count) {{ & $real @args; return }}
    $was = @{{}}
    foreach ($line in Get-Content -LiteralPath $file) {{
        $name, $value = "$line" -split '=', 2
        if ($name -like 'AGY_LLM_GATEWAY_*' -and $value) {{
            $was[$name] = [Environment]::GetEnvironmentVariable($name)
            Set-Item -LiteralPath "Env:$name" -Value $value
        }}
    }}
    try {{ & $real @args }} finally {{
        foreach ($name in $was.Keys) {{
            if ($null -eq $was[$name]) {{ Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }} else {{ Set-Item -LiteralPath "Env:$name" -Value $was[$name] }}
        }}
    }}
}}"#
            )
        }
    })
}

fn line(text: &str) -> &str {
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.strip_suffix('\r').unwrap_or(text)
}

/// Byte range of Vela's block in `text`: from its opening line through its
/// closing line and that line's ending. An opening line with no closing one
/// was cut by hand, and then only that line is Vela's.
fn block(text: &str) -> Option<std::ops::Range<usize>> {
    let mut offset = 0;
    let mut start = None;
    for raw in text.split_inclusive('\n') {
        let end = offset + raw.len();
        match start {
            None if line(raw) == OPEN => start = Some(offset..end),
            Some(ref open) if line(raw) == CLOSE => return Some(open.start..end),
            _ => {}
        }
        offset = end;
    }
    start
}

/// What starts `agy` on the gateway from a shell that has no function, such
/// as one whose startup file Vela does not know. `None` on Windows, where the
/// same takes a script rather than a line.
pub fn command(environment: &Path) -> Option<String> {
    let path = environment
        .to_str()
        .filter(|path| !path.chars().any(char::is_control))?;
    (!cfg!(windows)).then(|| format!("env $(cat '{}') agy", path.replace('\'', r"'\''")))
}

/// `text` with Vela's block holding `body`, in place of the one it has or
/// after the rest; `None` takes the block out together with the blank line
/// put before it. Every other byte is kept, the file's line endings included;
/// `crlf` is the ending for a file that has no line yet.
pub fn with_block(text: &str, body: Option<&str>, crlf: bool) -> String {
    let newline = if text.contains("\r\n") || (text.is_empty() && crlf) {
        "\r\n"
    } else {
        "\n"
    };
    let written = body.map(|body| {
        let mut out = String::new();
        for line in std::iter::once(OPEN)
            .chain(body.lines())
            .chain(std::iter::once(CLOSE))
        {
            out.push_str(line);
            out.push_str(newline);
        }
        out
    });
    match (block(text), written) {
        (Some(range), Some(written)) => {
            let ended = text[range.clone()].ends_with('\n');
            let written = if ended {
                written.as_str()
            } else {
                written.trim_end_matches(['\r', '\n'])
            };
            format!("{}{written}{}", &text[..range.start], &text[range.end..])
        }
        (Some(range), None) => {
            let before = &text[..range.start];
            let blank = before
                .strip_suffix('\n')
                .map(|rest| rest.strip_suffix('\r').unwrap_or(rest))
                .filter(|rest| rest.is_empty() || rest.ends_with('\n'))
                .map_or(before.len(), str::len);
            format!("{}{}", &text[..blank], &text[range.end..])
        }
        (None, Some(written)) => {
            let mut out = text.to_owned();
            if !text.is_empty() {
                if !text.ends_with('\n') {
                    out.push_str(newline);
                }
                out.push_str(newline);
            }
            out + &written
        }
        (None, None) => text.to_owned(),
    }
}

/// Writes the block into one startup file, or takes it out for `None`.
/// Says whether the file holds the block afterwards.
fn apply(profile: &Profile, body: Option<&str>) -> Result<bool> {
    let raw = storage::read_user_optional(&profile.path)?;
    if raw.is_none() && (body.is_none() || !profile.create) {
        return Ok(false);
    }
    let revision = storage::revision(raw.as_deref());
    let bytes = raw.as_deref().unwrap_or_default();
    let (marked, bytes) = match bytes.strip_prefix(BOM) {
        Some(rest) => (true, rest),
        None => (false, bytes),
    };
    let text = std::str::from_utf8(bytes)
        .map_err(|_| AppError::new(code::CONFIG, "启动文件不是 UTF-8 文本，已保留原文件"))?;
    let next = with_block(text, body, profile.kind == Kind::PowerShell);
    if next == text {
        return Ok(body.is_some());
    }
    // Windows PowerShell reads a profile without a byte order mark in the
    // system code page, which garbles a path outside ASCII.
    let mut out = if marked || (raw.is_none() && profile.kind == Kind::PowerShell) {
        BOM.to_vec()
    } else {
        Vec::new()
    };
    out.extend_from_slice(next.as_bytes());
    let target = storage::resolve_user_path(&profile.path)?;
    let permissions = std::fs::metadata(&target)
        .map(|meta| meta.permissions())
        .ok();
    storage::atomic_write(&target, &out, Some(&revision))?;
    // The replacement is created private; a startup file keeps the mode it had.
    if let Some(permissions) = permissions {
        std::fs::set_permissions(&target, permissions).map_err(storage::io_error)?;
    }
    Ok(body.is_some())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Installed {
    /// Startup files that hold the function now, as the user writes them.
    pub profiles: Vec<String>,
    pub failures: Vec<String>,
}

/// Puts the function in every startup file the user's shells read, or
/// brings it up to date. A file that cannot be handled is left as it is.
pub fn install(shells: &Shells, environment: &Path) -> Installed {
    let mut installed = Installed::default();
    for profile in shells.profiles() {
        let shown = shells.display(&profile.path);
        let Some(body) = function(profile.kind, environment) else {
            installed
                .failures
                .push(format!("{shown}：数据目录路径无法写入函数"));
            continue;
        };
        match apply(&profile, Some(&body)) {
            Ok(true) => installed.profiles.push(shown),
            Ok(false) => {}
            Err(error) => installed
                .failures
                .push(format!("{shown}：{}", error.message)),
        }
    }
    installed
}

/// Takes the function out of every startup file it is in, and says which
/// files could not be handled.
pub fn remove(shells: &Shells) -> Vec<String> {
    shells
        .profiles()
        .iter()
        .filter_map(|profile| {
            apply(profile, None)
                .err()
                .map(|error| format!("{}：{}", shells.display(&profile.path), error.message))
        })
        .collect()
}

#[cfg(test)]
#[path = "agy_shell_tests.rs"]
mod tests;
