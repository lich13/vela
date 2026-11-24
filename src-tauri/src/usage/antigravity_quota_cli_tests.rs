use super::*;
use serde_json::json;
use std::cell::RefCell;

/// A scripted `agy`: the version it reports, the outputs of its `/usage`
/// runs in turn, and every argument list it was started with.
struct Fixture {
    version: &'static str,
    usage: RefCell<Vec<Output>>,
    started: RefCell<Vec<&'static [&'static str]>>,
}
impl Fixture {
    fn new(version: &'static str, usage: Vec<Output>) -> Self {
        Self {
            version,
            usage: RefCell::new(usage),
            started: RefCell::new(Vec::new()),
        }
    }
    async fn run(&self, args: &'static [&'static str]) -> Result<Output> {
        self.started.borrow_mut().push(args);
        Ok(if args == VERSION {
            printed(self.version)
        } else {
            self.usage.borrow_mut().remove(0)
        })
    }
    async fn quota(&self, session: &tokio::sync::Mutex<Session>, account: &str) -> Result<Periods> {
        quota_with(session, account, |args, _| self.run(args)).await
    }
    fn usage_runs(&self) -> usize {
        self.started
            .borrow()
            .iter()
            .filter(|args| **args == USAGE)
            .count()
    }
}

fn session() -> tokio::sync::Mutex<Session> {
    tokio::sync::Mutex::new(Session::new())
}

fn printed(stdout: &str) -> Output {
    Output {
        code: Some(0),
        stdout: stdout.to_owned(),
        stderr: String::new(),
        timed_out: false,
    }
}

fn bucket(id: &str, reset: &str, remaining: f64) -> Value {
    json!({"id": id, "name": id, "window": "fixture", "remaining_fraction": remaining, "reset_time": reset})
}

fn envelope(groups: Value) -> String {
    let line =
        json!({"status": "SUCCESS", "command": {"name": "usage", "data": {"groups": groups}}});
    format!("Loading fixture\n{line}\n")
}

fn four_periods() -> String {
    envelope(json!([
        {"buckets": [
            bucket("gemini-5h", "2026-01-08T05:00:00Z", 0.745),
            bucket("gemini-weekly", "2026-01-09T00:00:00Z", 0.86),
        ]},
        {"buckets": [
            bucket("3p-5h", "2026-01-08T06:00:00Z", 0.42),
            bucket("3p-weekly", "2026-01-10T00:00:00Z", 0.685),
        ]},
    ]))
}

#[tokio::test]
async fn usage_envelope_reads_as_the_four_periods_the_app_reports() {
    let agy = Fixture::new("agy 1.1.11\n", vec![printed(&four_periods())]);
    let read = agy.quota(&session(), "account-a").await.unwrap();
    assert_eq!(read.len(), 4);
    assert_eq!(read["gemini-5h"].remaining_percent, Some(74.5));
    assert_eq!(
        read["gemini-5h"].end - read["gemini-5h"].start,
        5 * 3_600_000
    );
    assert_eq!(
        read["3p-weekly"].end - read["3p-weekly"].start,
        7 * 86_400_000
    );
    assert_eq!(read["3p-weekly"].remaining_percent, Some(68.5));
    assert_eq!(*agy.started.borrow(), [VERSION, USAGE]);

    // The app's local service names the same fields in camel case.
    let app = json!({"response": {"groups": [
        {"buckets": [
            {"bucketId": "gemini-5h", "resetTime": "2026-01-08T05:00:00Z", "remainingFraction": 0.745},
            {"bucketId": "gemini-weekly", "resetTime": "2026-01-09T00:00:00Z", "remainingFraction": 0.86},
        ]},
        {"buckets": [
            {"bucketId": "3p-5h", "resetTime": "2026-01-08T06:00:00Z", "remainingFraction": 0.42},
            {"bucketId": "3p-weekly", "resetTime": "2026-01-10T00:00:00Z", "remainingFraction": 0.685},
        ]},
    ]}});
    assert_eq!(read, super::super::parse(&app));
}

#[tokio::test]
async fn disabled_buckets_are_left_out() {
    let mut off = bucket("3p-5h", "2026-01-08T06:00:00Z", 1.0);
    off["disabled"] = true.into();
    let output = envelope(json!([{"buckets": [
        bucket("gemini-5h", "2026-01-08T05:00:00Z", 0.5),
        off,
    ]}]));
    let agy = Fixture::new("1.2.0", vec![printed(&output)]);
    let read = agy.quota(&session(), "account-a").await.unwrap();
    assert_eq!(read.keys().collect::<Vec<_>>(), ["gemini-5h"]);
}

#[tokio::test]
async fn an_older_or_unreadable_version_is_never_sent_the_usage_command() {
    for version in ["agy 1.1.10", "0.9.99", "unknown"] {
        let agy = Fixture::new(version, vec![]);
        let error = agy.quota(&session(), "account-a").await.unwrap_err();
        assert!(error.message.contains("版本过旧"), "{}", error.message);
        assert_eq!(*agy.started.borrow(), [VERSION]);
    }
}

