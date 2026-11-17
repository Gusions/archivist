//! Remember the password of an encrypted archive in the system keychain (macOS login keychain).
//! It is what lets a background backup open an encrypted archive without asking.
//! Other platforms: not supported yet (every call is a no-op / error), see README.

use anyhow::Result;
use std::path::Path;

/// Keychain item name. Overridable for tests so the real item is never touched.
fn service() -> String {
    std::env::var("ARCHIVIST_KEYCHAIN_SERVICE").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "Archivist".into())
}

pub fn supported() -> bool {
    cfg!(target_os = "macos")
}

/// One keychain entry per archive folder.
pub fn account(repo: &Path) -> String {
    std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf()).display().to_string()
}

#[cfg(target_os = "macos")]
pub fn save(account: &str, password: &str) -> Result<()> {
    security_framework::passwords::set_generic_password(&service(), account, password.as_bytes())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(target_os = "macos")]
pub fn load(account: &str) -> Option<String> {
    security_framework::passwords::get_generic_password(&service(), account)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

#[cfg(target_os = "macos")]
pub fn forget(account: &str) {
    let _ = security_framework::passwords::delete_generic_password(&service(), account);
}

#[cfg(not(target_os = "macos"))]
pub fn save(_account: &str, _password: &str) -> Result<()> {
    anyhow::bail!(tr!("rs.keychain_unsupported"))
}

#[cfg(not(target_os = "macos"))]
pub fn load(_account: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn forget(_account: &str) {}
