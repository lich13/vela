//! Which requests count as a success, which as an error and which as neither.
//!
//! The table below is what the success rate is meant to say of each source,
//! written out case by case rather than worked out from the code that decides
//! it. Every fixture is synthetic.

use super::*;

const SUCCESS: Option<bool> = Some(true);
const ERROR: Option<bool> = Some(false);
const NEITHER: Option<bool> = None;

/// Source, the last attempt's status and terminal state, whether the request
/// completed, and what it counts as.
type Case = (&'static str, Option<u16>, &'static str, bool, Option<bool>);

const CASES: &[Case] = &[
    // The gateway saw the answer: a 2xx or a protocol switch that ended well.
    ("proxy", Some(200), "success", true, SUCCESS),
    ("proxy", Some(201), "success", true, SUCCESS),
    ("proxy", Some(101), "success", true, SUCCESS),
    // Ending at the output limit is an answer in full.
    ("proxy", Some(200), "limited", true, SUCCESS),
    // The provider refused, has no such model, or failed, whatever it answered
    // with and even when nothing came back at all.
    ("proxy", Some(429), "rejected", false, ERROR),
    ("proxy", Some(401), "rejected", false, ERROR),
    ("proxy", Some(200), "rejected", false, ERROR),
    ("proxy", Some(404), "model_unavailable", false, ERROR),
    ("proxy", Some(503), "model_unavailable", false, ERROR),
    ("proxy", Some(500), "failure", false, ERROR),
    ("proxy", Some(200), "failure", false, ERROR),
    ("proxy", None, "failure", false, ERROR),
    ("proxy", None, "rejected", false, ERROR),
    ("proxy", None, "model_unavailable", false, ERROR),
    // An answer that was not a success, of a request kept before terminal
    // states were, is an error by its status alone.
    ("proxy", Some(500), "", false, ERROR),
    ("proxy", Some(200), "", false, ERROR),
    ("proxy", Some(200), "", true, SUCCESS),
    // The client gave the request up, or nothing was seen of its end: that is
    // not the provider's doing, whatever status had come by then.
    ("proxy", Some(200), "cancelled", false, NEITHER),
    ("proxy", None, "cancelled", false, NEITHER),
    ("proxy", Some(200), "unknown", false, NEITHER),
    ("proxy", None, "unknown", false, NEITHER),
    // Without a status and without a failure nothing is known.
    ("proxy", None, "", false, NEITHER),
    // Antigravity, from the app and from both kinds of session file, says
    // which requests ended well and which failed.
    ("antigravity", None, "success", true, SUCCESS),
    ("antigravity", None, "failed", false, ERROR),
    ("antigravity", None, "unknown", false, NEITHER),
    ("antigravity", None, "in_progress", false, NEITHER),
    ("antigravity", None, "cancelled", false, NEITHER),
    // A Claude Code session: a message that ended, an API error it wrote
    // down, and nothing of a message cut short or of one imported before
    // either was told apart.
    ("claude", None, "success", true, SUCCESS),
    ("claude", Some(429), "failed", false, ERROR),
    ("claude", Some(529), "failed", false, ERROR),
    ("claude", None, "failed", false, ERROR),
    ("claude", None, "completed", true, NEITHER),
    ("claude", None, "reported", false, NEITHER),
    // A Codex or Grok session keeps no reliable word of how a request ended,
    // so none of theirs counts, whatever a record of one happens to hold.
    ("codex", None, "failed", false, NEITHER),
    ("codex", None, "completed", true, NEITHER),
    ("codex", None, "reported", false, NEITHER),
    ("codex", Some(200), "success", true, NEITHER),
    ("grok", Some(200), "completed", true, NEITHER),
    ("grok", Some(200), "success", true, NEITHER),
    ("grok", Some(500), "failed", false, NEITHER),
    ("grok", None, "completed", true, NEITHER),
];

/// A request of `source` whose only attempt ended as the case says.
fn request((source, status, outcome, completed, _): Case) -> Record {
    Record {
        id: format!("fixture-{source}-{status:?}-{outcome}-{completed}"),
        client: if source == "proxy" { "claude" } else { source }.into(),
        source: source.into(),
        completed,
        attempts: vec![Attempt {
            status,
            outcome: outcome.into(),
            ..Attempt::default()
        }],
        ..Record::default()
    }
}

#[test]
fn each_source_counts_what_it_can_tell_and_nothing_else() {
    for case in CASES {
        let (source, status, outcome, completed, expected) = *case;
        assert_eq!(
            verdict(source, status, outcome, completed),
            expected,
            "{source} {status:?} {outcome:?} completed={completed}"
        );
    }
}

#[test]
fn a_request_counts_as_its_last_attempt_ended() {
    for case in CASES {
        let (source, status, outcome, completed, expected) = *case;
        let mut record = request(*case);
        assert_eq!(
            record.known_success(),
            expected,
            "{source} {status:?} {outcome:?} completed={completed}"
        );
        let mut totals = Totals::default();
        totals.add_record(&record);
        assert_eq!(
            (totals.requests, totals.status_known, totals.success),
            (
                1,
                u64::from(expected.is_some()),
                u64::from(expected == SUCCESS)
            ),
            "{source} {status:?} {outcome:?} completed={completed}"
        );
        // An earlier attempt that failed decides nothing once another followed.
        record.attempts.insert(
            0,
            Attempt {
                status: Some(500),
                outcome: "failure".into(),
                ..Attempt::default()
            },
        );
        assert_eq!(record.known_success(), expected);
    }
    // A request with no attempt has nothing to go by.
    let empty = Record {
        source: "proxy".into(),
        completed: true,
        ..Record::default()
    };
    assert_eq!(empty.known_success(), NEITHER);
}
