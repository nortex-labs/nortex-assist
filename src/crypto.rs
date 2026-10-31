//! Encrypted storage: a random master secret protected by Windows DPAPI,
//! HKDF-derived AES-256-GCM key for config and history files.

use aes_gcm::aead::{rand_core::RngCore, Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use std::path::{Path, PathBuf};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{LocalFree, HLOCAL};
use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};

const MAGIC: &[u8; 4] = b"NX03";
const IV_LEN: usize = 12;
const TAG_LEN: usize = 16;

pub struct Store {
    key: [u8; 32],
}

fn blob_to_vec(blob: &CRYPT_INTEGER_BLOB) -> Vec<u8> {
    unsafe {
        let v = std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(blob.pbData as _)));
        v
    }
}

fn dpapi_protect(data: &[u8]) -> Result<Vec<u8>, String> {
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(&input, PCWSTR::null(), None, None, None, 0, &mut out)
            .map_err(|e| format!("CryptProtectData: {e}"))?;
    }
    Ok(blob_to_vec(&out))
}

fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, 0, &mut out)
            .map_err(|e| format!("CryptUnprotectData: {e}"))?;
    }
    Ok(blob_to_vec(&out))
}

/// Copy of an unreadable file next to the original (once), so a decryption
/// failure never turns into silent data loss.
pub fn backup(file: &Path) {
    let mut b = file.as_os_str().to_owned();
    b.push(".bak");
    let b = PathBuf::from(b);
    if !b.exists() {
        let _ = std::fs::copy(file, b);
    }
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store, String> {
        let master_file = dir.join("master.bin");
        let mut master: Option<Vec<u8>> = None;
        if let Ok(existing) = std::fs::read(&master_file) {
            match dpapi_unprotect(&existing) {
                Ok(m) if m.len() >= 32 => master = Some(m),
                _ => backup(&master_file),
            }
        }
        let master = match master {
            Some(m) => m,
            None => {
                let mut m = vec![0u8; 48];
                OsRng.fill_bytes(&mut m);
                // If the master cannot be persisted, fail loudly: data encrypted with a
                // key that vanishes on restart would be lost.
                std::fs::write(&master_file, dpapi_protect(&m)?).map_err(|e| format!("master.bin: {e}"))?;
                m
            }
        };
        let mut key = [0u8; 32];
        Hkdf::<Sha256>::new(Some(b"nortex-master-v2"), &master)
            .expand(b"aes-256-gcm", &mut key)
            .map_err(|e| e.to_string())?;
        Ok(Store { key })
    }

    pub fn encrypt(&self, plain: &[u8]) -> Result<Vec<u8>, String> {
        let cipher = Aes256Gcm::new_from_slice(&self.key).map_err(|e| e.to_string())?;
        let mut iv = [0u8; IV_LEN];
        OsRng.fill_bytes(&mut iv);
        let ct = cipher.encrypt(Nonce::from_slice(&iv), plain).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(MAGIC.len() + IV_LEN + ct.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&iv);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    pub fn decrypt(&self, blob: &[u8]) -> Result<Vec<u8>, String> {
        if blob.len() < MAGIC.len() + IV_LEN + TAG_LEN || &blob[..4] != MAGIC {
            return Err("crypto-store: unknown format".into());
        }
        let cipher = Aes256Gcm::new_from_slice(&self.key).map_err(|e| e.to_string())?;
        let iv = &blob[4..4 + IV_LEN];
        cipher.decrypt(Nonce::from_slice(iv), &blob[4 + IV_LEN..]).map_err(|_| "crypto-store: decrypt failed".to_string())
    }
}

pub fn is_encrypted(blob: &[u8]) -> bool {
    blob.len() > MAGIC.len() && &blob[..4] == MAGIC
}
