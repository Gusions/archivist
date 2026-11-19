//! Remember the password of an encrypted archive in the system keychain (macOS login keychain).
//! It is what lets a background backup open an encrypted archive without asking.
//! Other platforms: not supported yet (every call is a no-op / error), see README.

use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// Keychain item name. Overridable for tests so the real item is never touched.
fn service() -> String {
    std::env::var("ARCHIVIST_KEYCHAIN_SERVICE").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "Archivist".into())
}

pub fn supported() -> bool {
    cfg!(target_os = "macos")
}

/// Remembers what the Keychain answered during this run, including "nothing" and "denied".
/// The UI polls its state every few seconds; without this every poll would ask the Keychain again
/// and macOS would show its access prompt again and again.
#[derive(Default)]
struct Memo(Mutex<HashMap<String, Option<String>>>);

impl Memo {
    /// The lock is held while the system is asked, so several threads that need the same answer at
    /// the same time (the UI fires a few requests at once) wait for the one prompt instead of each
    /// causing their own.
    fn get_or_load(&self, account: &str, load: impl FnOnce() -> Option<String>) -> Option<String> {
        let mut map = self.0.lock().unwrap();
        if let Some(v) = map.get(account) {
            return v.clone();
        }
        let v = load();
        map.insert(account.to_string(), v.clone());
        v
    }
    fn set(&self, account: &str, v: Option<String>) {
        self.0.lock().unwrap().insert(account.to_string(), v);
    }
}

fn memo() -> &'static Memo {
    static M: OnceLock<Memo> = OnceLock::new();
    M.get_or_init(Memo::default)
}

/// Save the password (and remember it for this run).
pub fn save(account: &str, password: &str) -> Result<()> {
    os_save(account, password)?;
    memo().set(account, Some(password.to_string()));
    Ok(())
}

/// The saved password, asking the system at most once per run for each archive.
pub fn load(account: &str) -> Option<String> {
    memo().get_or_load(account, || os_load(account))
}

pub fn forget(account: &str) {
    os_forget(account);
    memo().set(account, None);
}

/// One keychain entry per archive folder.
pub fn account(repo: &Path) -> String {
    std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf()).display().to_string()
}

#[cfg(target_os = "macos")]
fn os_save(account: &str, password: &str) -> Result<()> {
    security_framework::passwords::set_generic_password(&service(), account, password.as_bytes())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(target_os = "macos")]
fn os_load(account: &str) -> Option<String> {
    security_framework::passwords::get_generic_password(&service(), account)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

#[cfg(target_os = "macos")]
fn os_forget(account: &str) {
    let _ = security_framework::passwords::delete_generic_password(&service(), account);
}

#[cfg(not(target_os = "macos"))]
fn os_save(_account: &str, _password: &str) -> Result<()> {
    anyhow::bail!(tr!("rs.keychain_unsupported"))
}

#[cfg(not(target_os = "macos"))]
fn os_load(_account: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
fn os_forget(_account: &str) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn the_system_is_asked_once_even_when_the_answer_is_nothing() {
        let m = Memo::default();
        let calls = Cell::new(0);
        for _ in 0..5 {
            assert_eq!(m.get_or_load("a", || { calls.set(calls.get() + 1); None }), None);
        }
        assert_eq!(calls.get(), 1, "a missing or denied item must not be asked again on every poll");
        assert_eq!(m.get_or_load("b", || { calls.set(calls.get() + 1); Some("x".into()) }).as_deref(), Some("x"));
        assert_eq!(m.get_or_load("b", || unreachable!()).as_deref(), Some("x"));
        m.set("a", Some("new".into()));
        assert_eq!(m.get_or_load("a", || unreachable!()).as_deref(), Some("new"));
    }

    #[test]
    fn simultaneous_lookups_cause_a_single_system_request() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let m = Arc::new(Memo::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (m, calls) = (m.clone(), calls.clone());
                std::thread::spawn(move || {
                    m.get_or_load("same", || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(150)); // the user is looking at the prompt
                        Some("pw".into())
                    })
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap().as_deref(), Some("pw"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "each thread asked the system on its own, which means several prompts");
    }
}
