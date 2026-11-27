use super::*;
// The tests of every quota asked of a service share the one loopback service.
use crate::usage::quota_fixture::{serve, Reply, Service};
use serde_json::json;

impl Service {
    fn billing(&self) -> Billing {
        Billing::new(self.origin.clone())
    }
}
/// A Grok home whose `auth.json` is `document`.
fn home(document: &Value) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("auth.json"), document.to_string()).unwrap();
    home
}
/// A Grok home signed in to the official account with `key`.
fn signed_in(key: &str) -> tempfile::TempDir {
    home(&json!({SIGN_IN: {"key": key}}))
}
/// What is left of the week and of the month, and why either is missing.
fn said(quota: &Quota) -> (Option<f64>, Option<f64>, Option<&str>) {
    (quota.week, quota.month, quota.failed.as_deref())
}
/// What the account is told where the service answers the week's question
/// with `week` and the month's with `month`.
async fn told(week: Reply, month: Reply) -> Quota {
    let service = serve(move |request| match request.path.as_str() {
        WEEK => week.clone(),
        MONTH => month.clone(),
        _ => (404, "", json!({})),
    })
    .await;
    let home = signed_in("fixture-key");
    service.billing().quota(home.path()).await.unwrap()
}

#[tokio::test]
async fn a_home_without_an_official_login_is_not_asked_for() {
    let service = serve(|_| (200, "", json!({}))).await;
    let billing = service.billing();
    let empty = tempfile::tempdir().unwrap();
    assert!(billing.quota(empty.path()).await.is_none());
    for document in [
        json!({}),
        json!([]),
        // A key of xAI's API is not the login of an account.
        json!({"https://api.x.ai": {"key": "fixture-key"}}),
        json!({SIGN_IN: {}}),
        json!({SIGN_IN: {"key": ""}}),
        json!({SIGN_IN: "fixture-key"}),
    ] {
        let home = home(&document);
        assert!(billing.quota(home.path()).await.is_none(), "{document}");
    }
    let broken = tempfile::tempdir().unwrap();
    std::fs::write(broken.path().join("auth.json"), "{").unwrap();
    assert!(billing.quota(broken.path()).await.is_none());
    assert!(service.paths().is_empty());
}

#[tokio::test]
async fn the_week_is_read_from_its_credits_and_the_month_from_its_limit() {
    let service = serve(|request| match request.path.as_str() {
        WEEK => (
            200,
            "",
            json!({"config": {
                "creditUsagePercent": 34,
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "start": "2098-12-25T00:00:00Z",
                    "end": "2099-01-01T00:00:00Z",
                },
            }}),
        ),
        MONTH => (
            200,
            "",
            json!({"config": {
                "monthlyLimit": {"val": "2500"},
                "used": {"val": "300"},
                "billingPeriodEnd": "2099-01-01T00:00:00Z",
            }}),
        ),
        _ => (404, "", json!({})),
    })
    .await;
    let home = signed_in("fixture-key");
    let quota = service.billing().quota(home.path()).await.unwrap();
    assert_eq!(said(&quota), (Some(66.0), Some(88.0), None));

    // Both are asked for, each once, as the CLI asks.
    let mut paths = service.paths();
    paths.sort();
    assert_eq!(paths, [MONTH, WEEK]);
    let seen = service.seen.lock();
    assert!(seen.iter().all(|request| {
        request.head.starts_with("GET ")
            && request.head.contains("authorization: Bearer fixture-key")
            && request.head.contains("x-xai-token-auth: xai-grok-cli")
            && request.head.contains("x-grok-client-version: 1.0.92")
            && request
                .head
                .contains("user-agent: grok-pager/1.0.92 grok-shell/1.0.92 (macos; aarch64)")
            && request.body.is_empty()
    }));
}

#[test]
fn amounts_are_read_however_the_service_writes_them() {
    assert_eq!(week(&json!({"creditUsagePercent": "12.5"})), Some(87.5));
    assert_eq!(week(&json!({"credit_usage_percent": 100})), Some(0.0));
    // What is left is never less than nothing or more than all of it.
    assert_eq!(week(&json!({"creditUsagePercent": 250})), Some(0.0));
    assert_eq!(week(&json!({"creditUsagePercent": -1})), Some(100.0));
    for unsaid in [json!(null), json!("soon"), json!("NaN"), json!({"val": 5})] {
        assert_eq!(week(&json!({"creditUsagePercent": unsaid})), None);
    }

    assert_eq!(
        month(&json!({"monthlyLimit": 2000, "used": 500})),
        Some(75.0)
    );
    // Used beyond what the plan includes is paid for apart.
    assert_eq!(
        month(&json!({"monthly_limit": {"val": 2000}, "used": {"val": "3000"}})),
        Some(0.0)
    );
    // A plan without a monthly limit has no month to show.
    for config in [
        json!({"monthlyLimit": {"val": "0"}, "used": {"val": "0"}}),
        json!({"monthlyLimit": {}, "used": 5}),
        json!({"used": 5}),
        json!({"monthlyLimit": 2000}),
    ] {
        assert_eq!(month(&config), None, "{config}");
    }
}

