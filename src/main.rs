#[macro_use]
mod i18n;
mod backend;
mod backup;
mod crypto;
mod keychain;
mod schedule;
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
    /// Create an encrypted archive (asks for a password). Plain archives are created by `backup`.
    Init {
        #[arg(long)]
        encrypt: bool,
    },
    /// Change the password of an encrypted archive (data is not rewritten)
    Passwd,
    /// Back up automatically in the background (macOS: a launchd agent)
    Schedule {
        #[command(subcommand)]
        action: ScheduleCmd,
    },
    /// Remember or forget the archive password in the system keychain (macOS)
    Keychain {
        #[command(subcommand)]
        action: KeychainCmd,
    },
    /// Back up the vault
    Backup {
        #[arg(long)]
        vault: Option<PathBuf>,
    },
    /// List snapshots
    List,
    /// Show sizes and compression/deduplication ratio
    Stats,
    /// Restore a snapshot (id, prefix or "latest") into an empty folder.
    /// With --path, restore only those files or folders (repeatable).
    Restore {
        snapshot: String,
        dest: PathBuf,
        #[arg(long = "path")]
        paths: Vec<String>,
    },
    /// List the files of a snapshot, optionally only those whose path contains TEXT
    Files {
        snapshot: String,
        #[arg(long)]
        grep: Option<String>,
    },
    /// Show what changed between two snapshots (default: the one before the newest, and the newest)
    Diff { old: Option<String>, new: Option<String> },
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

#[derive(Subcommand)]
enum ScheduleCmd {
    /// Turn on background backups
    Install {
        /// Minutes between backups
        #[arg(long, default_value_t = 60)]
        every: u32,
    },
    /// Turn off background backups
    Remove,
    /// Show whether background backups are on
    Status,
}

