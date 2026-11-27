use super::*;
// The tests of every quota asked of a service share the one loopback service.
use crate::usage::quota_fixture::{serve, Service};

impl Service {
    /// Asks this service at three hosts: `/a`, `/b`, `/c`.
    fn cloud(&self) -> Cloud {
        Cloud::new(
            ["a", "b", "c"]
                .map(|host| format!("{}/{host}", self.origin))
                .to_vec(),
        )
    }
}

const PROJECT: &str = "/a/v1internal:loadCodeAssist";
fn summary(host: &str) -> String {
    format!("/{host}/v1internal:retrieveUserQuotaSummary")
}
fn bucket(id: &str, fraction: f64) -> Value {
    json!({"bucketId": id, "remainingFraction": fraction, "resetTime": "2099-01-01T00:00:00Z"})
}
fn groups(buckets: &[Value]) -> Value {
    json!({"description": "fixture", "groups": [{"displayName": "fixture", "buckets": buckets}]})
}
/// A login whose access token ends `ends_in` milliseconds from now.
fn login(access: &str, ends_in: i64) -> QuotaLogin {
    QuotaLogin {
        id: "account".into(),
        current: true,
        access_token: Some(access.into()),
        expires_at: Some(now() + ends_in),
    }
}
const HOUR: i64 = 3_600_000;

#[tokio::test]
async fn the_project_is_learned_once_and_hosts_are_asked_until_one_reports_gemini() {
    let service = serve(|request| match request.path.as_str() {
        PROJECT => (
            200,
            "",
            json!({"cloudaicompanionProject": {"id": "fixture-project"}}),
        ),
        "/a/v1internal:retrieveUserQuotaSummary" => (200, "", groups(&[bucket("3p-weekly", 0.5)])),
        "/b/v1internal:retrieveUserQuotaSummary" => (
            200,
            "",
            groups(&[bucket("3p-weekly", 0.9), bucket("gemini-5h", 0.25)]),
        ),
        _ => (500, "", json!({})),
    })
    .await;
    let cloud = service.cloud();
    let saved = login("fixture-access", HOUR);
    let periods = cloud.quota(&saved).await.unwrap();
    // What the first host reported stands; the second adds what it did not.
    assert_eq!(periods["3p-weekly"].remaining_percent, Some(50.0));
    assert_eq!(periods["gemini-5h"].remaining_percent, Some(25.0));
    assert_eq!(
        service.paths(),
        [PROJECT.into(), summary("a"), summary("b")]
    );
    {
        let seen = service.seen.lock();
        assert!(seen.iter().all(|request| {
            request
                .head
                .contains("authorization: Bearer fixture-access")
                && request.head.contains("user-agent: antigravity/")
        }));
        assert_eq!(
            serde_json::from_str::<Value>(&seen[0].body).unwrap(),
            json!({"metadata": {"ideType": "ANTIGRAVITY"}})
        );
        assert_eq!(
            serde_json::from_str::<Value>(&seen[1].body).unwrap(),
            json!({"project": "fixture-project"})
        );
    }
    cloud.quota(&saved).await.unwrap();
    assert_eq!(service.paths()[3..], [summary("a"), summary("b")]);
}

