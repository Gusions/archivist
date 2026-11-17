//! Storage backends: where the repository bytes physically live.
//!
//! The repository (`repo.rs`) only talks to this small key-value interface, so the
//! same archive logic can run on a local folder today and on S3, SFTP or WebDAV
//! later. Keys are `/`-separated relative paths such as `packs/pack-ab12.pack`.
//!
//! Contract every backend must honour:
//! - `put` is atomic and durable: after it returns, readers see the old value or the
//!   new one, never a partial one, and a crash does not lose it.
//! - `put_new` creates the key only if it does not exist (used for the lock).
//! - `delete` of a missing key is not an error.

use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub trait Backend: Send + Sync {
    fn get(&self, key: &str) -> Result<Vec<u8>>;
    /// Read `len` bytes starting at `off`.
    fn get_range(&self, key: &str, off: u64, len: usize) -> Result<Vec<u8>>;
    fn put(&self, key: &str, data: &[u8]) -> Result<()>;
    /// Create `key` only if absent. Returns `false` if it already exists.
    fn put_new(&self, key: &str, data: &[u8]) -> Result<bool>;
    fn delete(&self, key: &str) -> Result<()>;
    /// Keys under `prefix` (e.g. `"packs/"`) with their sizes. Not recursive.
    fn list(&self, prefix: &str) -> Result<Vec<(String, u64)>>;
    fn exists(&self, key: &str) -> Result<bool>;
    /// Refresh the modification time of `key` (lock heartbeat).
    fn touch(&self, key: &str) -> Result<()>;
    /// Time since `key` was last written or touched; `None` if it does not exist.
    fn age(&self, key: &str) -> Result<Option<Duration>>;
}

/// Reject keys that could escape the repository root.
fn check_key(key: &str) -> Result<()> {
    if key.is_empty() || key.starts_with('/') || key.contains('\\') || key.split('/').any(|p| p == ".." || p == ".") {
        bail!("invalid storage key: {key}");
    }
    Ok(())
}

/// Write a file atomically and durably: temp file, fsync, rename, fsync of the
/// parent directory. A crash leaves either the old file or the new one.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    sync_dir(path.parent().unwrap_or_else(|| Path::new(".")));
    Ok(())
}

/// Flush a directory entry to disk (no-op where unsupported, e.g. Windows).
pub(crate) fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// A repository in a local folder (an external disk, a network mount, ...).
pub struct LocalBackend {
    root: PathBuf,
}

impl LocalBackend {
    pub fn new(root: &Path) -> Self {
        LocalBackend { root: root.to_path_buf() }
    }

    fn path(&self, key: &str) -> Result<PathBuf> {
        check_key(key)?;
        Ok(self.root.join(key))
    }
}