#[derive(Subcommand)]
enum KeychainCmd {
    /// Save the password (asks for it unless ARCHIVIST_PASSWORD is set)
    Save,
    /// Remove the saved password
    Forget,
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

/// Ask twice for a new password (or take it from the environment for scripting).
fn new_password(env_override: Option<&str>) -> Result<String> {
    if let Some(p) = env_override.and_then(|k| std::env::var(k).ok()).filter(|p| !p.is_empty()) {
        return Ok(p);
    }
    let a = rpassword::prompt_password(tr!("cli.password_new"))?;
    if a.is_empty() {
        bail!(tr!("cli.password_empty"));
    }
    if a != rpassword::prompt_password(tr!("cli.password_repeat"))? {
        bail!(tr!("cli.password_mismatch"));
    }
    Ok(a)
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
    let raw_path = |cli: &Cli| -> Result<PathBuf> {
        match (&cli.repo, st.repo.as_str()) {
            (Some(p), _) => Ok(p.clone()),
            (None, s) if !s.is_empty() => Ok(PathBuf::from(s)),
            _ => bail!(tr!("cli.not_configured")),
        }
    };
    // for encrypted archives, get the password (env, file or prompt) before opening
    let repo_path = |cli: &Cli| -> Result<PathBuf> {
        let p = raw_path(cli)?;
        if Repo::is_encrypted_at(&p) && crypto::password_for(&p).is_none() {
            crypto::set_password(Some(rpassword::prompt_password(tr!("cli.password_prompt"))?));
        }
        Ok(p)
    };
    let prog = Progress::default();
    #[cfg(feature = "window")]
    let default_cmd = Cmd::Window;
    #[cfg(not(feature = "window"))]
    let default_cmd = Cmd::Ui { port: 7878, no_open: false, exit_when_closed: false };
    let cmd = cli.cmd.as_ref().unwrap_or(&default_cmd);
    match cmd {
        Cmd::Init { encrypt } => {
            if !*encrypt {
                bail!("use `archivist backup` to create a plain archive, or `archivist init --encrypt`");
            }
            let p = raw_path(&cli)?;
            let pass = new_password(Some("ARCHIVIST_PASSWORD"))?;
            Repo::init_encrypted(&p, &pass)?;
            println!("{}", tr!("cli.initialized", p = p.display()));
        }
        Cmd::Schedule { action } => match action {
            ScheduleCmd::Install { every } => {
                if st.vault.is_empty() || st.repo.is_empty() {
                    bail!(tr!("rs.schedule_needs_settings"));
                }
                let p = raw_path(&cli)?;
                if Repo::is_encrypted_at(&p) && keychain::load(&keychain::account(&p)).is_none() {
                    bail!(tr!("rs.schedule_needs_keychain"));
                }
                let every = (*every).clamp(5, 10_080);
                schedule::install(&std::env::current_exe()?, every)?;
                println!("{}", tr!("cli.schedule_installed", n = every, log = schedule::status().log));
            }
            ScheduleCmd::Remove => {
                schedule::remove()?;
                println!("{}", tr!("cli.schedule_removed"));
            }
            ScheduleCmd::Status => {
                let s = schedule::status();
                if s.installed {
                    println!("{}", tr!("cli.schedule_status_on", n = s.minutes));
                    if s.stale {
                        eprintln!("{}", tr!("cli.schedule_stale"));
                    }
                } else {
                    println!("{}", tr!("cli.schedule_status_off"));
                }
                if !s.last_log.is_empty() {
                    println!("{}", tr!("cli.schedule_last", line = s.last_log));
                }
            }
        },
        Cmd::Keychain { action } => {
            let p = raw_path(&cli)?;
            if !Repo::is_encrypted_at(&p) {
                bail!(tr!("rs.not_encrypted"));
            }
            match action {
                KeychainCmd::Save => {
                    let pass = match crypto::password() {
                        Some(p) => p,
                        None => rpassword::prompt_password(tr!("cli.password_prompt"))?,
                    };
                    crypto::set_password(Some(pass.clone()));
                    Repo::open(&p)?; // verify before saving
                    keychain::save(&keychain::account(&p), &pass)?;
                    println!("{}", tr!("cli.keychain_saved"));
                }
                KeychainCmd::Forget => {
                    keychain::forget(&keychain::account(&p));
                    println!("{}", tr!("cli.keychain_forgotten"));
                }
            }
        }
        Cmd::Passwd => {
            let repo = Repo::open(&repo_path(&cli)?)?;
            let pass = new_password(Some("ARCHIVIST_NEW_PASSWORD"))?;
            repo.change_password(&pass)?;
            println!("{}", tr!("cli.password_changed"));
        }
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
        Cmd::Files { snapshot, grep } => {
            let repo = Repo::open(&repo_path(&cli)?)?;
            let snap = repo.load_snapshot(snapshot)?;
            let (total, found) = ops::find_files(&snap, grep.as_deref().unwrap_or(""), usize::MAX);
            for e in found {
                println!("{:>9}  {}", human(e.size), e.path);
            }
            eprintln!("{}", tr!("cli.files_total", n = total));
        }
        Cmd::Diff { old, new } => {
            let repo = Repo::open(&repo_path(&cli)?)?;
            let metas = repo.list_meta()?;
            let new_id = match new {
                Some(n) => repo.resolve(n)?,
                None => metas.last().map(|m| m.id.clone()).ok_or_else(|| anyhow::anyhow!(tr!("rs.no_snapshots")))?,
            };
            let old_id = match old {
                Some(o) => repo.resolve(o)?,
                None => {
                    let i = metas.iter().position(|m| m.id == new_id).unwrap_or(0);
                    if i == 0 {
                        bail!(tr!("diff.first"));
                    }
                    metas[i - 1].id.clone()
                }
            };
            let changes = ops::diff_snapshots(&repo.load_snapshot(&old_id)?, &repo.load_snapshot(&new_id)?);
            let count = |k: &str| changes.iter().filter(|c| c.kind == k).count();
            for c in &changes {
                let sign = match c.kind {
                    "added" => '+',
                    "removed" => '-',
                    _ => '~',
                };
                println!("{sign} {}", c.path);
            }
            eprintln!("{}", tr!("cli.diff_summary", a = count("added"), m = count("modified"), r = count("removed")));
        }
        Cmd::Restore { snapshot, dest, paths } => {
            let repo = Repo::open(&repo_path(&cli)?)?;
            let only = (!paths.is_empty()).then_some(paths.as_slice());
            let r = ops::restore_selected(&repo, snapshot, dest, only, &prog)?;
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
