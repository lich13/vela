//! Gemini wire protocol. Requests and responses remain native: only the
//! address, the credential and what one kind of upstream refuses change.
use hyper::Uri;

const VERSIONS: [&str; 3] = ["/v1beta", "/v1alpha", "/v1"];

pub fn path(path: &str) -> &str {
    for version in VERSIONS {
        if let Some(rest) = path.strip_prefix(version).filter(|s| s.starts_with('/')) {
            return rest;
        }
    }
    path
}
pub fn model(pathname: &str) -> Option<String> {
    let (name, operation) = path(pathname).strip_prefix("/models/")?.rsplit_once(':')?;
    if !matches!(
        operation,
        "generateContent" | "streamGenerateContent" | "countTokens"
    ) {
        return None;
    }
    valid_model(name).then(|| name.to_owned())
}
pub fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 256
        && model
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}
pub fn accepts(pathname: &str) -> bool {
    path(pathname) == "/models"
        || model(pathname).is_some()
        || path(pathname)
            .strip_prefix("/models/")
            .is_some_and(valid_model)
}
/// The model list itself, which the gateway answers for its client.
pub fn listing(pathname: &str) -> bool {
    path(pathname) == "/models"
}
pub fn streams(pathname: &str) -> bool {
    pathname.ends_with(":streamGenerateContent")
}
pub fn counts(pathname: &str) -> bool {
    pathname.ends_with(":countTokens")
}
/// Antigravity's background metric calls, `POST /v1beta:<method>`. A gateway
/// is asked to answer them, not to pass them on.
pub fn telemetry(pathname: &str) -> bool {
    VERSIONS.iter().any(|version| {
        pathname
            .strip_prefix(version)
            .is_some_and(|rest| rest.starts_with(':'))
    })
}
/// A Vertex AI address names the publisher, and its models hang directly
/// under it: `…/v1/publishers/google`, with or without a project and location
/// before the publisher.
pub fn vertex(base: &str) -> bool {
    url::Url::parse(base).is_ok_and(|url| {
        url.path()
            .trim_end_matches('/')
            .ends_with("/publishers/google")
    })
}
pub fn target(base: &str, incoming: &Uri) -> Result<Uri, Box<dyn std::error::Error + Send + Sync>> {
    if !accepts(incoming.path()) {
        return Err("不支持的 Gemini 接口".into());
    }
    let mut url = url::Url::parse(base)?;
    let base_path = url.path().trim_end_matches('/').to_owned();
    let rest = path(incoming.path());
    if vertex(base) {
        // The address carries Vertex AI's own API version.
        url.set_path(&format!("{base_path}{rest}"));
    } else {
        let version = VERSIONS
            .into_iter()
            .find(|v| incoming.path().starts_with(&format!("{v}/")))
            .unwrap_or("/v1beta");
        let prefix = VERSIONS
            .into_iter()
            .find_map(|v| base_path.strip_suffix(v))
            .unwrap_or(&base_path);
        url.set_path(&format!("{prefix}{version}{rest}"));
    }
    // A downstream key must never survive upstream authentication.
    let query: Vec<_> = url::form_urlencoded::parse(incoming.query().unwrap_or("").as_bytes())
        .filter(|(k, _)| k != "key" && k != "api_key")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.set_query(None);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    Ok(url.as_str().parse()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sent(base: &str, incoming: &str) -> String {
        target(base, &incoming.parse().unwrap())
            .unwrap()
            .to_string()
    }
    #[test]
    fn native_paths_keep_prefix_and_strip_downstream_credentials() {
        let incoming =
            "/v1beta/models/gemini-fixture:streamGenerateContent?alt=sse&key=fixture-local";
        assert_eq!(
            sent("https://example.invalid/api/v1beta", incoming),
            "https://example.invalid/api/v1beta/models/gemini-fixture:streamGenerateContent?alt=sse"
        );
        // A saved address ends in the `/v1` every provider is given.
        assert_eq!(
            sent("https://example.invalid/v1", "/v1beta/models?pageSize=1000"),
            "https://example.invalid/v1beta/models?pageSize=1000"
        );
        assert_eq!(
            model(incoming.split('?').next().unwrap()).as_deref(),
            Some("gemini-fixture")
        );
        assert!(!accepts("/v1beta/models/x:delete"));
        assert!(!valid_model("../escape"));
    }
    #[test]
    fn vertex_addresses_keep_their_version_and_publisher() {
        let incoming = "/v1beta/models/gemini-fixture:streamGenerateContent?alt=sse";
        for base in [
            "https://example.invalid/v1/publishers/google",
            "https://example.invalid/v1/projects/fixture/locations/global/publishers/google/",
        ] {
            assert!(vertex(base));
            assert_eq!(
                sent(base, incoming),
                format!(
                    "{}/models/gemini-fixture:streamGenerateContent?alt=sse",
                    base.trim_end_matches('/')
                )
            );
        }
        assert!(!vertex("https://example.invalid/v1beta"));
        assert!(!vertex("https://example.invalid/v1/publishers/anthropic"));
    }
    #[test]
    fn metric_calls_are_told_from_model_calls() {
        assert!(telemetry("/v1beta:recordMetrics"));
        assert!(telemetry("/v1:any"));
        assert!(!telemetry("/v1beta/models/gemini-fixture:generateContent"));
        assert!(!telemetry("/v1beta"));
        assert!(listing("/v1beta/models") && !listing("/v1beta/models/gemini-fixture"));
        assert!(streams(
            "/v1beta/models/gemini-fixture:streamGenerateContent"
        ));
        assert!(counts("/v1beta/models/gemini-fixture:countTokens"));
    }
}
