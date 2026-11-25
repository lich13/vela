//! Complete upstream errors, kept outside the small journal index.
//!
//! They live in one private SQLite file, a row per error. A row holds what
//! used to be one file per error, compressed: a line of JSON for the headers
//! and the body after it.
use crate::{errors::code, storage};
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::{self, Read, Write},
    path::Path,
    sync::{Arc, OnceLock},
};

const MEMORY: usize = 2 * 1024 * 1024;
/// Rows written in one transaction when the files of an earlier version move in.
const ADOPTED: usize = 512;
type Failure = Box<dyn std::error::Error>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub base64: bool,
}
pub fn headers(values: &hyper::HeaderMap) -> Vec<Header> {
    values
        .iter()
        .map(|(name, value)| Header {
            name: name.to_string(),
            value: value
                .to_str()
                .map(str::to_owned)
                .unwrap_or_else(|_| STANDARD.encode(value.as_bytes())),
            base64: value.to_str().is_err(),
        })
        .collect()
}

#[derive(Default)]
pub struct Capture {
    memory: Vec<u8>,
    disk: Option<std::io::BufWriter<tempfile::NamedTempFile>>,
    problem: Option<String>,
}
impl Capture {
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.problem.is_some() {
            return;
        }
        if self.write(bytes).is_err() {
            self.problem = Some("错误原文暂存失败，内容不完整".into());
        }
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.disk.is_none() && self.memory.len().saturating_add(bytes.len()) > MEMORY {
            let file = storage::private_tempfile(&std::env::temp_dir())
                .map_err(|_| io::Error::other("error capture unavailable"))?;
            let mut writer = std::io::BufWriter::new(file);
            writer.write_all(&self.memory)?;
            self.memory = Vec::new();
            self.disk = Some(writer);
        }
        if let Some(writer) = &mut self.disk {
            writer.write_all(bytes)
        } else {
            self.memory.extend_from_slice(bytes);
            Ok(())
        }
    }
    pub fn prefix(&self) -> &[u8] {
        &self.memory[..self.memory.len().min(512)]
    }
    pub fn finish(mut self, headers: Vec<Header>, event: Option<String>) -> Raw {
        let body = if let Some(mut writer) = self.disk.take() {
            if writer.flush().is_err() {
                self.problem = Some("错误原文暂存失败，内容不完整".into());
            }
            match writer.into_inner() {
                Ok(file) => Body::Disk(Arc::new(file)),
                Err(_) => {
                    self.problem = Some("错误原文暂存失败，内容不完整".into());
                    Body::Memory(Arc::new(Vec::new()))
                }
            }
        } else {
            Body::Memory(Arc::new(self.memory))
        };
        Raw {
            headers,
            event,
            incomplete: self.problem,
            body,
            digest: Arc::new(OnceLock::new()),
        }
    }
}
#[derive(Clone, Debug)]
enum Body {
    Memory(Arc<Vec<u8>>),
    Disk(Arc<tempfile::NamedTempFile>),
}
#[derive(Clone, Debug)]
pub struct Raw {
    pub headers: Vec<Header>,
    pub event: Option<String>,
    pub incomplete: Option<String>,
    body: Body,
    digest: Arc<OnceLock<std::result::Result<String, ()>>>,
}
impl PartialEq for Raw {
    fn eq(&self, other: &Self) -> bool {
        self.headers == other.headers
            && self.event == other.event
            && self.incomplete == other.incomplete
            && matches!((self.digest(), other.digest()), (Ok(a), Ok(b)) if a == b)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reference {
    pub id: String,
    pub digest: String,
    pub incomplete: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub headers: Vec<Header>,
    pub event: Option<String>,
    pub body: String,
    pub base64: bool,
    pub incomplete: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Meta {
    headers: Vec<Header>,
    event: Option<String>,
    incomplete: Option<String>,
}
impl Raw {
    pub fn bytes(bytes: &[u8], headers: Vec<Header>, event: Option<String>) -> Self {
        let mut capture = Capture::default();
        capture.feed(bytes);
        capture.finish(headers, event)
    }
    fn reader(&self) -> io::Result<Box<dyn Read + '_>> {
        match &self.body {
            Body::Memory(bytes) => Ok(Box::new(io::Cursor::new(bytes.as_slice()))),
            Body::Disk(file) => Ok(Box::new(io::BufReader::new(file.reopen()?))),
        }
    }
    fn copy_to(&self, writer: &mut impl Write) -> io::Result<u64> {
        io::copy(&mut self.reader()?, writer)
    }
    pub fn value(&self) -> Option<serde_json::Value> {
        serde_json::from_reader(self.reader().ok()?).ok()
    }
    pub fn text(&self) -> io::Result<String> {
        let mut text = String::new();
        self.reader()?.read_to_string(&mut text)?;
        Ok(text)
    }
    fn digest(&self) -> &std::result::Result<String, ()> {
        self.digest.get_or_init(|| {
            let mut hash = Sha256::new();
            self.copy_to(&mut hash).map_err(|_| ())?;
            Ok(format!("{:x}", hash.finalize()))
        })
    }
    fn incomplete(&self) -> Option<String> {
        if self.digest().is_err() {
            Some("错误原文读取失败，内容不完整".into())
        } else {
            self.incomplete.clone()
        }
    }
    pub fn reference(&self, id: &str) -> Reference {
        let mut hash = Sha256::new();
        hash.update(self.digest().as_deref().unwrap_or_default().as_bytes());
        hash.update(serde_json::to_vec(&self.headers).unwrap_or_default());
        hash.update(self.event.as_deref().unwrap_or_default().as_bytes());
        Reference {
            id: id.into(),
            digest: format!("{:x}", hash.finalize()),
            incomplete: self.incomplete(),
        }
    }
    pub fn save(&self, store: &Path, id: &str) -> storage::Result<()> {
        let meta = Meta {
            headers: self.headers.clone(),
            event: self.event.clone(),
            incomplete: self.incomplete(),
        };
        let write = || -> Result<(), Failure> {
            let mut packed = zstd::Encoder::new(Vec::new(), zstd::DEFAULT_COMPRESSION_LEVEL)?;
            serde_json::to_writer(&mut packed, &meta)?;
            packed.write_all(b"\n")?;
            self.copy_to(&mut packed)?;
            open(store, true)?.execute(
                "INSERT OR REPLACE INTO errors(id,body) VALUES(?1,?2)",
                (id, packed.finish()?),
            )?;
            Ok(())
        };
        write().map_err(|_| storage::AppError::new(code::LOG_WRITE, "错误原文保存失败"))
    }
}
fn open(store: &Path, create: bool) -> Result<Connection, Failure> {
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let connection = Connection::open_with_flags(store, flags)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    if create {
        storage::protect(store, false)?;
        // Set before the table exists, this gives the pages of deleted rows
        // back to the file system as they are freed.
        connection.execute_batch(
            "PRAGMA auto_vacuum=FULL;
             CREATE TABLE IF NOT EXISTS errors(id TEXT PRIMARY KEY,body BLOB NOT NULL);",
        )?;
    }
    Ok(connection)
}
pub fn read(store: &Path, id: &str) -> storage::Result<View> {
    let read = || -> Result<View, Failure> {
        let packed: Vec<u8> =
            open(store, false)?.query_row("SELECT body FROM errors WHERE id=?1", [id], |row| {
                row.get(0)
            })?;
        let mut line = zstd::decode_all(packed.as_slice())?;
        let bytes = line.split_off(
            line.iter()
                .position(|b| *b == b'\n')
                .map_or(line.len(), |n| n + 1),
        );
        let meta: Meta = serde_json::from_slice(&line)?;
        let (body, base64) = match String::from_utf8(bytes) {
            Ok(text) => (text, false),
            Err(e) => (STANDARD.encode(e.into_bytes()), true),
        };
        Ok(View {
            headers: meta.headers,
            event: meta.event,
            body,
            base64,
            incomplete: meta.incomplete,
        })
    };
    read().map_err(|_| storage::AppError::new(code::LOG_READ, "错误原文读取失败或已过期"))
}
/// Deletes every stored error that `retained` does not name.
pub fn retain(store: &Path, retained: &HashSet<String>) -> storage::Result<()> {
    let prune = || -> Result<(), Failure> {
        if !store.exists() {
            return Ok(());
        }
        let mut connection = open(store, false)?;
        let tx = connection.transaction()?;
        {
            let stored = tx
                .prepare("SELECT id FROM errors")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut delete = tx.prepare("DELETE FROM errors WHERE id=?1")?;
            for id in stored.iter().filter(|id| !retained.contains(*id)) {
                delete.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    };
    prune().map_err(|_| storage::AppError::new(code::LOG_WRITE, "错误原文清理失败"))
}
/// Moves the errors an earlier version kept as one file each into the store.
/// A file is deleted once its row is committed, so an interrupted move
/// continues at the next start.
pub fn adopt(store: &Path, directory: &Path) -> storage::Result<()> {
    let adopt = || -> Result<(), Failure> {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".error")) else {
                continue;
            };
            if uuid::Uuid::parse_str(id).is_ok() && entry.file_type()?.is_file() {
                files.push((id.to_owned(), entry.path()));
            }
        }
        if !files.is_empty() {
            let mut connection = open(store, true)?;
            for batch in files.chunks(ADOPTED) {
                let tx = connection.transaction()?;
                {
                    let mut insert =
                        tx.prepare("INSERT OR IGNORE INTO errors(id,body) VALUES(?1,?2)")?;
                    for (id, path) in batch {
                        let packed = zstd::encode_all(
                            std::fs::File::open(path)?,
                            zstd::DEFAULT_COMPRESSION_LEVEL,
                        )?;
                        insert.execute((id, packed))?;
                    }
                }
                tx.commit()?;
                for (_, path) in batch {
                    std::fs::remove_file(path)?;
                }
            }
        }
        // Whatever else is in the directory was not written here and keeps it.
        let _ = std::fs::remove_dir(directory);
        Ok(())
    };
    adopt().map_err(|_| storage::AppError::new(code::LOG_WRITE, "错误原文保存失败"))
}

/// How many errors the store holds.
#[cfg(test)]
pub fn stored(store: &Path) -> usize {
    open(store, false)
        .unwrap()
        .query_row("SELECT count(*) FROM errors", [], |row| row.get(0))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_stored_error_reads_back_whole_and_the_file_shrinks_when_rows_leave() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("fixture.sqlite");
        assert!(read(&store, "fixture").is_err());
        retain(&store, &HashSet::new()).unwrap();
        assert!(
            !store.exists(),
            "reading and pruning do not create the store"
        );
        let header = Header {
            name: "x-fixture".into(),
            value: "kept".into(),
            base64: false,
        };
        // Bytes that do not compress, so the size of the file follows the rows.
        let noise: Vec<u8> = (0..64)
            .flat_map(|_| *uuid::Uuid::new_v4().as_bytes())
            .collect();
        for n in 0..200 {
            Raw::bytes(&noise, vec![header.clone()], Some("error".into()))
                .save(&store, &format!("fixture-{n}"))
                .unwrap();
        }
        Raw::bytes(b"fixture text", vec![], None)
            .save(&store, "fixture-text")
            .unwrap();
        let view = read(&store, "fixture-7").unwrap();
        assert_eq!(view.headers, vec![header]);
        assert_eq!(view.event.as_deref(), Some("error"));
        assert!(view.base64);
        assert_eq!(STANDARD.decode(view.body).unwrap(), noise);
        let text = read(&store, "fixture-text").unwrap();
        assert_eq!((text.body.as_str(), text.base64), ("fixture text", false));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&store).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let full = std::fs::metadata(&store).unwrap().len();
        retain(&store, &HashSet::from(["fixture-text".to_owned()])).unwrap();
        assert_eq!(stored(&store), 1);
        assert!(read(&store, "fixture-7").is_err());
        assert_eq!(read(&store, "fixture-text").unwrap().body, "fixture text");
        assert!(std::fs::metadata(&store).unwrap().len() < full / 4);
    }
    #[test]
    fn files_of_an_earlier_version_move_into_the_store_and_leave_the_rest_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("fixture.sqlite");
        let files = dir.path().join("fixture-files");
        adopt(&store, &files).unwrap();
        assert!(!store.exists(), "nothing to move creates nothing");
        std::fs::create_dir(&files).unwrap();
        let ids: Vec<_> = (0..ADOPTED + 3)
            .map(|_| uuid::Uuid::new_v4().to_string())
            .collect();
        for (n, id) in ids.iter().enumerate() {
            let mut file = br#"{"headers":[{"name":"x-fixture","value":"kept"}],"event":null,"incomplete":null}"#.to_vec();
            file.extend_from_slice(format!("\nfixture body {n}").as_bytes());
            std::fs::write(files.join(format!("{id}.error")), file).unwrap();
        }
        // A row already stored is kept as it is; its file only goes away.
        Raw::bytes(b"fixture stored first", vec![], None)
            .save(&store, &ids[0])
            .unwrap();
        std::fs::write(files.join("unrelated.txt"), b"fixture").unwrap();
        std::fs::write(files.join("not-an-id.error"), b"fixture").unwrap();
        adopt(&store, &files).unwrap();
        assert_eq!(stored(&store), ids.len());
        assert_eq!(read(&store, &ids[0]).unwrap().body, "fixture stored first");
        let last = read(&store, &ids[ADOPTED + 2]).unwrap();
        assert_eq!(last.body, format!("fixture body {}", ADOPTED + 2));
        assert_eq!(last.headers[0].value, "kept");
        assert!(!last.base64);
        let mut left: Vec<_> = std::fs::read_dir(&files)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["not-an-id.error", "unrelated.txt"]);
        std::fs::remove_file(files.join("unrelated.txt")).unwrap();
        std::fs::remove_file(files.join("not-an-id.error")).unwrap();
        adopt(&store, &files).unwrap();
        assert!(!files.exists(), "the emptied directory is removed");
        assert_eq!(stored(&store), ids.len());
    }
    #[test]
    fn spilling_keeps_all_chunks_and_releases_the_original_allocation() {
        let mut capture = Capture::default();
        capture.feed(&vec![b'a'; MEMORY]);
        assert!(capture.memory.capacity() >= MEMORY);
        capture.feed(b"fixture tail");
        assert_eq!(capture.memory.capacity(), 0);
        let raw = capture.finish(vec![], None);
        let text = raw.text().unwrap();
        assert_eq!(text.len(), MEMORY + b"fixture tail".len());
        assert!(text.starts_with(&"a".repeat(MEMORY)));
        assert!(text.ends_with("fixture tail"));
        assert!(raw.reference("fixture").incomplete.is_none());
    }
    #[test]
    fn spilled_json_remains_complete_and_reopen_respects_platform_file_lifetime() {
        let value = serde_json::json!({"error":{"message":"fixture".repeat(MEMORY / 7 + 1)}});
        let bytes = serde_json::to_vec(&value).unwrap();
        let raw = Raw::bytes(&bytes, vec![], None);
        assert_eq!(raw.value().unwrap(), value);
        assert_eq!(raw.text().unwrap().as_bytes(), bytes);
        let reference = raw.reference("fixture");
        assert!(reference.incomplete.is_none());
        assert_eq!(reference.digest, raw.clone().reference("other").digest);
        let missing = Raw::bytes(&bytes, vec![], None);
        let Body::Disk(file) = &missing.body else {
            panic!("spilled body");
        };
        std::fs::remove_file(file.path()).unwrap();
        #[cfg(unix)]
        {
            assert!(missing.text().is_err());
            assert!(missing.value().is_none());
            assert_eq!(
                missing.reference("fixture").incomplete.as_deref(),
                Some("错误原文读取失败，内容不完整")
            );
        }
        #[cfg(windows)]
        {
            // tempfile reopens the live Windows handle even after unlinking its name.
            assert_eq!(missing.text().unwrap().as_bytes(), bytes);
            assert_eq!(missing.value().unwrap(), value);
            assert!(missing.reference("fixture").incomplete.is_none());
        }
    }
}
