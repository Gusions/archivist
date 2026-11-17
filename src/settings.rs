use crate::ops::Retention;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    pub vault: String,
    pub repo: String,
    pub exclude: Vec<String>,
    pub level: i32,
    pub retention: Retention,
    /// 0 = disattivato. Funziona finché l'app è aperta.
    pub auto_minutes: u32,
    /// "system" | "light" | "dark"
    pub theme: String,
    /// "system" oppure codice lingua (en, it, …)
    pub lang: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            vault: String::new(),
            repo: String::new(),
            exclude: vec![
                ".trash".into(),
                ".obsidian/workspace.json".into(),
                ".obsidian/workspace-mobile.json".into(),
                ".DS_Store".into(),
                "Thumbs.db".into(),
            ],
            level: 6,
            retention: Retention::default(),
            auto_minutes: 0,
            theme: "system".into(),
            lang: "system".into(),
        }
    }
}

/// Settings folder. `ARCHIVIST_CONFIG_DIR` overrides the system default (separate profiles, tests);
/// a background agent installed with it set keeps using the same folder.
fn path() -> PathBuf {
    if let Some(d) = std::env::var_os("ARCHIVIST_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d).join("settings.json");
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("Archivist").join("settings.json")
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
    pub fn save(&self) -> Result<()> {
        let p = path();
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(p, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

/// Vault noti a Obsidian (legge obsidian.json nella cartella di configurazione).
pub fn detect_vaults() -> Vec<String> {
    let p = dirs::config_dir().map(|d| d.join("obsidian").join("obsidian.json"));
    let Some(p) = p else { return vec![] };
    let Ok(b) = std::fs::read(p) else { return vec![] };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&b) else { return vec![] };
    v.get("vaults")
        .and_then(|v| v.as_object())
        .map(|o| o.values().filter_map(|x| x.get("path").and_then(|p| p.as_str()).map(String::from)).collect())
        .unwrap_or_default()
}

/// Espande "~" iniziale nella home dell'utente.
pub fn expand(p: &str) -> String {
    let p = p.trim();
    if p == "~" || p.starts_with("~/") || p.starts_with("~\\") {
        if let Some(h) = dirs::home_dir() {
            return h.join(p[1..].trim_start_matches(['/', '\\'])).display().to_string();
        }
    }
    p.to_string()
}
