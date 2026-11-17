//! Backups that run while Archivist is closed: a per-user launchd agent on macOS.
//! The agent simply runs `archivist backup` every N minutes with the saved settings.
//! Windows (Task Scheduler) and Linux (systemd timer) are not automated yet, see README.

use anyhow::{bail, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// launchd label. Overridable for tests so the real agent is never touched.
fn label() -> String {
    std::env::var("ARCHIVIST_SCHEDULE_LABEL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "it.archivist.backup".into())
}

pub fn supported() -> bool {
    cfg!(target_os = "macos")
}

fn plist_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{}.plist", label())))
}

fn log_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/Logs/Archivist/backup.log"))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The launchd property list for a backup every `minutes` minutes.
pub fn plist(label: &str, exe: &Path, minutes: u32, log: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>backup</string>
  </array>
  <key>StartInterval</key>
  <integer>{secs}</integer>
  <key>RunAtLoad</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>LowPriorityIO</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        exe = xml_escape(&exe.display().to_string()),
        secs = minutes.max(1) * 60,
        log = xml_escape(&log.display().to_string()),
    )
}

/// Minutes between runs, read back from a property list written by `plist`.
pub fn parse_minutes(plist: &str) -> Option<u32> {
    let i = plist.find("<key>StartInterval</key>")?;
    let rest = &plist[i..];
    let a = rest.find("<integer>")? + "<integer>".len();
    let b = rest.find("</integer>")?;
    rest.get(a..b)?.trim().parse::<u32>().ok().map(|s| s / 60)
}

#[derive(Serialize, Clone, Default)]
pub struct Status {
    pub supported: bool,
    pub installed: bool,
    pub minutes: u32,
    /// the executable the agent runs no longer exists (app moved or deleted)
    pub stale: bool,
    pub log: String,
    /// last line of the log, if any
    pub last_log: String,
}

pub fn status() -> Status {
    let mut s = Status { supported: supported(), ..Default::default() };
    if !s.supported {
        return s;
    }
    if let Some(p) = plist_path() {
        if let Ok(txt) = std::fs::read_to_string(&p) {
            s.installed = true;
            s.minutes = parse_minutes(&txt).unwrap_or(0);
            // <string>/path/to/exe</string> right after ProgramArguments
            if let Some(i) = txt.find("<key>ProgramArguments</key>") {
                let r = &txt[i..];
                if let (Some(a), Some(b)) = (r.find("<string>"), r.find("</string>")) {
                    let exe = r[a + 8..b].replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">");
                    s.stale = !Path::new(&exe).exists();
                }
            }
        }
    }
    if let Some(l) = log_path() {
        s.log = l.display().to_string();
        if let Ok(txt) = std::fs::read_to_string(&l) {
            s.last_log = txt.lines().rev().find(|x| !x.trim().is_empty()).unwrap_or("").to_string();
        }
    }
    s
}

#[cfg(target_os = "macos")]
fn launchctl(args: &[&str]) -> Result<()> {
    let out = std::process::Command::new("launchctl").args(args).output()?;
    if !out.status.success() {
        bail!("launchctl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn domain() -> Result<String> {
    let out = std::process::Command::new("id").arg("-u").output()?;
    Ok(format!("gui/{}", String::from_utf8_lossy(&out.stdout).trim()))
}

/// Install (or replace) the agent and start it. Runs `exe backup` every `minutes`.
#[cfg(target_os = "macos")]
pub fn install(exe: &Path, minutes: u32) -> Result<()> {
    let (Some(p), Some(log)) = (plist_path(), log_path()) else { bail!(tr!("rs.schedule_no_home")) };
    if let Some(d) = log.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::backend::write_atomic(&p, plist(&label(), exe, minutes, &log).as_bytes())?;
    let dom = domain()?;
    let target = format!("{dom}/{}", label());
    let _ = launchctl(&["bootout", &target]); // not loaded yet is fine
    launchctl(&["bootstrap", &dom, &p.display().to_string()])
}

#[cfg(not(target_os = "macos"))]
pub fn install(_exe: &Path, _minutes: u32) -> Result<()> {
    bail!(tr!("rs.schedule_unsupported"))
}

#[cfg(target_os = "macos")]
pub fn remove() -> Result<()> {
    let dom = domain()?;
    let _ = launchctl(&["bootout", &format!("{dom}/{}", label())]);
    if let Some(p) = plist_path() {
        match std::fs::remove_file(p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn remove() -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_has_the_expected_shape_and_escapes_paths() {
        let p = plist("it.test", Path::new("/Apps/A&B <x>/archivist"), 30, Path::new("/tmp/l.log"));
        assert!(p.contains("<string>it.test</string>"));
        assert!(p.contains("<string>/Apps/A&amp;B &lt;x&gt;/archivist</string>"));
        assert!(p.contains("<string>backup</string>"));
        assert!(p.contains("<integer>1800</integer>"));
        assert!(p.contains("<key>RunAtLoad</key>"));
        assert!(!p.contains("A&B"), "raw ampersand would break the XML");
    }

    #[test]
    fn minutes_roundtrip_and_minimum() {
        assert_eq!(parse_minutes(&plist("l", Path::new("/x"), 90, Path::new("/l"))), Some(90));
        assert_eq!(parse_minutes(&plist("l", Path::new("/x"), 0, Path::new("/l"))), Some(1), "never less than a minute");
        assert_eq!(parse_minutes("<plist></plist>"), None);
    }
}
