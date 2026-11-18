//! Native "choose a folder" dialog, so paths do not have to be typed.
//! macOS: AppleScript `choose folder`; Windows: the .NET folder dialog through PowerShell;
//! Linux: zenity or kdialog when installed. Run through the system, so it works the same in the
//! window and in the browser UI. Returns `None` when the user cancels.

use std::path::Path;
use std::process::Command;

/// Is a folder dialog available on this machine?
pub fn supported() -> bool {
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        return true;
    }
    which("zenity") || which("kdialog")
}

#[allow(dead_code)] // only used on Linux
fn which(prog: &str) -> bool {
    Command::new("sh").arg("-c").arg(format!("command -v {prog}")).output().map(|o| o.status.success()).unwrap_or(false)
}

/// Escape a string for use inside an AppleScript double-quoted literal.
pub fn applescript_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Ask the user for a folder. `start` is an existing folder to open at, `prompt` the dialog message.
pub fn pick_folder(start: &str, prompt: &str) -> Option<String> {
    let start = Path::new(start).is_dir().then_some(start);
    #[cfg(target_os = "macos")]
    {
        let default = start.map(|s| format!(" default location (POSIX file \"{}\")", applescript_quote(s))).unwrap_or_default();
        let script = format!("POSIX path of (choose folder with prompt \"{}\"{})", applescript_quote(prompt), default);
        let out = Command::new("osascript").args(["-e", "tell current application to activate", "-e", &script]).output().ok()?;
        return clean(&out);
    }
    #[cfg(target_os = "windows")]
    {
        let ps = format!(
            "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description = '{}'; {} if ($d.ShowDialog() -eq 'OK') {{ $d.SelectedPath }}",
            prompt.replace('\'', "''"),
            start.map(|s| format!("$d.SelectedPath = '{}';", s.replace('\'', "''"))).unwrap_or_default()
        );
        let out = Command::new("powershell").args(["-NoProfile", "-Command", &ps]).output().ok()?;
        return clean(&out);
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let out = if which("zenity") {
            let mut c = Command::new("zenity");
            c.args(["--file-selection", "--directory", &format!("--title={prompt}")]);
            if let Some(s) = start {
                c.arg(format!("--filename={s}/"));
            }
            c.output().ok()?
        } else {
            let mut c = Command::new("kdialog");
            c.args(["--getexistingdirectory", start.unwrap_or("."), "--title", prompt]);
            c.output().ok()?
        };
        return clean(&out);
    }
}

/// Successful, non-empty output (a cancelled dialog exits non-zero or prints nothing).
#[allow(dead_code)]
fn clean(out: &std::process::Output) -> Option<String> {
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // AppleScript returns a trailing slash; keep the path as users write it
    let s = if s.len() > 1 { s.trim_end_matches('/').to_string() } else { s };
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applescript_quoting_blocks_injection() {
        assert_eq!(applescript_quote(r#"a"b\c"#), r#"a\"b\\c"#);
        // whatever the input, every quote in the output is escaped, so it cannot leave the literal
        let q = applescript_quote("x\" & (do shell script \"id\") & \"\\");
        let chars: Vec<char> = q.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '\\' {
                i += 2; // an escape pair
                continue;
            }
            assert_ne!(chars[i], '"', "unescaped quote in {q}");
            i += 1;
        }
    }

    #[cfg(unix)]
    #[test]
    fn cancelled_or_failed_dialog_is_none() {
        use std::os::unix::process::ExitStatusExt;
        let fail = std::process::Output { status: std::process::ExitStatus::from_raw(1 << 8), stdout: b"/x/".to_vec(), stderr: vec![] };
        assert_eq!(clean(&fail), None);
        let ok = std::process::Output { status: std::process::ExitStatus::from_raw(0), stdout: b"/Users/me/Notes/\n".to_vec(), stderr: vec![] };
        assert_eq!(clean(&ok).as_deref(), Some("/Users/me/Notes"));
        let empty = std::process::Output { status: std::process::ExitStatus::from_raw(0), stdout: b"\n".to_vec(), stderr: vec![] };
        assert_eq!(clean(&empty), None);
    }
}
