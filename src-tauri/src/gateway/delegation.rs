//! Requests a client makes beside its conversation, and the queues of
//! providers set to take them from the ordinary one. There are four queues:
//! the ordinary one, and those of the providers that take compactions, that
//! take web searches and that take auxiliary requests, each in list order.
//! A kind that has a queue is served by that queue alone: its requests go
//! from one provider of it to the next and never to the ordinary queue.
use super::{
    model::Delegated,
    replay::{RequestHints, SearchTools},
    routing::Requirement,
    ClientId, Route,
};
use crate::usage::model::Operation;

/// What Codex says a thread is for. These threads are the client's own
/// errands: reviewing a command before it runs, writing memories, naming or
/// recapping a thread, suggesting what to ask next. A thread the user or an
/// agent works in is not on the list, and neither is a purpose not seen yet.
const CODEX_THREADS: &[&str] = &[
    "guardian_review",
    "memory_consolidation",
    "thread_title",
    "system",
    "ambient_suggestions",
];
/// The same errands, named by the agent that runs them.
const CODEX_AGENTS: &[&str] = &["guardian", "memory_consolidation"];

#[derive(serde::Deserialize)]
struct TurnMetadata {
    #[serde(default)]
    thread_source: Option<String>,
    #[serde(default)]
    request_kind: Option<String>,
}

fn codex_auxiliary(headers: &hyper::HeaderMap) -> bool {
    headers
        .get_all("x-openai-subagent")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|agent| CODEX_AGENTS.contains(&agent.trim()))
        || headers
            .get_all("x-codex-turn-metadata")
            .iter()
            .filter(|value| value.len() <= 64 * 1024)
            .filter_map(|value| serde_json::from_slice::<TurnMetadata>(value.as_bytes()).ok())
            .any(|metadata| {
                metadata.request_kind.as_deref() == Some("memory")
                    || metadata
                        .thread_source
                        .as_deref()
                        .is_some_and(|source| CODEX_THREADS.contains(&source))
            })
}

/// The kind of request this is, when it is one a provider can take over.
/// Codex and Claude Code name their compactions and errands themselves: Codex
/// in its thread metadata, Claude Code in the request class it sends while
/// the gateway manages its settings. A web search is told by the tools the
/// request declares, which `hints` has from the one reading of the body:
/// Claude Code's search tool sends a message that declares a `web_search_…`
/// tool, Grok Build's a response whose only tool is `web_search`. A turn of
/// Grok's that declares that tool beside its others is the conversation.
/// Codex searches on a path of its own.
///
/// A request of two kinds is the one that comes first: a compaction, then a
/// search, then an errand. A search is one only while `searches_taken`, a
/// provider being set to take searches: Claude Code classes its searches as
/// errands, and with no such provider they stay the errands they were.
pub(super) fn class(
    client: ClientId,
    parts: &hyper::http::request::Parts,
    operation: Operation,
    hints: Option<&RequestHints>,
    searches_taken: bool,
) -> Option<Delegated> {
    if parts.method != hyper::Method::POST {
        return None;
    }
    if operation == Operation::Compaction {
        return matches!(client, ClientId::Codex | ClientId::Claude)
            .then_some(Delegated::Compaction);
    }
    let path = parts.uri.path().trim_end_matches('/');
    let tools = hints.map(|hints| hints.search_tools).unwrap_or_default();
    let (search, auxiliary) = match client {
        ClientId::Codex => (
            operation == Operation::WebSearch,
            matches!(path, "/responses" | "/v1/responses") && codex_auxiliary(&parts.headers),
        ),
        ClientId::Claude if path == "/v1/messages" => (
            tools != SearchTools::Absent,
            parts
                .headers
                .get("x-claude-code-request-class")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|class| class.trim().eq_ignore_ascii_case("auxiliary")),
        ),
        ClientId::Grok => (
            matches!(path, "/responses" | "/v1/responses") && tools == SearchTools::Alone,
            false,
        ),
        _ => (false, false),
    };
    if search && searches_taken {
        Some(Delegated::WebSearch)
    } else {
        auxiliary.then_some(Delegated::Auxiliary)
    }
}

