//! OpenAI's own Codex model catalog, kept beside the gateway store and
//! fetched at most once a day while it is the chosen source of the picker.
use crate::{errors::code, storage, AppError, Result};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) const SOURCE: &str =
    "https://raw.githubusercontent.com/openai/codex/refs/heads/main/codex-rs/models-manager/models.json";
const LIMIT: usize = 8 * 1024 * 1024;
const FRESH: Duration = Duration::from_secs(24 * 60 * 60);

fn failure(message: &str) -> AppError {
    AppError::new(code::MODELS, message)
}
pub(super) fn path(data: &Path) -> PathBuf {
    data.join("codex-models.json")
}
/// The catalog downloaded last, exactly as OpenAI published it.
pub(super) fn cached(data: &Path) -> Option<Vec<u8>> {
    storage::read_bounded(&path(data), LIMIT as u64)
        .ok()
        .flatten()
        .filter(|bytes| super::catalog::codex_models(bytes).is_some())
}
/// What the downloaded catalog holds, and since when.
#[derive(Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub models: usize,
    /// The first eight hex digits of the content's SHA-256.
    pub version: String,
    /// The last successful download, in milliseconds.
    pub updated_at: u64,
}
pub(super) fn status(data: &Path) -> Option<Status> {
    let bytes = cached(data)?;
    let updated_at = std::fs::metadata(path(data))
        .and_then(|m| m.modified())
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some(Status {
        models: super::catalog::codex_models(&bytes)?.len(),
        version: Sha256::digest(&bytes)[..4]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        updated_at,
    })
}
/// Whether a day has passed since the catalog was last downloaded.
fn due(data: &Path) -> bool {
    cached(data).is_none()
        || std::fs::metadata(path(data))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_none_or(|age| age >= FRESH)
}
/// Downloads the catalog when it is due, or at once when forced, and reports
/// whether its content changed. A failure leaves the catalog downloaded before
/// in place.
pub(super) async fn refresh(data: &Path, source: &str, force: bool) -> Result<bool> {
    if !force && !due(data) {
        return Ok(false);
    }
    let bytes = download(source).await?;
    let changed = cached(data).as_deref() != Some(&bytes[..]);
    // Rewritten even when unchanged: the file's time is the day's mark.
    storage::atomic_write_bounded(&path(data), &bytes, None, LIMIT as u64)?;
    Ok(changed)
}
async fn download(source: &str) -> Result<Vec<u8>> {
    let client =
        crate::download::catalog_client().map_err(|_| failure("官方模型列表连接初始化失败"))?;
    let mut response = client
        .get(source)
        .header("accept-encoding", "identity")
        .send()
        .await
        .map_err(|_| failure("官方模型列表下载失败"))?;
    if !response.status().is_success() {
        return Err(failure("官方模型列表响应未成功"));
    }
    let bytes = crate::download::body(
        &mut response,
        LIMIT as u64,
        "官方模型列表下载中断",
        "官方模型列表超过 8 MiB",
    )
    .await
    .map_err(failure)?;
    if super::catalog::codex_models(&bytes).is_none() {
        return Err(failure("官方模型列表格式无效"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serves each body in turn, the last one for every later request.
    async fn serve(bodies: Vec<(u16, Vec<u8>)>) -> (String, Arc<AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = format!("http://{}/models.json", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let at = counted.fetch_add(1, Ordering::SeqCst);
                let (status, body) = bodies[at.min(bodies.len() - 1)].clone();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request).await;
                let head = format!(
                    "HTTP/1.1 {status} Fixture\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(&body).await;
            }
        });
        (source, hits)
    }
    fn age(data: &Path) {
        let file = std::fs::File::options()
            .write(true)
            .open(path(data))
            .unwrap();
        file.set_modified(std::time::SystemTime::now() - FRESH)
            .unwrap();
    }

    #[tokio::test]
    async fn catalog_is_kept_verbatim_refreshed_daily_and_survives_bad_answers() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let first = br#"{"models":[{"slug":"fixture-a","extra":{"kept":true}}]}"#.to_vec();
        let second = br#"{"models":[{"slug":"fixture-a"},{"slug":"fixture-b"}]}"#.to_vec();
        let (source, hits) = serve(vec![
            (200, first.clone()),
            (200, first.clone()),
            (200, second.clone()),
            (500, second.clone()),
            (200, br#"{"data":[{"id":"fixture-a"}]}"#.to_vec()),
            (200, vec![b' '; LIMIT + 1]),
        ])
        .await;
        assert!(cached(data).is_none());
        assert!(status(data).is_none());
        assert!(refresh(data, &source, false).await.unwrap());
        assert_eq!(cached(data).unwrap(), first);
        let held = status(data).unwrap();
        assert_eq!(held.models, 1);
        assert_eq!(held.version.len(), 8);
        // Nothing is asked again within the day.
        assert!(!refresh(data, &source, false).await.unwrap());
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // A day later the same content only moves the mark.
        age(data);
        assert!(status(data).unwrap().updated_at < held.updated_at);
        assert!(!refresh(data, &source, false).await.unwrap());
        assert!(!refresh(data, &source, false).await.unwrap());
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(status(data).unwrap().version, held.version);
        assert!(status(data).unwrap().updated_at >= held.updated_at);
        // Asked for by hand, it is downloaded before the day is over.
        assert!(refresh(data, &source, true).await.unwrap());
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        assert_eq!(cached(data).unwrap(), second);
        let held = status(data).unwrap();
        assert_eq!(held.models, 2);
        // A refused, foreign or oversized answer leaves the catalog and stays due.
        for forced in [false, true, false] {
            age(data);
            assert!(refresh(data, &source, forced).await.is_err());
            assert_eq!(cached(data).unwrap(), second);
            assert_eq!(status(data).unwrap().version, held.version);
        }
        assert_eq!(hits.load(Ordering::SeqCst), 6);
        // A damaged file is not a catalog and is replaced by the next download.
        std::fs::write(path(data), b"broken").unwrap();
        assert!(cached(data).is_none());
        assert!(status(data).is_none());
    }
}
