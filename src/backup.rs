use crate::repo::{FileEntry, Meta, Repo, Snapshot};
use chrono::SecondsFormat;
use anyhow::{anyhow, bail, Result};
use fastcdc::v2020::StreamCDC;
use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::Instant;
use walkdir::WalkDir;

const CDC_MIN: u32 = 16 * 1024;
const CDC_AVG: u32 = 64 * 1024;
const CDC_MAX: u32 = 256 * 1024;

/// Formati già compressi: provare zstd è tempo perso.
const INCOMPRESSIBLE: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "avif", "mp4", "m4v", "mov", "mkv", "webm", "avi", "mp3",
    "m4a", "aac", "ogg", "opus", "flac", "zip", "gz", "bz2", "xz", "7z", "rar", "zst", "docx", "xlsx", "pptx",
];

#[derive(Default)]
pub struct Progress {
    pub phase: Mutex<String>,
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub bytes: AtomicU64,
    pub stored: AtomicU64,
    pub cancel: AtomicBool,
}

impl Progress {
    pub fn reset(&self, phase: &str) {
        *self.phase.lock().unwrap() = phase.to_string();
        for a in [&self.total, &self.done, &self.bytes, &self.stored] {
            a.store(0, Relaxed);
        }
        self.cancel.store(false, Relaxed);
    }
    pub fn set_phase(&self, p: &str) {
        *self.phase.lock().unwrap() = p.to_string();
    }
}

#[derive(Serialize, Default, Clone)]
pub struct BackupReport {
    pub snapshot: Option<String>,
    pub unchanged: bool,
    pub files: u64,
    pub bytes: u64,
    pub new_stored: u64,
    pub reused_files: u64,
    pub warnings: Vec<String>,
    pub secs: f64,
}

pub fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let p = p.trim().trim_matches('/');
        if p.is_empty() {
            continue;
        }
        for g in [p.to_string(), format!("{p}/**"), format!("**/{p}"), format!("**/{p}/**")] {
            b.add(Glob::new(&g).map_err(|e| anyhow!(tr!("rs.bad_pattern", p = p, e = e)))?);
        }
    }
    Ok(b.build()?)
}

fn rel_string(rel: &Path) -> Option<String> {
    let mut parts = vec![];
    for c in rel.components() {
        parts.push(c.as_os_str().to_str()?.to_string());
    }
    Some(parts.join("/"))
}

fn mtime_ms(m: &std::fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn chunk_file(repo: &Repo, path: &Path, level: i32, prog: &Progress) -> Result<(Vec<String>, u64)> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let compress = !INCOMPRESSIBLE.contains(&ext.as_str());
    let reader = BufReader::with_capacity(1 << 20, File::open(path)?);
    let mut hashes = vec![];
    let mut total = 0u64;
    for c in StreamCDC::new(reader, CDC_MIN, CDC_AVG, CDC_MAX) {
        let c = c.map_err(|e| anyhow!("{e}"))?;
        let (h, stored, _) = repo.put_chunk(&c.data, compress, level)?;
        total += c.data.len() as u64;
        prog.bytes.fetch_add(c.data.len() as u64, Relaxed);
        prog.stored.fetch_add(stored, Relaxed);
        hashes.push(h);
    }
    Ok((hashes, total))
}