#[tokio::test]
async fn an_answer_says_which_login_it_was_asked_with_and_never_the_login() {
    let service = serve(|_| (200, "", json!({"config": {"creditUsagePercent": 10}}))).await;
    let billing = service.billing();
    let asked = |key: &'static str| {
        let billing = &billing;
        async move {
            let home = signed_in(key);
            billing.quota(home.path()).await.unwrap().login
        }
    };
    let first = asked("fixture-key").await;
    assert_eq!(first, storage::digest(b"fixture-key"));
    assert!(!first.contains("fixture-key"));
    // The same login is the same account, and another is another, or this
    // one signed in again.
    assert_eq!(asked("fixture-key").await, first);
    assert_ne!(asked("fixture-other").await, first);
    // A login that could not be asked with is said all the same.
    let refused = told((401, "", json!({})), (401, "", json!({}))).await;
    assert_eq!(refused.login, first);
}

#[tokio::test]
async fn only_a_login_the_service_no_longer_takes_is_said_to_be_over() {
    assert_eq!(OVER, "登录已过期，请重新登录");
    for (status, why) in [(401, OVER), (403, "查询被拒绝")] {
        let refused = (status, "", json!({"error": "fixture"}));
        let quota = told(refused.clone(), refused).await;
        assert_eq!(said(&quota), (None, None, Some(why)));
    }
}

#[tokio::test]
async fn a_limited_login_is_left_alone_until_the_time_has_passed_or_another_signs_in() {
    let service = serve(|request| {
        if request.head.contains("Bearer fixture-limited") {
            (429, "Retry-After: 120\r\n", json!({}))
        } else {
            (200, "", json!({"config": {"creditUsagePercent": 10}}))
        }
    })
    .await;
    let billing = service.billing();
    let limited = signed_in("fixture-limited");
    for _ in 0..2 {
        let quota = billing.quota(limited.path()).await.unwrap();
        assert_eq!(said(&quota), (None, None, Some("查询受限，请稍后重试")));
    }
    // The two questions of the first time, and none since.
    assert_eq!(service.paths().len(), 2);
    let wait = billing.limited.lock().as_ref().unwrap().1 - now();
    assert!((110_000..=120_000).contains(&wait), "{wait}");

    // Another login is asked with at once, and the first is still left alone.
    let other = signed_in("fixture-key");
    let quota = billing.quota(other.path()).await.unwrap();
    // Both answered, so the month that is missing is one the plan lacks.
    assert_eq!(said(&quota), (Some(90.0), None, None));
    assert_eq!(service.paths().len(), 4);
    assert!(billing
        .quota(limited.path())
        .await
        .unwrap()
        .failed
        .is_some());
    assert_eq!(service.paths().len(), 4);

    // Once the time has passed it is asked with again.
    billing.limited.lock().as_mut().unwrap().1 = now() - 1;
    assert!(billing
        .quota(limited.path())
        .await
        .unwrap()
        .failed
        .is_some());
    assert_eq!(service.paths().len(), 6);
}

#[tokio::test]
async fn what_one_answer_gives_is_told_and_the_failure_of_the_other_is_why_the_rest_is_missing() {
    let month = json!({"config": {"monthlyLimit": {"val": "1000"}, "used": {"val": "250"}}});
    let quota = told((500, "", json!({})), (200, "", month)).await;
    assert_eq!(said(&quota), (None, Some(75.0), Some("额度服务暂不可用")));
    let week = json!({"config": {"creditUsagePercent": 40}});
    let quota = told((200, "", week), (0, "", json!({}))).await;
    assert_eq!(said(&quota), (Some(60.0), None, Some("额度服务没有应答")));
    // Either answer may carry what the other was asked for, and with both
    // periods told nothing is missing.
    let both = json!({"config": {"creditUsagePercent": 40, "monthlyLimit": 1000, "used": 100}});
    let quota = told((503, "", json!({})), (200, "", both)).await;
    assert_eq!(said(&quota), (Some(60.0), Some(90.0), None));

    // With nothing told, the first failure is the reason.
    for (week, month, message) in [
        (
            (500, "", json!({})),
            (401, "", json!({})),
            "额度服务暂不可用",
        ),
        ((0, "", json!({})), (500, "", json!({})), "额度服务没有应答"),
        (
            (200, "", json!([])),
            (200, "", json!([])),
            "额度响应无法解析",
        ),
    ] {
        let quota = told(week, month).await;
        assert_eq!(said(&quota), (None, None, Some(message)));
    }
    // Answers that name no amount are not a failure: the account has no such
    // period.
    let unsaid = (
        200,
        "",
        json!({"config": {"currentPeriod": {"type": "weekly"}}}),
    );
    let quota = told(unsaid, (200, "", json!({}))).await;
    assert_eq!(said(&quota), (None, None, None));
}

#[tokio::test]
async fn the_login_is_not_carried_to_wherever_the_service_points() {
    let service = serve(|_| (302, "Location: /elsewhere\r\n", json!({}))).await;
    let home = signed_in("fixture-key");
    let quota = service.billing().quota(home.path()).await.unwrap();
    assert_eq!(said(&quota), (None, None, Some("额度服务暂不可用")));
    let mut paths = service.paths();
    paths.sort();
    assert_eq!(paths, [MONTH, WEEK]);
}
