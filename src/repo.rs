//! Content-addressed repository: deduplicated, compressed chunks gathered in packs.
//!
//! Layout (keys on the storage backend, see `backend.rs`):
//!   format                     version marker
//!   packs/<name>.pack          concatenated chunks: [tag][payload] (tag 0 = raw, 1 = zstd)
//!   packs/<name>.idx           JSON index of the pack (written ONLY once the pack is complete)
//!   snapshots/<id>.meta.json   light metadata
//!   snapshots/<id>.snap.zst    file -> chunk list (compressed JSON)
//!   keys                       (v2 only) master keys wrapped with the passphrase
//!   lock                       exclusive lock with heartbeat
//!
//! A pack without `.idx` is debris of an interrupted backup and is ignored.

use crate::backend::{Backend, LocalBackend};
use crate::crypto::{self, KdfParams, Keys};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

const FORMAT: &str = "archivist-repo-v1";
/// Encrypted repositories (see crypto.rs). Older versions refuse to open them.
const FORMAT_ENC: &str = "archivist-repo-v2";
const KEYS_KEY: &str = "keys";
const PACK_TARGET: u64 = 64 * 1024 * 1024;
const LOCK_KEY: &str = "lock";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    /// mtime in millisecondi dall'epoca
    pub mtime: i64,
    pub chunks: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Meta {
    pub id: String,
    pub created: String,
    pub vault: String,
    pub files: u64,
    pub bytes: u64,
    /// byte realmente scritti (compressi) per questo snapshot
    pub added_bytes: u64,
}

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub meta: Meta,
    pub entries: Vec<FileEntry>,
    /// cartelle (anche vuote), così il ripristino è fedele
    #[serde(default)]
    pub dirs: Vec<String>,
}

#[derive(Clone, Copy)]
struct Loc {
    pack: u32,
    off: u64,
    len: u32,
}

#[derive(Serialize, Deserialize, Default)]
struct PackIndex {
    chunks: Vec<(String, u64, u32)>,
}

struct OpenPack {
    name: String,
    buf: Vec<u8>,
    entries: Vec<(String, u64, u32)>,
}

pub struct Repo {
    be: Arc<dyn Backend>,
    keys: Option<Keys>,
    packs: RwLock<Vec<String>>,
    index: RwLock<HashMap<String, Loc>>,
    writer: Mutex<Option<OpenPack>>,
}

/// How often the lock owner refreshes the lock, and how long a lock may go
/// untouched before it is considered abandoned (crashed process).
const LOCK_HEARTBEAT: Duration = Duration::from_secs(15);
const LOCK_STALE: Duration = Duration::from_secs(120);

/// Exclusive repository lock. A background heartbeat keeps it fresh while the
/// owner is alive, so a long backup is never mistaken for a dead one.
pub struct RepoLock {
    be: Arc<dyn Backend>,
    token: String,
    stop: Option<Sender<()>>,
    beat: Option<JoinHandle<()>>,
}

impl Drop for RepoLock {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(h) = self.beat.take() {
            let _ = h.join();
        }
        // only remove the lock if it is still ours
        if self.be.get(LOCK_KEY).is_ok_and(|c| c == self.token.as_bytes()) {
            let _ = self.be.delete(LOCK_KEY);
        }
    }
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct RepoStats {
    pub disk_bytes: u64,
    pub chunks: u64,
    pub packs: u64,
    pub snapshots: u64,
}

impl Repo {
    /// Is there a repository in this local folder?
    pub fn is_repo(root: &Path) -> bool {
        root.join("format").is_file()
    }

    /// Create a repository in a local folder (must be new or empty).
    pub fn init(root: &Path) -> Result<Repo> {
        if !Self::is_repo(root) && root.is_dir() && std::fs::read_dir(root)?.next().is_some() {
            bail!(tr!("rs.repo_not_empty", path = root.display()));
        }
        Self::init_with(Arc::new(LocalBackend::new(root)))
    }

    pub fn open_or_init(root: &Path) -> Result<Repo> {
        if Self::is_repo(root) {
            Self::open(root)
        } else {
            Self::init(root)
        }
    }

