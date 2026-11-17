use crate::backup::Progress;
use crate::repo::{FileEntry, Repo};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Datelike, FixedOffset};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Mutex;

/// Reader che concatena i chunk di un file.
pub struct ChunkReader<'a> {
    repo: &'a Repo,
    chunks: &'a [String],
    next: usize,
    buf: Vec<u8>,
    pos: usize,
}
impl<'a> ChunkReader<'a> {
    pub fn new(repo: &'a Repo, chunks: &'a [String]) -> Self {
        Self { repo, chunks, next: 0, buf: vec![], pos: 0 }
    }
}
impl Read for ChunkReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.buf.len() {
            if self.next >= self.chunks.len() {
                return Ok(0);
            }
            self.buf = self
                .repo
                .get_chunk(&self.chunks[self.next], false)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
            self.pos = 0;
            self.next += 1;
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn safe_join(base: &Path, rel: &str) -> Result<PathBuf> {
    let mut p = base.to_path_buf();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains('\\') || part.contains(':') && cfg!(windows) {
            bail!(tr!("rs.unsafe_path", p = rel));
        }
        p.push(part);
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        bail!(tr!("rs.unsafe_path", p = rel));
    }
    Ok(p)
}

#[derive(Serialize, Default)]
pub struct RestoreReport {
    pub files: u64,
    pub bytes: u64,
    pub errors: Vec<String>,
}

pub fn restore(repo: &Repo, spec: &str, dest: &Path, prog: &Progress) -> Result<RestoreReport> {
    let snap = repo.load_snapshot(spec)?;
    if dest.exists() && fs::read_dir(dest)?.next().is_some() {
        bail!(tr!("rs.dest_not_empty", p = dest.display()));
    }
    fs::create_dir_all(dest)?;
    for d in &snap.dirs {
        fs::create_dir_all(safe_join(dest, d)?)?;
    }
    prog.reset(&tr!("phase.restoring"));
    prog.total.store(snap.entries.len() as u64, Relaxed);
    let errors = Mutex::new(vec![]);
    snap.entries.par_iter().for_each(|e| {
        let r = (|| -> Result<()> {
            let target = safe_join(dest, &e.path)?;
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut w = BufWriter::with_capacity(1 << 20, File::create(&target)?);
            io::copy(&mut ChunkReader::new(repo, &e.chunks), &mut w)?;
            drop(w);
            let t = filetime::FileTime::from_unix_time(e.mtime.div_euclid(1000), (e.mtime.rem_euclid(1000) * 1_000_000) as u32);
            filetime::set_file_mtime(&target, t)?;
            prog.bytes.fetch_add(e.size, Relaxed);
            Ok(())
        })();
        prog.done.fetch_add(1, Relaxed);
        if let Err(err) = r {
            errors.lock().unwrap().push(format!("{}: {err}", e.path));
        }
    });
    Ok(RestoreReport { files: snap.meta.files, bytes: snap.meta.bytes, errors: errors.into_inner().unwrap() })
}

pub fn export_tar_zst(repo: &Repo, spec: &str, out: &Path, prog: &Progress) -> Result<u64> {
    let snap = repo.load_snapshot(spec)?;
    prog.reset(&tr!("phase.exporting"));
    prog.total.store(snap.entries.len() as u64, Relaxed);
    let f = BufWriter::new(File::create(out).with_context(|| tr!("rs.cannot_create", p = out.display()))?);
    let mut enc = zstd::stream::Encoder::new(f, 10)?;
    enc.multithread(num_threads())?;
    {
        let mut tb = tar::Builder::new(&mut enc);
        for e in &snap.entries {
            safe_join(Path::new("/x"), &e.path)?;
            let mut h = tar::Header::new_gnu();
            h.set_size(e.size);
            h.set_mode(0o644);
            h.set_mtime((e.mtime / 1000).max(0) as u64);
            h.set_cksum();
            tb.append_data(&mut h, &e.path, ChunkReader::new(repo, &e.chunks))?;
            prog.done.fetch_add(1, Relaxed);
            prog.bytes.fetch_add(e.size, Relaxed);
        }
        tb.finish()?;
    }
    enc.finish()?.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(fs::metadata(out)?.len())
}

fn num_threads() -> u32 {
    std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(2).min(8)
}

#[derive(Serialize, Default)]
pub struct VerifyReport {
    pub snapshots: u64,
    pub chunks_checked: u64,
    pub missing: Vec<String>,
    pub corrupt: Vec<String>,
    pub ok: bool,
}

