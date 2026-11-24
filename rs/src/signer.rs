//! The signing key set from the web UI, stored without a password:
//! - Windows: DPAPI for the current user in secrets/private_key.dpapi. A plain secrets/private_key
//!   (copied in by hand) is converted on first read and deleted.
//! - macOS: the login Keychain (service "StableALM", account "signer").
//! - Linux: the desktop keyring via Secret Service.
//! - elsewhere: plain secrets/private_key (mode 600).
//! The key is never returned by any API and never logged.
//! Each wallet (wallets.rs) has its own: the paths above are its secrets folder (paths::secrets), and
//! on macOS its Keychain account is "signer-<id>" ("signer" for the first wallet, as before).

use crate::eth::Account;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn plain_path() -> PathBuf {
    crate::paths::secrets().join("private_key")
}
#[cfg(windows)]
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

    pub fn run(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
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
    fn account() -> String {
        let id = crate::paths::wallet();
        if id == crate::paths::MAIN { "signer".into() } else { format!("signer-{id}") }
    }

    pub fn save(key: &str) -> Result<(), String> {
        set_generic_password(SERVICE, &account(), key.as_bytes()).map_err(|e| e.to_string())?;
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
        match get_generic_password(SERVICE, &account()) {
            Ok(b) => Ok(Some(String::from_utf8(b).map_err(|e| e.to_string())?)),
            Err(_) => Ok(None),
        }
    }
    pub fn delete() {
        let _ = delete_generic_password(SERVICE, &account());
        let _ = std::fs::remove_file(super::plain_path());
    }
    pub const KIND: &str = "macOS Keychain";
}

#[cfg(target_os = "linux")]
pub(crate) mod keyring_store {
    use keyring::{Entry, Error};
    use sha2::{Digest, Sha256};
    use std::os::unix::ffi::OsStrExt;

    pub const KIND: &str = "Linux Secret Service";

    fn service() -> String {
        let dir = crate::paths::app_secrets();
        let identity = std::fs::canonicalize(&dir).unwrap_or(dir);
        format!("StableALM:{:x}", Sha256::digest(identity.as_os_str().as_bytes()))
    }

    fn error(_: Error) -> String {
        "Could not access the Linux keyring. Start and unlock your desktop keyring and try again.".into()
    }

    #[cfg(not(test))]
    fn with_entry<T>(name: &str, f: impl FnOnce(&Entry) -> keyring::Result<T>) -> keyring::Result<T> {
        f(&Entry::new(&service(), name)?)
    }

    #[cfg(test)]
    fn with_entry<T>(name: &str, f: impl FnOnce(&Entry) -> keyring::Result<T>) -> keyring::Result<T> {
        use std::collections::HashMap;
        use std::sync::{LazyLock, Mutex};
        static ENTRIES: LazyLock<Mutex<HashMap<(String, String), Entry>>> = LazyLock::new(Mutex::default);
        let mut entries = ENTRIES.lock().unwrap();
        let entry = entries.entry((service(), name.into()))
            .or_insert_with(|| Entry::new_with_credential(Box::new(keyring::mock::MockCredential::default())));
        f(entry)
    }

    pub fn save(name: &str, value: &str) -> Result<(), String> {
        with_entry(name, |entry| entry.set_password(value)).map_err(error)
    }

    pub fn load(name: &str) -> Result<Option<String>, String> {
        match with_entry(name, Entry::get_password) {
            Ok(value) => Ok(Some(value)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(error(e)),
        }
    }

    pub fn delete(name: &str) -> Result<(), String> {
        match with_entry(name, Entry::delete_credential) {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(error(e)),
        }
    }

    pub fn remove_legacy(path: &std::path::Path) -> Result<(), String> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("could not remove the old credential file: {e}")),
        }
    }

    pub fn load_or_migrate(name: &str, path: &std::path::Path, validate: impl FnOnce(&str) -> Result<String, String>) -> Result<Option<String>, String> {
        let stored = load(name)?;
        let value = match std::fs::read_to_string(path) {
            Ok(value) => value,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(stored),
            Err(e) => return Err(format!("could not read the old credential file: {e}")),
        };
        let value = match stored {
            Some(value) => value,
            None => {
                let value = validate(&value)?;
                save(name, &value)?;
                value
            }
        };
        remove_legacy(path)?;
        Ok(Some(value))
    }
}

#[cfg(target_os = "linux")]
mod store {
    use super::keyring_store;
    pub use keyring_store::KIND;

    fn name() -> String {
        format!("signer-{}", crate::paths::wallet())
    }

    pub fn save(key: &str) -> Result<(), String> {
        keyring_store::save(&name(), key)?;
        keyring_store::remove_legacy(&super::plain_path())
    }
    pub fn load() -> Result<Option<String>, String> {
        keyring_store::load_or_migrate(&name(), &super::plain_path(), |value| {
            let key = super::normalise(value);
            crate::eth::Account::from_key(&key)?;
            Ok(key)
        })
    }
    pub fn delete() -> Result<(), String> {
        keyring_store::remove_legacy(&super::plain_path())?;
        keyring_store::delete(&name())
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
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

pub fn forget() -> Result<(), String> {
    let _g = LOCK.lock().unwrap();
    #[cfg(target_os = "linux")]
    return store::delete();
    #[cfg(not(target_os = "linux"))]
    {
        store::delete();
        Ok(())
    }
}

pub fn account() -> Result<Option<Account>, String> {
    let _g = LOCK.lock().unwrap();
    if crate::paths::wallet() == crate::paths::MAIN {
        if let Ok(k) = std::env::var("ALM_PRIVATE_KEY") {
            return Account::from_key(&normalise(&k)).map(Some);
        }
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
        Err(e) => json!({"stored": true, "address": null, "error": e, "storage": store::KIND}),
    }
}

/// An app-wide credential, such as the Claude API key or Telegram bot token.
pub fn save_secret(name: &str, value: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        let enc = store::run(value.as_bytes(), true)?;
        crate::paths::write_secret(&crate::paths::app_secrets().join(format!("{name}.dpapi")), &enc).map_err(|e| e.to_string())
    }
    #[cfg(target_os = "linux")]
    {
        let _g = LOCK.lock().unwrap();
        keyring_store::save(name, value)?;
        keyring_store::remove_legacy(&crate::paths::app_secrets().join(name))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        crate::paths::write_secret(&crate::paths::app_secrets().join(name), value.as_bytes()).map_err(|e| e.to_string())
    }
}

pub fn load_secret(name: &str) -> Option<String> {
    #[cfg(windows)]
    {
        let b = std::fs::read(crate::paths::app_secrets().join(format!("{name}.dpapi"))).ok()?;
        String::from_utf8(store::run(&b, false).ok()?).ok()
    }
    #[cfg(target_os = "linux")]
    {
        let _g = LOCK.lock().unwrap();
        keyring_store::load_or_migrate(name, &crate::paths::app_secrets().join(name), |value| Ok(value.trim().into())).ok().flatten()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        std::fs::read_to_string(crate::paths::app_secrets().join(name)).ok().map(|s| s.trim().to_string())
    }
}

pub fn delete_secret(name: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let _g = LOCK.lock().unwrap();
        keyring_store::remove_legacy(&crate::paths::app_secrets().join(name))?;
        keyring_store::delete(name)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = std::fs::remove_file(crate::paths::app_secrets().join(format!("{name}.dpapi")));
        let _ = std::fs::remove_file(crate::paths::app_secrets().join(name));
        Ok(())
    }
}
