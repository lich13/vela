//! Pieces shared by the modules that download a whole response into memory.
use std::time::Duration;

/// Client for the public catalogs: direct, no redirects, 30 seconds in all.
pub fn catalog_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .user_agent(concat!("vela/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
}

/// Reads the whole body, refusing one longer than `limit` bytes. The error is
/// the caller's wording for a body that broke off or for one that is too long.
pub async fn body(
    response: &mut reqwest::Response,
    limit: u64,
    interrupted: &'static str,
    too_large: &'static str,
) -> Result<Vec<u8>, &'static str> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| interrupted)? {
        if bytes.len() as u64 + chunk.len() as u64 > limit {
            return Err(too_large);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