#[tokio::test]
async fn a_token_that_has_ended_is_not_sent_and_one_that_is_refused_is_sent_to_no_other_host() {
    let service = serve(|request| match request.path.as_str() {
        _ if request.head.contains("Bearer fixture-refused") => (401, "", json!({})),
        PROJECT => (200, "", json!({"projectId": "fixture-project"})),
        "/a/v1internal:retrieveUserQuotaSummary" => (401, "", json!({})),
        _ => (200, "", groups(&[bucket("gemini-5h", 0.5)])),
    })
    .await;
    let cloud = service.cloud();
    // Past its end, within a minute of it, or not there at all: nothing is
    // asked, and nothing here renews it.
    let none = QuotaLogin {
        access_token: None,
        ..login("fixture-old", HOUR)
    };
    for ended in [login("fixture-old", -1), login("fixture-old", 30_000), none] {
        let failure = cloud.quota(&ended).await.unwrap_err();
        assert!(failure.expired() && !failure.stands());
        assert!(failure.error(true).message.contains("已到期"));
    }
    assert!(service.paths().is_empty());

    // The saved token says it has an hour left, and the service refuses it.
    let refused = login("fixture-refused", HOUR);
    assert!(matches!(cloud.quota(&refused).await, Err(Failure::Expired)));
    assert_eq!(service.paths(), [PROJECT]);
    // Refused for the quota alone, it is not sent to the other hosts.
    let unsaid = QuotaLogin {
        // A token that does not say when it ends is tried as it is.
        expires_at: None,
        ..login("fixture-access", HOUR)
    };
    assert!(matches!(cloud.quota(&unsaid).await, Err(Failure::Expired)));
    assert_eq!(service.paths()[1..], [PROJECT.into(), summary("a")]);
}

#[tokio::test]
async fn a_limited_service_is_left_alone_for_as_long_as_it_asked() {
    let service = serve(|request| match request.path.as_str() {
        PROJECT => (
            200,
            "",
            json!({"cloudaicompanionProject": "fixture-project"}),
        ),
        _ => (429, "Retry-After: 120\r\n", json!({})),
    })
    .await;
    let cloud = service.cloud();
    let saved = login("fixture-access", HOUR);
    assert!(matches!(cloud.quota(&saved).await, Err(Failure::Limited)));
    // The other hosts are the same service.
    assert_eq!(service.paths(), [PROJECT.into(), summary("a")]);
    assert!(matches!(cloud.quota(&saved).await, Err(Failure::Limited)));
    assert_eq!(service.paths().len(), 2);
    let wait = cloud.kept("account").limited_until - now();
    assert!((110_000..=120_000).contains(&wait), "{wait}");
}

#[tokio::test]
async fn hosts_that_fail_are_passed_over_and_an_answer_without_a_quota_is_a_failure() {
    let service = serve(|request| match request.path.as_str() {
        // An account whose project cannot be learned is asked without one.
        PROJECT => (500, "", json!({})),
        "/a/v1internal:retrieveUserQuotaSummary" => (503, "", json!({})),
        "/b/v1internal:retrieveUserQuotaSummary" => (200, "", json!({"groups": []})),
        _ => (200, "", groups(&[bucket("gemini-weekly", 0.125)])),
    })
    .await;
    let saved = login("fixture-access", HOUR);
    let periods = service.cloud().quota(&saved).await.unwrap();
    assert_eq!(periods["gemini-weekly"].remaining_percent, Some(12.5));
    assert_eq!(
        service.paths(),
        [PROJECT.into(), summary("a"), summary("b"), summary("c")]
    );
    assert_eq!(service.seen.lock()[1].body, "{}");

    let service = serve(|request| match request.path.as_str() {
        PROJECT => (
            200,
            "",
            json!({"cloudaicompanionProject": "fixture-project"}),
        ),
        "/a/v1internal:retrieveUserQuotaSummary" => (503, "", json!({})),
        _ => (200, "", groups(&[bucket("unknown-quota", 1.0)])),
    })
    .await;
    let failure = service.cloud().quota(&saved).await.unwrap_err();
    assert!(!failure.stands());
    assert!(failure.error(true).message.contains("暂不可用"));
    assert_eq!(service.paths().len(), 4);

    // A first host that gives no sign is not followed by waits for the others.
    let service = serve(|request| match request.path.as_str() {
        PROJECT => (
            200,
            "",
            json!({"cloudaicompanionProject": "fixture-project"}),
        ),
        _ => (0, "", json!({})),
    })
    .await;
    assert!(matches!(
        service.cloud().quota(&saved).await,
        Err(Failure::Unreachable)
    ));
    assert_eq!(service.paths(), [PROJECT.into(), summary("a")]);
}