/// The provider a request is handed to, and what it runs the request on.
#[derive(Debug, PartialEq)]
pub(super) struct Taker {
    pub id: String,
    /// The model the request is routed as there: the one the provider names,
    /// or else the client's.
    pub model: Option<String>,
    /// The reasoning effort the provider names. It replaces the client's,
    /// which is the conversation's and not this request's to inherit.
    pub effort: Option<String>,
}

/// The provider `id` as a taker of `kind` and its health there, when its
/// whitelist allows the model the request is routed as there. A `resource`
/// request, such as a Codex search, needs no model unless the provider names
/// one.
fn fit(
    id: &String,
    routes: &std::collections::HashMap<String, Route>,
    kind: Delegated,
    model: Option<&str>,
    resource: bool,
) -> Option<(Taker, super::Health)> {
    let route = routes.get(id)?;
    let (named, effort) = route.delegate.takes(kind)?;
    let routed = named.or(model);
    let requirement = if resource && named.is_none() {
        Requirement::Resource
    } else {
        Requirement::model(routed)
    };
    requirement
        .allows(route.provider.allowed_models.as_deref())
        .then(|| {
            (
                Taker {
                    id: id.clone(),
                    model: routed.map(str::to_owned),
                    effort: effort.map(str::to_owned),
                },
                route.health_for(&requirement),
            )
        })
}

/// Picks the provider next in the queue for requests of `kind` among `order`,
/// the providers the request may still be handed to in list order. A taker
/// must be able to serve now: on the model the request is routed as there,
/// its whitelist allows it and it is not cooling. The conversation's own
/// provider comes first when it is one, so that a compaction is read back
/// where it was written.
pub(super) fn taker(
    order: &[String],
    routes: &std::collections::HashMap<String, Route>,
    kind: Delegated,
    model: Option<&str>,
    owner: Option<&str>,
    resource: bool,
) -> Option<Taker> {
    order
        .iter()
        .filter(|id| Some(id.as_str()) == owner)
        .chain(order.iter())
        .filter_map(|id| fit(id, routes, kind, model, resource))
        .find_map(|(taker, health)| health.available.then_some(taker))
}

