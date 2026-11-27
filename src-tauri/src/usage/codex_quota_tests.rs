use super::*;
// The tests of every quota asked of a service share the one loopback service.
use crate::usage::quota_fixture::{serve, Service};
use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde_json::json;

const PATH: &str = "/backend-api/wham/usage";
impl Service {
    fn chatgpt(&self) -> ChatGpt {
        ChatGpt::new(format!("{}{PATH}", self.origin))
    }
}
/// A login the plan of which is Plus, of the account known by `key`.
fn login(key: &str) -> QuotaLogin {
    QuotaLogin {
        key: key.into(),
        name: "fixture".into(),
        current: false,
        plan: Some("Plus".into()),
        access_token: "fixture-access".into(),
        workspace: "fixture-workspace".into(),
        expires_at: None,
    }
}
fn window(used: f64, seconds: Option<i64>) -> Value {
    match seconds {
        Some(seconds) => json!({
            "used_percent": used,
            "limit_window_seconds": seconds,
            "reset_after_seconds": 60,
            "reset_at": 4_102_444_800_i64,
        }),
        None => json!({"used_percent": used}),
    }
}
fn answer(primary: Value, secondary: Value) -> Value {
    json!({
        "plan_type": "pro",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": primary,
            "secondary_window": secondary,
        },
        "credits": {"has_credits": false, "unlimited": false, "balance": null},
    })
}
/// What a saved account is told where ChatGPT answers `body`.
async fn told(body: Value) -> Result<Quota> {
    let service = serve(move |_| (200, "", body.clone())).await;
    service.chatgpt().quota(&login("account")).await
}
fn left(quota: &Quota) -> Vec<(&str, f64)> {
    quota
        .periods
        .iter()
        .map(|period| (period.label.as_str(), period.percent))
        .collect()
}

#[tokio::test]
async fn periods_are_named_by_how_long_they_last_and_the_shorter_comes_first() {
    let quota = told(answer(
        window(28.0, Some(18_000)),
        window(69.0, Some(604_800)),
    ))
    .await
    .unwrap();
    assert_eq!(left(&quota), [("5h", 72.0), ("7d", 31.0)]);

    // A plan may count over other lengths, and name the longer one first.
    let quota = told(answer(
        window(10.0, Some(2_592_000)),
        window(40.5, Some(5_400)),
    ))
    .await
    .unwrap();
    assert_eq!(left(&quota), [("90m", 59.5), ("30d", 90.0)]);

    // A length is counted in minutes, rounded up.
    let quota = told(answer(window(0.0, Some(86_341)), Value::Null))
        .await
        .unwrap();
    assert_eq!(left(&quota), [("1d", 100.0)]);

    // Without a length each is taken for what it usually is, and what is
    // left is never less than nothing or more than all of it.
    let quota = told(answer(window(130.0, None), window(-5.0, None)))
        .await
        .unwrap();
    assert_eq!(left(&quota), [("5h", 0.0), ("7d", 100.0)]);
}

#[tokio::test]
async fn a_period_the_answer_lacks_is_left_out_and_an_answer_without_any_is_no_failure() {
    let quota = told(answer(window(12.0, Some(604_800)), Value::Null))
        .await
        .unwrap();
    assert_eq!(left(&quota), [("7d", 88.0)]);
    let quota = told(json!({
        "rate_limit": {"allowed": true, "secondary_window": window(50.0, Some(18_000))},
    }))
    .await
    .unwrap();
    assert_eq!(left(&quota), [("5h", 50.0)]);
    // A period that does not say how much of it was used cannot be shown.
    let quota = told(answer(
        json!({"limit_window_seconds": 18_000}),
        window(1.0, None),
    ))
    .await
    .unwrap();
    assert_eq!(left(&quota), [("7d", 99.0)]);

    for body in [
        answer(Value::Null, Value::Null),
        json!({"plan_type": "pro", "rate_limit": null}),
        json!({"plan_type": "pro"}),
    ] {
        let quota = told(body).await.unwrap();
        assert!(quota.periods.is_empty());
        assert_eq!(quota.plan.as_deref(), Some("Pro"));
    }
}

#[tokio::test]
async fn the_plan_is_the_one_the_answer_names_and_otherwise_the_one_the_login_does() {
    let quota = told(json!({"plan_type": "team"})).await.unwrap();
    assert_eq!(quota.plan.as_deref(), Some("Team"));
    // A plan without a name of its own is shown as the answer writes it.
    let quota = told(json!({"plan_type": "prolite"})).await.unwrap();
    assert_eq!(quota.plan.as_deref(), Some("prolite"));
    for body in [
        json!({}),
        json!({"plan_type": null}),
        json!({"plan_type": "not a plan"}),
    ] {
        let quota = told(body).await.unwrap();
        assert_eq!(quota.plan.as_deref(), Some("Plus"));
    }
}

#[tokio::test]
async fn the_login_is_sent_as_codex_sends_it_with_the_workspace_where_it_names_one() {
    let service = serve(|_| (200, "", json!({}))).await;
    let chatgpt = service.chatgpt();
    chatgpt.quota(&login("account")).await.unwrap();
    let alone = QuotaLogin {
        workspace: String::new(),
        ..login("alone")
    };
    chatgpt.quota(&alone).await.unwrap();
    assert_eq!(service.paths(), [PATH, PATH]);
    let seen = service.seen.lock();
    assert!(seen.iter().all(|request| {
        request.head.starts_with("GET ")
            && request
                .head
                .contains("authorization: Bearer fixture-access")
            && request.head.contains("user-agent: codex-tui/")
            && request.body.is_empty()
    }));
    assert!(seen[0]
        .head
        .contains("chatgpt-account-id: fixture-workspace"));
    assert!(!seen[1].head.contains("chatgpt-account-id"));
}

