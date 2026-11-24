//! A process-local cache for the polled auth file. A missing or unusual
//! metadata stamp always falls back to the existing bounded read, never to
//! stale bytes.
use crate::storage::{self, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    /// Which file it is: a replacement of the same length can carry the same
    /// time where the clock behind it is coarse, as on Linux.
    #[cfg(unix)]
    file: (u64, u64),
}

fn stamp(path: &Path) -> Option<Stamp> {
    let m = fs::symlink_metadata(path).ok()?;
    if !m.is_file() || m.len() > 2 * 1024 * 1024 {
        return None;
    }
    Some(Stamp {
        len: m.len(),
        modified: m.modified().ok(),
        #[cfg(unix)]
        file: {
            use std::os::unix::fs::MetadataExt;
            (m.dev(), m.ino())
        },
    })
}

pub struct Cache<T> {
    entry: Option<(PathBuf, Stamp, Arc<T>)>,
}
impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self { entry: None }
    }
}
impl<T> Cache<T> {
    pub fn clear(&mut self) {
        self.entry = None;
    }
    pub fn read_user(
        &mut self,
        path: &Path,
        parse: impl FnOnce(Option<Vec<u8>>) -> T,
    ) -> Result<Arc<T>> {
        let resolved = storage::resolve_user_path(path)?;
        self.read(&resolved, parse)
    }
    pub fn read(
        &mut self,
        path: &Path,
        parse: impl FnOnce(Option<Vec<u8>>) -> T,
    ) -> Result<Arc<T>> {
        let before = stamp(path);
        if let (Some(saved), Some((saved_path, saved_stamp, value))) = (before, self.entry.as_ref())
        {
            if path == saved_path && saved == *saved_stamp {
                return Ok(value.clone());
            }
        }
        self.clear();
        let bytes = storage::read_optional(path)?;
        let after = stamp(path);
        let value = Arc::new(parse(bytes));
        if let Some(saved) = before.filter(|v| Some(*v) == after) {
            self.entry = Some((path.to_owned(), saved, value.clone()));
        }
        Ok(value)
    }
}

#[cfg(test)]
#[path = "auth_cache_tests.rs"]
mod tests;
