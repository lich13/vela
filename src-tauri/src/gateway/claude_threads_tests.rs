//! Claude Code conversations, each kept with one provider once a provider of
//! the queue asks for new or compacted ones; end to end through loopback
//! upstreams.
use super::delegation_tests::{answered, message, send, upstream, Seen};
use super::*;

const SESSION: &str = "x-claude-code-session-id";
const CLASS: &str = "x-claude-code-request-class";
const COMPACTION: (&str, &str) = ("x-claude-code-compaction", "manual");
/// What Claude Code first sends of a conversation: nobody has answered yet.
const FIRST: &[u8] = br#"{"model":"claude-fixture","stream":false,"max_tokens":32,
  "messages":[{"role":"user","content":"fixture"},{"role":"system","content":"fixture"}]}"#;
const LATER: &[u8] = br#"{"model":"claude-fixture","stream":false,"max_tokens":32,
  "messages":[{"role":"user","content":"fixture"},{"role":"assistant","content":"fixture"},
  {"role":"user","content":"fixture"}]}"#;

/// A Claude gateway on its queue of two providers, and what each was sent.
async fn pair() -> (Fixture, [LoopbackServer; 2], [Seen; 2]) {
    let (first, first_seen) = upstream(|n| answered(200, message(&format!("msg_first_{n}")))).await;
    let (second, second_seen) =
        upstream(|n| answered(200, message(&format!("msg_second_{n}")))).await;
    let f = Fixture::new(ClientId::Claude, &[first.url("/v1"), second.url("/v1")]).await;
    f.edit(Edit::Mode {
        mode: "auto".into(),
    });
    (f, [first, second], [first_seen, second_seen])
}
/// Sets what the fixture's provider at `index` asks to be given.
fn asks(f: &Fixture, index: usize, compacted: bool, new: bool) {
    let id = f.gateway.view().providers[index].id.clone();
    f.edit(Edit::TakeoverProvider {
        id,
        handoff_after_compaction: Some(compacted),
        take_new_threads: Some(new),
    });
}
/// Sends a request and tells which provider answered it. The gateway has
/// settled what the answer means for the conversation before this returns.
async fn asked(
    f: &Fixture,
    seen: &[Seen; 2],
    headers: &[(&str, &str)],
    body: &[u8],
) -> std::result::Result<usize, u16> {
    let before = [seen[0].lock().len(), seen[1].lock().len()];
    let (status, _) = send(&f.gateway, "/v1/messages", headers, body).await;
    super::super::test_support::settled(&f.gateway).await;
    if status != 200 {
        return Err(status);
    }
    let after = [seen[0].lock().len(), seen[1].lock().len()];
    Ok(match (after[0] - before[0], after[1] - before[1]) {
        (1, 0) => 0,
        (0, 1) => 1,
        other => panic!("one provider answers one request: {other:?}"),
    })
}

#[tokio::test]
async fn with_no_provider_asking_every_request_starts_at_the_head_of_the_queue() {
    let (f, _upstreams, seen) = pair().await;
    f.start().await;
    for body in [FIRST, LATER] {
        let answered = asked(&f, &seen, &[(SESSION, "fixture-session")], body).await;
        assert_eq!(answered, Ok(0));
    }
    // Nothing is kept of a conversation nobody asked for.
    assert!(f
        .gateway
        .0
        .compaction
        .owner(&crate::gateway::test_support::digest("fixture-session"))
        .is_none());
    f.stop().await;
}