/// `full`: rilegge e rifà l'hash di ogni chunk. Altrimenti controlla solo la presenza.
pub fn verify(repo: &Repo, full: bool, prog: &Progress) -> Result<VerifyReport> {
    prog.reset(&tr!("phase.verifying"));
    let metas = repo.list_meta()?;
    let mut referenced: HashSet<String> = HashSet::new();
    let mut missing = vec![];
    for m in &metas {
        let s = repo.load_snapshot(&m.id)?;
        for e in &s.entries {
            for h in &e.chunks {
                if referenced.insert(h.clone()) && !repo.has_chunk(h) {
                    missing.push(tr!("rs.missing_ref", h = h, id = m.id, file = e.path));
                }
            }
        }
    }
    let mut rep = VerifyReport { snapshots: metas.len() as u64, missing, ..Default::default() };
    if full {
        let hashes: Vec<_> = referenced.iter().filter(|h| repo.has_chunk(h)).cloned().collect();
        prog.total.store(hashes.len() as u64, Relaxed);
        let bad: Vec<String> = hashes
            .par_iter()
            .filter_map(|h| {
                let r = repo.get_chunk(h, true);
                prog.done.fetch_add(1, Relaxed);
                r.err().map(|e| format!("{h}: {e}"))
            })
            .collect();
        rep.chunks_checked = hashes.len() as u64;
        rep.corrupt = bad;
    } else {
        rep.chunks_checked = referenced.len() as u64;
    }
    rep.ok = rep.missing.is_empty() && rep.corrupt.is_empty();
    Ok(rep)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Retention {
    pub keep_last: u32,
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
}
impl Default for Retention {
    fn default() -> Self {
        Self { keep_last: 5, keep_daily: 7, keep_weekly: 4, keep_monthly: 12 }
    }
}

#[derive(Serialize, Default)]
pub struct PruneReport {
    pub removed_snapshots: Vec<String>,
    pub kept: u64,
    pub freed_bytes: u64,
}

/// Calcola gli snapshot da tenere. `metas` ordinati dal più vecchio al più recente.
pub fn select_keep(metas: &[(String, String)], r: &Retention) -> HashSet<String> {
    let mut keep = HashSet::new();
    let parsed: Vec<(String, DateTime<FixedOffset>)> = metas
        .iter()
        .rev()
        .filter_map(|(id, c)| DateTime::parse_from_rfc3339(c).ok().map(|d| (id.clone(), d)))
        .collect(); // dal più recente
    for (id, _) in parsed.iter().take(r.keep_last as usize) {
        keep.insert(id.clone());
    }
    let mut bucket = |n: u32, key: &dyn Fn(&DateTime<FixedOffset>) -> String| {
        let mut seen = HashSet::new();
        for (id, d) in &parsed {
            let k = key(d);
            if seen.contains(&k) {
                continue;
            }
            if seen.len() as u32 >= n {
                break;
            }
            seen.insert(k);
            keep.insert(id.clone());
        }
    };
    bucket(r.keep_daily, &|d| d.format("%Y-%m-%d").to_string());
    bucket(r.keep_weekly, &|d| format!("{}-{}", d.iso_week().year(), d.iso_week().week()));
    bucket(r.keep_monthly, &|d| d.format("%Y-%m").to_string());
    if let Some((id, _)) = parsed.first() {
        keep.insert(id.clone()); // l'ultimo snapshot non si tocca mai
    }
    keep
}

pub fn gc(repo: &Repo) -> Result<u64> {
    let mut referenced: HashSet<String> = HashSet::new();
    for m in repo.list_meta()? {
        for e in repo.load_snapshot(&m.id)?.entries {
            referenced.extend(e.chunks);
        }
    }
    repo.compact(&referenced)
}

pub fn prune(repo: &Repo, r: &Retention, prog: &Progress) -> Result<PruneReport> {
    let _lock = repo.lock()?;
    prog.reset(&tr!("phase.pruning"));
    let metas = repo.list_meta()?;
    let pairs: Vec<_> = metas.iter().map(|m| (m.id.clone(), m.created.clone())).collect();
    let keep = select_keep(&pairs, r);
    let mut rep = PruneReport { kept: keep.len() as u64, ..Default::default() };
    for m in &metas {
        if !keep.contains(&m.id) {
            repo.delete_snapshot(&m.id)?;
            rep.removed_snapshots.push(m.id.clone());
        }
    }
    prog.set_phase(&tr!("phase.reclaiming"));
    rep.freed_bytes = gc(repo)?;
    Ok(rep)
}

pub fn forget(repo: &Repo, spec: &str) -> Result<String> {
    let _lock = repo.lock()?;
    let id = repo.delete_snapshot(spec)?;
    gc(repo)?;
    Ok(id)
}

#[allow(dead_code)]
pub fn total_logical(entries: &[FileEntry]) -> u64 {
    entries.iter().map(|e| e.size).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snaps(days: &[(u32, u32)]) -> Vec<(String, String)> {
        // (giorno di novembre, ora) -> id uguale al timestamp; ordine dal più vecchio
        days.iter()
            .map(|(d, h)| {
                let t = format!("2026-11-{:02}T{:02}:00:00.000+01:00", d, h);
                (t.clone(), t)
            })
            .collect()
    }

    #[test]
    fn keeps_latest_and_one_per_day() {
        let m = snaps(&[(1, 9), (1, 18), (2, 9), (2, 18), (3, 9)]);
        let r = Retention { keep_last: 1, keep_daily: 2, keep_weekly: 0, keep_monthly: 0 };
        let k = select_keep(&m, &r);
        // ultimo (3/11) + il più recente del giorno prima (2/11 18:00)
        assert!(k.contains(&m[4].0) && k.contains(&m[3].0));
        assert_eq!(k.len(), 2);
    }

    #[test]
    fn never_deletes_latest_even_with_empty_policy() {
        let m = snaps(&[(1, 9), (2, 9)]);
        let r = Retention { keep_last: 0, keep_daily: 0, keep_weekly: 0, keep_monthly: 0 };
        let k = select_keep(&m, &r);
        assert_eq!(k.len(), 1);
        assert!(k.contains(&m[1].0));
    }

    #[test]
    fn rejects_path_traversal() {
        assert!(safe_join(Path::new("/dest"), "../etc/passwd").is_err());
        assert!(safe_join(Path::new("/dest"), "a/../../b").is_err());
        assert!(safe_join(Path::new("/dest"), "Note/ok.md").is_ok());
    }
}
