use crate::{
    errors::code,
    storage::{self, AppError, Result},
    Runtime,
};
#[cfg(any(target_os = "macos", windows, test))]
use base64::engine::general_purpose::STANDARD;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;
#[cfg(feature = "desktop")]
type R<'a> = tauri::State<'a, Arc<Runtime>>;
const CREDENTIAL_LIMIT: u64 = 256 * 1024;
/// The only account kind. Earlier versions also stored `adc` and `vertex_key`
/// entries and still require the field, so it stays in the vault file.
const OAUTH_KIND: &str = "oauth";
/// What an account is called until its login says whose it is.
const UNNAMED: &str = "OAuth 账号";
static AUTH_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
fn err(c: &str, msg: &str) -> AppError {
    AppError::new(c, msg)
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityAccountItem {
    pub id: String,
    pub name: String,
    pub source_type: String,
    pub email: Option<String>,
    pub current: bool,
    pub updated_at: u64,
    pub credential_revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityAuthState {
    #[serde(skip)]
    pub oauth_added: bool,
    pub accounts: Vec<AntigravityAccountItem>,
    pub oauth_present: bool,
    pub oauth_email: Option<String>,
    pub oauth_auth_method: Option<String>,
    /// Running `agy` sessions. Each keeps the login it started with and writes
    /// it back over the shared one whenever its token is refreshed.
    #[serde(default)]
    pub cli_sessions: usize,
    /// How many of them the switch that produced this state ended.
    #[serde(default)]
    pub cli_closed: usize,
    #[serde(default)]
    pub app_restarted: bool,
    /// The switch succeeded but the application could not be reopened.
    #[serde(default)]
    pub relaunch_failed: bool,
    pub revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActivationSpan {
    pub account_id: String,
    pub since_ms: i64,
}
#[derive(Clone, Debug, Default)]
pub struct ProviderTimeline {
    spans: Vec<ActivationSpan>,
}
impl ProviderTimeline {
    pub fn provider_at(&self, started_at: i64) -> Option<&str> {
        self.spans
            .iter()
            .rfind(|s| s.since_ms <= started_at)
            .map(|s| s.account_id.as_str())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredEntry {
    id: String,
    name: String,
    kind: String,
    source_type: String,
    email: Option<String>,
    payload: String,
    updated_at: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vault {
    schema: u32,
    entries: Vec<StoredEntry>,
    #[serde(default)]
    observed_revisions: Vec<String>,
    #[serde(default)]
    active_oauth_id: Option<String>,
    #[serde(default)]
    activations: Vec<ActivationSpan>,
}
#[derive(Clone, Debug)]
pub struct Paths {
    pub gemini_home: PathBuf,
    pub data_dir: PathBuf,
}
impl Paths {
    pub fn resolve(data_dir: &Path) -> Result<Self> {
        Ok(Self::from_roots(
            &dirs::home_dir().ok_or_else(|| err(code::PATH, "无法定位用户主目录"))?,
            data_dir,
        ))
    }
    pub fn from_roots(user_home: &Path, data_dir: &Path) -> Self {
        Self {
            gemini_home: user_home.join(".gemini"),
            data_dir: data_dir.to_path_buf(),
        }
    }
    pub fn oauth_file(&self) -> PathBuf {
        self.gemini_home.join("jetski-standalone-oauth-token")
    }
    pub fn vault_file(&self) -> PathBuf {
        self.data_dir.join("antigravity-accounts.json")
    }
    /// Antigravity keeps the OAuth login in the macOS keychain or the Windows
    /// credential manager. Only the real home directory maps to them, so
    /// fixtures never read or write that entry.
    fn system_store(&self) -> bool {
        cfg!(any(target_os = "macos", windows))
            && !cfg!(test)
            && dirs::home_dir().is_some_and(|home| home.join(".gemini") == self.gemini_home)
    }
    /// The login file, where a login can be kept in it. The keychain holds any
    /// login; Windows leaves one the credential manager cannot hold in the file.
    pub fn watched_files(&self) -> Vec<PathBuf> {
        if cfg!(target_os = "macos") && self.system_store() {
            Vec::new()
        } else {
            vec![self.oauth_file()]
        }
    }
}
pub trait AppLifecycle {
    fn is_running(&self) -> Result<bool>;
    fn quit(&self) -> Result<()>;
    fn launch(&self) -> Result<()>;
    /// Ends the running `agy` sessions and says how many there were. A
    /// session left running would write its own login back over the new one.
    fn quit_cli(&self) -> Result<usize>;
}
#[derive(Default)]
pub struct SystemAppLifecycle {
    #[cfg(target_os = "macos")]
    last_bundle: Mutex<Option<PathBuf>>,
}
#[cfg(target_os = "macos")]
pub(crate) fn parse_running_antigravity_ps(ps_output: &str) -> Vec<(i32, PathBuf)> {
    ps_output
        .lines()
        .filter_map(|line| {
            let (pid, comm) = line.trim().split_once(char::is_whitespace)?;
            let bundle = comm.trim().strip_suffix("/Contents/MacOS/Antigravity")?;
            Some((pid.trim().parse().ok()?, PathBuf::from(bundle)))
        })
        .collect()
}
/// How many `agy` sessions `ps -axo comm=` lists.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn count_running_agy_ps(ps_output: &str) -> usize {
    ps_output
        .lines()
        .filter(|line| {
            Path::new(line.trim())
                .file_name()
                .is_some_and(|name| name == "agy")
        })
        .count()
}
/// `agy` shares the app's login. Only the real home is asked, as with the
/// keychain, so fixtures never look at this machine's processes.
fn cli_sessions(_paths: &Paths) -> usize {
    #[cfg(target_os = "macos")]
    if _paths.system_store() {
        return std::process::Command::new("ps")
            .args(["-axo", "comm="])
            .output()
            .map_or(0, |out| {
                count_running_agy_ps(&String::from_utf8_lossy(&out.stdout))
            });
    }
    0
}
#[cfg(target_os = "macos")]
impl SystemAppLifecycle {
    fn detect_running(&self) -> Result<Vec<(i32, PathBuf)>> {
        let out = std::process::Command::new("ps")
            .args(["-axo", "pid=,comm="])
            .output()
            .map_err(storage::io_error)?;
        let list = parse_running_antigravity_ps(&String::from_utf8_lossy(&out.stdout));
        if let Some((_, bundle)) = list.first() {
            {
                let mut guard = self.last_bundle.lock();
                *guard = Some(bundle.clone());
            }
        }
        Ok(list)
    }
    fn wait_until_stopped(&self, polls: usize) -> Result<bool> {
        for _ in 0..polls {
            if !self.is_running()? {
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(false)
    }
}
impl AppLifecycle for SystemAppLifecycle {
    fn is_running(&self) -> Result<bool> {
        #[cfg(target_os = "macos")]
        return Ok(!self.detect_running()?.is_empty());
        #[cfg(not(target_os = "macos"))]
        Ok(false)
    }
    fn quit(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            let running = self.detect_running()?;
            if running.is_empty() {
                return Ok(());
            }
            let _ = std::process::Command::new("osascript")
                .args([
                    "-e",
                    "tell application id \"com.google.antigravity\" to quit",
                ])
                .output();
            if self.wait_until_stopped(25)? {
                return Ok(());
            }
            for (pid, _) in &running {
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .output();
            }
            if !self.wait_until_stopped(15)? {
                return Err(err(
                    code::PROCESS,
                    "等待 Antigravity 退出超时，未修改任何凭据文件，请手动关闭后重试",
                ));
            }
        }
        Ok(())
    }
    fn launch(&self) -> Result<()> {
        #[cfg(target_os = "macos")]
        {
            let mut cmd = std::process::Command::new("open");
            if let Some(p) = self.last_bundle.lock().clone().filter(|p| p.exists()) {
                cmd.arg("-a").arg(p);
            } else {
                cmd.args(["-b", "com.google.antigravity"]);
            }
            if !cmd.status().map_err(storage::io_error)?.success() {
                return Err(err(
                    code::PROCESS,
                    "凭据已切换，但自动重新启动 Antigravity 失败，请手动打开应用",
                ));
            }
        }
        Ok(())
    }
    fn quit_cli(&self) -> Result<usize> {
        #[cfg(target_os = "macos")]
        {
            let ended = crate::process_control::force_quit(crate::process_control::Target::Agy)?;
            if ended.failed > 0 {
                return Err(err(
                    code::PROCESS,
                    "仍有 agy 会话未能关闭，未修改任何凭据文件，请手动退出后重试",
                ));
            }
            Ok(ended.sessions)
        }
        #[cfg(not(target_os = "macos"))]
        Ok(0)
    }
}
fn epoch_dur(t: SystemTime) -> Duration {
    t.duration_since(UNIX_EPOCH).unwrap_or_default()
}
fn now_secs() -> u64 {
    epoch_dur(SystemTime::now()).as_secs()
}
fn now_ms() -> i64 {
    epoch_dur(SystemTime::now()).as_millis() as i64
}
fn file_mtime_ms(path: &Path) -> Option<i64> {
    Some(epoch_dur(fs::metadata(path).ok()?.modified().ok()?).as_millis() as i64)
}
/// When the current login took effect: the token file's modification time, or
/// now where the system keeps the login.
fn observed_activation_ms(paths: &Paths) -> i64 {
    if paths.system_store() {
        return now_ms();
    }
    file_mtime_ms(&paths.oauth_file()).unwrap_or_else(now_ms)
}
fn clean_opt(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
        .map(str::to_owned)
}
fn map_str(map: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| clean_opt(map.get(*k).and_then(Value::as_str)))
}
fn payload_digest(raw: &str) -> String {
    storage::digest(raw.trim().as_bytes())
}
fn with_lock<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _g = AUTH_LOCK.lock();
    f()
}
fn oauth_refresh_token(raw: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    let map = v.as_object()?;
    map_str(map, &["refresh_token"])
        .or_else(|| map_str(map.get("token")?.as_object()?, &["refresh_token"]))
}
fn clean_label(raw: Option<&str>, fallback: &str) -> Result<String> {
    let t = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback);
    if t.is_empty() || t.chars().count() > 100 || t.chars().any(char::is_control) {
        return Err(err(code::NAME, "名称需为 1–100 个字符，不能包含控制字符"));
    }
    Ok(t.to_owned())
}
pub(crate) fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(storage::io_error)?;
    }
    storage::atomic_write(path, bytes, None)
}
fn read_text_optional(path: &Path) -> Result<Option<String>> {
    let Some(bytes) = storage::read_bounded(path, CREDENTIAL_LIMIT)? else {
        return Ok(None);
    };
    let text =
        String::from_utf8(bytes).map_err(|_| err(code::UTF8, "凭据文件不是有效的 UTF-8 文本"))?;
    Ok((!text.trim().is_empty()).then_some(text))
}
#[cfg(target_os = "macos")]
const KEYCHAIN_ITEM: [&str; 4] = ["-s", "gemini", "-a", "antigravity"];
#[cfg(any(target_os = "macos", windows, test))]
const KEYRING_BASE64: &str = "go-keyring-base64:";
#[cfg(any(target_os = "macos", windows, test))]
const KEYRING_HEX: &str = "go-keyring-encoded:";
/// `security -i` rejects longer command lines.
#[cfg(any(target_os = "macos", test))]
const KEYCHAIN_COMMAND_LIMIT: usize = 4096;
#[cfg(any(target_os = "macos", test))]
type KeychainFingerprint = (u64, u64, u128);
#[cfg(target_os = "macos")]
static KEYCHAIN_CACHE: Mutex<Option<(KeychainFingerprint, Option<String>)>> = Mutex::new(None);

/// The credential inside a value written by Antigravity's keyring library,
/// which on macOS prefixes the encoding it used.
#[cfg(any(target_os = "macos", windows, test))]
fn decode_keyring(stored: &str) -> Option<String> {
    let stored = stored.trim();
    let bytes = if let Some(encoded) = stored.strip_prefix(KEYRING_BASE64) {
        STANDARD.decode(encoded).ok()?
    } else if let Some(encoded) = stored.strip_prefix(KEYRING_HEX) {
        (0..encoded.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(encoded.get(i..i + 2)?, 16).ok())
            .collect::<Option<_>>()?
    } else {
        stored.as_bytes().to_vec()
    };
    String::from_utf8(bytes)
        .ok()
        .filter(|text| !text.trim().is_empty())
}
#[cfg(any(target_os = "macos", test))]
fn encode_keyring(raw: &str) -> String {
    format!("{KEYRING_BASE64}{}", STANDARD.encode(raw))
}
#[cfg(any(target_os = "macos", test))]
fn cached_keychain_read<F>(
    fp: Option<KeychainFingerprint>,
    cache: &Mutex<Option<(KeychainFingerprint, Option<String>)>>,
    fetch: F,
) -> Result<Option<String>>
where
    F: FnOnce() -> Result<Option<String>>,
{
    if let Some(fp) = fp {
        if let Some((cached_fp, value)) = cache.lock().as_ref() {
            if *cached_fp == fp {
                return Ok(value.clone());
            }
        }
    }
    let value = fetch()?;
    if let Some(fp) = fp {
        *cache.lock() = Some((fp, value.clone()));
    }
    Ok(value)
}
#[cfg(target_os = "macos")]
fn keychain_fingerprint() -> Option<KeychainFingerprint> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(dirs::home_dir()?.join("Library/Keychains/login.keychain-db")).ok()?;
    let ns = epoch_dur(meta.modified().ok()?).as_nanos();
    Some((meta.ino(), meta.len(), ns))
}
/// Runs `/usr/bin/security`. A locked keychain asks for its password in a
/// dialog, so the wait is bounded and cannot hold the credential lock forever.
#[cfg(target_os = "macos")]
fn security(args: &[&str], input: Option<&str>, timeout: Duration) -> Result<std::process::Output> {
    use std::{io::Write, process::Stdio};
    let fail = || err(code::AUTH, "无法访问 macOS 钥匙串中的 Antigravity 登录凭据");
    let mut child = std::process::Command::new("/usr/bin/security")
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| fail())?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().ok_or_else(fail)?;
        stdin.write_all(input.as_bytes()).map_err(|_| fail())?;
    }
    let deadline = std::time::Instant::now() + timeout;
    while child.try_wait().map_err(|_| fail())?.is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(fail());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().map_err(|_| fail())
}
#[cfg(target_os = "macos")]
fn keychain_oauth_uncached(interactive: bool) -> Result<Option<String>> {
    let mut args = vec!["find-generic-password"];
    args.extend(KEYCHAIN_ITEM);
    args.push("-w");
    let timeout = Duration::from_secs(if interactive { 60 } else { 5 });
    let output = security(&args, None, timeout)?;
    match output.status.code() {
        Some(0) => Ok(decode_keyring(&String::from_utf8_lossy(&output.stdout))),
        // errSecItemNotFound: nobody is signed in.
        Some(44) => Ok(None),
        _ => Err(err(
            code::AUTH,
            "无法读取 macOS 钥匙串中的 Antigravity 登录凭据",
        )),
    }
}
#[cfg(target_os = "macos")]
fn keychain_oauth(interactive: bool) -> Result<Option<String>> {
    cached_keychain_read(keychain_fingerprint(), &KEYCHAIN_CACHE, || {
        keychain_oauth_uncached(interactive)
    })
}
#[cfg(any(target_os = "macos", test))]
fn build_keychain_write_command(raw: &str) -> Result<String> {
    let command = format!(
        "add-generic-password -U -s gemini -a antigravity -w {}\n",
        encode_keyring(raw)
    );
    if command.len() > KEYCHAIN_COMMAND_LIMIT {
        return Err(err(code::AUTH, "Antigravity 登录凭据过大，无法写入钥匙串"));
    }
    Ok(command)
}
/// Replaces the keychain login, or removes it when `raw` is `None`.
#[cfg(target_os = "macos")]
fn set_keychain_oauth(raw: Option<&str>) -> Result<()> {
    let fail = || err(code::AUTH, "无法写入 macOS 钥匙串中的 Antigravity 登录凭据");
    *KEYCHAIN_CACHE.lock() = None;
    let Some(raw) = raw else {
        let mut args = vec!["delete-generic-password"];
        args.extend(KEYCHAIN_ITEM);
        security(&args, None, Duration::from_secs(60))?;
        let cleared = keychain_oauth_uncached(true)?.is_none();
        *KEYCHAIN_CACHE.lock() = None;
        return cleared.then_some(()).ok_or_else(fail);
    };
    // The command goes through stdin so the credential never shows up in the
    // process arguments.
    let command = build_keychain_write_command(raw)?;
    security(&["-i"], Some(&command), Duration::from_secs(60))?;
    let verified = keychain_oauth_uncached(true)?.as_deref().map(str::trim) == Some(raw.trim());
    *KEYCHAIN_CACHE.lock() = None;
    verified.then_some(()).ok_or_else(fail)
}
/// The largest login the Windows credential manager holds
/// (`CRED_MAX_CREDENTIAL_BLOB_SIZE`). Antigravity's keyring library refuses a
/// longer one, and Antigravity then keeps the login in the token file.
#[cfg(any(windows, test))]
const CREDENTIAL_BLOB_LIMIT: usize = 2560;
/// The Windows credential manager entry holding the login. The rules around
/// it sit behind this seam so they are tested on every platform.
#[cfg(any(windows, test))]
trait CredentialStore {
    fn read(&self) -> Result<Option<Vec<u8>>>;
    fn write(&self, blob: &[u8]) -> Result<()>;
    fn delete(&self) -> Result<()>;
}
#[cfg(any(windows, test))]
fn credential_login(store: &impl CredentialStore) -> Result<Option<String>> {
    Ok(store
        .read()?
        .and_then(|blob| decode_keyring(&String::from_utf8_lossy(&blob))))
}
/// Replaces the credential manager login, or removes it when `raw` is `None`.
#[cfg(any(windows, test))]
fn set_credential_login(store: &impl CredentialStore, raw: Option<&str>) -> Result<()> {
    let fail = || {
        err(
            code::AUTH,
            "无法写入 Windows 凭据管理器中的 Antigravity 登录凭据",
        )
    };
    let Some(raw) = raw else {
        store.delete()?;
        let cleared = credential_login(store)?.is_none();
        return cleared.then_some(()).ok_or_else(fail);
    };
    if raw.len() > CREDENTIAL_BLOB_LIMIT {
        return Err(err(
            code::AUTH,
            "Antigravity 登录凭据过大，无法写入 Windows 凭据管理器",
        ));
    }
    store.write(raw.as_bytes())?;
    let verified = credential_login(store)?.as_deref().map(str::trim) == Some(raw.trim());
    verified.then_some(()).ok_or_else(fail)
}
/// Antigravity asks the credential manager first and the token file after.
#[cfg(any(windows, test))]
fn read_windows_oauth(paths: &Paths, store: &impl CredentialStore) -> Result<Option<String>> {
    match credential_login(store)? {
        Some(raw) => Ok(Some(raw)),
        None => read_text_optional(&paths.oauth_file()),
    }
}
/// A login too large for the credential manager goes to the token file, and
/// the entry that would be read ahead of it is removed.
#[cfg(any(windows, test))]
fn write_windows_oauth(paths: &Paths, store: &impl CredentialStore, raw: &str) -> Result<()> {
    if raw.len() > CREDENTIAL_BLOB_LIMIT {
        write_private_file(&paths.oauth_file(), raw.as_bytes())?;
        return set_credential_login(store, None);
    }
    set_credential_login(store, Some(raw))?;
    if paths.oauth_file().exists() {
        write_private_file(&paths.oauth_file(), raw.as_bytes())?;
    }
    Ok(())
}
#[cfg(windows)]
mod credential_manager {
    use super::{err, CredentialStore, Result, CREDENTIAL_BLOB_LIMIT};
    use crate::errors::code;
    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_NOT_FOUND},
        Security::Credentials::{
            CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW,
            CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
        },
    };

    const _: () = assert!(CREDENTIAL_BLOB_LIMIT == CRED_MAX_CREDENTIAL_BLOB_SIZE as usize);
    /// Antigravity's keyring library names the entry `service:user`.
    const TARGET: &str = "gemini:antigravity";
    const USER: &str = "antigravity";

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    pub(super) struct CredentialManager;

    impl CredentialStore for CredentialManager {
        fn read(&self) -> Result<Option<Vec<u8>>> {
            let target = wide(TARGET);
            let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
            // SAFETY: `target` is NUL-terminated and outlives the call, and
            // `credential` is a valid place for the returned pointer.
            let found =
                unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) } != 0;
            if !found {
                // SAFETY: only reads the calling thread's last error code.
                return if unsafe { GetLastError() } == ERROR_NOT_FOUND {
                    Ok(None)
                } else {
                    Err(err(
                        code::AUTH,
                        "无法读取 Windows 凭据管理器中的 Antigravity 登录凭据",
                    ))
                };
            }
            // SAFETY: CredReadW succeeded, so `credential` points to a
            // CREDENTIALW whose blob is `CredentialBlobSize` readable bytes
            // until the CredFree below, which runs once, after the copy.
            let blob = unsafe {
                let stored = &*credential;
                let blob = if stored.CredentialBlob.is_null() {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(
                        stored.CredentialBlob,
                        stored.CredentialBlobSize as usize,
                    )
                    .to_vec()
                };
                CredFree(credential.cast());
                blob
            };
            Ok(Some(blob))
        }
        fn write(&self, blob: &[u8]) -> Result<()> {
            let fail = || {
                err(
                    code::AUTH,
                    "无法写入 Windows 凭据管理器中的 Antigravity 登录凭据",
                )
            };
            let size = u32::try_from(blob.len()).map_err(|_| fail())?;
            let (mut target, mut user) = (wide(TARGET), wide(USER));
            let credential = CREDENTIALW {
                Type: CRED_TYPE_GENERIC,
                TargetName: target.as_mut_ptr(),
                CredentialBlobSize: size,
                CredentialBlob: blob.as_ptr().cast_mut(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                UserName: user.as_mut_ptr(),
                ..Default::default()
            };
            // SAFETY: `credential` and the buffers it points to outlive the
            // call, both strings are NUL-terminated, `size` is the length of
            // `blob`, and CredWriteW only reads through these pointers.
            if unsafe { CredWriteW(&credential, 0) } == 0 {
                return Err(fail());
            }
            Ok(())
        }
        fn delete(&self) -> Result<()> {
            let target = wide(TARGET);
            // SAFETY: `target` is NUL-terminated and outlives the call;
            // GetLastError only reads the calling thread's last error code.
            let deleted = unsafe {
                CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) != 0
                    || GetLastError() == ERROR_NOT_FOUND
            };
            deleted.then_some(()).ok_or_else(|| {
                err(
                    code::AUTH,
                    "无法删除 Windows 凭据管理器中的 Antigravity 登录凭据",
                )
            })
        }
    }
}
#[cfg(windows)]
use credential_manager::CredentialManager;
/// The current login source; macOS never consults the legacy token file.
fn read_oauth(paths: &Paths, _interactive: bool) -> Result<Option<String>> {
    #[cfg(target_os = "macos")]
    if paths.system_store() {
        return keychain_oauth(_interactive);
    }
    #[cfg(windows)]
    if paths.system_store() {
        return read_windows_oauth(paths, &CredentialManager);
    }
    read_text_optional(&paths.oauth_file())
}
fn write_oauth(paths: &Paths, raw: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    if paths.system_store() {
        set_keychain_oauth(Some(raw))?;
        if paths.oauth_file().exists() {
            write_private_file(&paths.oauth_file(), raw.as_bytes())?;
        }
        return Ok(());
    }
    #[cfg(windows)]
    if paths.system_store() {
        return write_windows_oauth(paths, &CredentialManager, raw);
    }
    write_private_file(&paths.oauth_file(), raw.as_bytes())
}
/// Reads a vault file. Earlier versions also stored ADC and Vertex API Key
/// accounts in it; those entries are dropped.
fn parse_vault(bytes: &[u8]) -> Option<Vault> {
    let mut vault: Vault = serde_json::from_slice(bytes).ok()?;
    vault.entries.retain(|e| e.kind == OAUTH_KIND);
    Some(vault)
}
fn load_vault(paths: &Paths) -> Result<(Vault, String)> {
    storage::private_dir(&paths.data_dir)?;
    let loaded = storage::read_optional(&paths.vault_file())?;
    let rev = storage::revision(loaded.as_deref());
    let mut vault = match loaded {
        Some(b) => parse_vault(&b)
            .ok_or_else(|| err(code::STORE, "Antigravity 凭据库无法读取，原文件已保留"))?,
        None => Vault::default(),
    };
    if vault.schema == 0 {
        vault.schema = 1;
    } else if vault.schema != 1 {
        return Err(err(
            code::STORE_VERSION,
            "Antigravity 凭据库版本较新，请更新 Vela",
        ));
    }
    Ok((vault, rev))
}
fn save_vault(paths: &Paths, vault: &Vault) -> Result<String> {
    storage::private_dir(&paths.data_dir)?;
    let bytes = serde_json::to_vec_pretty(vault)
        .map_err(|_| err(code::STORE, "无法序列化 Antigravity 凭据库"))?;
    storage::atomic_write(&paths.vault_file(), &bytes, None)?;
    Ok(storage::revision(Some(&bytes)))
}
fn decode_jwt_email(token: &str) -> Option<String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(token.split('.').nth(1)?.trim_end_matches('='))
        .ok()?;
    let val = serde_json::from_slice::<Value>(&bytes).ok()?;
    clean_opt(val.get("email").and_then(Value::as_str))
}
pub fn parse_oauth_metadata(raw: &str) -> Result<(String, Option<String>)> {
    let value: Value = serde_json::from_str(raw.trim()).map_err(|_| {
        err(
            code::JSON,
            "OAuth 令牌文件不是有效的 JSON（请先在 Antigravity 中完成官方 OAuth 登录）",
        )
    })?;
    let Some(map) = value.as_object() else {
        return Err(err(code::CONFIG_TYPE, "OAuth 令牌顶层必须是 JSON 对象"));
    };
    let nested = map.get("token").and_then(Value::as_object);
    let has_tok = map_str(map, &["access_token", "refresh_token"]).is_some()
        || nested.is_some_and(|t| map_str(t, &["access_token", "refresh_token"]).is_some());
    if !has_tok {
        return Err(err(
            code::AUTH,
            "OAuth 令牌缺少有效令牌字段（access_token / refresh_token）",
        ));
    }
    let u_email = map
        .get("user")
        .and_then(Value::as_object)
        .and_then(|u| map_str(u, &["email"]));
    let top_email = map_str(
        map,
        &["id_token_email", "email", "account", "client_email", "user"],
    );
    let tok_email = nested.and_then(|t| map_str(t, &["email", "id_token_email"]));
    let jwt =
        map_str(map, &["id_token"]).or_else(|| nested.and_then(|t| map_str(t, &["id_token"])));
    let email = u_email
        .or(top_email)
        .or(tok_email)
        .or_else(|| decode_jwt_email(&jwt?));
    let method = map_str(map, &["auth_method", "provider"])
        .or_else(|| nested.and_then(|t| map_str(t, &["token_type"])))
        .unwrap_or_else(|| "oauth".to_owned());
    Ok((method, email))
}
fn remember_observed(vault: &mut Vault, digest: &str) {
    vault.observed_revisions.retain(|r| r != digest);
    if vault.observed_revisions.len() >= 64 {
        let drop_n = vault.observed_revisions.len() - 63;
        vault.observed_revisions.drain(..drop_n);
    }
    vault.observed_revisions.push(digest.to_owned());
}
impl Vault {
    fn index_of(&self, id: &str) -> Result<usize> {
        self.entries
            .iter()
            .position(|e| e.id == id)
            .ok_or_else(|| err(code::ACCOUNT, "未找到指定的 Antigravity 凭据条目"))
    }
    fn push_new(
        &mut self,
        name: String,
        source_type: &str,
        email: Option<String>,
        payload: String,
    ) -> String {
        let id = Uuid::new_v4().to_string();
        self.entries.push(StoredEntry {
            id: id.clone(),
            name,
            kind: OAUTH_KIND.to_owned(),
            source_type: source_type.to_owned(),
            email,
            payload,
            updated_at: now_secs(),
        });
        id
    }
    /// Drops an unnamed entry whose login a named entry also holds. Earlier
    /// versions took a login apart from itself once it gained its address;
    /// what the placeholder was active for goes to the entry that stays.
    fn merge_placeholders(&mut self) -> bool {
        let tokens: Vec<_> = self
            .entries
            .iter()
            .map(|e| oauth_refresh_token(&e.payload))
            .collect();
        let merged: Vec<(String, String)> = self
            .entries
            .iter()
            .zip(&tokens)
            .filter(|(e, token)| e.email.is_none() && e.name == UNNAMED && token.is_some())
            .filter_map(|(placeholder, token)| {
                let kept = self
                    .entries
                    .iter()
                    .zip(&tokens)
                    .find(|(e, held)| e.email.is_some() && *held == token)?;
                Some((placeholder.id.clone(), kept.0.id.clone()))
            })
            .collect();
        for (gone, kept) in &merged {
            self.entries.retain(|e| &e.id != gone);
            for span in &mut self.activations {
                if &span.account_id == gone {
                    span.account_id.clone_from(kept);
                }
            }
            if self.active_oauth_id.as_ref() == Some(gone) {
                self.active_oauth_id = Some(kept.clone());
            }
        }
        !merged.is_empty()
    }
}
fn find_oauth_idx(
    v: &Vault,
    email: &Option<String>,
    dig: &str,
    rt: &Option<String>,
) -> Option<usize> {
    let ok_rt = |e: &StoredEntry| !matches!((oauth_refresh_token(&e.payload), rt), (Some(a), Some(b)) if &a != b);
    if let Some(i) = v.entries.iter().position(|e| match (&e.email, email) {
        (Some(ae), Some(de)) => ae.eq_ignore_ascii_case(de),
        (None, None) => {
            payload_digest(&e.payload) == dig
                || (rt.is_some() && &oauth_refresh_token(&e.payload) == rt)
        }
        _ => false,
    }) {
        return Some(i);
    }
    // A login stays itself while its refresh token does: Antigravity writes a
    // new login without the address and adds it at the first refresh.
    if rt.is_some() {
        if let Some(i) = v
            .entries
            .iter()
            .position(|e| &oauth_refresh_token(&e.payload) == rt)
        {
            return Some(i);
        }
    }
    if email.is_some() {
        return None;
    }
    if let Some(id) = v.active_oauth_id.as_deref() {
        if let Some(i) = v
            .entries
            .iter()
            .position(|e| e.id == id && e.email.is_none() && ok_rt(e))
        {
            return Some(i);
        }
    }
    let mut it = v
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.email.is_none());
    let (pos, entry) = it.next()?;
    (it.next().is_none() && ok_rt(entry)).then_some(pos)
}
fn entry_matches(e: &StoredEntry, digest: Option<&str>, email: Option<&str>) -> bool {
    digest == Some(payload_digest(&e.payload).as_str())
        || matches!((e.email.as_deref(), email), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b))
}
fn state_with_oauth(
    paths: &Paths,
    oauth_raw: Option<String>,
) -> Result<(AntigravityAuthState, bool)> {
    let (mut vault, mut vault_rev) = load_vault(paths)?;
    let (mut dirty, mut clean) = (vault.merge_placeholders(), true);
    let mut oauth_added = false;
    let (mut oauth_auth_method, mut oauth_email, mut oauth_digest) = (None, None, None);
    if let Some(raw) = oauth_raw {
        match parse_oauth_metadata(&raw) {
            Ok((auth_method, email)) => {
                let (digest, disk_rt) = (payload_digest(&raw), oauth_refresh_token(&raw));
                if let Some(i) = find_oauth_idx(&vault, &email, &digest, &disk_rt) {
                    let entry_id = vault.entries[i].id.clone();
                    let e = &mut vault.entries[i];
                    if payload_digest(&e.payload) != digest {
                        e.payload = raw;
                        // A login written without its address is still the
                        // account it was; one that gains it takes its name.
                        if let Some(address) = &email {
                            if e.email.is_none() && e.name == UNNAMED {
                                e.name = clean_label(None, address).unwrap_or(e.name.clone());
                            }
                            e.email = email.clone();
                        }
                        e.source_type = auth_method.clone();
                        e.updated_at = now_secs();
                        dirty = true;
                    }
                    if vault.active_oauth_id.as_deref() != Some(entry_id.as_str()) {
                        vault.active_oauth_id = Some(entry_id);
                        dirty = true;
                    }
                } else if !vault.observed_revisions.iter().any(|r| r == &digest) {
                    let name = clean_label(None, email.as_deref().unwrap_or(UNNAMED))?;
                    let id = vault.push_new(name, &auth_method, email.clone(), raw);
                    vault.active_oauth_id = Some(id);
                    oauth_added = true;
                    dirty = true;
                }
                oauth_auth_method = Some(auth_method);
                oauth_email = email;
                oauth_digest = Some(digest);
            }
            Err(e) if e.code == code::JSON => clean = false,
            Err(_) => {}
        }
    }
    // An entry found by its address may only now share a placeholder's token.
    dirty |= vault.merge_placeholders();
    let accounts: Vec<AntigravityAccountItem> = vault
        .entries
        .iter()
        .map(|e| AntigravityAccountItem {
            id: e.id.clone(),
            name: e.name.clone(),
            source_type: e.source_type.clone(),
            email: e.email.clone(),
            current: entry_matches(e, oauth_digest.as_deref(), oauth_email.as_deref()),
            updated_at: e.updated_at,
            credential_revision: payload_digest(&e.payload),
        })
        .collect();
    let mut state = AntigravityAuthState {
        oauth_added,
        accounts,
        oauth_present: oauth_digest.is_some(),
        oauth_email,
        oauth_auth_method,
        cli_sessions: cli_sessions(paths),
        cli_closed: 0,
        app_restarted: false,
        relaunch_failed: false,
        revision: String::new(),
    };
    if let Some(id) = active_account_id(&state) {
        let since = if vault.activations.is_empty() {
            observed_activation_ms(paths)
        } else {
            now_ms()
        };
        dirty |= push_span(&mut vault.activations, &id, since);
    }
    if dirty {
        vault_rev = save_vault(paths, &vault)?;
    }
    let oauth_rev = oauth_digest.as_deref().unwrap_or("none");
    state.revision = storage::digest(format!("{vault_rev}:{oauth_rev}").as_bytes());
    Ok((state, clean))
}
pub fn state_at(paths: &Paths) -> Result<AntigravityAuthState> {
    state_with_oauth(paths, read_oauth(paths, true)?).map(|(s, _)| s)
}
fn verify_revision(paths: &Paths, expected: &str) -> Result<AntigravityAuthState> {
    let current = state_at(paths)?;
    (current.revision == expected)
        .then_some(current)
        .ok_or_else(|| err(code::CONFLICT, "Antigravity 凭据状态已变更，请刷新后重试"))
}
fn restore_file(path: &Path, prev: Option<&[u8]>) -> Result<()> {
    match prev {
        Some(bytes) => write_private_file(path, bytes),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(storage::io_error(e)),
        },
    }
}
fn with_lifecycle(
    paths: &Paths,
    rev: &str,
    lc: &dyn AppLifecycle,
    mutate: impl FnOnce() -> Result<()>,
) -> Result<AntigravityAuthState> {
    let was_running = lc.is_running()?;
    with_lock(|| verify_revision(paths, rev).map(|_| ()))?;
    if was_running {
        lc.quit()?;
    }
    // The app goes first: nothing has been ended yet if it will not quit.
    let closed = lc.quit_cli().inspect_err(|_| {
        if was_running {
            let _ = lc.launch();
        }
    })?;
    let next = with_lock(|| {
        verify_revision(paths, rev)?;
        let snapshots: Vec<_> = paths
            .watched_files()
            .into_iter()
            .chain(std::iter::once(paths.vault_file()))
            .map(|f| storage::read_optional(&f).map(|b| (f, b)))
            .collect::<Result<_>>()?;
        #[cfg(target_os = "macos")]
        let keychain = paths
            .system_store()
            .then(|| keychain_oauth(true))
            .transpose()?;
        #[cfg(windows)]
        let credential = paths
            .system_store()
            .then(|| credential_login(&CredentialManager))
            .transpose()?;
        let next = mutate().and_then(|_| state_at(paths));
        if let Err(e) = next {
            let res: Vec<_> = snapshots
                .iter()
                .filter_map(|(f, p)| {
                    restore_file(f, p.as_deref())
                        .err()
                        .map(|_| f.display().to_string())
                })
                .collect();
            #[cfg(target_os = "macos")]
            let res = {
                let mut res = res;
                if let Some(before) = &keychain {
                    if keychain_oauth_uncached(true).ok().as_ref() != Some(before)
                        && set_keychain_oauth(before.as_deref()).is_err()
                    {
                        res.push("钥匙串登录凭据".into());
                    }
                }
                res
            };
            #[cfg(windows)]
            let res = {
                let mut res = res;
                if let Some(before) = &credential {
                    if credential_login(&CredentialManager).ok().as_ref() != Some(before)
                        && set_credential_login(&CredentialManager, before.as_deref()).is_err()
                    {
                        res.push("凭据管理器登录凭据".into());
                    }
                }
                res
            };
            let msg = format!("{}（回滚残留未恢复：{}）", e.message, res.join(", "));
            return Err(if res.is_empty() {
                e
            } else {
                err(&e.code, &msg)
            });
        }
        next
    });
    // A completed switch stays in place when the application cannot be
    // reopened; the caller only asks the user to open it.
    let relaunched = was_running.then(|| lc.launch().is_ok());
    next.map(|mut s| {
        s.app_restarted = relaunched == Some(true);
        s.relaunch_failed = relaunched == Some(false);
        s.cli_closed = closed;
        s
    })
}
pub fn switch_account_at(
    paths: &Paths,
    lc: &dyn AppLifecycle,
    id: &str,
    rev: &str,
) -> Result<AntigravityAuthState> {
    let payload = with_lock(|| {
        let (vault, _) = load_vault(paths)?;
        let payload = vault.entries[vault.index_of(id)?].payload.clone();
        parse_oauth_metadata(&payload)?;
        Ok(payload)
    })?;
    with_lifecycle(paths, rev, lc, || write_oauth(paths, &payload))
}
pub fn rename_account_at(
    paths: &Paths,
    id: &str,
    name: &str,
    rev: &str,
) -> Result<AntigravityAuthState> {
    with_lock(|| {
        verify_revision(paths, rev)?;
        let (cleaned, (mut vault, _)) = (clean_label(Some(name), "")?, load_vault(paths)?);
        let idx = vault.index_of(id)?;
        vault.entries[idx].name = cleaned;
        vault.entries[idx].updated_at = now_secs();
        save_vault(paths, &vault)?;
        state_at(paths)
    })
}
pub fn delete_account_at(paths: &Paths, id: &str, rev: &str) -> Result<AntigravityAuthState> {
    with_lock(|| {
        if verify_revision(paths, rev)?
            .accounts
            .iter()
            .any(|a| a.id == id && a.current)
        {
            return Err(err(
                code::CONFLICT,
                "当前正在使用中的凭据不可直接删除，请先切换到其他凭据",
            ));
        }
        let (mut vault, _) = load_vault(paths)?;
        let idx = vault.index_of(id)?;
        let target = vault.entries[idx].clone();
        let holds_target = |raw: &str| {
            let email = parse_oauth_metadata(raw).ok().and_then(|(_, email)| email);
            entry_matches(&target, Some(&payload_digest(raw)), email.as_deref())
        };
        // The login left behind by an account that is not in use lives where
        // the app reads it: the keychain on macOS, the credential manager or
        // the token file on Windows, the token file elsewhere.
        #[cfg(target_os = "macos")]
        if paths.system_store() && keychain_oauth(true)?.is_some_and(|raw| holds_target(&raw)) {
            set_keychain_oauth(None)?;
        }
        #[cfg(windows)]
        if paths.system_store()
            && credential_login(&CredentialManager)?.is_some_and(|raw| holds_target(&raw))
        {
            set_credential_login(&CredentialManager, None)?;
        }
        for f in paths.watched_files() {
            if let Some(raw) = read_text_optional(&f)? {
                if holds_target(&raw) {
                    let meta = fs::symlink_metadata(&f).map_err(storage::io_error)?;
                    if meta.file_type().is_symlink() || !meta.is_file() {
                        return Err(err(code::SYMLINK, "拒绝删除非普通文件或符号链接的凭据文件"));
                    }
                    fs::remove_file(&f).map_err(storage::io_error)?;
                }
            }
        }
        let removed = vault.entries.remove(idx);
        remember_observed(&mut vault, &payload_digest(&removed.payload));
        if vault.active_oauth_id.as_deref() == Some(id) {
            vault.active_oauth_id = None;
        }
        save_vault(paths, &vault)?;
        state_at(paths)
    })
}
fn auth_stamp(paths: &Paths, oauth_revision: &str) -> String {
    let mut stamp = oauth_revision.to_owned();
    for path in paths
        .watched_files()
        .into_iter()
        .chain([paths.vault_file()])
    {
        if let Ok(m) = fs::metadata(&path) {
            let ns = m.modified().ok().map_or(0, |t| epoch_dur(t).as_nanos());
            stamp.push_str(&format!("1:{}:{ns};", m.len()));
        } else {
            stamp.push_str("0;");
        }
    }
    stamp
}
pub(crate) fn current_oauth_identity(data: &Path, home: &Path) -> Result<Option<(String, String)>> {
    with_lock(|| {
        let paths = Paths {
            gemini_home: home.to_path_buf(),
            data_dir: data.to_path_buf(),
        };
        let (vault, _) = load_vault(&paths)?;
        let Some(raw) = read_oauth(&paths, false)? else {
            return Ok(None);
        };
        let Some(email) = parse_oauth_metadata(&raw)?.1 else {
            return Ok(None);
        };
        let digest = payload_digest(&raw);
        Ok(vault
            .entries
            .iter()
            .find(|e| entry_matches(e, Some(&digest), Some(&email)))
            .map(|e| (e.id.clone(), email)))
    })
}
pub(crate) fn usage_accounts(
    data: &Path,
) -> Result<Vec<crate::usage::antigravity::quota::Account>> {
    with_lock(|| {
        let (vault, _) = load_vault(&Paths::resolve(data)?)?;
        Ok(vault
            .entries
            .into_iter()
            .map(|e| crate::usage::antigravity::quota::Account {
                id: e.id,
                name: e.name,
            })
            .collect())
    })
}
pub fn active_account_id(state: &AntigravityAuthState) -> Option<String> {
    state
        .accounts
        .iter()
        .find(|a| a.current)
        .map(|a| a.id.clone())
}
fn push_span(spans: &mut Vec<ActivationSpan>, id: &str, fb_ms: i64) -> bool {
    if spans.last().map(|s| s.account_id.as_str()) == Some(id) {
        return false;
    }
    let since_ms = fb_ms.max(spans.last().map_or(0, |s| s.since_ms));
    spans.push(ActivationSpan {
        account_id: id.to_owned(),
        since_ms,
    });
    true
}
pub fn provider_timeline(data_dir: &Path, gemini_home: Option<&Path>) -> ProviderTimeline {
    let v_file = data_dir.join("antigravity-accounts.json");
    let Ok(Some(b)) = storage::read_optional(&v_file) else {
        return ProviderTimeline::default();
    };
    let Some(vault) = parse_vault(&b) else {
        return ProviderTimeline::default();
    };
    let mut spans = vault.activations;
    if let Some(g) = gemini_home {
        let p = Paths {
            gemini_home: g.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
        };
        if let Some(raw) = read_oauth(&p, false).ok().flatten() {
            let d = payload_digest(&raw);
            let em = parse_oauth_metadata(&raw).ok().and_then(|(_, email)| email);
            if let Some(e) = vault
                .entries
                .iter()
                .find(|e| entry_matches(e, Some(&d), em.as_deref()))
            {
                push_span(&mut spans, &e.id, observed_activation_ms(&p));
            }
        }
    }
    if spans.is_empty() {
        if let Some(entry) = vault
            .active_oauth_id
            .as_deref()
            .and_then(|id| vault.entries.iter().find(|e| e.id == id))
        {
            push_span(&mut spans, &entry.id, now_ms());
        } else if vault.entries.len() == 1 {
            push_span(&mut spans, &vault.entries[0].id, now_ms());
        }
    }
    ProviderTimeline { spans }
}
pub fn poll_state(
    paths: &Paths,
    last_stamp: &mut Option<String>,
) -> Result<Option<AntigravityAuthState>> {
    with_lock(|| {
        let oauth = read_oauth(paths, false)?;
        let oauth_revision = storage::revision(oauth.as_deref().map(str::as_bytes));
        let stamp = auth_stamp(paths, &oauth_revision);
        if last_stamp.as_ref() == Some(&stamp) {
            return Ok(None);
        }
        let (state, clean) = state_with_oauth(paths, oauth)?;
        if clean {
            // State observation may have saved a new account or activation.
            *last_stamp = Some(auth_stamp(paths, &oauth_revision));
        }
        Ok(Some(state))
    })
}
/// Migration only. Earlier versions also kept ADC and Vertex API Key accounts
/// in the vault, and while a key was in use pointed the app's own gateway
/// settings at a Vela listener, saving what they replaced under `gateway`.
/// Nothing else reads those fields.
mod retired {
    use super::*;

