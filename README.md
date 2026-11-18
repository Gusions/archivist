<div align="center">

<img src="docs/logo.png" width="112" alt="Archivist logo">

# Archivist

**Lossless, incremental backups for your Obsidian vault.**<br>
Restore it byte for byte. No cloud, no account.

[Download](https://github.com/Gusions/archivist/releases/latest) &nbsp;·&nbsp; [Website](https://archivist.gusions.com) &nbsp;·&nbsp; [Quick start](#quick-start) &nbsp;·&nbsp; [Command line](#command-line) &nbsp;·&nbsp; [FAQ](#faq) &nbsp;·&nbsp; **English** | [Italiano](README.it.md)

[![Release](https://img.shields.io/github/v/release/Gusions/archivist?style=flat-square&color=0A66E8&label=release)](https://github.com/Gusions/archivist/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/Gusions/archivist/ci.yml?branch=main&style=flat-square&label=CI)](https://github.com/Gusions/archivist/actions/workflows/ci.yml)
[![Platforms](https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux-0A66E8?style=flat-square)](#platforms)
[![License: MIT](https://img.shields.io/badge/license-MIT-0A66E8?style=flat-square)](LICENSE)

<br>

<img src="docs/screenshots/snapshots-light.png" width="780" alt="Archivist, snapshots view">

</div>

<br>

A vault grows for years. Archivist keeps every backup as a **full snapshot**, writes **only what changed**, and **never alters your files**.

| In the vault | What Archivist does | Quality loss |
|---|---|:---:|
| Notes, text, JSON, CSS | Block-level dedup + zstd compression | None |
| PNG, JPEG, WebP, PDF, audio, video | Stored as they are (already compressed) | None |
| An edited note (a few lines) | Writes only the changed blocks, a few KB | None |
| A moved or duplicated file | Zero extra space | None |

## Contents

[Quick start](#quick-start) · [Features](#features) · [How it works](#how-it-works) · [Command line](#command-line) · [Encryption](#encryption) · [Background backups](#background-backups) · [Guarantees](#guarantees) · [Configuration](#configuration) · [Adding a language](#adding-a-language) · [Development](#development) · [FAQ](#faq)

## Quick start

**Download.** Grab the latest build from [Releases](../../releases): `Archivist-macOS-arm64.dmg` for Apple Silicon, plus zips for macOS, Windows and Linux.

**Or build from source** (needs [Rust](https://rustup.rs)):

```bash
cargo build --release --features window
./target/release/archivist                # opens the native window
```

In the window, pick your vault (Obsidian vaults are detected automatically), pick a backup folder (ideally on another disk) and press **Back up now**. That is the whole setup.

> [!NOTE]
> The macOS app is not notarized. If Gatekeeper blocks it on first launch, run `xattr -d com.apple.quarantine <path-to-app>`. To build the `.dmg` yourself, run `./make-dmg.sh` (produces `dist/Archivist.app` and `dist/Archivist.dmg`).

### Platforms

| Platform | Window | Status |
|---|---|---|
| macOS (arm64, x64) | WebKit (system) | Built and tested by CI; arm64 `.dmg` in every release |
| Windows x64 | WebView2 (preinstalled on Win 10/11) | Built and tested by CI |
| Linux x64 | WebKitGTK | Built and run in CI and in development |

No bundled browser: the binary is a few MB.

## Features

| | |
|---|---|
| **Snapshots** | Every backup is a complete picture of the vault, stored incrementally |
| **Deduplication** | Block level (FastCDC + BLAKE3) |
| **Compression** | Lossless zstd where it helps, none where it does not |
| **Exact restore** | Contents, modification times, empty folders |
| **Encryption** | Optional XChaCha20-Poly1305 with an Argon2id passphrase; names and paths are encrypted too |
| **Integrity checks** | Quick (all blocks present) or full (re-read and re-hash every block) |
| **Retention** | Latest N plus daily, weekly, monthly; the newest snapshot is never deleted |
| **Single files** | Search a snapshot, see what changed against the previous one, restore one file or folder |
| **Export** | Any snapshot as a portable `.tar.zst` |
| **Background backups** | macOS launchd agent, password kept in the Keychain |
| **Native UI** | macOS-style window with light, dark and system themes |
| **Translatable** | Every string lives in `i18n/<code>.json` (English and Italian included) |

## How it works

```
vault files
   │  split into ~64 KB content-defined chunks (FastCDC)
   ▼
BLAKE3 hash per chunk ──── already stored? ──── yes ──▶ reference only
   │ no
   ▼
zstd compress (skipped for incompressible types)
   ▼
append to a pack file (~64 MB)  ──▶  snapshot written last = visible only when complete
```

## Command line

```bash
archivist --repo /Volumes/Disk/ObsidianBackup backup --vault ~/Documents/MyVault
archivist restore latest ~/Desktop/Restore
```

**Snapshots**

| Command | What it does |
|---|---|
| `archivist backup` | Create a snapshot |
| `archivist list` / `stats` | List snapshots / show sizes and ratio |
| `archivist files <id\|latest> [--grep TEXT]` | List the files of a snapshot |
| `archivist diff [old] [new]` | Added, modified and removed files between two snapshots |
| `archivist restore <id\|latest> <dir> [--path P]...` | Restore into an empty or new folder; `--path` restores only those files or folders |
| `archivist export <id\|latest> <file>` | Export as `.tar.zst` |

**Maintenance**

| Command | What it does |
|---|---|
| `archivist verify [--full]` | Check integrity |
| `archivist prune` | Apply the retention policy |
| `archivist forget <id>` | Delete one snapshot and reclaim unreferenced space |

**Archive and app**

| Command | What it does |
|---|---|
| `archivist init --encrypt` | Create an encrypted archive (asks for a password) |
| `archivist copy <dest> [--encrypt]` | Copy all snapshots into another archive, re-encoding every block. With `--encrypt` it creates an encrypted archive from a plain one; copying again only adds new snapshots |
| `archivist passwd` | Change the password (data is not rewritten) |
| `archivist schedule install\|status\|remove` | Background backups (macOS) |
| `archivist keychain save\|forget` | Remember or forget the archive password in the Keychain (macOS) |
| `archivist window` | Open the native window (build with `--features window`) |
| `archivist ui` | Local web UI in the browser instead of the window |

## Encryption

Create an encrypted archive with `archivist init --encrypt`, or from the app (Settings, Encryption). Contents, file names, the directory tree and the vault path are encrypted and authenticated (XChaCha20-Poly1305, key derived from your password with Argon2id). Not hidden: the number of snapshots, their timestamps and block sizes.

> [!WARNING]
> If you lose the password, the data cannot be recovered. Older Archivist versions refuse to open encrypted archives.

To encrypt an existing plain archive, use Settings, Encryption, "Create encrypted copy" or `archivist copy <new-folder> --encrypt`: every snapshot is copied into a new encrypted archive and the original is left untouched. In scripts, set `ARCHIVIST_PASSWORD` or `ARCHIVIST_PASSWORD_FILE`; otherwise the CLI asks.

## Background backups

| Platform | How |
|---|---|
| **macOS** | `archivist schedule install --every 60` (or Settings, Backup, "Back up in the background") installs a per-user launchd agent that runs `archivist backup` every N minutes, even when the app is closed |
| **Windows** | `schtasks /Create /SC HOURLY /TN "Archivist" /TR "C:\path\archivist.exe backup"` (not automated yet) |
| **Linux** | A systemd user timer or cron running `archivist backup` (not automated yet) |

- Log: `~/Library/Logs/Archivist/backup.log`. `archivist schedule status` shows whether it is on and the last log line; `archivist schedule remove` turns it off.
- The agent points to the binary that installed it: if you move or delete the app, install again.
- The first time, macOS may ask to let Archivist access the folder that holds your vault (Documents, iCloud Drive...); if it does not, the log shows a permission error.
- **Encrypted archives** need their password in the Keychain: `archivist keychain save`, or tick "Remember in the Keychain" when you create or unlock the archive. On Windows and Linux use `ARCHIVIST_PASSWORD_FILE`.
- The in-app schedule (Settings, Backup, Automatic backup) runs only while Archivist is open.

## Guarantees

- A snapshot becomes visible only after it is fully written. An interrupted backup leaves at most orphaned data, cleaned by the next `prune`. Earlier backups are never modified.
- Restore and export re-hash every block they read: a damaged archive fails loudly instead of producing silently wrong files.
- Archive writes are durable and atomic, with a heartbeat lock; crash and round-trip tests run in CI on Linux, macOS and Windows.
- The local UI listens on `127.0.0.1` only, requires a random token on every call and rejects non-local `Host` headers (DNS-rebinding defense).
- Restore rejects paths containing `..` and non-empty destinations.

## Configuration

Settings (vault, archive folder, exclusions, retention, theme, language) are saved from the UI to `~/Library/Application Support/Archivist/settings.json` (macOS) or `%APPDATA%\Archivist\settings.json` (Windows). The CLI uses them as defaults.

| Variable | Purpose |
|---|---|
| `ARCHIVIST_PASSWORD` / `ARCHIVIST_PASSWORD_FILE` | Archive password for scripts and background jobs |
| `ARCHIVIST_CONFIG_DIR` | Use another settings folder (separate profiles); an agent installed with it set keeps using it |

## Adding a language

Copy `i18n/en.json` to `i18n/xx.json`, translate the values (keep the `{placeholders}`), then add one line to `CATALOGS` in `src/i18n.rs`. Missing keys fall back to English.

## Development

```bash
cargo test                                  # engine tests
cargo build --release --features window     # native window build
```

| File | Role |
|---|---|
| `src/repo.rs` | Archive format |
| `src/backend.rs` | Storage backends |
| `src/crypto.rs` | Encryption |
| `src/backup.rs` | Chunking and snapshots |
| `src/copy.rs` | Copy and encrypt an archive |
| `src/ops.rs` | Restore, export, verify, retention |
| `src/schedule.rs`, `src/keychain.rs` | Background backups, Keychain |
| `src/server.rs` | Local API |
| `src/window.rs` | Native window and menu |
| `ui/index.html`, `i18n/` | Interface and strings |

## FAQ

<details>
<summary><b>Is it a replacement for Time Machine or cloud sync?</b></summary>

No. It protects one thing, an Obsidian vault, with history and integrity checks. Keep your other backups.
</details>

<details>
<summary><b>Will it shrink my images?</b></summary>

No, and by design. Photos and PDFs are already compressed, so they are stored unchanged. You save space on text, on repeated content and on history, not on media.
</details>

<details>
<summary><b>What is not preserved?</b></summary>

Symlinks are skipped; Unix permissions and extended attributes are not stored (modification times are).
</details>

<details>
<summary><b>Can two machines write to the same archive?</b></summary>

Not at the same time. The lock is local, not network-wide.
</details>

<details>
<summary><b>Vault with <code>.git</code>?</b></summary>

It is included like any other folder. Add `.git` to the exclusions if you do not want it.
</details>

## Contributing

Issues and pull requests are welcome, especially translations and platform testing.

## License

[MIT](LICENSE)
