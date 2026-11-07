//! The encrypted credentials vault (`<data>/credentials_vault.enc`), in the same format the
//! Python engine reads and writes:
//!
//!   Windows:   "DPAPI\x01" + CryptProtectData(json, entropy)        (tied to the Windows account)
//!   elsewhere: "AESGCM\x02" + nonce(16) + tag(16) + AES-256-GCM(json)
//!              with key = SHA-256(<config>/.credential_key + entropy)
//!
//! The JSON is {"version": 1, "providers": {<id>: {cookie | token | api_key, username, …}}}.
//! Every change re-reads the file first, so changes the Python engine made meanwhile are kept.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const CONTEXT: &str = "pawchive_auth_vault";

fn entropy() -> Vec<u8> {
    format!("pawchive_{CONTEXT}_secure_salt").into_bytes()
}

pub struct Vault {
    file: PathBuf,
    config_dir: PathBuf,
}

impl Vault {
    pub fn new(data_dir: &Path, config_dir: &Path) -> Self {
        Self { file: data_dir.join("credentials_vault.enc"), config_dir: config_dir.to_path_buf() }
    }

    /// The providers map, read fresh from disk ({} when there's no vault yet).
    pub fn read(&self) -> Result<Map<String, Value>, String> {
        let raw = match std::fs::read(&self.file) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(e) => return Err(format!("couldn't read the vault: {e}")),
        };
        if raw.is_empty() {
            return Ok(Map::new());
        }
        let plain = decrypt(&raw, &self.config_dir)?;
        let doc: Value = serde_json::from_slice(&plain).map_err(|e| format!("the vault isn't valid JSON: {e}"))?;
        Ok(doc.get("providers").and_then(Value::as_object).cloned().unwrap_or_default())
    }

    /// Re-reads the vault, applies `change` and writes it back (atomically).
    pub fn update<F: FnOnce(&mut Map<String, Value>)>(&self, change: F) -> Result<(), String> {
        let mut providers = self.read()?;
        change(&mut providers);
        let doc = json!({"version": 1, "providers": providers});
        let plain = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
        let enc = encrypt(&plain, &self.config_dir)?;
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = self.file.with_extension("enc.tmp");
        std::fs::write(&tmp, &enc).map_err(|e| format!("couldn't write the vault: {e}"))?;
        std::fs::rename(&tmp, &self.file).map_err(|e| format!("couldn't write the vault: {e}"))
    }
}

// ── Encryption ────────────────────────────────────────────────────────────────

fn encrypt(plain: &[u8], config_dir: &Path) -> Result<Vec<u8>, String> {
    #[cfg(windows)]
    {
        let _ = config_dir;
        let mut out = b"DPAPI\x01".to_vec();
        out.extend(dpapi::protect(plain, &entropy(), &format!("pawchive_{CONTEXT}"))?);
        Ok(out)
    }
    #[cfg(not(windows))]
    {
        aes::encrypt(plain, config_dir)
    }
}

fn decrypt(raw: &[u8], config_dir: &Path) -> Result<Vec<u8>, String> {
    if let Some(blob) = raw.strip_prefix(b"DPAPI\x01") {
        #[cfg(windows)]
        return dpapi::unprotect(blob, Some(&entropy()));
        #[cfg(not(windows))]
        return Err(format!("the vault was saved on Windows ({} bytes)", blob.len()));
    }
    if raw.starts_with(b"AESGCM\x02") {
        return aes::decrypt(&raw[7..], config_dir);
    }
    // "AESGCM\x01" (a key from the network card's address) was upgraded by the Python engine
    let _ = config_dir;
    Err("the vault uses a format this version can't read".into())
}

#[cfg(windows)]
pub(super) mod dpapi {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: CryptProtectData / CryptUnprotectData filled `out` with a LocalAlloc'd buffer
        unsafe {
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            let _ = LocalFree(Some(HLOCAL(out.pbData as *mut _)));
            v
        }
    }

    pub fn protect(plain: &[u8], entropy: &[u8], description: &str) -> Result<Vec<u8>, String> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        let ent = blob(entropy);
        // SAFETY: the input blobs point into live slices for the duration of the call
        unsafe {
            CryptProtectData(&blob(plain), &HSTRING::from(description), Some(&ent), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
                .map_err(|e| format!("Windows couldn't encrypt the vault: {e}"))?;
        }
        Ok(take(out))
    }

    pub fn unprotect(enc: &[u8], entropy: Option<&[u8]>) -> Result<Vec<u8>, String> {
        let mut out = CRYPT_INTEGER_BLOB::default();
        let ent = entropy.map(blob);
        // SAFETY: as above
        unsafe {
            CryptUnprotectData(&blob(enc), None, ent.as_ref().map(|e| e as *const _), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)
                .map_err(|e| format!("Windows couldn't decrypt the vault (saved by another Windows account?): {e}"))?;
        }
        Ok(take(out))
    }
}

mod aes {
    use super::entropy;
    use aes_gcm::aead::consts::U16;
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::aes::Aes256;
    use aes_gcm::{AesGcm, Nonce};
    use sha2::{Digest, Sha256};
    use std::path::Path;

    /// PyCryptodome's AES-GCM default: a 16-byte nonce
    type Cipher = AesGcm<Aes256, U16>;

    fn key(config_dir: &Path) -> Result<[u8; 32], String> {
        let path = config_dir.join(".credential_key");
        let local = std::fs::read(&path).map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
        if local.len() != 32 {
            return Err("the vault key file is damaged".into());
        }
        let mut h = Sha256::new();
        h.update(&local);
        h.update(entropy());
        Ok(h.finalize().into())
    }

    pub fn decrypt(data: &[u8], config_dir: &Path) -> Result<Vec<u8>, String> {
        if data.len() < 32 {
            return Err("the vault is damaged".into());
        }
        let (nonce, rest) = data.split_at(16);
        let (tag, ct) = rest.split_at(16);
        let cipher = Cipher::new_from_slice(&key(config_dir)?).map_err(|e| e.to_string())?;
        // aes-gcm expects ciphertext followed by the tag
        let mut sealed = ct.to_vec();
        sealed.extend_from_slice(tag);
        cipher.decrypt(Nonce::from_slice(nonce), sealed.as_ref()).map_err(|_| "couldn't decrypt the vault".to_string())
    }

    #[allow(dead_code)]
    pub fn encrypt(plain: &[u8], config_dir: &Path) -> Result<Vec<u8>, String> {
        let cipher = Cipher::new_from_slice(&key(config_dir)?).map_err(|e| e.to_string())?;
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
        let sealed = cipher.encrypt(Nonce::from_slice(&nonce), plain).map_err(|e| e.to_string())?;
        let (ct, tag) = sealed.split_at(sealed.len() - 16);
        let mut out = b"AESGCM\x02".to_vec();
        out.extend_from_slice(&nonce);
        out.extend_from_slice(tag);
        out.extend_from_slice(ct);
        Ok(out)
    }
}
