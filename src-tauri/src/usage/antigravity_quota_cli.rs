//! Account quota read through the `agy` CLI while the Antigravity app has no
//! answer. `agy /usage` answers without a model turn from 1.1.11 on; anything
//! older takes it for a billed prompt, so it is never sent there.
use super::{periods, Periods};
use crate::{
    storage::{AppError, Result},
    usage::model::failure,
};
use serde_json::Value;
use std::{future::Future, process::Stdio, sync::LazyLock, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    time::Instant,
};

const MINIMUM_VERSION: (u64, u64, u64) = (1, 1, 11);
const VERSION: &[&str] = &["--version"];
const USAGE: &[&str] = &[
    "-p",
    "/usage",
    "--output-format",
    "json",
    "--print-timeout",
    "20s",
];
const VERSION_LIMIT: Duration = Duration::from_secs(5);
const USAGE_LIMIT: Duration = Duration::from_secs(30);
/// Every usage statistics query asks for the quota.
const FRESH: Duration = Duration::from_secs(60);
const OUTPUT_LIMIT: u64 = 512 * 1024;
const LOGIN_PROMPT: &str = "Authentication required. Please visit the URL";

/// What one run of the CLI left behind.
pub(super) struct Output {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Why `agy` is not asked again for the login it was seen with.
#[derive(Clone, Copy)]
enum Refusal {
    /// It asked for an interactive login, which opens the browser.
    SignedOut,
    /// It spent a model turn on the question.
    ModelTurn,
}
impl Refusal {
    fn error(self) -> AppError {
        failure(match self {
            Self::SignedOut => "agy 需要登录，已暂停通过 agy 读取额度；登录后重启 Vela 恢复",
            Self::ModelTurn => "agy 把 /usage 当成了提问，已停止通过 agy 读取额度",
        })
    }
}

/// Kept for as long as Vela runs; nothing of it is written to disk.
pub(super) struct Session {
    refused: Option<(String, Refusal)>,
    fresh: Option<(String, Instant, Periods)>,
}
impl Session {
    const fn new() -> Self {
        Self {
            refused: None,
            fresh: None,
        }
    }
}
static SESSION: tokio::sync::Mutex<Session> = tokio::sync::Mutex::const_new(Session::new());

/// The quota periods of the signed-in account `account`. The caller has
/// checked that a login exists: a signed-out `agy` opens the browser.
pub(super) async fn quota(account: &str) -> Result<Periods> {
    quota_with(&SESSION, account, system).await
}

async fn quota_with<F, Fut>(
    session: &tokio::sync::Mutex<Session>,
    account: &str,
    run: F,
) -> Result<Periods>
where
    F: Fn(&'static [&'static str], Duration) -> Fut,
    Fut: Future<Output = Result<Output>>,
{
    // One question at a time: a second caller waits and reads the answer of
    // the first.
    let mut session = session.lock().await;
    if session
        .refused
        .as_ref()
        .is_some_and(|(id, _)| id != account)
    {
        session.refused = None;
    }
    if let Some((_, refusal)) = &session.refused {
        return Err(refusal.error());
    }
    if let Some((id, at, periods)) = &session.fresh {
        if id == account && at.elapsed() < FRESH {
            return Ok(periods.clone());
        }
    }
    let version = run(VERSION, VERSION_LIMIT).await?;
    if version.timed_out {
        return Err(timed_out());
    }
    if !supported(&version.stdout) && !supported(&version.stderr) {
        return Err(failure(
            "agy 版本过旧，升级到 1.1.11 或更高版本后才能读取额度",
        ));
    }
    let output = run(USAGE, USAGE_LIMIT).await?;
    match read(&output) {
        Ok(periods) => {
            session.fresh = Some((account.to_owned(), Instant::now(), periods.clone()));
            Ok(periods)
        }
        Err((refusal, error)) => {
            if let Some(refusal) = refusal {
                session.refused = Some((account.to_owned(), refusal));
            }
            Err(error)
        }
    }
}

fn timed_out() -> AppError {
    failure("agy 读取额度超时，请稍后再试")
}

