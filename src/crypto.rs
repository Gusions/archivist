//! Encryption for repositories (format v2).
//!
//! - Two random 32-byte master keys are generated per repository: `enc` (data) and `id` (chunk ids).
//! - They are stored in the repository file `keys`, wrapped with XChaCha20-Poly1305 under a key
//!   derived from the passphrase with Argon2id. Changing the passphrase only rewraps this file.
//! - Every chunk, pack index, snapshot and metadata blob is sealed with XChaCha20-Poly1305
//!   (random 192-bit nonce, authenticated with the blob's identity as associated data, so blobs
//!   cannot be swapped or replayed under another name).
//! - Chunk ids are `BLAKE3-keyed(id key, data)`: deduplication still works inside one repository
//!   but ids reveal nothing about content across repositories.
//!
//! Not hidden: file names of packs and snapshots (timestamps), blob sizes, and the number of
//! snapshots. Contents, file names, vault path and the directory tree are encrypted.

use anyhow::{anyhow, bail, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use zeroize::Zeroize;

const NONCE_LEN: usize = 24;
const KEYFILE_AAD: &[u8] = b"archivist-keys-v1";

#[derive(Clone)]
pub struct Keys {
    enc: [u8; 32],
    id: [u8; 32],
}

impl Drop for Keys {
    fn drop(&mut self) {
        self.enc.zeroize();
        self.id.zeroize();
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct KdfParams {
    /// memory in KiB
    pub m: u32,
    pub t: u32,
    pub p: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        KdfParams { m: 64 * 1024, t: 3, p: 4 }
    }
}

impl KdfParams {
    /// Cheap parameters, for tests only.
    #[cfg(test)]
    pub fn fast() -> Self {
        KdfParams { m: 8, t: 1, p: 1 }
    }
    fn validate(&self) -> Result<()> {
        // refuse absurd values from a tampered key file (memory/CPU exhaustion)
        if self.m < 8 || self.m > 2 * 1024 * 1024 || self.t == 0 || self.t > 20 || self.p == 0 || self.p > 16 {
            bail!(tr!("rs.keyfile_bad"));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct KdfDoc {
    alg: String,
    salt: String,
    #[serde(flatten)]
    params: KdfParams,
}

#[derive(Serialize, Deserialize)]
struct KeyFile {
    v: u32,
    kdf: KdfDoc,
    nonce: String,
    wrapped: String,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        bail!(tr!("rs.keyfile_bad"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| anyhow!(tr!("rs.keyfile_bad"))))
        .collect()
}

fn derive_kek(pass: &str, salt: &[u8], p: &KdfParams) -> Result<[u8; 32]> {
    p.validate()?;
    let params = Params::new(p.m, p.t, p.p, Some(32)).map_err(|e| anyhow!("{e}"))?;
    let mut out = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(pass.as_bytes(), salt, &mut out)
        .map_err(|e| anyhow!("{e}"))?;
    Ok(out)
}

fn aead_seal(key: &[u8; 32], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
    let nonce: [u8; NONCE_LEN] = rand::random();
    let ct = XChaCha20Poly1305::new(key.into())
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad })
        .map_err(|_| anyhow!("encryption failed"))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

fn aead_open(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < NONCE_LEN + 16 {
        return None;
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    XChaCha20Poly1305::new(key.into()).decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad }).ok()
}

impl Keys {
    pub fn generate() -> Keys {
        Keys { enc: rand::random(), id: rand::random() }
    }

    /// Seal a blob. `aad` is the identity of the blob (chunk id or storage key).
    pub fn seal(&self, aad: &str, plain: &[u8]) -> Result<Vec<u8>> {
        aead_seal(&self.enc, aad.as_bytes(), plain)
    }

    pub fn open(&self, aad: &str, sealed: &[u8]) -> Result<Vec<u8>> {
        aead_open(&self.enc, aad.as_bytes(), sealed).ok_or_else(|| anyhow!(tr!("rs.decrypt_failed", k = aad)))
    }

    pub fn chunk_id(&self, data: &[u8]) -> String {
        blake3::keyed_hash(&self.id, data).to_hex().to_string()
    }

    /// Serialize the master keys wrapped under `pass` (new random salt and nonce).
    pub fn to_keyfile(&self, pass: &str, params: KdfParams) -> Result<Vec<u8>> {
        let salt: [u8; 16] = rand::random();
        let mut kek = derive_kek(pass, &salt, &params)?;
        let mut plain = [0u8; 64];
        plain[..32].copy_from_slice(&self.enc);
        plain[32..].copy_from_slice(&self.id);
        let sealed = aead_seal(&kek, KEYFILE_AAD, &plain);
        kek.zeroize();
        plain.zeroize();
        let sealed = sealed?;
        let doc = KeyFile {
            v: 1,
            kdf: KdfDoc { alg: "argon2id".into(), salt: hex(&salt), params },
            nonce: hex(&sealed[..NONCE_LEN]),
            wrapped: hex(&sealed[NONCE_LEN..]),
        };
        Ok(serde_json::to_vec_pretty(&doc)?)
    }

    pub fn from_keyfile(raw: &[u8], pass: &str) -> Result<Keys> {
        let doc: KeyFile = serde_json::from_slice(raw).map_err(|_| anyhow!(tr!("rs.keyfile_bad")))?;
        if doc.v != 1 || doc.kdf.alg != "argon2id" {
            bail!(tr!("rs.keyfile_bad"));
        }
        let mut kek = derive_kek(pass, &unhex(&doc.kdf.salt)?, &doc.kdf.params)?;
        let mut sealed = unhex(&doc.nonce)?;
        sealed.extend(unhex(&doc.wrapped)?);
        let plain = aead_open(&kek, KEYFILE_AAD, &sealed);
        kek.zeroize();
        let mut plain = plain.ok_or_else(|| anyhow!(tr!("rs.wrong_password")))?;
        if plain.len() != 64 {
            plain.zeroize();
            bail!(tr!("rs.keyfile_bad"));
        }
        let mut k = Keys { enc: [0; 32], id: [0; 32] };
        k.enc.copy_from_slice(&plain[..32]);
        k.id.copy_from_slice(&plain[32..]);
        plain.zeroize();
        Ok(k)
    }
}

// ---------- passphrase for this process ----------

static PASSWORD: RwLock<Option<String>> = RwLock::new(None);

/// Set the passphrase used when opening encrypted repositories (CLI prompt, UI unlock).
pub fn set_password(p: Option<String>) {
    *PASSWORD.write().unwrap() = p;
}

/// Passphrase in order of precedence: set explicitly, `ARCHIVIST_PASSWORD`,
/// first line of the file named by `ARCHIVIST_PASSWORD_FILE`.
pub fn password() -> Option<String> {
    if let Some(p) = PASSWORD.read().unwrap().clone() {
        return Some(p);
    }
    if let Ok(p) = std::env::var("ARCHIVIST_PASSWORD") {
        if !p.is_empty() {
            return Some(p);
        }
    }
    if let Ok(f) = std::env::var("ARCHIVIST_PASSWORD_FILE") {
        if let Ok(s) = std::fs::read_to_string(f) {
            let l = s.lines().next().unwrap_or("").to_string();
            if !l.is_empty() {
                return Some(l);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip_and_binding() {
        let k = Keys::generate();
        let s = k.seal("packs/a.idx", b"secret").unwrap();
        assert_ne!(&s[NONCE_LEN..], b"secret");
        assert_eq!(k.open("packs/a.idx", &s).unwrap(), b"secret");
        // wrong identity, flipped bit and truncation are all rejected
        assert!(k.open("packs/b.idx", &s).is_err());
        let mut bad = s.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(k.open("packs/a.idx", &bad).is_err());
        assert!(k.open("packs/a.idx", &s[..10]).is_err());
        // two seals of the same data differ (random nonce)
        assert_ne!(s, k.seal("packs/a.idx", b"secret").unwrap());
    }

    #[test]
    fn keyfile_roundtrip_wrong_password_and_rewrap() {
        let k = Keys::generate();
        let f = k.to_keyfile("correct horse", KdfParams::fast()).unwrap();
        let k2 = Keys::from_keyfile(&f, "correct horse").unwrap();
        assert_eq!(k.chunk_id(b"x"), k2.chunk_id(b"x"));
        assert!(Keys::from_keyfile(&f, "wrong").is_err());
        let f2 = k2.to_keyfile("new pass", KdfParams::fast()).unwrap();
        assert_ne!(f, f2);
        assert!(Keys::from_keyfile(&f2, "correct horse").is_err());
        assert_eq!(Keys::from_keyfile(&f2, "new pass").unwrap().chunk_id(b"x"), k.chunk_id(b"x"));
    }

    #[test]
    fn chunk_ids_depend_on_the_repository_key() {
        let a = Keys::generate();
        let b = Keys::generate();
        assert_eq!(a.chunk_id(b"same"), a.chunk_id(b"same"));
        assert_ne!(a.chunk_id(b"same"), b.chunk_id(b"same"));
        assert_ne!(a.chunk_id(b"same"), blake3::hash(b"same").to_hex().to_string());
    }

    #[test]
    fn tampered_kdf_params_are_refused() {
        let k = Keys::generate();
        let f = k.to_keyfile("p", KdfParams::fast()).unwrap();
        let mut v: serde_json::Value = serde_json::from_slice(&f).unwrap();
        v["kdf"]["m"] = serde_json::json!(u32::MAX);
        assert!(Keys::from_keyfile(&serde_json::to_vec(&v).unwrap(), "p").is_err());
    }
}
