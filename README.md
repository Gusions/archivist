<div align="center">

<img src="docs/logo.png" width="120" alt="Archivist logo">

# Archivist

*Back up your Obsidian vault. Restore it byte for byte.*  
*Incremental, deduplicated, lossless.*  

**English** · [Italiano](README.it.md) · [Website](https://archivist.gusions.com) · [Download](https://github.com/Gusions/archivist/releases/latest)

[![Version](https://img.shields.io/badge/version-0.1.0-0A66E8)](Cargo.toml)
[![Languages](https://img.shields.io/badge/languages-2-0A66E8)](#adding-a-language)
[![Platforms](https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux-0A66E8)](#platforms)
[![Build](https://github.com/Gusions/archivist/actions/workflows/release.yml/badge.svg)](https://github.com/Gusions/archivist/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-0A66E8)](LICENSE)
[![Stars](https://img.shields.io/github/stars/Gusions/archivist?style=flat&color=0A66E8)](https://github.com/Gusions/archivist/stargazers)

Snapshots, not copies · lossless by design · integrity checks built in · no cloud, no account · translatable

</div>

**A vault grows for years. Archivist keeps every backup as a full snapshot, writes only what changed, and never alters your files.**

| What is in the vault | What Archivist does | Quality loss |
|---|---|---|
| Notes, text, JSON, CSS | Block-level dedup + zstd compression | None |
| PNG, JPEG, WebP, PDF, audio, video | Stored as they are (already compressed) | None |
| Edited note (a few lines) | Writes only the changed blocks, a few KB | None |
| Moved or duplicated file | Zero extra space | None |

<p align="center"><img src="docs/screenshots/overview-light.png" width="780" alt="Archivist overview"></p>

## Quick start

```bash
cargo build --release --features window   # needs Rust: https://rustup.rs
./target/release/archivist                # opens the native window
```

In the window: pick your vault (Obsidian vaults are detected automatically), pick a backup folder (ideally on another disk), press **Back up now**. That is the whole setup.

Prefer a ready-made build? Download one from [Releases](../../releases). On macOS, `./make-dmg.sh` produces `dist/Archivist.app` and `dist/Archivist.dmg`.

> If Gatekeeper blocks the app on first launch: `xattr -d com.apple.quarantine <path>`

### Platforms

| Platform | Window | Status |
|---|---|---|
| macOS (arm64, x64) | WebKit (system) | Built by CI; menu bar and `.dmg` script less tested than Linux |
| Windows x64 | WebView2 (preinstalled on Win 10/11) | Built by CI |
| Linux x64 | WebKitGTK | Built and run in CI and in development |

No bundled browser: the binary is a few MB.

## Features

- **Incremental snapshots**: each backup is a complete picture of the vault.
- **Block-level deduplication** (FastCDC + BLAKE3).
- **Lossless compression** (zstd) where it helps, none where it does not.
- **Exact restore**: contents, modification times, empty folders.
- **Encryption** (optional, new archives): XChaCha20-Poly1305 with an Argon2id passphrase; contents, file names and vault path are encrypted.
- **Integrity checks**: quick (all blocks present) or full (re-read and re-hash every block).
- **Retention policy**: latest N plus daily, weekly, monthly. The newest snapshot is never deleted.
- **Browse and restore single files**: search a snapshot, see what changed against the previous one, restore one file or folder (app and CLI).
- **Export** any snapshot as a portable `.tar.zst`.
- **Native macOS-style UI** with light, dark and system themes.
- **Translatable**: every string lives in `i18n/<code>.json` (English and Italian included).

## How it works

```
vault files
   |  split into ~64 KB content-defined chunks (FastCDC)
   v
BLAKE3 hash per chunk ---- already stored? ---- yes --> reference only
   |  no
   v
zstd compress (skipped for incompressible types)
   v
append to pack file (~64 MB)  -->  snapshot written last = visible only when complete
```

| Command | What it does |
|---|---|
| `archivist init --encrypt` | Create an encrypted archive (asks for a password) |
| `archivist passwd` | Change the password (data is not rewritten) |
| `archivist backup` | Create a snapshot |
| `archivist list` / `stats` | List snapshots / show sizes and ratio |
| `archivist verify [--full]` | Check integrity |
| `archivist restore <id\|latest> <dir> [--path P]...` | Restore into an empty or new folder; `--path` restores only those files or folders |
| `archivist files <id\|latest> [--grep TEXT]` | List the files of a snapshot |
| `archivist diff [old] [new]` | Show added, modified and removed files between two snapshots |
| `archivist export <id\|latest> <file>` | Export as `.tar.zst` |
| `archivist prune` | Apply the retention policy |
| `archivist ui` | Local web UI in the browser instead of the window |

```bash
archivist --repo /Volumes/Disk/ObsidianBackup backup --vault ~/Documents/MyVault
archivist restore latest ~/Desktop/Restore
```

Settings (vault, archive folder, exclusions, retention, theme, language) are saved from the UI to `~/Library/Application Support/Archivist/settings.json` (macOS) or `%APPDATA%\Archivist\settings.json` (Windows); the CLI uses them as defaults.

## Guarantees

- A snapshot becomes visible only after it is fully written. An interrupted backup leaves at most orphaned data, cleaned by the next `prune`. Earlier backups are never modified.
- The local UI listens on `127.0.0.1` only, requires a random token on every call and rejects non-local `Host` headers (DNS-rebinding defense).
- Restore rejects paths containing `..` and non-empty destinations.

## Background backups

- **macOS**: `archivist schedule install --every 60` (or Settings, Backup, "Back up in the background") installs a per-user launchd agent that runs `archivist backup` every N minutes, even when the app is closed. Log: `~/Library/Logs/Archivist/backup.log`. `archivist schedule status` shows whether it is on and the last log line; `archivist schedule remove` turns it off. The agent points to the binary that installed it: if you move or delete the app, install again. The first time, macOS may ask to let Archivist access the folder that holds your vault (Documents, iCloud Drive...); if it does not, the log will show a permission error.
- **Encrypted archives** need their password in the Keychain for this: `archivist keychain save` or tick "Remember in the Keychain" when you create or unlock the archive. `archivist keychain forget` removes it.
- **Windows**: `schtasks /Create /SC HOURLY /TN "Archivist" /TR "C:\path\archivist.exe backup"`. **Linux**: a systemd user timer or cron running `archivist backup`. Not automated yet, and encrypted archives need `ARCHIVIST_PASSWORD_FILE` there.
- The in-app schedule (Settings, Backup, Automatic backup) runs only while Archivist is open.

## Adding a language

Copy `i18n/en.json` to `i18n/xx.json`, translate the values (keep the `{placeholders}`), then add one line to `CATALOGS` in `src/i18n.rs`. Missing keys fall back to English.

## Development

```bash
cargo test                                  # engine tests
cargo build --release --features window     # native window build
```

Layout: `src/repo.rs` (archive format), `src/backup.rs` (chunking and snapshots), `src/ops.rs` (restore, export, verify, retention), `src/server.rs` (local API), `src/window.rs` (native window and menu), `ui/index.html` (interface), `i18n/` (strings).

## FAQ

**Is it a replacement for Time Machine or cloud sync?**
No. It protects one thing, an Obsidian vault, with history and integrity checks. Keep your other backups.

**Will it shrink my images?**
No, and by design. Photos and PDFs are already compressed, so they are stored unchanged. You save space on text, on repeated content and on history, not on media.

**Is the archive encrypted?**
Optionally. Create an encrypted archive with `archivist init --encrypt`; contents, file names, the directory tree and the vault path are encrypted and authenticated (XChaCha20-Poly1305, key from your password via Argon2id). Not hidden: the number of snapshots, their timestamps and blob sizes. **If you lose the password the data cannot be recovered.** You can also create, unlock and change the password of an encrypted archive from the app (Settings, Encryption). Existing plain archives are not converted yet: create the encrypted archive in a new folder. For scripts set `ARCHIVIST_PASSWORD` or `ARCHIVIST_PASSWORD_FILE`; otherwise the CLI asks. Older Archivist versions refuse to open encrypted archives.

**What is not preserved?**
Symlinks are skipped; Unix permissions and extended attributes are not stored (modification times are).

**Can two machines write to the same archive?**
Not at the same time. The lock is local, not network-wide.

**Vault with `.git`?**
It is included like any other folder. Add `.git` to the exclusions if you do not want it.

## Contributing

Issues and pull requests are welcome, especially translations and platform testing.

## License

[MIT](LICENSE)