pub fn run_backup(repo: &Repo, vault: &Path, exclude: &[String], level: i32, prog: &Progress) -> Result<BackupReport> {
    let t0 = Instant::now();
    if !vault.is_dir() {
        bail!(tr!("rs.vault_missing", p = vault.display()));
    }
    let _lock = repo.lock()?;
    prog.reset(&tr!("phase.scanning"));

    let (prev, prev_dirs): (HashMap<String, FileEntry>, Vec<String>) = match repo.latest_snapshot()? {
        Some(s) => (s.entries.into_iter().map(|e| (e.path.clone(), e)).collect(), s.dirs),
        None => (HashMap::new(), vec![]),
    };
    let prev_list = {
        let mut v: Vec<_> = prev.values().cloned().collect();
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    };

    let gs = build_globset(exclude)?;
    let mut files: Vec<(String, PathBuf, u64, i64)> = vec![];
    let mut dirs: Vec<String> = vec![];
    let mut warnings = vec![];
    let walker = WalkDir::new(vault).follow_links(false).into_iter().filter_entry(|e| {
        e.depth() == 0 || e.path().strip_prefix(vault).map_or(true, |r| !gs.is_match(r))
    });
    for ent in walker {
        match ent {
            Ok(e) if e.file_type().is_file() => {
                let rel = e.path().strip_prefix(vault).unwrap();
                match (rel_string(rel), e.metadata()) {
                    (Some(r), Ok(m)) => files.push((r, e.path().to_path_buf(), m.len(), mtime_ms(&m))),
                    (None, _) => warnings.push(tr!("rs.non_utf8", p = rel.display())),
                    (_, Err(err)) => warnings.push(format!("{}: {err}", rel.display())),
                }
            }
            Ok(e) if e.file_type().is_dir() && e.depth() > 0 => {
                if let Some(r) = rel_string(e.path().strip_prefix(vault).unwrap()) {
                    dirs.push(r);
                }
            }
            Ok(_) => {}
            Err(err) => warnings.push(err.to_string()),
        }
    }
    prog.total.store(files.len() as u64, Relaxed);
    prog.set_phase(&tr!("phase.backing_up"));

    let reused = AtomicU64::new(0);
    let warn_m = Mutex::new(vec![]);
    let results: Vec<Option<FileEntry>> = files
        .par_iter()
        .map(|(rel, path, size, mtime)| {
            if prog.cancel.load(Relaxed) {
                return None;
            }
            let r = (|| -> Result<FileEntry> {
                if let Some(p) = prev.get(rel) {
                    if p.size == *size && p.mtime == *mtime && p.chunks.iter().all(|h| repo.has_chunk(h)) {
                        reused.fetch_add(1, Relaxed);
                        prog.bytes.fetch_add(*size, Relaxed);
                        return Ok(p.clone());
                    }
                }
                let (chunks, total) = chunk_file(repo, path, level, prog)?;
                Ok(FileEntry { path: rel.clone(), size: total, mtime: *mtime, chunks })
            })();
            prog.done.fetch_add(1, Relaxed);
            match r {
                Ok(e) => Some(e),
                Err(err) => {
                    warn_m.lock().unwrap().push(format!("{rel}: {err}"));
                    None
                }
            }
        })
        .collect();

    repo.flush()?;
    if prog.cancel.load(Relaxed) {
        bail!(tr!("rs.cancelled"));
    }
    warnings.extend(warn_m.into_inner().unwrap());
    let mut entries: Vec<FileEntry> = results.into_iter().flatten().collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let bytes: u64 = entries.iter().map(|e| e.size).sum();
    let stored = prog.stored.load(Relaxed);
    let mut rep = BackupReport {
        files: entries.len() as u64,
        bytes,
        new_stored: stored,
        reused_files: reused.load(Relaxed),
        warnings,
        ..Default::default()
    };

    dirs.sort();
    if !prev_list.is_empty() && prev_list == entries && prev_dirs == dirs {
        rep.unchanged = true;
        rep.secs = t0.elapsed().as_secs_f64();
        return Ok(rep);
    }

    let now = chrono::Local::now();
    let id = repo.unique_id(&now.format("%Y%m%d-%H%M%S").to_string());
    let snap = Snapshot {
        meta: Meta {
            id: id.clone(),
            created: now.to_rfc3339_opts(SecondsFormat::Millis, false),
            vault: vault.display().to_string(),
            files: rep.files,
            bytes,
            added_bytes: stored,
        },
        entries,
        dirs,
    };
    repo.save_snapshot(&snap)?;
    rep.snapshot = Some(id);
    rep.secs = t0.elapsed().as_secs_f64();
    Ok(rep)
}
