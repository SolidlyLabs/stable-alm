//! Where the app keeps its state.
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

/// Why signing is not allowed right now (Pause / STOP file, or ALM_DISABLED=1), checked immediately
/// before every broadcast so a pause stops the next transaction, not only the next cycle.
pub fn execution_blocked() -> Option<String> {
    if stop_file().exists() {
        return Some("paused (kill switch is on)".into());
    }
    if std::env::var("ALM_DISABLED").as_deref() == Ok("1") {
        return Some("ALM_DISABLED=1 is set".into());
    }
    None
}

/// Write a secret (private key, dashboard token) atomically. On Unix the secrets folder is created
/// mode 700 and the file mode 600 from the start, whatever the umask; Windows relies on the
/// per-user profile folder (and DPAPI for the key).
pub fn write_secret(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(tmp, path)
}

/// Repair permissions of an existing secrets folder (Unix): folder 700, files 600.
pub fn harden_secrets() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let d = secrets();
        if d.exists() {
            let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                if e.path().is_file() {
                    let _ = std::fs::set_permissions(e.path(), std::fs::Permissions::from_mode(0o600));
                }
            }
        }
    }
}

/// First start: create the folders and an empty pool list (nothing is touched until pools are
/// imported or added).
pub fn seed() {
    for d in [config(), logs(), data(), secrets()] {
        let _ = std::fs::create_dir_all(d);
    }
    harden_secrets();
    let pools = config().join("pools.json");
    if !pools.exists() {
        let _ = std::fs::write(pools, "[]\n");
    }
}