/// The provider of the queue for `kind` among `order` that is cooling and
/// recovers first: the one a request waits for when none can serve it now.
pub(super) fn recovering(
    order: &[String],
    routes: &std::collections::HashMap<String, Route>,
    kind: Delegated,
    model: Option<&str>,
    resource: bool,
) -> Option<Taker> {
    order
        .iter()
        .filter_map(|id| fit(id, routes, kind, model, resource))
        .filter(|(_, health)| !health.available)
        .min_by_key(|(_, health)| health.retry_in)
        .map(|(taker, _)| taker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::{
        circuit::Outcome,
        model::{Delegate, Settings},
        test_support::{policy_provider, route},
    };
    use std::collections::HashMap;

    /// A route to provider `id` that takes requests over on `compaction` and
    /// `auxiliary`: `None` takes none, an empty model the client's own.
    fn taking(id: &str, compaction: Option<&str>, auxiliary: Option<&str>) -> Route {
        let model = |model: Option<&str>| model.filter(|m| !m.is_empty()).map(str::to_owned);
        Route {
            delegate: Delegate {
                compaction: compaction.is_some(),
                compaction_model: model(compaction),
                auxiliary: auxiliary.is_some(),
                auxiliary_model: model(auxiliary),
                ..Default::default()
            },
            ..route(policy_provider(id, false, false))
        }
    }
    fn among(routes: impl IntoIterator<Item = Route>) -> (Vec<String>, HashMap<String, Route>) {
        let routes: Vec<_> = routes.into_iter().collect();
        (
            routes.iter().map(|r| r.provider.id.clone()).collect(),
            routes
                .into_iter()
                .map(|r| (r.provider.id.clone(), r))
                .collect(),
        )
    }
    /// The taker of a request that needs a model.
    fn taker(
        order: &[String],
        routes: &HashMap<String, Route>,
        kind: Delegated,
        model: Option<&str>,
        owner: Option<&str>,
    ) -> Option<Taker> {
        super::taker(order, routes, kind, model, owner, false)
    }
    fn pick(id: &str, model: Option<&str>) -> Option<Taker> {
        Some(Taker {
            id: id.to_owned(),
            model: model.map(str::to_owned),
            effort: None,
        })
    }

    #[test]
    fn the_first_provider_set_to_take_the_kind_takes_it_on_its_model() {
        let (order, routes) = among([
            taking("first", None, None),
            taking("compacts", Some("compact-fixture"), None),
            taking("assists", None, Some("")),
            taking("both", Some(""), Some("small-fixture")),
        ]);
        let asked = Some("asked-fixture");
        assert_eq!(
            taker(&order, &routes, Delegated::Compaction, asked, None),
            pick("compacts", Some("compact-fixture"))
        );
        // Without a model of its own the taker runs what the client asked for.
        assert_eq!(
            taker(&order, &routes, Delegated::Auxiliary, asked, None),
            pick("assists", asked)
        );
        assert_eq!(
            taker(&order, &routes, Delegated::Auxiliary, None, None),
            pick("assists", None)
        );
        // The conversation's own provider comes first when it takes the kind.
        assert_eq!(
            taker(&order, &routes, Delegated::Compaction, asked, Some("both")),
            pick("both", asked)
        );
        assert_eq!(
            taker(&order, &routes, Delegated::Compaction, asked, Some("first")),
            pick("compacts", Some("compact-fixture")),
            "an owner that takes nothing over changes nothing"
        );
        // Only the providers the request may consider are asked.
        let considered = ["first".to_owned(), "both".to_owned()];
        assert_eq!(
            taker(&considered, &routes, Delegated::Compaction, asked, None),
            pick("both", asked)
        );
        assert_eq!(
            taker(&order[..1], &routes, Delegated::Auxiliary, asked, None),
            None
        );
    }

    #[test]
    fn a_taker_names_the_effort_of_each_kind_apart_from_its_model() {
        let mut route = taking("sets", Some(""), Some("small-fixture"));
        route.delegate.compaction_effort = Some("high".into());
        route.delegate.auxiliary_effort = Some("low".into());
        route.delegate.web_search = true;
        route.delegate.web_search_model = Some("search-fixture".into());
        route.delegate.web_search_effort = Some("medium".into());
        let (order, routes) = among([route, taking("follows", Some(""), Some(""))]);
        let asked = Some("asked-fixture");
        let effort = |order: &[String], kind| {
            let taker = taker(order, &routes, kind, asked, None).unwrap();
            (taker.id, taker.model, taker.effort)
        };
        assert_eq!(
            effort(&order, Delegated::Compaction),
            (
                "sets".into(),
                Some("asked-fixture".into()),
                Some("high".into())
            )
        );
        assert_eq!(
            effort(&order, Delegated::Auxiliary),
            (
                "sets".into(),
                Some("small-fixture".into()),
                Some("low".into())
            )
        );
        // Without one the client's effort is left as it is.
        assert_eq!(
            effort(&order[1..], Delegated::Auxiliary),
            ("follows".into(), Some("asked-fixture".into()), None)
        );
        // Searches are a kind of their own, taken only where they are set.
        assert_eq!(
            effort(&order, Delegated::WebSearch),
            (
                "sets".into(),
                Some("search-fixture".into()),
                Some("medium".into())
            )
        );
        assert_eq!(
            taker(&order[1..], &routes, Delegated::WebSearch, asked, None),
            None
        );
    }

    #[test]
    fn a_taker_must_be_able_to_serve_the_model_it_would_run() {
        let listed = |mut route: Route, models: &[&str]| {
            route.provider.allowed_models = Some(models.iter().map(|m| (*m).to_owned()).collect());
            route
        };
        let asked = Some("asked-fixture");
        // The whitelist is read for the model the request is routed as.
        let (order, routes) = among([
            listed(
                taking("names-other", None, Some("small-fixture")),
                &["asked-fixture"],
            ),
            listed(taking("follows", None, Some("")), &["small-fixture"]),
            listed(
                taking("serves", None, Some("small-fixture")),
                &["small-fixture"],
            ),
        ]);
        assert_eq!(
            taker(&order, &routes, Delegated::Auxiliary, asked, None),
            pick("serves", Some("small-fixture"))
        );
        assert_eq!(
            taker(
                &order,
                &routes,
                Delegated::Auxiliary,
                Some("small-fixture"),
                None
            ),
            pick("follows", Some("small-fixture"))
        );

        // A request for a resource, such as a Codex search, is no model's:
        // a whitelist turns it away only when the provider names a model.
        let searching = |id: &str, model: Option<&str>| {
            let mut route = taking(id, None, None);
            route.delegate.web_search = true;
            route.delegate.web_search_model = model.map(str::to_owned);
            listed(route, &["small-fixture"])
        };
        let (order, routes) = among([
            searching("names-other", Some("other-fixture")),
            searching("follows", None),
        ]);
        assert_eq!(
            super::taker(&order, &routes, Delegated::WebSearch, asked, None, true),
            pick("follows", asked)
        );
        assert_eq!(
            taker(&order, &routes, Delegated::WebSearch, asked, None),
            None
        );

        // Neither does one that is cooling down.
        let (order, routes) = among([
            taking("cooling", Some(""), None),
            taking("ready", Some(""), None),
        ]);
        routes["cooling"]
            .provider_circuit
            .acquire()
            .unwrap()
            .finish(Outcome::HardRejected(None), &Settings::default());
        assert_eq!(
            taker(
                &order,
                &routes,
                Delegated::Compaction,
                asked,
                Some("cooling")
            ),
            pick("ready", asked)
        );
        // It is the one a request waits for when none is ready.
        assert_eq!(
            recovering(&order, &routes, Delegated::Compaction, asked, false),
            pick("cooling", asked)
        );
        assert_eq!(
            recovering(&order, &routes, Delegated::Auxiliary, asked, false),
            None
        );
    }

    fn parts(path: &str, headers: &[(&'static str, &str)]) -> hyper::http::request::Parts {
        let mut request = hyper::Request::builder().method("POST").uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.body(()).unwrap().into_parts().0
    }

    #[test]
    fn claude_names_auxiliary_requests_in_its_request_class() {
        let class = |value| {
            class(
                ClientId::Claude,
                &parts("/v1/messages", &[("x-claude-code-request-class", value)]),
                Operation::Model,
                None,
                true,
            )
        };
        assert_eq!(class("auxiliary"), Some(Delegated::Auxiliary));
        for other in [
            "main",
            "subagent",
            "workflow",
            "side_reply",
            "compaction",
            "",
        ] {
            assert_eq!(class(other), None, "{other}");
        }
        assert_eq!(
            super::class(
                ClientId::Claude,
                &parts("/v1/messages", &[]),
                Operation::Model,
                None,
                true
            ),
            None
        );
        // Counting tokens is not a model turn, whatever class it carries.
        assert_eq!(
            super::class(
                ClientId::Claude,
                &parts(
                    "/v1/messages/count_tokens",
                    &[("x-claude-code-request-class", "auxiliary")]
                ),
                Operation::Model,
                None,
                true
            ),
            None
        );
    }

    #[test]
    fn codex_names_auxiliary_threads_in_its_metadata() {
        let class = |headers: &[(&'static str, &str)]| {
            class(
                ClientId::Codex,
                &parts("/v1/responses", headers),
                Operation::Model,
                None,
                true,
            )
        };
        assert_eq!(
            class(&[("x-openai-subagent", "guardian")]),
            Some(Delegated::Auxiliary)
        );
        for source in CODEX_THREADS {
            let metadata = format!(r#"{{"thread_source":"{source}","request_kind":"turn"}}"#);
            assert_eq!(
                class(&[("x-codex-turn-metadata", &metadata)]),
                Some(Delegated::Auxiliary),
                "{source}"
            );
        }
        assert_eq!(
            class(&[("x-codex-turn-metadata", r#"{"request_kind":"memory"}"#)]),
            Some(Delegated::Auxiliary)
        );
        // The user's turns, spawned agents, reviews and anything unknown stay.
        for headers in [
            &[][..],
            &[("x-openai-subagent", "collab_spawn")],
            &[("x-openai-subagent", "review")],
            &[(
                "x-codex-turn-metadata",
                r#"{"thread_source":"user","request_kind":"turn"}"#,
            )],
            &[(
                "x-codex-turn-metadata",
                r#"{"thread_source":"subagent","request_kind":"prewarm"}"#,
            )],
            &[("x-codex-turn-metadata", r#"{"thread_source":"automation"}"#)],
            &[("x-codex-turn-metadata", "not json")],
        ] {
            assert_eq!(class(headers), None, "{headers:?}");
        }
    }

    #[test]
    fn compaction_is_delegated_for_codex_and_claude_only() {
        let request = parts("/v1/responses", &[("x-openai-subagent", "guardian")]);
        for client in [ClientId::Codex, ClientId::Claude] {
            assert_eq!(
                class(client, &request, Operation::Compaction, None, true),
                Some(Delegated::Compaction)
            );
        }
        for client in [ClientId::Grok, ClientId::Dsh] {
            assert_eq!(
                class(client, &request, Operation::Compaction, None, true),
                None
            );
            assert_eq!(class(client, &request, Operation::Model, None, true), None);
        }
    }

    /// What the gateway keeps of a request body, read the way a request is.
    fn hints(body: serde_json::Value) -> RequestHints {
        let mut projector = crate::gateway::metadata::Projector::default();
        projector.feed(body.to_string().as_bytes());
        RequestHints::from_value(projector.finish().unwrap(), true).unwrap()
    }
    const ERRAND: (&str, &str) = ("x-claude-code-request-class", "auxiliary");

    #[test]
    fn a_claude_search_is_a_message_that_declares_a_web_search_tool() {
        use serde_json::json;
        let message = |tools: serde_json::Value| {
            hints(json!({
                "model": "claude-fixture", "max_tokens": 32, "tools": tools,
                "messages": [{"role": "user", "content": "fixture"}]
            }))
        };
        // What Claude Code's search tool sends, and what a turn of the
        // conversation declares: tools of the client's own, the one named
        // WebSearch among them.
        let search = message(json!([
            {"type": "web_search_20250305", "name": "web_search", "max_uses": 8}
        ]));
        let turn = message(json!([
            {"name": "WebSearch", "description": "fixture", "input_schema": {"type": "object"}},
            {"type": "custom", "name": "fixture", "input_schema": {"type": "object"}}
        ]));
        let class = |headers: &[(&'static str, &str)], hints: &RequestHints, taken| {
            class(
                ClientId::Claude,
                &parts("/v1/messages", headers),
                Operation::Model,
                Some(hints),
                taken,
            )
        };
        assert_eq!(class(&[], &search, true), Some(Delegated::WebSearch));
        assert_eq!(class(&[], &turn, true), None);
        // Claude Code classes its searches as errands. One is a search first,
        // and the errand it was while no provider takes searches.
        assert_eq!(class(&[ERRAND], &search, true), Some(Delegated::WebSearch));
        assert_eq!(class(&[ERRAND], &search, false), Some(Delegated::Auxiliary));
        assert_eq!(class(&[], &search, false), None);
        assert_eq!(class(&[ERRAND], &turn, true), Some(Delegated::Auxiliary));
        // A compaction comes before either.
        assert_eq!(
            super::class(
                ClientId::Claude,
                &parts("/v1/messages", &[ERRAND]),
                Operation::Compaction,
                Some(&search),
                true
            ),
            Some(Delegated::Compaction)
        );
        // The tool is found wherever it stands among the others.
        let mut tools = vec![json!({"name": "fixture", "input_schema": {"type": "object"}}); 100];
        tools.push(json!({"type": "web_search_20260209", "name": "web_search"}));
        assert_eq!(
            class(&[], &message(tools.into()), true),
            Some(Delegated::WebSearch)
        );
        // Counting the tokens of a search is not searching.
        assert_eq!(
            super::class(
                ClientId::Claude,
                &parts("/v1/messages/count_tokens", &[]),
                Operation::Model,
                Some(&search),
                true
            ),
            None
        );
    }

    #[test]
    fn a_grok_search_is_a_response_whose_only_tool_is_web_search() {
        use serde_json::json;
        // What Grok Build's search tool sends.
        let search = hints(json!({
            "model": "fixture-search", "input": "fixture query", "store": false,
            "max_output_tokens": 1024, "temperature": 0.2, "top_p": 0.9,
            "tools": [{"type": "web_search", "filters": {"allowed_domains": ["example.invalid"]}}]
        }));
        let turn = |tools: serde_json::Value| {
            hints(json!({
                "model": "fixture-model", "stream": true, "tools": tools,
                "input": [{"role": "user", "content": "fixture"}]
            }))
        };
        let class = |path: &str, hints: &RequestHints, taken| {
            class(
                ClientId::Grok,
                &parts(path, &[]),
                Operation::Model,
                Some(hints),
                taken,
            )
        };
        for path in ["/v1/responses", "/responses"] {
            assert_eq!(class(path, &search, true), Some(Delegated::WebSearch));
        }
        assert_eq!(class("/v1/responses", &search, false), None);
        // A model that searches for itself has the tool beside the others of
        // its turn, which is the conversation and stays where it is.
        for tools in [
            json!([{"type": "function", "name": "fixture"}, {"type": "web_search"}, {"type": "x_search"}]),
            json!([{"type": "web_search"}, {"type": "x_search"}]),
            json!([{"type": "function", "name": "web_search"}]),
            json!([]),
        ] {
            assert_eq!(
                class("/v1/responses", &turn(tools.clone()), true),
                None,
                "{tools}"
            );
        }
        assert_eq!(class("/v1/chat/completions", &search, true), None);
        // A body that could not be read tells nothing.
        assert_eq!(
            super::class(
                ClientId::Grok,
                &parts("/v1/responses", &[]),
                Operation::Model,
                None,
                true
            ),
            None
        );
    }

    #[test]
    fn a_codex_search_is_a_request_on_its_search_path() {
        use serde_json::json;
        // Codex declares the tool in its turns and searches on a path of its
        // own: a turn that declares the tool is the conversation.
        let declared = hints(json!({
            "model": "gpt-fixture", "tools": [{"type": "web_search"}],
            "input": [{"role": "user", "content": "fixture"}]
        }));
        let class = |path: &str, headers: &[(&'static str, &str)], operation| {
            class(
                ClientId::Codex,
                &parts(path, headers),
                operation,
                Some(&declared),
                true,
            )
        };
        assert_eq!(class("/v1/responses", &[], Operation::Model), None);
        assert_eq!(
            class(
                "/v1/responses",
                &[("x-openai-subagent", "guardian")],
                Operation::Model
            ),
            Some(Delegated::Auxiliary)
        );
        assert_eq!(
            class("/v1/alpha/search", &[], Operation::WebSearch),
            Some(Delegated::WebSearch)
        );
        // With no provider set to take searches it is the ordinary queue's.
        assert_eq!(
            super::class(
                ClientId::Codex,
                &parts("/v1/alpha/search", &[]),
                Operation::WebSearch,
                Some(&declared),
                false,
            ),
            None
        );
    }
}