#[tokio::test]
async fn a_login_past_its_end_is_not_sent_and_one_the_service_refuses_is_over() {
    let service = serve(|_| (200, "", json!({}))).await;
    let chatgpt = service.chatgpt();
    let seconds = u64::try_from(now() / 1000).unwrap();
    let ended = QuotaLogin {
        expires_at: Some(seconds - 1),
        ..login("account")
    };
    let error = chatgpt.quota(&ended).await.unwrap_err();
    assert_eq!(error.message, "登录已过期，请重新登录");
    assert!(service.paths().is_empty());
    // One that still has time is sent.
    let lasting = QuotaLogin {
        expires_at: Some(seconds + 3600),
        ..login("account")
    };
    chatgpt.quota(&lasting).await.unwrap();
    assert_eq!(service.paths().len(), 1);

    // A question that is refused may be a workspace the account has left, or
    // something between here and the service: only 401 says the login ended.
    for (status, said) in [(401, "登录已过期，请重新登录"), (403, "查询被拒绝")] {
        let service = serve(move |_| (status, "", json!({"detail": "fixture"}))).await;
        let chatgpt = service.chatgpt();
        // The account may sign in again, so it is asked for every time.
        for _ in 0..2 {
            let error = chatgpt.quota(&login("account")).await.unwrap_err();
            assert_eq!(error.message, said);
        }
        assert_eq!(service.paths().len(), 2);
    }
}

#[tokio::test]
async fn a_limited_account_is_left_alone_for_as_long_as_the_service_asked() {
    let service = serve(|request| {
        if request.head.contains("chatgpt-account-id: fixture-limited") {
            (429, "Retry-After: 120\r\n", json!({}))
        } else {
            (200, "", json!({}))
        }
    })
    .await;
    let chatgpt = service.chatgpt();
    let limited = QuotaLogin {
        workspace: "fixture-limited".into(),
        ..login("limited")
    };
    for _ in 0..2 {
        let error = chatgpt.quota(&limited).await.unwrap_err();
        assert_eq!(error.message, "查询受限，请稍后重试");
    }
    assert_eq!(service.paths().len(), 1);
    let wait = chatgpt.limited.lock()["limited"] - now();
    assert!((110_000..=120_000).contains(&wait), "{wait}");
    // Another account is asked for all the same.
    chatgpt.quota(&login("account")).await.unwrap();
    assert_eq!(service.paths().len(), 2);

    // An account is known by its key, not by its workspace: another one of
    // the same workspace is asked for, and left alone in its turn.
    let other = QuotaLogin {
        workspace: "fixture-limited".into(),
        ..login("other")
    };
    for _ in 0..2 {
        chatgpt.quota(&other).await.unwrap_err();
    }
    assert_eq!(service.paths().len(), 3);
    assert!(chatgpt.limited.lock().contains_key("other"));

    // Once the time has passed it is asked for again.
    chatgpt.limited.lock().insert("limited".into(), now() - 1);
    chatgpt.quota(&limited).await.unwrap_err();
    assert_eq!(service.paths().len(), 4);
}

#[test]
fn a_wait_is_read_in_seconds_or_as_a_date_and_kept_within_an_hour() {
    let wait = |value: Option<&str>| {
        let mut headers = HeaderMap::new();
        if let Some(value) = value {
            headers.insert(RETRY_AFTER, value.parse().unwrap());
        }
        limited_until(&headers) - now()
    };
    let near = |wait: i64, seconds: i64| (seconds * 1000 - 2000..=seconds * 1000).contains(&wait);
    assert!(near(wait(Some("120")), 120));
    let later = std::time::SystemTime::now() + Duration::from_secs(300);
    assert!(near(wait(Some(&httpdate::fmt_http_date(later))), 300));
    // A service that does not say is left alone for a minute.
    assert!(near(wait(None), 60));
    assert!(near(wait(Some("soon")), 60));
    assert!(near(wait(Some("86400")), 3600));
    for passed in ["0", "-30", "Thu, 01 Jan 2015 00:00:00 GMT"] {
        assert!(near(wait(Some(passed)), 1), "{passed}");
    }
}

#[tokio::test]
async fn a_service_that_fails_or_cannot_be_read_is_told_from_one_that_gives_no_answer() {
    for (status, extra, body, message) in [
        (500, "", json!({}), "额度服务暂不可用"),
        // The login is not carried to wherever the service points.
        (
            302,
            "Location: /elsewhere\r\n",
            json!({}),
            "额度服务暂不可用",
        ),
        (200, "", json!([]), "额度响应无法解析"),
        (0, "", json!({}), "额度服务没有应答"),
    ] {
        let service = serve(move |_| (status, extra, body.clone())).await;
        let error = service
            .chatgpt()
            .quota(&login("account"))
            .await
            .unwrap_err();
        assert_eq!(error.message, message);
        assert_eq!(service.paths(), [PATH]);
    }
}