    /// Open a local repository. Encrypted ones use the process passphrase (`crypto::password`).
    pub fn open(root: &Path) -> Result<Repo> {
        Self::open_with(Arc::new(LocalBackend::new(root)), &root.display().to_string(), crypto::password().as_deref())
    }

    /// Create a plain (unencrypted) repository on any backend; no-op if one already exists.
    pub fn init_with(be: Arc<dyn Backend>) -> Result<Repo> {
        if !be.exists("format")? {
            // the format marker goes last: its presence means the repository is usable
            be.put("format", FORMAT.as_bytes())?;
        }
        Self::open_with(be, "", None)
    }

    /// Create an encrypted repository in a local folder (must be new or empty).
    pub fn init_encrypted(root: &Path, pass: &str) -> Result<Repo> {
        if !Self::is_repo(root) && root.is_dir() && std::fs::read_dir(root)?.next().is_some() {
            bail!(tr!("rs.repo_not_empty", path = root.display()));
        }
        Self::init_encrypted_with(Arc::new(LocalBackend::new(root)), pass, KdfParams::default())
    }

    pub fn init_encrypted_with(be: Arc<dyn Backend>, pass: &str, kdf: KdfParams) -> Result<Repo> {
        if be.exists("format")? {
            bail!(tr!("rs.repo_not_empty", path = "(repository)"));
        }
        be.put(KEYS_KEY, &Keys::generate().to_keyfile(pass, kdf)?)?;
        be.put("format", FORMAT_ENC.as_bytes())?;
        Self::open_with(be, "", Some(pass))
    }

    /// Does the local folder hold an encrypted repository?
    pub fn is_encrypted_at(root: &Path) -> bool {
        std::fs::read_to_string(root.join("format")).is_ok_and(|f| f.trim() == FORMAT_ENC)
    }

    #[allow(dead_code)] // used by the UI unlock flow and tests
    pub fn is_encrypted(&self) -> bool {
        self.keys.is_some()
    }

    /// Re-wrap the master keys under a new password. Data is not rewritten.
    pub fn change_password(&self, new_pass: &str) -> Result<()> {
        let Some(k) = &self.keys else { bail!(tr!("rs.not_encrypted")) };
        self.be.put(KEYS_KEY, &k.to_keyfile(new_pass, KdfParams::default())?)
    }

    pub fn open_with(be: Arc<dyn Backend>, label: &str, pass: Option<&str>) -> Result<Repo> {
        let f = be.get("format").with_context(|| tr!("rs.not_a_repo", path = label))?;
        let f = String::from_utf8_lossy(&f);
        let keys = match f.trim() {
            FORMAT => None,
            FORMAT_ENC => {
                let Some(pass) = pass else { bail!(tr!("rs.password_required")) };
                let raw = be.get(KEYS_KEY).with_context(|| tr!("rs.keyfile_bad"))?;
                Some(Keys::from_keyfile(&raw, pass)?)
            }
            v => bail!(tr!("rs.unsupported_format", v = v)),
        };
        let repo = Repo {
            be,
            keys,
            packs: RwLock::new(vec![]),
            index: RwLock::new(HashMap::new()),
            writer: Mutex::new(None),
        };
        repo.load_index()?;
        Ok(repo)
    }

    // ---------- sealing (identity when the repository is not encrypted) ----------

    fn chunk_id(&self, data: &[u8]) -> String {
        match &self.keys {
            Some(k) => k.chunk_id(data),
            None => blake3::hash(data).to_hex().to_string(),
        }
    }

    fn put_sealed(&self, key: &str, data: &[u8]) -> Result<()> {
        match &self.keys {
            Some(k) => self.be.put(key, &k.seal(key, data)?),
            None => self.be.put(key, data),
        }
    }

    fn get_sealed(&self, key: &str) -> Result<Vec<u8>> {
        let raw = self.be.get(key)?;
        match &self.keys {
            Some(k) => k.open(key, &raw),
            None => Ok(raw),
        }
    }

