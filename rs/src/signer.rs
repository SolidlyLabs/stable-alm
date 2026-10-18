//! The signing key set from the web UI, stored without a password:
//! - Windows: DPAPI for the current user in secrets/private_key.dpapi. A plain secrets/private_key
//!   (copied in by hand) is converted on first read and deleted.
//! - macOS: the login Keychain (service "StableALM", account "signer").
//! - elsewhere: plain secrets/private_key (mode 600).
//! The key is never returned by any API and never logged.

use crate::eth::Account;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn plain_path() -> PathBuf {
    crate::paths::secrets().join("private_key")
}
fn dpapi_path() -> PathBuf {
    crate::paths::secrets().join("private_key.dpapi")
}

fn normalise(k: &str) -> String {
    let k = k.trim();
    if k.starts_with("0x") {
        k.to_string()
    } else {
        format!("0x{k}")
    }
}

#[cfg(windows)]
mod store {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};
    const ENTROPY: &[u8] = b"stable-alm signer v1";
    const UI_FORBIDDEN: u32 = 0x1;

    fn blob(d: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: d.len() as u32, pbData: d.as_ptr() as *mut u8 }
    }

    fn run(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
        let inp = blob(data);
        let ent = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = unsafe {
            if protect {
                CryptProtectData(&inp, std::ptr::null(), &ent, std::ptr::null(), std::ptr::null(), UI_FORBIDDEN, &mut out)
            } else {
                CryptUnprotectData(&inp, std::ptr::null_mut(), &ent, std::ptr::null(), std::ptr::null(), UI_FORBIDDEN, &mut out)
            }
        };
        if ok == 0 {
            return Err("DPAPI failed".into());
        }
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
        unsafe { LocalFree(out.pbData as _) };
        Ok(v)
    }

    pub fn save(key: &str) -> Result<(), String> {
        let enc = run(key.as_bytes(), true)?;
        crate::paths::write_secret(&super::dpapi_path(), &enc).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(super::plain_path());
        Ok(())
    }

    pub fn load() -> Result<Option<String>, String> {
        let plain = super::plain_path();
        if plain.exists() {
            let k = super::normalise(&std::fs::read_to_string(&plain).map_err(|e| e.to_string())?);
            crate::eth::Account::from_key(&k)?; // never delete a plain file that is not a valid key
            save(&k)?;
        }
        match std::fs::read(super::dpapi_path()) {
            Ok(b) => Ok(Some(String::from_utf8(run(&b, false)?).map_err(|e| e.to_string())?)),
            Err(_) => Ok(None),
        }
    }

    pub fn delete() {
        let _ = std::fs::remove_file(super::dpapi_path());
        let _ = std::fs::remove_file(super::plain_path());
    }
    pub const KIND: &str = "Windows DPAPI";
}

#[cfg(target_os = "macos")]
mod store {
    use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};
    const SERVICE: &str = "StableALM";
    const ACCOUNT: &str = "signer";

    pub fn save(key: &str) -> Result<(), String> {
        set_generic_password(SERVICE, ACCOUNT, key.as_bytes()).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(super::plain_path());
        Ok(())
    }
    pub fn load() -> Result<Option<String>, String> {
        let plain = super::plain_path();
        if plain.exists() {
            let k = super::normalise(&std::fs::read_to_string(&plain).map_err(|e| e.to_string())?);
            crate::eth::Account::from_key(&k)?;
            save(&k)?;
        }
        match get_generic_password(SERVICE, ACCOUNT) {
            Ok(b) => Ok(Some(String::from_utf8(b).map_err(|e| e.to_string())?)),
            Err(_) => Ok(None),
        }
    }
    pub fn delete() {
        let _ = delete_generic_password(SERVICE, ACCOUNT);
        let _ = std::fs::remove_file(super::plain_path());
    }
    pub const KIND: &str = "macOS Keychain";
}

#[cfg(not(any(windows, target_os = "macos")))]
mod store {
    pub fn save(key: &str) -> Result<(), String> {
        // folder 700, file 600, written atomically (paths::write_secret)
        crate::paths::write_secret(&super::plain_path(), key.as_bytes()).map_err(|e| e.to_string())
    }
    pub fn load() -> Result<Option<String>, String> {
        Ok(std::fs::read_to_string(super::plain_path()).ok())
    }
    pub fn delete() {
        let _ = std::fs::remove_file(super::plain_path());
    }
    pub const KIND: &str = "plain file";
}

/// Validate, store; returns the address.
pub fn save_key(k: &str) -> Result<String, String> {
    let k = normalise(k);
    let a = Account::from_key(&k)?;
    let _g = LOCK.lock().unwrap();
    store::save(&k)?;
    Ok(a.address)
}

pub fn forget() {
    let _g = LOCK.lock().unwrap();
    store::delete();
}

pub fn account() -> Result<Option<Account>, String> {
    let _g = LOCK.lock().unwrap();
    if let Ok(k) = std::env::var("ALM_PRIVATE_KEY") {
        return Account::from_key(&normalise(&k)).map(Some);
    }
    match store::load()? {
        Some(k) => Account::from_key(&normalise(&k)).map(Some),
        None => Ok(None),
    }
}

pub fn configured() -> bool {
    matches!(account(), Ok(Some(_)))
}

pub fn load_account() -> Result<Account, String> {
    account()?.ok_or_else(|| "No signer: set the private key in the dashboard".to_string())
}

pub fn status() -> Value {
    match account() {
        Ok(Some(a)) => json!({"stored": true, "address": a.address, "storage": store::KIND}),
        Ok(None) => json!({"stored": false, "address": null, "storage": store::KIND}),
        Err(e) => json!({"stored": true, "address": null, "error": format!("stored key is invalid: {e}"), "storage": store::KIND}),
    }
}
