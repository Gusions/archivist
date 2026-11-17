//! Repository content-addressed: chunk deduplicati e compressi, raccolti in pack.
//!
//! Layout su disco:
//!   <repo>/format                  marker di versione
//!   <repo>/packs/<name>.pack       chunk concatenati: [tag][payload] (tag 0 = raw, 1 = zstd)
//!   <repo>/packs/<name>.idx        indice JSON del pack (scritto SOLO a pack completo)
//!   <repo>/snapshots/<id>.meta.json  metadati leggeri
//!   <repo>/snapshots/<id>.snap.zst   elenco file -> chunk (JSON compresso)
//!
//! Un pack senza .idx è considerato scarto di un backup interrotto e viene ignorato.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

const FORMAT: &str = "archivist-repo-v1";
const PACK_TARGET: u64 = 64 * 1024 * 1024;

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
    w: BufWriter<File>,
    off: u64,
    entries: Vec<(String, u64, u32)>,
}

pub struct Repo {
    pub root: PathBuf,
    packs: RwLock<Vec<String>>,
    index: RwLock<HashMap<String, Loc>>,
    writer: Mutex<Option<OpenPack>>,
}

/// How often the lock owner touches the lock file, and how long a lock may go
/// untouched before it is considered abandoned (crashed process).
const LOCK_HEARTBEAT: Duration = Duration::from_secs(15);
const LOCK_STALE: Duration = Duration::from_secs(120);

/// Exclusive repository lock. A background heartbeat keeps the file fresh while
/// the owner is alive, so a long backup is never mistaken for a dead one.
pub struct RepoLock {
    path: PathBuf,
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
        if fs::read_to_string(&self.path).is_ok_and(|c| c == self.token) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Write a file atomically and durably: temp file, fsync, rename, fsync of the
/// parent directory. A crash leaves either the old file or the new one, never a
/// partial one.
pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("dat")
    ));
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

#[derive(Serialize, Clone, Debug, Default)]
pub struct RepoStats {
    pub disk_bytes: u64,
    pub chunks: u64,
    pub packs: u64,
    pub snapshots: u64,
}

impl Repo {
    pub fn is_repo(root: &Path) -> bool {
        root.join("format").is_file()
    }

    pub fn init(root: &Path) -> Result<Repo> {
        if !Self::is_repo(root) {
            if root.is_dir() && fs::read_dir(root)?.next().is_some() {
                bail!(tr!("rs.repo_not_empty", path = root.display()));
            }
            fs::create_dir_all(root)?;
        }
        fs::create_dir_all(root.join("packs"))?;
        fs::create_dir_all(root.join("snapshots"))?;
        // the format marker last: its presence means the layout is complete
        if !Self::is_repo(root) {
            write_atomic(&root.join("format"), FORMAT.as_bytes())?;
        }
        Self::open(root)
    }

    pub fn open_or_init(root: &Path) -> Result<Repo> {
        if Self::is_repo(root) {
            Self::open(root)
        } else {
            Self::init(root)
        }
    }

    pub fn open(root: &Path) -> Result<Repo> {
        let f = fs::read_to_string(root.join("format"))
            .with_context(|| tr!("rs.not_a_repo", path = root.display()))?;
        if f.trim() != FORMAT {
            bail!(tr!("rs.unsupported_format", v = f.trim()));
        }
        let repo = Repo {
            root: root.to_path_buf(),
            packs: RwLock::new(vec![]),
            index: RwLock::new(HashMap::new()),
            writer: Mutex::new(None),
        };
        repo.load_index()?;
        Ok(repo)
    }

    fn load_index(&self) -> Result<()> {
        let mut packs = self.packs.write().unwrap();
        let mut index = self.index.write().unwrap();
        for e in fs::read_dir(self.root.join("packs"))? {
            let p = e?.path();
            if p.extension().and_then(|s| s.to_str()) != Some("idx") {
                continue;
            }
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            if !self.pack_path(&name).is_file() {
                continue;
            }
            let idx: PackIndex = serde_json::from_slice(&fs::read(&p)?)
                .with_context(|| tr!("rs.corrupt_index", path = p.display()))?;
            let pi = packs.len() as u32;
            packs.push(name);
            for (h, off, len) in idx.chunks {
                index.entry(h).or_insert(Loc { pack: pi, off, len });
            }
        }
        Ok(())
    }

    fn pack_path(&self, name: &str) -> PathBuf {
        self.root.join("packs").join(format!("{name}.pack"))
    }
    fn idx_path(&self, name: &str) -> PathBuf {
        self.root.join("packs").join(format!("{name}.idx"))
    }