    fn load_index(&self) -> Result<()> {
        let mut packs = self.packs.write().unwrap();
        let mut index = self.index.write().unwrap();
        let listing = self.be.list("packs/")?;
        let present: HashSet<&str> = listing.iter().map(|(k, _)| k.as_str()).collect();
        for (key, _) in &listing {
            let Some(name) = key.strip_prefix("packs/").and_then(|k| k.strip_suffix(".idx")) else {
                continue;
            };
            if !present.contains(Self::pack_key(name).as_str()) {
                continue;
            }
            let idx: PackIndex = serde_json::from_slice(&self.get_sealed(key)?)
                .with_context(|| tr!("rs.corrupt_index", path = key))?;
            let pi = packs.len() as u32;
            packs.push(name.to_string());
            for (h, off, len) in idx.chunks {
                index.entry(h).or_insert(Loc { pack: pi, off, len });
            }
        }
        Ok(())
    }

    fn pack_key(name: &str) -> String {
        format!("packs/{name}.pack")
    }
    fn idx_key(name: &str) -> String {
        format!("packs/{name}.idx")
    }
    fn meta_key(id: &str) -> String {
        format!("snapshots/{id}.meta.json")
    }
    fn snap_key(id: &str) -> String {
        format!("snapshots/{id}.snap.zst")
    }

    pub fn lock(&self) -> Result<RepoLock> {
        if self.be.age(LOCK_KEY)?.is_some_and(|a| a > LOCK_STALE) {
            let _ = self.be.delete(LOCK_KEY);
        }
        let token = format!("{}:{:016x}", std::process::id(), rand::random::<u64>());
        if !self.be.put_new(LOCK_KEY, token.as_bytes())? {
            bail!(tr!("rs.repo_busy"));
        }
        let (tx, rx) = mpsc::channel::<()>();
        let be = self.be.clone();
        let beat = std::thread::spawn(move || {
            // recv_timeout returns Err(Disconnected) as soon as the lock is dropped
            while let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(LOCK_HEARTBEAT) {
                let _ = be.touch(LOCK_KEY);
            }
        });
        Ok(RepoLock { be: self.be.clone(), token, stop: Some(tx), beat: Some(beat) })
    }

    pub fn has_chunk(&self, h: &str) -> bool {
        self.index.read().unwrap().contains_key(h)
    }

    fn encode(data: &[u8], try_compress: bool, level: i32) -> Result<Vec<u8>> {
        if try_compress {
            let c = zstd::bulk::compress(data, level)?;
            // keep the compressed version only if it saves at least 3%
            if c.len() * 100 < data.len() * 97 {
                let mut out = Vec::with_capacity(c.len() + 1);
                out.push(1);
                out.extend_from_slice(&c);
                return Ok(out);
            }
        }
        let mut out = Vec::with_capacity(data.len() + 1);
        out.push(0);
        out.extend_from_slice(data);
        Ok(out)
    }

    /// Store a chunk. Returns (hash, bytes written, new?).
    pub fn put_chunk(&self, data: &[u8], try_compress: bool, level: i32) -> Result<(String, u64, bool)> {
        let hash = self.chunk_id(data);
        if self.has_chunk(&hash) {
            return Ok((hash, 0, false));
        }
        let enc = self.seal_chunk(&hash, Self::encode(data, try_compress, level)?)?;
        let stored = self.append_raw(&hash, &enc)?;
        Ok((hash, stored, stored > 0))
    }

    /// Append already-encoded bytes to the current pack. 0 if the chunk already exists.
    fn append_raw(&self, hash: &str, enc: &[u8]) -> Result<u64> {
        let mut guard = self.writer.lock().unwrap();
        if self.has_chunk(hash) {
            return Ok(0);
        }
        if guard.is_none() {
            let name = format!("pack-{:016x}", rand::random::<u64>());
            *guard = Some(OpenPack { name, buf: Vec::new(), entries: vec![] });
        }
        let pk = guard.as_mut().unwrap();
        let pi = {
            let mut packs = self.packs.write().unwrap();
            match packs.iter().position(|n| *n == pk.name) {
                Some(i) => i as u32,
                None => {
                    packs.push(pk.name.clone());
                    (packs.len() - 1) as u32
                }
            }
        };
        let off = pk.buf.len() as u64;
        pk.buf.extend_from_slice(enc);
        let loc = Loc { pack: pi, off, len: enc.len() as u32 };
        pk.entries.push((hash.to_string(), off, enc.len() as u32));
        self.index.write().unwrap().insert(hash.to_string(), loc);
        if pk.buf.len() as u64 >= PACK_TARGET {
            let done = guard.take().unwrap();
            self.finalize(done)?;
        }
        Ok(enc.len() as u64)
    }

