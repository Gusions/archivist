<div align="center">

<img src="docs/logo.png" width="120" alt="Archivist logo">

# Archivist

*Back up your Obsidian vault. Restore it byte for byte.*  
*Incremental, deduplicated, lossless. Optionally encrypted.*  

**English** · [Italiano](README.it.md) · [Website](https://archivist.gusions.com) · [Download](https://github.com/Gusions/archivist/releases/latest)

[![Version](https://img.shields.io/badge/version-0.1.0-0A66E8)](Cargo.toml)
[![Languages](https://img.shields.io/badge/languages-2-0A66E8)](#adding-a-language)
[![Platforms](https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux-0A66E8)](#platforms)
[![CI](https://github.com/Gusions/archivist/actions/workflows/ci.yml/badge.svg)](https://github.com/Gusions/archivist/actions/workflows/ci.yml)
[![Build](https://github.com/Gusions/archivist/actions/workflows/release.yml/badge.svg)](https://github.com/Gusions/archivist/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-0A66E8)](LICENSE)
[![Stars](https://img.shields.io/github/stars/Gusions/archivist?style=flat&color=0A66E8)](https://github.com/Gusions/archivist/stargazers)

Snapshots, not copies · lossless by design · optional encryption · integrity checks built in · no cloud, no account · translatable

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

Then, when you want them, all from **Settings**: encrypt the archive, turn on backups that run while the app is closed, and browse a snapshot to restore a single note. Details below.

### Platforms

| Platform | Window | Background backups | Keychain | Status |
|---|---|---|---|---|
| macOS (arm64, x64) | WebKit (system) | Yes (launchd agent) | Yes | Built and unit-tested by CI; the app, the launchd agent and the Keychain were exercised on macOS arm64 |
| Windows x64 | WebView2 (preinstalled on Win 10/11) | Manual (`schtasks`) | Not yet | Built and unit-tested by CI |
| Linux x64 | WebKitGTK | Manual (systemd timer or cron) | Not yet | Built and unit-tested by CI |

No bundled browser: the binary is a few MB.

## Features

**Backup**
- **Incremental snapshots**: each backup is a complete picture of the vault.
- **Block-level deduplication** (FastCDC + BLAKE3) and **lossless compression** (zstd) where it helps, none where it does not.
- **Retention policy**: latest N plus daily, weekly, monthly. The newest snapshot is never deleted.
- **Background backups** on macOS, even with the app closed.

**Safety**
- **Optional encryption**: XChaCha20-Poly1305 with an Argon2id password. Contents, file names, the folder tree and the vault path are encrypted.
- **Integrity checks**: quick (all blocks present) or full (re-read and re-hash every block).
- **Durable writes**: files are written to a temporary name, flushed to disk, renamed atomically; a snapshot becomes visible only when complete.
- **Password in the macOS Keychain** (optional), so background backups and app launches do not ask.

**Restore**
- **Exact restore**: contents, modification times, empty folders.
- **Browse a snapshot**: search by name, see what was added, modified or removed since the previous snapshot, restore one file or folder (also one you deleted later).
- **Export** any snapshot as a portable `.tar.zst`.

**Everyday**
- **Encrypt an existing archive**: copies every snapshot into a new encrypted archive, the original is left untouched.
- **Native macOS-style UI** with light, dark and system themes; **translatable** (English and Italian included).

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
encrypt (encrypted archives only)
   v
append to pack file (~64 MB)  -->  snapshot written last = visible only when complete
```

The archive logic talks to a small storage interface (`src/backend.rs`), so where the bytes live is separate from what they mean. Today the only implementation is a local folder (an external disk, a network mount); remote destinations are planned.

## Commands

| Command | What it does |
|---|---|
| `archivist backup` | Create a snapshot |
| `archivist list` / `stats` | List snapshots / show sizes and ratio |
| `archivist verify [--full]` | Check integrity |
| `archivist restore <id\|latest> <dir> [--path P]...` | Restore into an empty or new folder; `--path` restores only those files or folders |
| `archivist files <id\|latest> [--grep TEXT]` | List the files of a snapshot |
| `archivist diff [old] [new]` | Show added, modified and removed files between two snapshots |
| `archivist export <id\|latest> <file>` | Export as `.tar.zst` |
| `archivist prune` | Apply the retention policy |
| `archivist forget <id>` | Delete a snapshot and reclaim unreferenced space |
| `archivist init --encrypt` | Create an encrypted archive (asks for a password) |
| `archivist copy <dest> [--encrypt]` | Copy all snapshots into another archive, re-encoding every block. With `--encrypt` it creates an encrypted archive from a plain one; copying again only adds new snapshots |
| `archivist passwd` | Change the password (data is not rewritten) |
| `archivist keychain save\|forget` | Remember or forget the password in the macOS Keychain |
| `archivist schedule install [--every MIN]\|remove\|status` | Background backups (macOS) |
| `archivist ui` | Local web UI in the browser instead of the window |
| `archivist window` | The native window (needs a build with `--features window`) |

```bash
archivist --repo /Volumes/Disk/ObsidianBackup backup --vault ~/Documents/MyVault
archivist restore latest ~/Desktop/Restore
archivist restore latest ~/Desktop/One --path Notes/idea.md
```

Settings (vault, archive folder, exclusions, retention, theme, language) are saved from the UI to `~/Library/Application Support/Archivist/settings.json` (macOS) or `%APPDATA%\Archivist\settings.json` (Windows); the CLI uses them as defaults.

### Environment variables

| Variable | Used for |
|---|---|
| `ARCHIVIST_PASSWORD` | Password of the archive (and the new password of `init --encrypt`), instead of the prompt |
| `ARCHIVIST_PASSWORD_FILE` | A file whose first line is the password |
| `ARCHIVIST_NEW_PASSWORD` | The new password of `passwd` and `copy --encrypt` in scripts |
| `ARCHIVIST_CONFIG_DIR` | Another settings folder (separate profiles); a background agent installed with it keeps using it |

Where the password comes from, in order: set in the app session, `ARCHIVIST_PASSWORD`, `ARCHIVIST_PASSWORD_FILE`, the macOS Keychain, then a prompt.

## Encryption

- **Create** an encrypted archive from Settings, Encryption (or `archivist init --encrypt`). New archives only; password at least 8 characters in the app.
- **What is protected**: file contents, file names, the directory tree, the vault path, snapshot metadata. Everything is encrypted and authenticated (XChaCha20-Poly1305); the key comes from your password with Argon2id. Changing the password rewraps one small key file, no data is rewritten. An encrypted archive refuses unencrypted blocks.
- **What is not hidden**: the number of snapshots, their timestamps and the size of the stored blocks.
- **If you lose the password the data cannot be recovered.** There is no reset.
- **Encrypt an existing archive**: Settings > Encryption > "Create encrypted copy", or `archivist copy <new-folder> --encrypt`. Every snapshot is copied into a new encrypted archive; the old archive is not changed or deleted, remove it yourself when you are sure.
- Older Archivist versions refuse to open encrypted archives instead of misreading them.
- This is a new scheme built on well-known libraries (RustCrypto). It has not been audited by a third party.

## Background backups

- **macOS**: `archivist schedule install --every 60` (or Settings, Backup, "Back up in the background") installs a per-user launchd agent that runs `archivist backup` every N minutes, even when the app is closed. Log: `~/Library/Logs/Archivist/backup.log`. `archivist schedule status` shows whether it is on and the last log line; `archivist schedule remove` turns it off. The agent points to the binary that installed it: if you move or delete the app, install again. The first time, macOS may ask to let Archivist access the folder that holds your vault (Documents, iCloud Drive...); if it does not, the log will show a permission error.
- **Encrypted archives** need their password in the Keychain for this: `archivist keychain save` or tick "Remember in the Keychain" when you create or unlock the archive. `archivist keychain forget` removes it.
- **Windows**: `schtasks /Create /SC HOURLY /TN "Archivist" /TR "C:\path\archivist.exe backup"`. **Linux**: a systemd user timer or cron running `archivist backup`. Not automated yet, and encrypted archives need `ARCHIVIST_PASSWORD_FILE` there.
- The in-app schedule (Settings, Backup, Automatic backup) runs only while Archivist is open.

## Guarantees

- A snapshot becomes visible only after it is fully written and flushed to disk. An interrupted backup leaves at most orphaned data, cleaned by the next `prune`. Earlier backups are never modified.
- Only one writer at a time: the archive is locked while a backup, prune or copy runs. The lock is kept alive by a heartbeat, so a long backup is not mistaken for a crashed one, and an abandoned lock expires after two minutes.
- The local UI listens on `127.0.0.1` only, requires a random token on every call and rejects non-local `Host` headers (DNS-rebinding defense).
- Restore rejects paths containing `..` and non-empty destinations.
- Damage is detected: `verify --full` re-hashes every block, and restore and export check each block's hash as they read it, so a damaged file is reported instead of being restored silently. In an encrypted archive every block is also authenticated.

## Not yet

- Remote destinations (S3, SFTP, WebDAV): the storage interface is ready, no implementation yet.
- Background backups and the Keychain on Windows and Linux.
- Converting an archive in place (today: copy to a new archive).
- A folder tree and a file preview or text diff when browsing a snapshot (today: search and a change list).
- Symlinks, Unix permissions and extended attributes.

## Adding a language

Copy `i18n/en.json` to `i18n/xx.json`, translate the values (keep the `{placeholders}`), then add one line to `CATALOGS` in `src/i18n.rs`. Missing keys fall back to English.

## Development

```bash
cargo test                                  # engine tests (run on Linux, macOS and Windows by CI)
cargo build --release --features window     # native window build
```

Layout: `src/repo.rs` (archive format), `src/backend.rs` (storage interface and local folder), `src/crypto.rs` (encryption), `src/backup.rs` (chunking and snapshots), `src/ops.rs` (restore, diff, export, verify, retention), `src/copy.rs` (copy between archives), `src/schedule.rs` and `src/keychain.rs` (macOS background agent and Keychain), `src/server.rs` (local API), `src/window.rs` (native window and menu), `ui/index.html` (interface), `i18n/` (strings).

For tests that must not touch the real system, `ARCHIVIST_SCHEDULE_LABEL` and `ARCHIVIST_KEYCHAIN_SERVICE` rename the launchd agent and the Keychain item.

## FAQ

**Is it a replacement for Time Machine or cloud sync?**
No. It protects one thing, an Obsidian vault, with history and integrity checks. Keep your other backups.

**Will it shrink my images?**
No, and by design. Photos and PDFs are already compressed, so they are stored unchanged. You save space on text, on repeated content and on history, not on media.

**I deleted a note last week. How do I get it back?**
Snapshots, Browse on a snapshot that still had it, search its name, Restore. Or `archivist files latest --grep name` and `archivist restore <id> <dir> --path <file>`.

**Can I get my data back without Archivist?**
Yes for the data, in two ways: `archivist export` writes a plain `.tar.zst` of any snapshot. A plain archive is also just packs of zstd-compressed blocks plus JSON; an encrypted one needs the password and Archivist.

**Does it back up while the app is closed?**
On macOS, yes, if you turn on "Back up in the background". On Windows and Linux use the system scheduler (see above).

**Where is my password stored?**
Never in a file. The app keeps it in memory for the session. On macOS the create and unlock dialogs have "Remember in the Keychain" ticked by default, which stores it in your login Keychain so launches and background backups do not ask; untick it to keep the password in memory only.

**What is not preserved?**
Symlinks are skipped; Unix permissions and extended attributes are not stored (modification times are).

**Can two machines write to the same archive?**
Not at the same time. The lock is local, not network-wide.

**Vault with `.git`?**
It is included like any other folder. Add `.git` to the exclusions if you do not want it.

## Contributing

Issues and pull requests are welcome, especially translations and platform testing (Windows and Linux are only covered by CI).

## License

[MIT](LICENSE)