    pub fn lock(&self) -> Result<RepoLock> {
        let p = self.root.join("lock");
        if let Ok(m) = fs::metadata(&p) {
            let stale = m
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| d > LOCK_STALE)
                .unwrap_or(false);
            if stale {
                let _ = fs::remove_file(&p);
            }
        }
        let token = format!("{}:{:016x}", std::process::id(), rand::random::<u64>());
        match OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(mut f) => {
                f.write_all(token.as_bytes())?;
                f.sync_all()?;
            }
            Err(_) => bail!(tr!("rs.repo_busy")),
        }
        let (tx, rx) = mpsc::channel::<()>();
        let hb_path = p.clone();
        let beat = std::thread::spawn(move || {
            // recv_timeout returns Err(Disconnected) as soon as the lock is dropped
            while let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(LOCK_HEARTBEAT) {
                if let Ok(f) = OpenOptions::new().write(true).open(&hb_path) {
                    let _ = f.set_modified(std::time::SystemTime::now());
                }
            }
        });
        Ok(RepoLock { path: p, token, stop: Some(tx), beat: Some(beat) })
    }

    pub fn has_chunk(&self, h: &str) -> bool {
        self.index.read().unwrap().contains_key(h)
    }

    fn encode(data: &[u8], try_compress: bool, level: i32) -> Result<Vec<u8>> {
        if try_compress {
            let c = zstd::bulk::compress(data, level)?;
            // tieni la versione compressa solo se risparmia almeno il 3%
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

    /// Salva un chunk. Ritorna (hash, byte scritti, nuovo?).
    pub fn put_chunk(&self, data: &[u8], try_compress: bool, level: i32) -> Result<(String, u64, bool)> {
        let hash = blake3::hash(data).to_hex().to_string();
        if self.has_chunk(&hash) {
            return Ok((hash, 0, false));
        }
        let enc = Self::encode(data, try_compress, level)?;
        let stored = self.append_raw(&hash, &enc)?;
        Ok((hash, stored, stored > 0))
    }

    /// Scrive i byte già codificati nel pack corrente. 0 se il chunk esiste già.
    fn append_raw(&self, hash: &str, enc: &[u8]) -> Result<u64> {
        let mut guard = self.writer.lock().unwrap();
        if self.has_chunk(hash) {
            return Ok(0);
        }
        if guard.is_none() {
            let name = format!("pack-{:016x}", rand::random::<u64>());
            let f = File::create(self.pack_path(&name))?;
            *guard = Some(OpenPack { name, w: BufWriter::with_capacity(1 << 20, f), off: 0, entries: vec![] });
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
        pk.w.write_all(enc)?;
        let loc = Loc { pack: pi, off: pk.off, len: enc.len() as u32 };
        pk.entries.push((hash.to_string(), pk.off, enc.len() as u32));
        pk.off += enc.len() as u64;
        self.index.write().unwrap().insert(hash.to_string(), loc);
        if pk.off >= PACK_TARGET {
            let done = guard.take().unwrap();
            self.finalize(done)?;
        }
        Ok(enc.len() as u64)
    }

    fn finalize(&self, mut pk: OpenPack) -> Result<()> {
        pk.w.flush()?;
        pk.w.get_ref().sync_all()?;
        if pk.entries.is_empty() {
            drop(pk.w);
            let _ = fs::remove_file(self.pack_path(&pk.name));
            return Ok(());
        }
        drop(pk.w);
        // pack content is already on disk; make its directory entry durable too,
        // then publish the index atomically (the index makes the pack visible)
        sync_dir(&self.root.join("packs"));
        write_atomic(&self.idx_path(&pk.name), &serde_json::to_vec(&PackIndex { chunks: pk.entries })?)?;
        Ok(())
    }

    /// Chiude il pack in scrittura. Da chiamare a fine backup.
    pub fn flush(&self) -> Result<()> {
        let mut guard = self.writer.lock().unwrap();
        if let Some(pk) = guard.take() {
            self.finalize(pk)?;
        }
        Ok(())
    }

    fn read_raw(&self, loc: Loc) -> Result<Vec<u8>> {
        let name = self.packs.read().unwrap()[loc.pack as usize].clone();
        let mut f = File::open(self.pack_path(&name))?;
        f.seek(SeekFrom::Start(loc.off))?;
        let mut buf = vec![0u8; loc.len as usize];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn decode(raw: &[u8]) -> Result<Vec<u8>> {
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
        let data = Self::decode(&self.read_raw(loc)?)?;
        if verify && blake3::hash(&data).to_hex().as_str() != hash {
            bail!(tr!("rs.chunk_corrupt", h = hash));
        }
        Ok(data)
    }

    // ---------- snapshot ----------

    fn snap_dir(&self) -> PathBuf {
        self.root.join("snapshots")
    }

    pub fn list_meta(&self) -> Result<Vec<Meta>> {
        let mut v = vec![];
        for e in fs::read_dir(self.snap_dir())? {
            let p = e?.path();
            if p.file_name().and_then(|s| s.to_str()).map_or(false, |n| n.ends_with(".meta.json")) {
                if let Ok(m) = serde_json::from_slice::<Meta>(&fs::read(&p)?) {
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
        let raw = fs::read(self.snap_dir().join(format!("{id}.snap.zst")))?;
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
        while self.snap_dir().join(format!("{id}.meta.json")).exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    pub fn save_snapshot(&self, s: &Snapshot) -> Result<()> {
        let id = &s.meta.id;
        let z = zstd::stream::encode_all(&serde_json::to_vec(s)?[..], 9)?;
        write_atomic(&self.snap_dir().join(format!("{id}.snap.zst")), &z)?;
        // meta last: the snapshot "exists" only when it is complete
        write_atomic(&self.snap_dir().join(format!("{id}.meta.json")), &serde_json::to_vec_pretty(&s.meta)?)?;
        Ok(())
    }

    pub fn delete_snapshot(&self, spec: &str) -> Result<String> {
        let id = self.resolve(spec)?;
        fs::remove_file(self.snap_dir().join(format!("{id}.meta.json")))?;
        let _ = fs::remove_file(self.snap_dir().join(format!("{id}.snap.zst")));
        Ok(id)
    }

    // ---------- statistiche e compattazione ----------

    pub fn stats(&self) -> Result<RepoStats> {
        let mut disk = 0;
        let mut packs = 0;
        for e in fs::read_dir(self.root.join("packs"))? {
            let e = e?;
            disk += e.metadata()?.len();
            if e.path().extension().and_then(|s| s.to_str()) == Some("pack") {
                packs += 1;
            }
        }
        for e in fs::read_dir(self.snap_dir())? {
            disk += e?.metadata()?.len();
        }
        Ok(RepoStats {
            disk_bytes: disk,
            chunks: self.index.read().unwrap().len() as u64,
            packs,
            snapshots: self.list_meta()?.len() as u64,
        })
    }

    /// Elimina i chunk non referenziati riscrivendo i pack parzialmente usati.
    /// Ritorna i byte liberati.
    pub fn compact(&self, referenced: &HashSet<String>) -> Result<u64> {
        self.flush()?;
        let before = self.stats()?.disk_bytes;
        let mut names = vec![];
        for e in fs::read_dir(self.root.join("packs"))? {
            let p = e?.path();
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
            let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            match ext {
                "idx" => names.push(stem),
                // pack orfani (backup interrotto) o file temporanei
                "pack" if !self.idx_path(&stem).exists() => {
                    let _ = fs::remove_file(&p);
                }
                "tmp" => {
                    let _ = fs::remove_file(&p);
                }
                _ => {}
            }
        }
        // i pack idx.tmp hanno estensione "tmp" (gestito sopra)
        for name in names {
            let idx: PackIndex = serde_json::from_slice(&fs::read(self.idx_path(&name))?)?;
            let used: Vec<_> = idx.chunks.iter().filter(|(h, _, _)| referenced.contains(h)).collect();
            if used.len() == idx.chunks.len() {
                continue;
            }
            // nuovo pack con i soli chunk usati (scritto PRIMA di cancellare il vecchio)
            let mut newname = None;
            if !used.is_empty() {
                let nn = format!("pack-{:016x}", rand::random::<u64>());
                let mut src = File::open(self.pack_path(&name))?;
                let mut w = BufWriter::new(File::create(self.pack_path(&nn))?);
                let mut off = 0u64;
                let mut entries = vec![];
                for (h, o, l) in &used {
                    src.seek(SeekFrom::Start(*o))?;
                    let mut buf = vec![0u8; *l as usize];
                    src.read_exact(&mut buf)?;
                    w.write_all(&buf)?;
                    entries.push((h.clone(), off, *l));
                    off += *l as u64;
                }
                w.flush()?;
                w.get_ref().sync_all()?;
                drop(w);
                write_atomic(&self.idx_path(&nn), &serde_json::to_vec(&PackIndex { chunks: entries })?)?;
                newname = Some(nn);
            }
            let _ = newname;
            fs::remove_file(self.idx_path(&name))?;
            fs::remove_file(self.pack_path(&name))?;
        }
        // ricarica l'indice
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
    use crate::backup::{run_backup, Progress};
    use crate::ops;

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
        assert!(!repo.root.join("lock").exists());
        assert!(repo.lock().is_ok());
    }

    #[test]
    fn stale_lock_is_taken_over_fresh_one_is_not() {
        let d = tempfile::tempdir().unwrap();
        let repo = Repo::init(&d.path().join("r")).unwrap();
        let p = repo.root.join("lock");
        fs::write(&p, "999999").unwrap();
        assert!(repo.lock().is_err(), "a fresh foreign lock must be respected");
        let old = std::time::SystemTime::now() - Duration::from_secs(3600);
        File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
        assert!(repo.lock().is_ok(), "an abandoned lock must be taken over");
    }

    #[test]
    fn dropping_old_lock_does_not_remove_new_owners_lock() {
        let d = tempfile::tempdir().unwrap();
        let repo = Repo::init(&d.path().join("r")).unwrap();
        let first = repo.lock().unwrap();
        // simulate takeover by another process
        fs::write(repo.root.join("lock"), "other:owner").unwrap();
        drop(first);
        assert!(repo.root.join("lock").exists());
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_tmp() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names.len(), 1);
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
}