fn supported(text: &str) -> bool {
    static NUMBER: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(\d+)\.(\d+)\.(\d+)").unwrap());
    NUMBER.captures(text).is_some_and(|found| {
        let part = |index: usize| found[index].parse::<u64>().unwrap_or(0);
        (part(1), part(2), part(3)) >= MINIMUM_VERSION
    })
}

/// The quota in the output, or the failure to show and whether `agy` must not
/// be asked again.
fn read(output: &Output) -> std::result::Result<Periods, (Option<Refusal>, AppError)> {
    let mut model_turn = false;
    let mut reported = None;
    for line in output.stdout.lines().map(str::trim) {
        if !line.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let command = &value["command"];
        if value["status"] == "SUCCESS"
            && command["name"] == "usage"
            && command["data"]["groups"].is_array()
        {
            return Ok(periods(&command["data"]["groups"]));
        }
        model_turn |= value["num_turns"].as_u64().is_some_and(|turns| turns > 0)
            || value["conversation_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty());
        if value["status"] == "ERROR" {
            let error = &value["error"];
            reported = error
                .as_str()
                .or_else(|| error["message"].as_str())
                .map(sanitized)
                .filter(|text| !text.is_empty());
        }
    }
    let said = |needle: &str| output.stdout.contains(needle) || output.stderr.contains(needle);
    if said(LOGIN_PROMPT) {
        return Err((Some(Refusal::SignedOut), Refusal::SignedOut.error()));
    }
    if model_turn {
        return Err((Some(Refusal::ModelTurn), Refusal::ModelTurn.error()));
    }
    if output.timed_out {
        return Err((None, timed_out()));
    }
    static TOO_MANY: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\b429\b").unwrap());
    if said("RESOURCE_EXHAUSTED")
        || TOO_MANY.is_match(&output.stdout)
        || TOO_MANY.is_match(&output.stderr)
    {
        return Err((None, failure("agy 读取额度被限流，请稍后再试")));
    }
    let message = match (reported, output.code) {
        (Some(text), _) => format!("agy 读取额度失败：{text}"),
        (None, Some(code)) if code != 0 => format!("agy 未返回额度（退出码 {code}）"),
        (None, _) => "agy 未返回额度".to_owned(),
    };
    Err((None, failure(&message)))
}

/// `agy`'s own error text, cut short and without addresses or anything that
/// looks like a token.
fn sanitized(text: &str) -> String {
    static PRIVATE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+|[A-Za-z0-9._~+/=-]{32,}").unwrap()
    });
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    PRIVATE
        .replace_all(&line, "***")
        .chars()
        .take(160)
        .collect()
}

async fn system(args: &'static [&'static str], limit: Duration) -> Result<Output> {
    let names: &[&str] = if cfg!(windows) {
        &["agy.exe", "agy.cmd"]
    } else {
        &["agy"]
    };
    let program = crate::login::find_cli(names)
        .ok_or_else(|| failure("未找到 agy，无法在 Antigravity app 未运行时读取额度"))?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn().map_err(|_| failure("无法启动 agy"))?;
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    // What was printed before a timeout still tells why nothing came.
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = tokio::time::timeout(limit, async {
        let (_, _, exit) = tokio::join!(
            drain(stdout, &mut out),
            drain(stderr, &mut err),
            child.wait()
        );
        exit
    })
    .await;
    if exit.is_err() {
        let _ = child.kill().await;
    }
    Ok(Output {
        code: exit
            .as_ref()
            .ok()
            .and_then(|exit| exit.as_ref().ok()?.code()),
        stdout: String::from_utf8_lossy(&out).into_owned(),
        stderr: String::from_utf8_lossy(&err).into_owned(),
        timed_out: exit.is_err(),
    })
}

/// Keeps the first part of the output and lets the rest through, so a
/// talkative process is never left blocked on a full pipe.
async fn drain(pipe: Option<impl AsyncRead + Unpin>, kept: &mut Vec<u8>) {
    let Some(mut pipe) = pipe else {
        return;
    };
    let _ = (&mut pipe).take(OUTPUT_LIMIT).read_to_end(kept).await;
    let _ = tokio::io::copy(&mut pipe, &mut tokio::io::sink()).await;
}

#[cfg(test)]
#[path = "antigravity_quota_cli_tests.rs"]
mod tests;