#[tokio::test]
async fn a_login_prompt_stops_further_runs_until_the_login_changes() {
    let prompt = Output {
        code: None,
        stdout: String::new(),
        stderr: format!("{LOGIN_PROMPT}: https://example.invalid/auth\n"),
        timed_out: true,
    };
    let agy = Fixture::new("1.1.11", vec![prompt, printed(&four_periods())]);
    let session = session();
    let first = agy.quota(&session, "account-a").await.unwrap_err();
    assert!(first.message.contains("需要重新登录"), "{}", first.message);
    assert_eq!(agy.started.borrow().len(), 2);

    let second = agy.quota(&session, "account-a").await.unwrap_err();
    assert_eq!(second.message, first.message);
    assert_eq!(agy.started.borrow().len(), 2, "no process is started again");

    assert_eq!(agy.quota(&session, "account-b").await.unwrap().len(), 4);
}

#[tokio::test]
async fn a_spent_model_turn_stops_further_runs() {
    for evidence in [
        json!({"status": "SUCCESS", "result": "fixture answer", "num_turns": 1}),
        json!({"status": "ERROR", "error": "fixture", "conversation_id": "fixture-conversation"}),
    ] {
        let agy = Fixture::new("1.1.11", vec![printed(&format!("{evidence}\n"))]);
        let session = session();
        let first = agy.quota(&session, "account-a").await.unwrap_err();
        assert!(first.message.contains("当成了提问"), "{}", first.message);
        let second = agy.quota(&session, "account-a").await.unwrap_err();
        assert_eq!(second.message, first.message);
        assert_eq!(agy.usage_runs(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn an_answer_is_reused_for_a_minute_by_the_same_account() {
    let agy = Fixture::new(
        "1.1.11",
        vec![
            printed(&four_periods()),
            printed(&four_periods()),
            printed(&four_periods()),
        ],
    );
    let session = session();
    let first = agy.quota(&session, "account-a").await.unwrap();
    tokio::time::advance(Duration::from_secs(59)).await;
    assert_eq!(agy.quota(&session, "account-a").await.unwrap(), first);
    assert_eq!(agy.usage_runs(), 1);

    agy.quota(&session, "account-b").await.unwrap();
    assert_eq!(agy.usage_runs(), 2);

    tokio::time::advance(Duration::from_secs(60)).await;
    agy.quota(&session, "account-b").await.unwrap();
    assert_eq!(agy.usage_runs(), 3);
}

#[tokio::test(start_paused = true)]
async fn a_timeout_is_reported_for_a_minute_and_then_asked_again() {
    let silent = Output {
        code: None,
        stdout: String::new(),
        stderr: String::new(),
        timed_out: true,
    };
    let agy = Fixture::new("1.1.11", vec![silent, printed(&four_periods())]);
    let session = session();
    let error = agy.quota(&session, "account-a").await.unwrap_err();
    assert!(error.message.contains("超时"), "{}", error.message);
    tokio::time::advance(Duration::from_secs(59)).await;
    let kept = agy.quota(&session, "account-a").await.unwrap_err();
    assert_eq!(kept.message, error.message);
    assert_eq!(agy.usage_runs(), 1, "a hung agy is not waited for again");
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(agy.quota(&session, "account-a").await.unwrap().len(), 4);
}

#[tokio::test]
async fn failures_are_named_without_addresses_or_tokens() {
    let limited =
        json!({"status": "ERROR", "error": "RESOURCE_EXHAUSTED for fixture@example.invalid"});
    let token = "fixture-token-".repeat(4);
    let other = json!({"status": "ERROR", "error": {"message": format!(
        "quota lookup failed for fixture@example.invalid\nwith {token} {}", "detail ".repeat(60)
    )}});
    let mut exited = printed("");
    exited.code = Some(3);
    exited.stderr = "HTTP 429 Too Many Requests".into();
    let agy = Fixture::new(
        "1.1.11",
        vec![
            printed(&format!("{limited}\n")),
            exited,
            printed(&format!("{other}\n")),
            printed("not json\n"),
        ],
    );
    let mut messages = Vec::new();
    for _ in 0..4 {
        messages.push(
            agy.quota(&session(), "account-a")
                .await
                .unwrap_err()
                .message,
        );
    }
    assert!(messages[0].contains("限流"), "{}", messages[0]);
    assert!(messages[1].contains("限流"), "{}", messages[1]);
    assert!(
        messages[2].starts_with("agy 读取额度失败：quota lookup failed for *** with *** detail"),
        "{}",
        messages[2]
    );
    assert!(messages[2].chars().count() <= 160 + "agy 读取额度失败：".chars().count());
    assert_eq!(messages[3], "agy 未返回额度");
    for message in &messages {
        assert!(!message.contains("example.invalid") && !message.contains(&token));
    }
}
