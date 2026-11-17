#[macro_use]
mod i18n;
mod backend;
mod backup;
mod ops;
mod repo;
mod server;
mod settings;
#[cfg(feature = "window")]
mod window;

use anyhow::{bail, Result};
use backup::Progress;
use clap::{Parser, Subcommand};
use repo::Repo;
use settings::Settings;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "archivist", version, about = "Lossless incremental backup for Obsidian vaults")]
struct Cli {
    /// Backup archive folder (default: from settings)
    #[arg(long, global = true)]
    repo: Option<PathBuf>,
    /// Without a command, opens the app window
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Back up the vault
    Backup {
        #[arg(long)]
        vault: Option<PathBuf>,
    },
    /// List snapshots
    List,
    /// Show sizes and compression/deduplication ratio
    Stats,
    /// Restore a snapshot (id, prefix or "latest") into an empty folder
    Restore { snapshot: String, dest: PathBuf },
    /// Export a snapshot as a portable .tar.zst
    Export { snapshot: String, file: PathBuf },
    /// Check integrity (--full re-reads and re-hashes every block)
    Verify {
        #[arg(long)]
        full: bool,
    },
    /// Apply the retention policy and free space
    Prune,
    /// Delete a snapshot and reclaim unreferenced space
    Forget { snapshot: String },
    /// Open the native window (needs a build with --features window)
    Window,
    /// Start the local web interface
    Ui {
        #[arg(long, default_value_t = 7878)]
        port: u16,
        #[arg(long)]
        no_open: bool,
        /// Exit by itself when the browser tab is closed
        #[arg(long)]
        exit_when_closed: bool,
    },
}

fn human(b: u64) -> String {
    let u = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < u.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", v, u[i])
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}", tr!("cli.error", m = format!("{e:#}")));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let st = Settings::load();
    i18n::set_lang(&st.lang);
    let repo_path = |cli: &Cli| -> Result<PathBuf> {
        match (&cli.repo, st.repo.as_str()) {
            (Some(p), _) => Ok(p.clone()),
            (None, s) if !s.is_empty() => Ok(PathBuf::from(s)),
            _ => bail!(tr!("cli.not_configured")),
        }
    };
    let prog = Progress::default();
    #[cfg(feature = "window")]
    let default_cmd = Cmd::Window;
    #[cfg(not(feature = "window"))]
    let default_cmd = Cmd::Ui { port: 7878, no_open: false, exit_when_closed: false };
    let cmd = cli.cmd.as_ref().unwrap_or(&default_cmd);
    match cmd {
        Cmd::Backup { vault } => {
            let vault = vault.clone().or_else(|| (!st.vault.is_empty()).then(|| PathBuf::from(&st.vault)));
            let Some(vault) = vault else { bail!(tr!("cli.no_vault")) };
            let repo = Repo::open_or_init(&repo_path(&cli)?)?;
            let r = backup::run_backup(&repo, &vault, &st.exclude, st.level, &prog)?;
            for w in &r.warnings {
                eprintln!("{}", tr!("cli.warning", m = w));
            }
            if r.unchanged {
                println!("{}", tr!("cli.unchanged", files = r.files, size = human(r.bytes)));
            } else {
                println!(
                    "{}",
                    tr!("cli.done", id = r.snapshot.unwrap_or_default(), files = r.files, size = human(r.bytes), added = human(r.new_stored), secs = format!("{:.1}", r.secs))
                );
            }
        }
        Cmd::List => {
            let repo = Repo::open(&repo_path(&cli)?)?;
            for m in repo.list_meta()? {
                println!("{}", tr!("cli.list_line", id = m.id, date = &m.created[..19], files = format!("{:>7}", m.files), size = format!("{:>9}", human(m.bytes)), added = human(m.added_bytes)));
            }
        }
        Cmd::Stats => {
            let repo = Repo::open(&repo_path(&cli)?)?;
            let s = repo.stats()?;
            let logical = repo.latest_snapshot()?.map(|s| s.meta.bytes).unwrap_or(0);
            println!("{}", tr!("cli.stats.snapshots", n = s.snapshots));
            println!("{}", tr!("cli.stats.logical", size = human(logical)));
            println!("{}", tr!("cli.stats.disk", size = human(s.disk_bytes)));
            println!("{}", tr!("cli.stats.chunks", n = s.chunks, p = s.packs));
            if logical > 0 && s.disk_bytes > 0 {
                println!("{}", tr!("cli.stats.ratio", x = format!("{:.2}", logical as f64 / s.disk_bytes as f64)));
            }
        }
        Cmd::Restore { snapshot, dest } => {
            let repo = Repo::open(&repo_path(&cli)?)?;
            let r = ops::restore(&repo, snapshot, dest, &prog)?;
            println!("{}", tr!("cli.restored", files = r.files, size = human(r.bytes), dest = dest.display()));
            for e in &r.errors {
                eprintln!("{}", tr!("cli.error_line", m = e));
            }
            if !r.errors.is_empty() {
                std::process::exit(1);
            }
        }
        Cmd::Export { snapshot, file } => {
            let repo = Repo::open(&repo_path(&cli)?)?;
            let n = ops::export_tar_zst(&repo, snapshot, file, &prog)?;
            println!("{}", tr!("cli.exported", file = file.display(), size = human(n)));
        }
        Cmd::Verify { full } => {
            let repo = Repo::open(&repo_path(&cli)?)?;
            let r = ops::verify(&repo, *full, &prog)?;
            println!("{}", tr!("cli.verify_line", snaps = r.snapshots, chunks = r.chunks_checked, full = if *full { tr!("cli.verify_full") } else { String::new() }));
            for m in r.missing.iter().chain(r.corrupt.iter()) {
                eprintln!("{}", tr!("cli.problem", m = m));
            }
            if r.ok {
                println!("{}", tr!("cli.intact"));
            } else {
                std::process::exit(2);
            }
        }
        Cmd::Prune => {
            let repo = Repo::open(&repo_path(&cli)?)?;
            let r = ops::prune(&repo, &st.retention, &prog)?;
            println!("{}", tr!("cli.pruned", n = r.removed_snapshots.len(), size = human(r.freed_bytes)));
        }
        Cmd::Forget { snapshot } => {
            let repo = Repo::open(&repo_path(&cli)?)?;
            println!("{}", tr!("cli.forgot", id = ops::forget(&repo, snapshot)?));
        }
        Cmd::Ui { port, no_open, exit_when_closed } => server::run(*port, !*no_open, *exit_when_closed, None)?,
        Cmd::Window => {
            #[cfg(feature = "window")]
            window::run()?;
            #[cfg(not(feature = "window"))]
            bail!(tr!("cli.no_window"));
        }
    }
    Ok(())
}
