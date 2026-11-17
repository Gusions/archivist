//! Traduzioni condivise da motore, CLI e interfaccia.
//! Catalogo: `i18n/<codice>.json` (chiave -> testo con segnaposto `{nome}`).
//! Per aggiungere una lingua: crea il file JSON e aggiungi una riga in `CATALOGS`.
//! L'inglese è la lingua base: le chiavi mancanti ricadono su di esso.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

/// (codice, nome nativo, contenuto)
pub const CATALOGS: &[(&str, &str, &str)] = &[
    ("en", "English", include_str!("../i18n/en.json")),
    ("it", "Italiano", include_str!("../i18n/it.json")),
];

const FALLBACK: &str = "en";

static PARSED: OnceLock<HashMap<&'static str, HashMap<String, String>>> = OnceLock::new();
static LANG: RwLock<String> = RwLock::new(String::new());

fn parsed() -> &'static HashMap<&'static str, HashMap<String, String>> {
    PARSED.get_or_init(|| {
        CATALOGS
            .iter()
            .map(|(code, _, raw)| (*code, serde_json::from_str(raw).expect("catalogo i18n non valido")))
            .collect()
    })
}

pub fn available() -> Vec<(&'static str, &'static str)> {
    CATALOGS.iter().map(|(c, n, _)| (*c, *n)).collect()
}

/// Riduce un tag tipo "it-IT" o "it_IT.UTF-8" a un codice disponibile ("it"), altrimenti inglese.
pub fn resolve(tag: &str) -> String {
    let base = tag.split(|c| c == '-' || c == '_' || c == '.').next().unwrap_or("").to_lowercase();
    if CATALOGS.iter().any(|(c, _, _)| *c == base) {
        base
    } else {
        FALLBACK.to_string()
    }
}

/// Lingua del sistema (variabili d'ambiente / locale di sistema).
pub fn system_lang() -> String {
    resolve(&sys_locale::get_locale().unwrap_or_default())
}

/// Imposta la lingua attiva: "system" oppure un codice.
pub fn set_lang(setting: &str) {
    let l = if setting.is_empty() || setting == "system" { system_lang() } else { resolve(setting) };
    *LANG.write().unwrap() = l;
}

pub fn current() -> String {
    let l = LANG.read().unwrap().clone();
    if l.is_empty() {
        system_lang()
    } else {
        l
    }
}

pub fn t(key: &str, args: &[(&str, String)]) -> String {
    let cats = parsed();
    let lang = current();
    let mut s = cats
        .get(lang.as_str())
        .and_then(|c| c.get(key))
        .or_else(|| cats.get(FALLBACK).and_then(|c| c.get(key)))
        .cloned()
        .unwrap_or_else(|| key.to_string());
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// Catalogo completo per la UI: lingua richiesta sopra l'inglese.
pub fn catalog_json(lang: &str) -> Value {
    let cats = parsed();
    let mut m: HashMap<String, String> = cats.get(FALLBACK).cloned().unwrap_or_default();
    if let Some(c) = cats.get(lang) {
        m.extend(c.clone());
    }
    serde_json::to_value(m).unwrap()
}

#[macro_export]
macro_rules! tr {
    ($key:expr) => { $crate::i18n::t($key, &[]) };
    ($key:expr, $($k:ident = $v:expr),+ $(,)?) => {
        $crate::i18n::t($key, &[$((stringify!($k), $v.to_string())),+])
    };
}
