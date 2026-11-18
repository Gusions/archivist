# Changelog

## 0.2.0

Encryption, background backups, and a lot more safety. Archives from 0.1.0 open unchanged. Encrypted archives (new in this version) are refused by older versions instead of being misread.

### Added
- **Optional encryption** for new archives: XChaCha20-Poly1305, key from your password with Argon2id. Contents, file names, folder tree and vault path are encrypted; `archivist init --encrypt`, `passwd`, and from the app (Settings, Encryption) create, unlock and change the password. Not independently audited.
- **Encrypt an existing archive**: `archivist copy <dest> --encrypt` or Settings, Encryption, "Create encrypted copy". Copies every snapshot into a new encrypted archive; the original is untouched. Copying again is an incremental mirror.
- **Background backups on macOS**: `archivist schedule install|remove|status` or Settings, Backup. A launchd agent runs `archivist backup` even with the app closed.
- **Password in the macOS Keychain** (`archivist keychain save|forget`, "Remember in the Keychain"): background backups and app launches open encrypted archives without asking.
- **Browse and restore single files**: a Browse view on every snapshot with search and a Changes tab (added, modified, removed since the previous snapshot); `archivist files`, `archivist diff`, `archivist restore --path`.
- **Storage interface** (`src/backend.rs`): the archive logic no longer touches the filesystem directly; local folder implemented, remote destinations can follow.
- `ARCHIVIST_CONFIG_DIR` to use another settings folder; CI running tests and clippy on Linux, macOS and Windows.

### Fixed
- **Restore and export now verify block hashes.** Before, restoring from a plain archive wrote a corrupted block without any error (`verify --full` found it, restore did not).
- **Durable writes**: pack indexes, snapshots, metadata are written to a temporary name, flushed, renamed atomically; a power loss can no longer leave a truncated index that makes the archive unopenable.
- **Lock**: it is now kept alive by a heartbeat and expires after two minutes instead of twelve hours, so a long backup is not mistaken for a crashed one and a crashed one does not block you for half a day; a lock is only removed by its owner.

### Known limits
- Background backups and the Keychain are macOS only; Windows and Linux use the system scheduler (see README).
- The first background run on macOS may need your permission to read the vault folder (Documents, iCloud Drive).
- Destinations are local folders; no remote storage yet.

## 0.1.0

First release: incremental, deduplicated, lossless snapshots of an Obsidian vault; integrity checks, retention, export, native window and web UI.