#[tokio::test]
async fn a_new_conversation_goes_to_the_provider_asking_for_new_ones_and_stays_there() {
    let (f, _upstreams, seen) = pair().await;
    asks(&f, 1, false, true);
    f.start().await;

    let new = [(SESSION, "fixture-new")];
    assert_eq!(asked(&f, &seen, &new, FIRST).await, Ok(1));
    assert_eq!(asked(&f, &seen, &new, LATER).await, Ok(1));
    // Its agents and its errands are sent where the conversation is.
    for class in ["subagent", "auxiliary"] {
        let headers = [new[0], (CLASS, class)];
        assert_eq!(asked(&f, &seen, &headers, FIRST).await, Ok(1), "{class}");
    }

    // A conversation first seen with an answer in it is not a new one, and
    // neither is a request that names no conversation.
    let old = [(SESSION, "fixture-old")];
    assert_eq!(asked(&f, &seen, &old, LATER).await, Ok(0));
    assert_eq!(asked(&f, &seen, &old, LATER).await, Ok(0));
    assert_eq!(asked(&f, &seen, &[], FIRST).await, Ok(0));

    // The switch is taken back: the next request starts at the head again.
    asks(&f, 1, false, false);
    assert_eq!(asked(&f, &seen, &new, LATER).await, Ok(0));
    f.stop().await;
}

#[tokio::test]
async fn a_compacted_conversation_moves_to_the_provider_ahead_that_asks_for_compacted_ones() {
    let (f, _upstreams, seen) = pair().await;
    asks(&f, 0, true, false);
    asks(&f, 1, false, true);
    f.start().await;

    let session = [(SESSION, "fixture-compacted")];
    assert_eq!(asked(&f, &seen, &session, FIRST).await, Ok(1));
    assert_eq!(asked(&f, &seen, &session, LATER).await, Ok(1));
    // The compaction is its owner's to do.
    assert_eq!(
        asked(&f, &seen, &[session[0], COMPACTION], LATER).await,
        Ok(1)
    );
    assert_eq!(
        f.gateway.view().handoff_waiting.len(),
        1,
        "the provider ahead is shown waiting for the compacted conversation"
    );
    // An errand between the two is not the request after it.
    let errand = [session[0], (CLASS, "auxiliary")];
    assert_eq!(asked(&f, &seen, &errand, LATER).await, Ok(1));
    // The next request is, and it takes the conversation with it.
    assert_eq!(asked(&f, &seen, &session, FIRST).await, Ok(0));
    assert_eq!(asked(&f, &seen, &session, LATER).await, Ok(0));
    assert!(f.gateway.view().handoff_waiting.is_empty());
    f.stop().await;
}

#[tokio::test]
async fn a_compaction_that_failed_leaves_the_conversation_where_it_was() {
    let (first, first_seen) = upstream(|n| answered(200, message(&format!("msg_first_{n}")))).await;
    // The second provider's third answer, the compaction, is a refusal.
    let (second, second_seen) = upstream(|n| {
        if n == 3 {
            let refusal =
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"fixture"}}"#;
            answered(400, refusal.into())
        } else {
            answered(200, message(&format!("msg_second_{n}")))
        }
    })
    .await;
    let f = Fixture::new(ClientId::Claude, &[first.url("/v1"), second.url("/v1")]).await;
    f.edit(Edit::Mode {
        mode: "auto".into(),
    });
    let seen = [first_seen, second_seen];
    asks(&f, 0, true, false);
    asks(&f, 1, false, true);
    f.start().await;

    let session = [(SESSION, "fixture-refused")];
    assert_eq!(asked(&f, &seen, &session, FIRST).await, Ok(1));
    assert_eq!(asked(&f, &seen, &session, LATER).await, Ok(1));
    assert_eq!(
        asked(&f, &seen, &[session[0], COMPACTION], LATER).await,
        Err(400)
    );
    assert_eq!(asked(&f, &seen, &session, LATER).await, Ok(1));
    f.stop().await;
}

#[tokio::test]
async fn a_client_without_conversations_of_its_own_refuses_the_two_switches() {
    let (upstream, _seen) = upstream(|_| answered(200, message("msg_fixture"))).await;
    let f = Fixture::new(ClientId::Grok, &[upstream.url("/v1")]).await;
    let view = f.gateway.view();
    let refused = f.gateway.edit(
        Edit::TakeoverProvider {
            id: view.providers[0].id.clone(),
            handoff_after_compaction: None,
            take_new_threads: Some(true),
        },
        &view.revision,
        &f.home,
    );
    assert_eq!(
        refused.err().map(|error| error.code),
        Some(crate::errors::code::SETTINGS.into())
    );
}
