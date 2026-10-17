//! Where the app keeps its state (same layout as alm/paths.py).
//!
//! HOME: ALM_HOME if set, else %LOCALAPPDATA%\StableALM (Windows) or
//! ~/Library/Application Support/StableALM (macOS), else ~/.stable-alm.
//! Inside: config/, logs/, data/, secrets/.

use std::path::PathBuf;
use std::sync::OnceLock;

static HOME: OnceLock<PathBuf> = OnceLock::new();

pub fn home() -> &'static PathBuf {
    HOME.get_or_init(|| {
        if let Ok(h) = std::env::var("ALM_HOME") {
            if !h.is_empty() {
                return PathBuf::from(h);
            }
        }
        #[cfg(windows)]
        if let Ok(l) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(l).join("StableALM");
        }
        let user = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
        if cfg!(target_os = "macos") {
            return user.join("Library").join("Application Support").join("StableALM");
        }
        user.join(".stable-alm")
    })
}

pub fn config() -> PathBuf {
    home().join("config")
}
pub fn logs() -> PathBuf {
    home().join("logs")
}
pub fn data() -> PathBuf {
    home().join("data")
}
pub fn secrets() -> PathBuf {
    std::env::var("ALM_SECRETS_DIR").map(PathBuf::from).unwrap_or_else(|_| home().join("secrets"))
}
pub fn stop_file() -> PathBuf {
    std::env::var("ALM_STOP_FILE").map(PathBuf::from).unwrap_or_else(|_| config().join("STOP"))
}

/// First start: create the folders and an empty pool list (nothing is touched until pools are
/// imported or added).
pub fn seed() {
    for d in [config(), logs(), data(), secrets()] {
        let _ = std::fs::create_dir_all(d);
    }
    let pools = config().join("pools.json");
    if !pools.exists() {
        let _ = std::fs::write(pools, "[]\n");
    }
}