impl Backend for LocalBackend {
    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let p = self.path(key)?;
        fs::read(&p).with_context(|| p.display().to_string())
    }

    fn get_range(&self, key: &str, off: u64, len: usize) -> Result<Vec<u8>> {
        let p = self.path(key)?;
        let mut f = File::open(&p).with_context(|| p.display().to_string())?;
        f.seek(SeekFrom::Start(off))?;
        let mut buf = vec![0u8; len];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn put(&self, key: &str, data: &[u8]) -> Result<()> {
        write_atomic(&self.path(key)?, data)
    }

    fn put_new(&self, key: &str, data: &[u8]) -> Result<bool> {
        let p = self.path(key)?;
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        match OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(mut f) => {
                f.write_all(data)?;
                f.sync_all()?;
                Ok(true)
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn delete(&self, key: &str) -> Result<()> {
        match fs::remove_file(self.path(key)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<(String, u64)>> {
        let dir = prefix.trim_end_matches('/');
        let path = if dir.is_empty() { self.root.clone() } else { self.path(dir)? };
        let rd = match fs::read_dir(&path) {
            Ok(rd) => rd,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut out = vec![];
        for e in rd {
            let e = e?;
            let m = e.metadata()?;
            if m.is_file() {
                let name = e.file_name().to_string_lossy().to_string();
                let key = if dir.is_empty() { name } else { format!("{dir}/{name}") };
                out.push((key, m.len()));
            }
        }
        out.sort();
        Ok(out)
    }

    fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.path(key)?.is_file())
    }

    fn touch(&self, key: &str) -> Result<()> {
        let f = OpenOptions::new().write(true).open(self.path(key)?)?;
        f.set_modified(std::time::SystemTime::now())?;
        Ok(())
    }

    fn age(&self, key: &str) -> Result<Option<Duration>> {
        match fs::metadata(self.path(key)?) {
            Ok(m) => Ok(m.modified().ok().and_then(|t| t.elapsed().ok())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
pub use mem::MemBackend;

#[cfg(test)]
mod mem {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::time::Instant;

    /// In-memory backend: proves the repository logic does not depend on a filesystem.
    #[derive(Default)]
    pub struct MemBackend {
        files: Mutex<BTreeMap<String, (Vec<u8>, Instant)>>,
    }

    impl MemBackend {
        /// Pretend `key` was last written `age` ago (to test stale locks).
        pub fn backdate(&self, key: &str, age: Duration) {
            if let Some(e) = self.files.lock().unwrap().get_mut(key) {
                e.1 = Instant::now() - age;
            }
        }
        pub fn keys(&self) -> Vec<String> {
            self.files.lock().unwrap().keys().cloned().collect()
        }
        pub fn get_raw(&self, key: &str) -> Vec<u8> {
            self.files.lock().unwrap().get(key).map(|e| e.0.clone()).unwrap_or_default()
        }
        pub fn raw_put(&self, key: &str, data: &[u8]) {
            self.files.lock().unwrap().insert(key.to_string(), (data.to_vec(), Instant::now()));
        }
    }

    impl Backend for MemBackend {
        fn get(&self, key: &str) -> Result<Vec<u8>> {
            check_key(key)?;
            match self.files.lock().unwrap().get(key) {
                Some(e) => Ok(e.0.clone()),
                None => bail!("not found: {key}"),
            }
        }
        fn get_range(&self, key: &str, off: u64, len: usize) -> Result<Vec<u8>> {
            let d = self.get(key)?;
            let (o, e) = (off as usize, off as usize + len);
            if e > d.len() {
                bail!("range out of bounds: {key}");
            }
            Ok(d[o..e].to_vec())
        }
        fn put(&self, key: &str, data: &[u8]) -> Result<()> {
            check_key(key)?;
            self.raw_put(key, data);
            Ok(())
        }
        fn put_new(&self, key: &str, data: &[u8]) -> Result<bool> {
            check_key(key)?;
            let mut f = self.files.lock().unwrap();
            if f.contains_key(key) {
                return Ok(false);
            }
            f.insert(key.to_string(), (data.to_vec(), Instant::now()));
            Ok(true)
        }
        fn delete(&self, key: &str) -> Result<()> {
            check_key(key)?;
            self.files.lock().unwrap().remove(key);
            Ok(())
        }
        fn list(&self, prefix: &str) -> Result<Vec<(String, u64)>> {
            let pre = if prefix.is_empty() || prefix.ends_with('/') { prefix.to_string() } else { format!("{prefix}/") };
            Ok(self
                .files
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| k.starts_with(&pre) && !k[pre.len()..].contains('/'))
                .map(|(k, v)| (k.clone(), v.0.len() as u64))
                .collect())
        }
        fn exists(&self, key: &str) -> Result<bool> {
            check_key(key)?;
            Ok(self.files.lock().unwrap().contains_key(key))
        }
        fn touch(&self, key: &str) -> Result<()> {
            match self.files.lock().unwrap().get_mut(key) {
                Some(e) => {
                    e.1 = Instant::now();
                    Ok(())
                }
                None => bail!("not found: {key}"),
            }
        }
        fn age(&self, key: &str) -> Result<Option<Duration>> {
            check_key(key)?;
            Ok(self.files.lock().unwrap().get(key).map(|e| e.1.elapsed()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(b: &dyn Backend) {
        b.put("a/x.bin", b"hello world").unwrap();
        b.put("a/x.bin", b"hello there").unwrap();
        assert_eq!(b.get("a/x.bin").unwrap(), b"hello there");
        assert_eq!(b.get_range("a/x.bin", 6, 5).unwrap(), b"there");
        assert!(b.exists("a/x.bin").unwrap() && !b.exists("a/y.bin").unwrap());
        assert!(b.put_new("lock", b"1").unwrap());
        assert!(!b.put_new("lock", b"2").unwrap());
        assert_eq!(b.get("lock").unwrap(), b"1");
        assert_eq!(b.list("a/").unwrap(), vec![("a/x.bin".to_string(), 11)]);
        assert!(b.list("nothing/").unwrap().is_empty());
        assert!(b.age("lock").unwrap().is_some() && b.age("missing").unwrap().is_none());
        b.touch("lock").unwrap();
        b.delete("a/x.bin").unwrap();
        b.delete("a/x.bin").unwrap(); // idempotent
        assert!(b.get("a/x.bin").is_err());
        assert!(b.put("../escape", b"x").is_err());
        assert!(b.put("/abs", b"x").is_err());
    }

    #[test]
    fn local_backend_honours_contract() {
        let d = tempfile::tempdir().unwrap();
        contract(&LocalBackend::new(d.path()));
        // no temp files left behind by atomic puts
        assert!(fs::read_dir(d.path().join("a")).unwrap().next().is_none());
    }

    #[test]
    fn mem_backend_honours_contract() {
        contract(&MemBackend::default());
    }
}
