//! Copy snapshots from one archive to another, re-encoding every chunk for the destination.
//! This is how a plain archive becomes an encrypted one (chunk ids are keyed per archive, so
//! chunks cannot just be moved), and it doubles as an incremental mirror: copying again into
//! an existing destination only adds snapshots it does not have yet.

use crate::backup::{compressible, Progress};
use crate::repo::{FileEntry, Repo, Snapshot};
use anyhow::{bail, Result};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Mutex;

#[derive(Serialize, Default, Clone, Debug)]
pub struct CopyReport {
    pub copied: u64,
    pub skipped: u64,
    pub files: u64,
    /// bytes actually written to the destination (compressed, encrypted)
    pub written: u64,
}

/// Copy every snapshot of `src` that `dst` does not have yet, oldest first.
/// A snapshot becomes visible in `dst` only after all its data is written, so an interrupted
/// copy can simply be run again.
pub fn copy_repo(src: &Repo, dst: &Repo, level: i32, prog: &Progress) -> Result<CopyReport> {
    let _src_lock = src.lock()?;
    let _dst_lock = dst.lock()?;
    let have: HashSet<String> = dst.list_meta()?.into_iter().map(|m| m.id).collect();
    let todo: Vec<_> = src.list_meta()?.into_iter().filter(|m| !have.contains(&m.id)).collect();
    let mut rep = CopyReport { skipped: (have.len() as u64).min(src.list_meta()?.len() as u64), ..Default::default() };

    prog.reset(&tr!("phase.copying"));
    let mut snaps = Vec::with_capacity(todo.len());
    let mut total = 0u64;
    for m in &todo {
        let s = src.load_snapshot(&m.id)?;
        total += s.entries.len() as u64;
        snaps.push(s);
    }
    prog.total.store(total, Relaxed);

    // old chunk id -> new chunk id, shared across snapshots so shared data is processed once
    let map: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
    for s in snaps {
        if prog.cancel.load(Relaxed) {
            bail!(tr!("rs.cancelled"));
        }
        let before = prog.stored.load(Relaxed);
        let entries: Vec<Result<FileEntry>> = s
            .entries
            .par_iter()
            .map(|e| {
                if prog.cancel.load(Relaxed) {
                    bail!(tr!("rs.cancelled"));
                }
                let comp = compressible(&e.path);
                let mut chunks = Vec::with_capacity(e.chunks.len());
                for old in &e.chunks {
                    let known = map.lock().unwrap().get(old).cloned();
                    let new = match known {
                        Some(n) => n,
                        None => {
                            // verify=true: a damaged source chunk stops the copy instead of spreading
                            let data = src.get_chunk(old, true).map_err(|err| anyhow::anyhow!("{}: {err}", e.path))?;
                            let (h, stored, _) = dst.put_chunk(&data, comp, level)?;
                            prog.stored.fetch_add(stored, Relaxed);
                            prog.bytes.fetch_add(data.len() as u64, Relaxed);
                            map.lock().unwrap().insert(old.clone(), h.clone());
                            h
                        }
                    };
                    chunks.push(new);
                }
                prog.done.fetch_add(1, Relaxed);
                Ok(FileEntry { path: e.path.clone(), size: e.size, mtime: e.mtime, chunks })
            })
            .collect();
        let entries = entries.into_iter().collect::<Result<Vec<_>>>()?;
        dst.flush()?;
        let mut meta = s.meta.clone();
        meta.added_bytes = prog.stored.load(Relaxed).saturating_sub(before);
        rep.written += meta.added_bytes;
        rep.files += entries.len() as u64;
        // snapshot last: it exists in the destination only when complete
        dst.save_snapshot(&Snapshot { meta, entries, dirs: s.dirs })?;
        rep.copied += 1;
    }
    Ok(rep)
}