    /// Upload the pack, then publish its index (the index makes the pack visible).
    fn finalize(&self, pk: OpenPack) -> Result<()> {
        if pk.entries.is_empty() {
            return Ok(());
        }
        self.be.put(&Self::pack_key(&pk.name), &pk.buf)?;
        self.put_sealed(&Self::idx_key(&pk.name), &serde_json::to_vec(&PackIndex { chunks: pk.entries })?)?;
        Ok(())
    }

    /// Close the pack being written. Call at the end of a backup.
    pub fn flush(&self) -> Result<()> {
        let mut guard = self.writer.lock().unwrap();
        if let Some(pk) = guard.take() {
            self.finalize(pk)?;
        }
        Ok(())
    }

    fn read_raw(&self, loc: Loc) -> Result<Vec<u8>> {
        let name = self.packs.read().unwrap()[loc.pack as usize].clone();
        // a chunk of the pack still being written lives only in memory
        if let Some(pk) = self.writer.lock().unwrap().as_ref() {
            if pk.name == name {
                let (o, e) = (loc.off as usize, loc.off as usize + loc.len as usize);
                return Ok(pk.buf[o..e].to_vec());
            }
        }
        self.be.get_range(&Self::pack_key(&name), loc.off, loc.len as usize)
    }

    /// Encrypted repositories wrap every encoded chunk as [2][nonce][ciphertext].
    fn seal_chunk(&self, id: &str, encoded: Vec<u8>) -> Result<Vec<u8>> {
        match &self.keys {
            None => Ok(encoded),
            Some(k) => {
                let sealed = k.seal(id, &encoded)?;
                let mut out = Vec::with_capacity(sealed.len() + 1);
                out.push(2);
                out.extend_from_slice(&sealed);
                Ok(out)
            }
        }
    }

    fn decode(&self, raw: &[u8], id: &str) -> Result<Vec<u8>> {
        let inner;
        let raw = match (&self.keys, raw.first()) {
            (Some(k), Some(2)) => {
                inner = k.open(id, &raw[1..])?;
                &inner[..]
            }
            // an encrypted repository never accepts plain chunks (downgrade protection)
            (Some(_), _) | (None, Some(2)) => bail!(tr!("rs.bad_chunk")),
            (None, _) => raw,
        };
        match raw.first() {
            Some(0) => Ok(raw[1..].to_vec()),
            Some(1) => Ok(zstd::stream::decode_all(&raw[1..])?),
            _ => bail!(tr!("rs.bad_chunk")),
        }
    }

    pub fn get_chunk(&self, hash: &str, verify: bool) -> Result<Vec<u8>> {
        let loc = *self
            .index
            .read()
            .unwrap()
            .get(hash)
            .ok_or_else(|| anyhow!(tr!("rs.chunk_missing", h = hash)))?;
        let data = self.decode(&self.read_raw(loc)?, hash)?;
        if verify && self.chunk_id(&data) != hash {
            bail!(tr!("rs.chunk_corrupt", h = hash));
        }
        Ok(data)
    }

    // ---------- snapshots ----------

    pub fn list_meta(&self) -> Result<Vec<Meta>> {
        let mut v = vec![];
        for (key, _) in self.be.list("snapshots/")? {
            if key.ends_with(".meta.json") {
                if let Ok(m) = serde_json::from_slice::<Meta>(&self.get_sealed(&key)?) {
                    v.push(m);
                }
            }
        }
        v.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
        Ok(v)
    }

