//! Error responses of the relays, the platforms behind them and the proxies in
//! front of them, each read off the wire and handled as ruled.
use super::{
    failure_policy as policy,
    protocol::{Protocol, Terminal},
};
use crate::events::{Details, ErrorBasis, HealthScope};
use serde::Deserialize;
use std::collections::BTreeMap;

/// One response a line: sub2api, new-api, the platforms' own, then proxies.
const CORPUS: &str = include_str!("fixtures/upstream_errors.jsonl");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sample {
    id: String,
    status: u16,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    body: String,
    /// The response is an event stream.
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    basis: Option<ErrorBasis>,
    handling: Handling,
    /// Why the handling above is not the one wanted: a gap, not a ruling.
    #[serde(default)]
    gap: Option<String>,
}

/// What the gateway does about a response.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Handling {
    /// The provider is at fault until someone acts: long cooldown.
    Hard,
    /// A rate limit or a lack of capacity: wait it out.
    Limit,
    /// This model is refused here; the provider's others are not affected.
    Model,
    /// The request itself is refused: answered to the client, not retried.
    Client,
    /// A fault of the service: retried and counted towards the circuit.
    Fault,
    /// Served to the client as a success.
    Ok,
}

fn read(sample: &Sample) -> (Details, Option<Terminal>) {
    let mut headers = hyper::HeaderMap::new();
    for (name, value) in &sample.headers {
        headers.insert(
            hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
    let mut protocol = Protocol::new(sample.stream);
    protocol.response(sample.status, sample.stream, "identity");
    protocol.response_headers(&headers);
    protocol.feed(sample.body.as_bytes());
    if sample.status < 400 {
        protocol.finish(Some(sample.status), crate::errors::code::OK);
        return (protocol.observation.error.clone(), protocol.terminal());
    }
    protocol.finish(Some(sample.status), crate::errors::code::HTTP);
    // A rejection is settled on what the protocol captured, as in forwarding.
    let details = if protocol.observation.error.raw.is_some() {
        protocol.observation.error.clone()
    } else {
        policy::test_support::http(sample.status, &headers, sample.body.as_bytes()).0
    };
    (details, None)
}

fn handling(status: u16, details: &Details, terminal: Option<Terminal>) -> Handling {
    let decision = policy::decision(Some(status), details);
    if details.error_basis == Some(ErrorBasis::ModelUnavailable)
        || (decision.hard && details.health_scope == Some(HealthScope::Model))
    {
        Handling::Model
    } else if decision.hard {
        Handling::Hard
    } else if decision.temporary {
        Handling::Limit
    } else if decision.retryable {
        Handling::Fault
    } else if details.error_basis.is_none()
        && matches!(terminal, Some(Terminal::Success | Terminal::Limited))
    {
        Handling::Ok
    } else {
        Handling::Client
    }
}

#[test]
fn every_upstream_error_is_read_and_handled_as_ruled() {
    let mut ids = std::collections::HashSet::new();
    let mut wrong = vec![];
    for line in CORPUS.lines() {
        let sample: Sample = serde_json::from_str(line).unwrap();
        assert!(ids.insert(sample.id.clone()), "{} twice", sample.id);
        assert!(sample.gap.as_deref() != Some(""), "{}", sample.id);
        let (details, terminal) = read(&sample);
        let got = (
            details.error_basis,
            handling(sample.status, &details, terminal),
        );
        if got != (sample.basis, sample.handling) {
            wrong.push(format!("{}\t{:?}\t{:?}", sample.id, got.0, got.1));
        }
    }
    assert!(ids.len() > 700, "{}", ids.len());
    assert!(wrong.is_empty(), "\n{}", wrong.join("\n"));
}
