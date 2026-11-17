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
use std::sync::{Mutex, RwLock};

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

pub struct RepoLock(PathBuf);
impl Drop for RepoLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
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
    pub fn is_repo(root: &Path) -> bool {
        root.join("format").is_file()
    }

    pub fn init(root: &Path) -> Result<Repo> {
        if !Self::is_repo(root) {
            if root.is_dir() && fs::read_dir(root)?.next().is_some() {
                bail!(tr!("rs.repo_not_empty", path = root.display()));
            }
            fs::create_dir_all(root)?;
            fs::write(root.join("format"), FORMAT)?;
        }
        fs::create_dir_all(root.join("packs"))?;
        fs::create_dir_all(root.join("snapshots"))?;
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
                .map(|d| d.as_secs() > 12 * 3600)
                .unwrap_or(false);
            if stale {
                let _ = fs::remove_file(&p);
            }
        }
        match OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(mut f) => {
                let _ = write!(f, "{}", std::process::id());
                Ok(RepoLock(p))
            }
            Err(_) => bail!(tr!("rs.repo_busy")),
        }
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
        let tmp = self.root.join("packs").join(format!("{}.idx.tmp", pk.name));
        fs::write(&tmp, serde_json::to_vec(&PackIndex { chunks: pk.entries })?)?;
        fs::rename(&tmp, self.idx_path(&pk.name))?;
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
        let tmp = self.snap_dir().join(format!("{id}.snap.tmp"));
        fs::write(&tmp, z)?;
        fs::rename(&tmp, self.snap_dir().join(format!("{id}.snap.zst")))?;
        // il meta per ultimo: lo snapshot "esiste" solo quando è completo
        let tmp = self.snap_dir().join(format!("{id}.meta.tmp"));
        fs::write(&tmp, serde_json::to_vec_pretty(&s.meta)?)?;
        fs::rename(&tmp, self.snap_dir().join(format!("{id}.meta.json")))?;
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
                fs::write(self.idx_path(&nn), serde_json::to_vec(&PackIndex { chunks: entries })?)?;
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