    pub fn resolve(&self, spec: &str) -> Result<String> {
        let metas = self.list_meta()?;
        if metas.is_empty() {
            bail!(tr!("rs.no_snapshots"));
        }
        if spec == "latest" {
            return Ok(metas.last().unwrap().id.clone());
        }
        if let Some(m) = metas.iter().find(|m| m.id == spec) {
            return Ok(m.id.clone());
        }
        let m: Vec<_> = metas.iter().filter(|m| m.id.starts_with(spec)).collect();
        match m.len() {
            0 => bail!(tr!("rs.snap_not_found", s = spec)),
            1 => Ok(m[0].id.clone()),
            _ => bail!(tr!("rs.snap_ambiguous", s = spec)),
        }
    }

    pub fn load_snapshot(&self, spec: &str) -> Result<Snapshot> {
        let id = self.resolve(spec)?;
        let raw = self.get_sealed(&Self::snap_key(&id))?;
        Ok(serde_json::from_slice(&zstd::stream::decode_all(&raw[..])?)?)
    }

    pub fn latest_snapshot(&self) -> Result<Option<Snapshot>> {
        if self.list_meta()?.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.load_snapshot("latest")?))
    }

    pub fn unique_id(&self, base: &str) -> String {
        let mut id = base.to_string();
        let mut n = 1;
        while self.be.exists(&Self::meta_key(&id)).unwrap_or(false) {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    pub fn save_snapshot(&self, s: &Snapshot) -> Result<()> {
        let id = &s.meta.id;
        let z = zstd::stream::encode_all(&serde_json::to_vec(s)?[..], 9)?;
        self.put_sealed(&Self::snap_key(id), &z)?;
        // meta last: the snapshot "exists" only when it is complete
        self.put_sealed(&Self::meta_key(id), &serde_json::to_vec_pretty(&s.meta)?)?;
        Ok(())
    }

    pub fn delete_snapshot(&self, spec: &str) -> Result<String> {
        let id = self.resolve(spec)?;
        self.be.delete(&Self::meta_key(&id))?;
        let _ = self.be.delete(&Self::snap_key(&id));
        Ok(id)
    }

    // ---------- statistics and compaction ----------

    pub fn stats(&self) -> Result<RepoStats> {
        let mut disk = 0;
        let mut packs = 0;
        for (key, size) in self.be.list("packs/")? {
            disk += size;
            if key.ends_with(".pack") {
                packs += 1;
            }
        }
        for (_, size) in self.be.list("snapshots/")? {
            disk += size;
        }
        Ok(RepoStats {
            disk_bytes: disk,
            chunks: self.index.read().unwrap().len() as u64,
            packs,
            snapshots: self.list_meta()?.len() as u64,
        })
    }

    /// Delete unreferenced chunks by rewriting partly used packs.
    /// Returns the bytes freed.
    pub fn compact(&self, referenced: &HashSet<String>) -> Result<u64> {
        self.flush()?;
        let before = self.stats()?.disk_bytes;
        let listing = self.be.list("packs/")?;
        let keys: HashSet<&str> = listing.iter().map(|(k, _)| k.as_str()).collect();
        let mut names = vec![];
        for (key, _) in &listing {
            let Some(file) = key.strip_prefix("packs/") else { continue };
            if let Some(stem) = file.strip_suffix(".idx") {
                names.push(stem.to_string());
            } else if let Some(stem) = file.strip_suffix(".pack") {
                // orphan pack (interrupted backup)
                if !keys.contains(Self::idx_key(stem).as_str()) {
                    let _ = self.be.delete(key);
                }
            } else if file.ends_with(".tmp") {
                let _ = self.be.delete(key);
            }
        }
        for name in names {
            let idx: PackIndex = serde_json::from_slice(&self.get_sealed(&Self::idx_key(&name))?)?;
            let used: Vec<_> = idx.chunks.iter().filter(|(h, _, _)| referenced.contains(h)).collect();
            if used.len() == idx.chunks.len() {
                continue;
            }
            // new pack with only the used chunks (written BEFORE the old one is deleted)
            if !used.is_empty() {
                let nn = format!("pack-{:016x}", rand::random::<u64>());
                let src = self.be.get(&Self::pack_key(&name))?;
                let mut buf = Vec::new();
                let mut entries = vec![];
                for (h, o, l) in &used {
                    let (o, l) = (*o as usize, *l as usize);
                    entries.push((h.clone(), buf.len() as u64, l as u32));
                    buf.extend_from_slice(&src[o..o + l]);
                }
                self.be.put(&Self::pack_key(&nn), &buf)?;
                self.put_sealed(&Self::idx_key(&nn), &serde_json::to_vec(&PackIndex { chunks: entries })?)?;
            }
            self.be.delete(&Self::idx_key(&name))?;
            self.be.delete(&Self::pack_key(&name))?;
        }
        // reload the index
        self.packs.write().unwrap().clear();
        self.index.write().unwrap().clear();
        self.load_index()?;
        let after = self.stats()?.disk_bytes;
        Ok(before.saturating_sub(after))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MemBackend;
    use crate::backup::{run_backup, Progress};
    use crate::ops;
    use std::fs;

    fn make_vault(dir: &Path) {
        fs::create_dir_all(dir.join("Notes/empty")).unwrap();
        fs::write(dir.join("Notes/a.md"), "# A\n".repeat(5000)).unwrap();
        fs::write(dir.join("Notes/b.md"), "hello").unwrap();
        fs::write(dir.join("img.png"), (0..200_000u32).map(|i| (i * 31 % 251) as u8).collect::<Vec<_>>()).unwrap();
    }

    fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<_> = walkdir::WalkDir::new(root)
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_file())
            .map(|e| {
                (e.path().strip_prefix(root).unwrap().to_string_lossy().to_string(), fs::read(e.path()).unwrap())
            })
            .collect();
        v.sort();
        v
    }

    #[test]
    fn lock_is_exclusive_and_released() {
        let d = tempfile::tempdir().unwrap();
        let repo = Repo::init(&d.path().join("r")).unwrap();
        let l = repo.lock().unwrap();
        assert!(repo.lock().is_err());
        drop(l);
        assert!(!d.path().join("r").join("lock").exists());
        assert!(repo.lock().is_ok());
    }

    #[test]
    fn stale_lock_is_taken_over_fresh_one_is_not() {
        let d = tempfile::tempdir().unwrap();
        let repo = Repo::init(&d.path().join("r")).unwrap();
        let p = d.path().join("r").join("lock");
        fs::write(&p, "999999").unwrap();
        assert!(repo.lock().is_err(), "a fresh foreign lock must be respected");
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
        assert!(repo.lock().is_ok(), "an abandoned lock must be taken over");
    }

    #[test]
    fn dropping_old_lock_does_not_remove_new_owners_lock() {
        let d = tempfile::tempdir().unwrap();
        let repo = Repo::init(&d.path().join("r")).unwrap();
        let first = repo.lock().unwrap();
        // simulate takeover by another process
        fs::write(d.path().join("r").join("lock"), "other:owner").unwrap();
        drop(first);
        assert!(d.path().join("r").join("lock").exists());
    }

    #[test]
    fn backup_restore_roundtrip_is_byte_exact() {
        let d = tempfile::tempdir().unwrap();
        let vault = d.path().join("v");
        make_vault(&vault);
        let repo = Repo::init(&d.path().join("r")).unwrap();
        let prog = Progress::default();
        let rep = run_backup(&repo, &vault, &[], 3, &prog).unwrap();
        assert!(rep.snapshot.is_some() && !rep.unchanged);

        let out = d.path().join("out");
        ops::restore(&repo, "latest", &out, &prog).unwrap();
        assert_eq!(tree(&vault), tree(&out));
        assert!(out.join("Notes/empty").is_dir(), "empty folders are restored");

        // second run with no changes creates no new snapshot
        let rep2 = run_backup(&repo, &vault, &[], 3, &prog).unwrap();
        assert!(rep2.unchanged);
    }

    #[test]
    fn interrupted_backup_leaves_repo_valid_and_old_snapshots_intact() {
        let d = tempfile::tempdir().unwrap();
        let vault = d.path().join("v");
        make_vault(&vault);
        let rroot = d.path().join("r");
        let prog = Progress::default();
        {
            let repo = Repo::init(&rroot).unwrap();
            run_backup(&repo, &vault, &[], 3, &prog).unwrap();
        }
        // debris of a crashed backup: pack without index, partial index and snapshot temp files
        fs::write(rroot.join("packs/pack-deadbeefdeadbeef.pack"), vec![7u8; 4096]).unwrap();
        fs::write(rroot.join("packs/pack-deadbeefdeadbeef.idx.tmp"), b"{\"chunks\":[[\"ab").unwrap();
        fs::write(rroot.join("snapshots/20990101-000000.snap.zst"), b"partial").unwrap();
        fs::write(rroot.join("snapshots/20990101-000000.meta.tmp"), b"{").unwrap();

        let repo = Repo::open(&rroot).unwrap();
        // the half-written snapshot has no meta, so it is invisible
        assert_eq!(repo.list_meta().unwrap().len(), 1);
        let out = d.path().join("out");
        ops::restore(&repo, "latest", &out, &prog).unwrap();
        assert_eq!(tree(&vault), tree(&out));
        assert!(ops::verify(&repo, true, &prog).unwrap().ok);

        // the next backup works and gc removes the orphaned pack
        fs::write(vault.join("Notes/c.md"), "new").unwrap();
        run_backup(&repo, &vault, &[], 3, &prog).unwrap();
        ops::gc(&repo).unwrap();
        assert!(!rroot.join("packs/pack-deadbeefdeadbeef.pack").exists());
        assert!(!rroot.join("packs/pack-deadbeefdeadbeef.idx.tmp").exists());
    }

    #[test]
    fn full_lifecycle_on_an_in_memory_backend() {
        let d = tempfile::tempdir().unwrap();
        let vault = d.path().join("v");
        make_vault(&vault);
        let mem = Arc::new(MemBackend::default());
        let repo = Repo::init_with(mem.clone()).unwrap();
        let prog = Progress::default();
        run_backup(&repo, &vault, &[], 3, &prog).unwrap();
        fs::write(vault.join("Notes/c.md"), "second").unwrap();
        run_backup(&repo, &vault, &[], 3, &prog).unwrap();
        assert_eq!(repo.list_meta().unwrap().len(), 2);

        // reopen from the same bytes: nothing depends on in-process state
        let repo = Repo::open_with(mem.clone(), "mem", None).unwrap();
        let out = d.path().join("out");
        ops::restore(&repo, "latest", &out, &prog).unwrap();
        assert_eq!(tree(&vault), tree(&out));
        assert!(ops::verify(&repo, true, &prog).unwrap().ok);

        // dropping the oldest snapshot and compacting keeps the latest restorable
        let first = repo.list_meta().unwrap()[0].id.clone();
        repo.delete_snapshot(&first).unwrap();
        ops::gc(&repo).unwrap();
        let out2 = d.path().join("out2");
        ops::restore(&repo, "latest", &out2, &prog).unwrap();
        assert_eq!(tree(&vault), tree(&out2));
        // only protocol keys are used, nothing leaks outside the layout
        assert!(mem.keys().iter().all(|k| k == "format"
            || k.starts_with("packs/")
            || k.starts_with("snapshots/")
            || k == "lock"));
    }

    #[test]
    fn stale_lock_on_any_backend() {
        let mem = Arc::new(MemBackend::default());
        let repo = Repo::init_with(mem.clone()).unwrap();
        mem.raw_put("lock", b"someone");
        assert!(repo.lock().is_err());
        mem.backdate("lock", Duration::from_secs(3600));
        assert!(repo.lock().is_ok());
    }

    #[test]
    fn open_rejects_unknown_format() {
        let mem = Arc::new(MemBackend::default());
        mem.raw_put("format", b"archivist-repo-v99");
        assert!(Repo::open_with(mem, "mem", None).is_err());
    }

    fn enc_repo(mem: &Arc<MemBackend>) -> Repo {
        Repo::init_encrypted_with(mem.clone(), "pw", KdfParams::fast()).unwrap()
    }

    #[test]
    fn encrypted_lifecycle_and_nothing_readable_at_rest() {
        let d = tempfile::tempdir().unwrap();
        let vault = d.path().join("v");
        make_vault(&vault);
        fs::write(vault.join("Notes/SecretName.md"), "TOP-SECRET-MARKER ".repeat(300)).unwrap();
        let mem = Arc::new(MemBackend::default());
        let repo = enc_repo(&mem);
        assert!(repo.is_encrypted());
        let prog = Progress::default();
        run_backup(&repo, &vault, &[], 3, &prog).unwrap();
        fs::write(vault.join("Notes/c.md"), "second").unwrap();
        run_backup(&repo, &vault, &[], 3, &prog).unwrap();

        // no stored blob (except the format marker) contains plaintext, names or the vault path
        for k in mem.keys() {
            if k == "format" {
                continue;
            }
            let raw = mem.get_raw(&k);
            for needle in ["TOP-SECRET-MARKER", "SecretName", "Notes", "\"vault\"", "hello"] {
                assert!(!raw.windows(needle.len()).any(|w| w == needle.as_bytes()), "{k} leaks {needle}");
            }
        }

        // reopen with the password: restore, verify, gc all work
        let repo = Repo::open_with(mem.clone(), "mem", Some("pw")).unwrap();
        let out = d.path().join("out");
        ops::restore(&repo, "latest", &out, &prog).unwrap();
        assert_eq!(tree(&vault), tree(&out));
        assert!(ops::verify(&repo, true, &prog).unwrap().ok);
        let first = repo.list_meta().unwrap()[0].id.clone();
        repo.delete_snapshot(&first).unwrap();
        ops::gc(&repo).unwrap();
        let out2 = d.path().join("out2");
        ops::restore(&repo, "latest", &out2, &prog).unwrap();
        assert_eq!(tree(&vault), tree(&out2));
    }

    #[test]
    fn wrong_or_missing_password_is_refused() {
        let mem = Arc::new(MemBackend::default());
        enc_repo(&mem);
        let e = |r: Result<Repo>| r.err().unwrap().to_string();
        assert!(e(Repo::open_with(mem.clone(), "m", None)).contains("password"));
        assert!(e(Repo::open_with(mem.clone(), "m", Some("nope"))).to_lowercase().contains("password"));
    }

    #[test]
    fn change_password_keeps_data_readable() {
        let d = tempfile::tempdir().unwrap();
        let vault = d.path().join("v");
        make_vault(&vault);
        let mem = Arc::new(MemBackend::default());
        let repo = enc_repo(&mem);
        let prog = Progress::default();
        run_backup(&repo, &vault, &[], 3, &prog).unwrap();
        repo.change_password("second").unwrap();
        assert!(Repo::open_with(mem.clone(), "m", Some("pw")).is_err());
        let repo = Repo::open_with(mem.clone(), "m", Some("second")).unwrap();
        let out = d.path().join("out");
        ops::restore(&repo, "latest", &out, &prog).unwrap();
        assert_eq!(tree(&vault), tree(&out));
    }

    #[test]
    fn tampering_is_detected() {
        let d = tempfile::tempdir().unwrap();
        let vault = d.path().join("v");
        make_vault(&vault);
        let mem = Arc::new(MemBackend::default());
        let repo = enc_repo(&mem);
        let prog = Progress::default();
        run_backup(&repo, &vault, &[], 3, &prog).unwrap();
        // flip one byte in a pack: full verify must report corruption, restore must fail
        let pack = mem.keys().into_iter().find(|k| k.ends_with(".pack")).unwrap();
        let mut raw = mem.get_raw(&pack);
        let mid = raw.len() / 2;
        raw[mid] ^= 0xff;
        mem.raw_put(&pack, &raw);
        let repo = Repo::open_with(mem.clone(), "m", Some("pw")).unwrap();
        assert!(!ops::verify(&repo, true, &prog).unwrap().ok);
        let rep = ops::restore(&repo, "latest", &d.path().join("out"), &prog).unwrap();
        assert!(!rep.errors.is_empty(), "restore must report the damaged file");
    }

    #[test]
    fn plain_chunks_are_rejected_in_encrypted_repo() {
        let mem = Arc::new(MemBackend::default());
        let repo = enc_repo(&mem);
        assert!(repo.decode(&[0, 1, 2, 3], "id").is_err());
        let plain = Repo::init_with(Arc::new(MemBackend::default())).unwrap();
        assert!(plain.decode(&[2, 1, 2, 3], "id").is_err());
    }
}
