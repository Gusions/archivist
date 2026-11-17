use crate::backup::Progress;
use crate::repo::{FileEntry, Repo, Snapshot};
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

/// Does `path` fall under one of the selectors (an exact file, or a folder and everything in it)?
fn selected(path: &str, only: &[String]) -> bool {
    only.iter().any(|s| {
        let s = s.trim_matches('/');
        !s.is_empty() && (path == s || path.strip_prefix(s).is_some_and(|r| r.starts_with('/')))
    })
}

#[allow(dead_code)] // convenience wrapper, used by tests
pub fn restore(repo: &Repo, spec: &str, dest: &Path, prog: &Progress) -> Result<RestoreReport> {
    restore_selected(repo, spec, dest, None, prog)
}

/// Restore a snapshot, or only the files/folders listed in `only` (paths relative to the vault).
pub fn restore_selected(repo: &Repo, spec: &str, dest: &Path, only: Option<&[String]>, prog: &Progress) -> Result<RestoreReport> {
    let snap = repo.load_snapshot(spec)?;
    if dest.exists() && fs::read_dir(dest)?.next().is_some() {
        bail!(tr!("rs.dest_not_empty", p = dest.display()));
    }
    let entries: Vec<&FileEntry> = match only {
        Some(sel) => snap.entries.iter().filter(|e| selected(&e.path, sel)).collect(),
        None => snap.entries.iter().collect(),
    };
    if only.is_some() && entries.is_empty() {
        bail!(tr!("rs.no_match"));
    }
    fs::create_dir_all(dest)?;
    for d in &snap.dirs {
        if only.is_none_or(|sel| selected(d, sel)) {
            fs::create_dir_all(safe_join(dest, d)?)?;
        }
    }
    prog.reset(&tr!("phase.restoring"));
    prog.total.store(entries.len() as u64, Relaxed);
    let errors = Mutex::new(vec![]);
    entries.par_iter().for_each(|e| {
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
    let (files, bytes) = match only {
        Some(_) => (entries.len() as u64, entries.iter().map(|e| e.size).sum()),
        None => (snap.meta.files, snap.meta.bytes),
    };
    Ok(RestoreReport { files, bytes, errors: errors.into_inner().unwrap() })
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct FileChange {
    pub path: String,
    /// "added" | "modified" | "removed"
    pub kind: &'static str,
    /// size in the newer snapshot (the older one for removed files)
    pub size: u64,
}

/// What changed between an older and a newer snapshot. Compares content (chunk lists), not
/// modification times, so a file that was only touched does not show up.
pub fn diff_snapshots(old: &Snapshot, new: &Snapshot) -> Vec<FileChange> {
    use std::collections::HashMap;
    let o: HashMap<&str, &FileEntry> = old.entries.iter().map(|e| (e.path.as_str(), e)).collect();
    let n: HashMap<&str, &FileEntry> = new.entries.iter().map(|e| (e.path.as_str(), e)).collect();
    let mut v = vec![];
    for (p, e) in &n {
        match o.get(p) {
            None => v.push(FileChange { path: p.to_string(), kind: "added", size: e.size }),
            Some(x) if x.chunks != e.chunks => v.push(FileChange { path: p.to_string(), kind: "modified", size: e.size }),
            _ => {}
        }
    }
    for (p, e) in &o {
        if !n.contains_key(p) {
            v.push(FileChange { path: p.to_string(), kind: "removed", size: e.size });
        }
    }
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}

/// Files whose path contains `query` (case-insensitive). Returns (total matches, first `limit`).
pub fn find_files<'a>(snap: &'a Snapshot, query: &str, limit: usize) -> (usize, Vec<&'a FileEntry>) {
    let q = query.trim().to_lowercase();
    let all: Vec<&FileEntry> = snap.entries.iter().filter(|e| q.is_empty() || e.path.to_lowercase().contains(&q)).collect();
    let total = all.len();
    (total, all.into_iter().take(limit).collect())
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

    fn entry(p: &str, chunks: &[&str]) -> FileEntry {
        FileEntry { path: p.into(), size: 10, mtime: 0, chunks: chunks.iter().map(|c| c.to_string()).collect() }
    }
    fn snap(entries: Vec<FileEntry>) -> Snapshot {
        use crate::repo::Meta;
        Snapshot {
            meta: Meta { id: "x".into(), created: "2026-01-01T00:00:00.000+00:00".into(), vault: "v".into(), files: entries.len() as u64, bytes: 0, added_bytes: 0 },
            entries,
            dirs: vec![],
        }
    }

    #[test]
    fn selectors_match_files_and_folders_only_on_boundaries() {
        let sel = vec!["Notes".to_string(), "a/b.md".to_string()];
        assert!(selected("Notes/x.md", &sel) && selected("Notes", &sel) && selected("a/b.md", &sel));
        assert!(!selected("Notes2/x.md", &sel), "prefix must end at a folder boundary");
        assert!(!selected("a/b.md.bak", &sel));
        assert!(!selected("anything", &["".to_string(), "/".to_string()]));
    }

    #[test]
    fn diff_reports_added_modified_removed_and_ignores_touched() {
        let old = snap(vec![entry("keep.md", &["1"]), entry("edit.md", &["2"]), entry("gone.md", &["3"])]);
        let mut touched = entry("keep.md", &["1"]);
        touched.mtime = 999;
        let new = snap(vec![touched, entry("edit.md", &["2", "9"]), entry("new.md", &["4"])]);
        let d = diff_snapshots(&old, &new);
        let got: Vec<_> = d.iter().map(|c| (c.path.as_str(), c.kind)).collect();
        assert_eq!(got, vec![("edit.md", "modified"), ("gone.md", "removed"), ("new.md", "added")]);
    }

    #[test]
    fn find_files_is_case_insensitive_and_limited() {
        let s = snap(vec![entry("Notes/Idea.md", &["1"]), entry("notes/other.md", &["2"]), entry("img.png", &["3"])]);
        assert_eq!(find_files(&s, "NOTES", 10).0, 2);
        let (total, first) = find_files(&s, "", 1);
        assert_eq!((total, first.len()), (3, 1));
        assert_eq!(find_files(&s, "zzz", 10).0, 0);
    }
}

