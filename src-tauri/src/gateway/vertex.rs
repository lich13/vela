//! Claude on Vertex AI. The Messages API stays whole; Google's address names
//! the model and the method, so both leave the request's own text for it.
use hyper::header::{self, HeaderMap, HeaderValue};
use serde_json::{Map, Value};

/// The body version Vertex AI asks for where Anthropic takes a header.
const VERSION: &str = "vertex-2023-10-16";

/// A Vertex AI address for Anthropic's models ends at the publisher:
/// `…/v1/projects/<project>/locations/<location>/publishers/anthropic`.
pub fn anthropic(base: &str) -> bool {
    url::Url::parse(base).is_ok_and(|url| {
        url.path()
            .trim_end_matches('/')
            .ends_with("/publishers/anthropic")
    })
}

/// Vertex AI writes the snapshot date of a model after an `@`:
/// `claude-haiku-4-5-20251001` is `claude-haiku-4-5@20251001` there. Newer
/// models carry no date and keep their name.
pub fn model(id: &str) -> String {
    match id.rsplit_once('-') {
        Some((name, date))
            if !name.is_empty() && date.len() == 8 && date.bytes().all(|c| c.is_ascii_digit()) =>
        {
            format!("{name}@{date}")
        }
        _ => id.to_owned(),
    }
}

/// Vertex AI has Claude's Messages API and nothing beside it.
pub fn accepts(path: &str) -> bool {
    matches!(
        path.trim_end_matches('/'),
        "/v1/messages" | "/v1/messages/count_tokens"
    )
}

fn addressable(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 256
        && model
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.@".contains(&c))
}

/// Turns a Messages request into Vertex AI's and returns where it goes.
/// `None` for a path Vertex AI has no method for, or a model no address can
/// name; `body` is then not to be sent.
pub fn request(base: &str, path: &str, body: &mut Map<String, Value>) -> Option<String> {
    let method = match path.trim_end_matches('/') {
        "/v1/messages" => {
            let model = model(body.remove("model")?.as_str()?);
            if !addressable(&model) {
                return None;
            }
            let stream = body.get("stream").and_then(Value::as_bool) == Some(true);
            format!(
                "{model}:{}",
                if stream {
                    "streamRawPredict"
                } else {
                    "rawPredict"
                }
            )
        }
        "/v1/messages/count_tokens" => {
            // Counting has one address for every model, which the body names.
            let named = body.get_mut("model")?;
            *named = model(named.as_str()?).into();
            "count-tokens:rawPredict".to_owned()
        }
        _ => return None,
    };
    body.entry("anthropic_version")
        .or_insert_with(|| VERSION.into());
    Some(format!("{}/models/{method}", base.trim_end_matches('/')))
}

/// Google takes a key in a header of its own, never as a bearer token. The
/// version travels in the body, and of several `anthropic-beta` headers
/// Vertex AI reads one: they are joined into it.
pub fn headers(headers: &mut HeaderMap, key: &str) -> Option<()> {
    headers.remove(header::AUTHORIZATION);
    headers.remove("x-api-key");
    headers.remove("anthropic-version");
    headers.insert("x-goog-api-key", HeaderValue::from_str(key).ok()?);
    let betas: Vec<&str> = headers
        .get_all("anthropic-beta")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    if betas.len() > 1 {
        let joined = HeaderValue::from_str(&betas.join(",")).ok()?;
        headers.insert("anthropic-beta", joined);
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BASE: &str =
        "https://example.invalid/v1/projects/fixture/locations/global/publishers/anthropic";

    fn sent(path: &str, body: Value) -> Option<(String, Value)> {
        let mut body = body;
        let address = request(BASE, path, body.as_object_mut().unwrap())?;
        Some((address, body))
    }

    #[test]
    fn only_an_anthropic_publisher_address_is_claude_on_vertex() {
        assert!(anthropic(BASE));
        assert!(anthropic(&format!("{BASE}/")));
        assert!(anthropic("https://example.invalid/v1/publishers/anthropic"));
        assert!(!anthropic("https://example.invalid/v1/publishers/google"));
        assert!(!anthropic("https://example.invalid/v1"));
        assert!(!anthropic("publishers/anthropic"));
    }

    #[test]
    fn the_model_and_the_method_move_into_the_address() {
        let (address, body) = sent(
            "/v1/messages",
            json!({"model":"claude-fixture-5","max_tokens":8,"stream":true,"messages":[]}),
        )
        .unwrap();
        assert_eq!(
            address,
            format!("{BASE}/models/claude-fixture-5:streamRawPredict")
        );
        assert_eq!(
            body,
            json!({"max_tokens":8,"stream":true,"messages":[],"anthropic_version":"vertex-2023-10-16"})
        );
        // Not streamed, and with a version of the client's own choosing.
        let (address, body) = sent(
            "/v1/messages/",
            json!({"model":"claude-fixture-4-5-20251001","anthropic_version":"2026-10-21","messages":[]}),
        )
        .unwrap();
        assert_eq!(
            address,
            format!("{BASE}/models/claude-fixture-4-5@20251001:rawPredict")
        );
        assert_eq!(
            body,
            json!({"anthropic_version":"2026-10-21","messages":[]})
        );
    }

    #[test]
    fn counting_keeps_the_model_in_the_body() {
        let (address, body) = sent(
            "/v1/messages/count_tokens",
            json!({"model":"claude-fixture-4-5-20251001","messages":[]}),
        )
        .unwrap();
        assert_eq!(address, format!("{BASE}/models/count-tokens:rawPredict"));
        assert_eq!(
            body,
            json!({"model":"claude-fixture-4-5@20251001","messages":[],"anthropic_version":"vertex-2023-10-16"})
        );
    }

    #[test]
    fn what_vertex_cannot_address_is_not_sent() {
        assert!(sent("/v1/models", json!({"model":"claude-fixture"})).is_none());
        assert!(sent("/v1/messages", json!({"messages":[]})).is_none());
        assert!(sent("/v1/messages", json!({"model":7})).is_none());
        for model in ["", "a/b", "a:b", "a?b", "a b"] {
            assert!(
                sent("/v1/messages", json!({ "model": model })).is_none(),
                "{model:?}"
            );
        }
        assert_eq!(model("claude-fixture-5-5"), "claude-fixture-5-5");
        assert_eq!(model("claude-fixture-2025"), "claude-fixture-2025");
        assert_eq!(model("-20251001"), "-20251001");
    }

    #[test]
    fn the_key_replaces_every_other_credential_and_betas_become_one_header() {
        let mut sent = HeaderMap::new();
        sent.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer fixture"),
        );
        sent.insert("x-api-key", HeaderValue::from_static("fixture"));
        sent.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        headers(&mut sent, "fixture-key").unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent["x-goog-api-key"], "fixture-key");
        sent.append(
            "anthropic-beta",
            HeaderValue::from_static("fixture-a,fixture-b"),
        );
        headers(&mut sent, "fixture-key").unwrap();
        assert_eq!(sent["anthropic-beta"], "fixture-a,fixture-b");
        sent.append("anthropic-beta", HeaderValue::from_static("fixture-c"));
        headers(&mut sent, "fixture-key").unwrap();
        assert_eq!(
            sent.get_all("anthropic-beta").iter().collect::<Vec<_>>(),
            ["fixture-a,fixture-b,fixture-c"]
        );
        assert!(headers(&mut sent, "line\nbreak").is_none());
        assert!(accepts("/v1/messages/") && accepts("/v1/messages/count_tokens"));
        assert!(!accepts("/v1/models") && !accepts("/v1/messages/batches"));
    }
}