    /// The app settings a Vertex key replaced.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Snapshot {
        gateway: Option<Value>,
        models: Option<Value>,
        file_existed: bool,
    }
    /// The key that was in use, with the listener values written for it.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Selection {
        token: String,
        port: u16,
        previous: Snapshot,
    }
    impl Selection {
        /// Whether the app settings still hold what Vela wrote.
        fn matches(&self, settings: &Value) -> bool {
            let url = format!("http://127.0.0.1:{}/antigravity", self.port);
            settings
                .pointer("/gateway/enabled")
                .and_then(Value::as_bool)
                == Some(true)
                && settings.pointer("/gateway/url").and_then(Value::as_str) == Some(url.as_str())
                && settings.pointer("/gateway/apiKey").and_then(Value::as_str)
                    == Some(self.token.as_str())
        }
    }
    /// Puts the app's gateway and model settings back as they were before the
    /// key took them over. Settings changed since then are left alone.
    fn restore_app_settings(paths: &Paths, selection: Selection) -> Result<()> {
        let file = paths
            .gemini_home
            .with_file_name(".antigravity")
            .join("settings.json");
        let Ok(Some(raw)) = storage::read_optional(&file) else {
            return Ok(());
        };
        let Ok(mut settings) = serde_json::from_slice::<Value>(&raw) else {
            return Ok(());
        };
        if !selection.matches(&settings) {
            return Ok(());
        }
        let Some(object) = settings.as_object_mut() else {
            return Ok(());
        };
        for (key, previous) in [
            ("gateway", selection.previous.gateway),
            ("models", selection.previous.models),
        ] {
            match previous {
                Some(value) => object.insert(key.to_owned(), value),
                None => object.remove(key),
            };
        }
        if !selection.previous.file_existed && object.is_empty() {
            return restore_file(&file, None);
        }
        let bytes = serde_json::to_vec_pretty(&settings)
            .map_err(|_| err(code::JSON, "Antigravity App 设置无法保存"))?;
        write_private_file(&file, &bytes)
    }
    /// Runs at startup, before the vault is first used. Returns whether the
    /// vault was rewritten; a migrated vault is left untouched.
    pub(crate) fn migrate(paths: &Paths) -> Result<bool> {
        with_lock(|| {
            let Some(bytes) = storage::read_optional(&paths.vault_file())? else {
                return Ok(false);
            };
            let raw: Value = serde_json::from_slice(&bytes).unwrap_or_default();
            let selection = raw.get("gateway").filter(|v| !v.is_null());
            let retired_entries = raw["entries"]
                .as_array()
                .is_some_and(|entries| entries.iter().any(|e| e["kind"] != OAUTH_KIND));
            if selection.is_none() && !retired_entries {
                return Ok(false);
            }
            let (vault, _) = load_vault(paths)?;
            if let Some(selection) = selection.and_then(|v| Selection::deserialize(v).ok()) {
                restore_app_settings(paths, selection)?;
            }
            save_vault(paths, &vault)?;
            Ok(true)
        })
    }
}
pub(crate) use retired::migrate as migrate_retired_accounts;
#[cfg(any(feature = "desktop", test))]
const EXCHANGE_SCHEMA: &str = "vela-antigravity-accounts";
#[cfg(any(feature = "desktop", test))]
/// The saved accounts as one file, logins included, that Vela reads back.
#[derive(Serialize, Deserialize)]
struct ExchangeFile {
    schema: String,
    version: u32,
    accounts: Vec<ExchangedAccount>,
}
#[cfg(any(feature = "desktop", test))]
#[derive(Serialize, Deserialize)]
struct ExchangedAccount {
    name: String,
    payload: String,
}
#[cfg(any(feature = "desktop", test))]
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityImport {
    pub state: AntigravityAuthState,
    pub added: usize,
    /// Accounts the file holds that were already saved.
    pub skipped: usize,
}
#[cfg(any(feature = "desktop", test))]
/// Writes the chosen accounts, in the order they are saved.
pub fn export_accounts_at(
    paths: &Paths,
    rev: &str,
    ids: &[String],
    target: &Path,
) -> Result<usize> {
    with_lock(|| {
        verify_revision(paths, rev)?;
        let (vault, _) = load_vault(paths)?;
        if ids.is_empty() {
            return Err(err(code::ACCOUNT, "请选择要导出的 Antigravity 账号"));
        }
        if ids
            .iter()
            .any(|id| !vault.entries.iter().any(|e| &e.id == id))
        {
            return Err(err(code::ACCOUNT, "所选 Antigravity 账号已不存在"));
        }
        let file = ExchangeFile {
            schema: EXCHANGE_SCHEMA.to_owned(),
            version: 1,
            accounts: vault
                .entries
                .iter()
                .filter(|e| ids.contains(&e.id))
                .map(|e| ExchangedAccount {
                    name: e.name.clone(),
                    payload: e.payload.clone(),
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file)
            .map_err(|_| err(code::EXPORT, "无法序列化 Antigravity 账号"))?;
        storage::atomic_write(target, &bytes, None)?;
        Ok(file.accounts.len())
    })
}
#[cfg(any(feature = "desktop", test))]
/// Adds the accounts of an exported file that are not saved yet. The login in
/// use and the accounts already saved are left as they are.
pub fn import_accounts_at(paths: &Paths, rev: &str, source: &Path) -> Result<AntigravityImport> {
    let unknown = || {
        err(
            code::IMPORT,
            "所选文件不是 Vela 导出的 Antigravity 账号文件",
        )
    };
    let bytes = storage::read_optional(source)?.ok_or_else(unknown)?;
    let file: ExchangeFile = serde_json::from_slice(&bytes).map_err(|_| unknown())?;
    if file.schema != EXCHANGE_SCHEMA {
        return Err(unknown());
    }
    if file.version != 1 {
        return Err(err(code::IMPORT, "账号文件版本较新，请更新 Vela"));
    }
    let incoming = file
        .accounts
        .into_iter()
        .map(|account| {
            if account.payload.len() as u64 > CREDENTIAL_LIMIT {
                return Err(unknown());
            }
            let (method, email) = parse_oauth_metadata(&account.payload).map_err(|_| unknown())?;
            let name = clean_label(Some(&account.name), email.as_deref().unwrap_or(UNNAMED))?;
            Ok((name, method, email, account.payload))
        })
        .collect::<Result<Vec<_>>>()?;
    with_lock(|| {
        verify_revision(paths, rev)?;
        let (mut vault, _) = load_vault(paths)?;
        let before = vault.entries.len();
        let total = incoming.len();
        for (name, method, email, payload) in incoming {
            let (digest, token) = (payload_digest(&payload), oauth_refresh_token(&payload));
            let saved = vault.entries.iter().any(|e| {
                entry_matches(e, Some(&digest), email.as_deref())
                    || (token.is_some() && oauth_refresh_token(&e.payload) == token)
            });
            if !saved {
                vault.push_new(name, &method, email, payload);
            }
        }
        let added = vault.entries.len() - before;
        if added > 0 {
            save_vault(paths, &vault)?;
        }
        Ok(AntigravityImport {
            state: state_at(paths)?,
            added,
            skipped: total - added,
        })
    })
}
pub(crate) fn with_usage_sync<T>(
    mut synchronize: impl FnMut(),
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    synchronize();
    let state = action()?;
    synchronize();
    Ok(state)
}
pub async fn get_antigravity_auth_state(r: &Runtime) -> Result<AntigravityAuthState> {
    let paths = r.antigravity_paths();
    let state = tokio::task::spawn_blocking(move || with_lock(|| state_at(&paths)))
        .await
        .map_err(|_| err(code::PROCESS, "读取 Antigravity 账号任务异常中断"))??;
    r.emit(crate::runtime::Event::AntigravityAuth(&state));
    Ok(state)
}
pub async fn switch_antigravity_account(
    r: &Arc<Runtime>,
    id: String,
    expected_revision: String,
) -> Result<AntigravityAuthState> {
    let rt = r.clone();
    tokio::task::spawn_blocking(move || {
        with_usage_sync(
            || {
                let roots = vec![("antigravity".into(), Ok(rt.antigravity_home()))];
                let _ = rt.usage.sync_roots(roots, None);
            },
            || {
                switch_account_at(
                    &rt.antigravity_paths(),
                    &SystemAppLifecycle::default(),
                    &id,
                    &expected_revision,
                )
            },
        )
    })
    .await
    .map_err(|_| err(code::PROCESS, "切换凭据任务异常中断"))?
}
pub fn rename_antigravity_account(
    r: &Runtime,
    id: String,
    name: String,
    expected_revision: String,
) -> Result<AntigravityAuthState> {
    rename_account_at(&r.antigravity_paths(), &id, &name, &expected_revision)
}
pub fn delete_antigravity_account(
    r: &Runtime,
    id: String,
    expected_revision: String,
) -> Result<AntigravityAuthState> {
    delete_account_at(&r.antigravity_paths(), &id, &expected_revision)
}
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn export_antigravity_accounts(
    app: tauri::AppHandle,
    r: R<'_>,
    ids: Vec<String>,
    expected_revision: String,
) -> Result<Option<usize>> {
    use tauri_plugin_dialog::DialogExt;
    let paths = r.antigravity_paths();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(file) = app
            .dialog()
            .file()
            .set_file_name("vela-antigravity-accounts.json")
            .add_filter("JSON", &["json"])
            .blocking_save_file()
        else {
            return Ok(None);
        };
        let path = file
            .into_path()
            .map_err(|_| err(code::PATH, "请选择本地路径"))?;
        export_accounts_at(&paths, &expected_revision, &ids, &path).map(Some)
    })
    .await
    .map_err(|_| err(code::EXPORT, "账号导出失败"))?
}
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn import_antigravity_accounts(
    app: tauri::AppHandle,
    r: R<'_>,
    expected_revision: String,
) -> Result<Option<AntigravityImport>> {
    use tauri_plugin_dialog::DialogExt;
    let paths = r.antigravity_paths();
    let handle = app.clone();
    let imported = tauri::async_runtime::spawn_blocking(move || {
        let Some(file) = handle
            .dialog()
            .file()
            .set_title("选择 Vela 导出的 Antigravity 账号文件")
            .add_filter("JSON", &["json"])
            .blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = file
            .into_path()
            .map_err(|_| err(code::PATH, "请选择本地 JSON 文件"))?;
        import_accounts_at(&paths, &expected_revision, &path).map(Some)
    })
    .await
    .map_err(|_| err(code::IMPORT, "账号导入失败"))??;
    if let Some(imported) = &imported {
        r.emit(crate::runtime::Event::AntigravityAuth(&imported.state));
    }
    Ok(imported)
}
#[cfg(test)]
#[path = "antigravity_auth_tests.rs"]
mod tests;
