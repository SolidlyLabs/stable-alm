//! Where the app keeps its state.
//!
//! HOME: ALM_HOME if set, else %LOCALAPPDATA%\StableALM (Windows) or
//! ~/Library/Application Support/StableALM (macOS), else ~/.stable-alm.
//! Inside: config/, logs/, data/, secrets/.
//!
//! Several wallets (wallets.rs): the first wallet, `main`, is the folders above exactly as they were.
//! Every other wallet has its own config/ and logs/ under wallets/<id>/, and its key under
//! secrets/wallets/<id>/. Code runs for one wallet inside `scope(id, ..)`: there `config()`, `logs()`
//! and `secrets()` are that wallet's folders, so its pools, capital, ledger, history and pending
//! transactions stay its own. `app_config()`, `app_logs()` and `app_secrets()` are always the app's
//! own (chains, routers, strategy library, the kill switch, alm.log, the dashboard token).
//! `data/` (market data) is shared: what is wallet-specific there has the address in its name.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::OnceLock;

static HOME: OnceLock<PathBuf> = OnceLock::new();

pub const MAIN: &str = "main";

thread_local! {
    static WALLET: RefCell<String> = RefCell::new(MAIN.to_string());
    #[cfg(test)]
    static TEST_HOME: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// The wallet this thread works for now (`main` outside any scope).
pub fn wallet() -> String {
    WALLET.with(|w| w.borrow().clone())
}

/// Run `f` for wallet `id`: its folders, settings, pools and key. Scopes nest and are restored even
/// when `f` panics.
pub fn scope<T>(id: &str, f: impl FnOnce() -> T) -> T {
    struct Back(String);
    impl Drop for Back {
        fn drop(&mut self) {
            let prev = std::mem::take(&mut self.0);
            WALLET.with(|w| *w.borrow_mut() = prev);
        }
    }
    let _back = Back(WALLET.with(|w| std::mem::replace(&mut *w.borrow_mut(), id.to_string())));
    f()
}

/// A thread that keeps the scope of the thread that starts it.
pub fn spawn<F: FnOnce() + Send + 'static>(f: F) -> std::thread::JoinHandle<()> {
    let id = wallet();
    #[cfg(test)]
    let th = TEST_HOME.with(|h| h.borrow().clone());
    std::thread::spawn(move || {
        #[cfg(test)]
        TEST_HOME.with(|h| *h.borrow_mut() = th);
        scope(&id, f)
    })
}

/// Tests: this thread's app home is `dir` (a temporary folder), never the user's.
#[cfg(test)]
pub fn set_test_home(dir: &std::path::Path) {
    TEST_HOME.with(|h| *h.borrow_mut() = Some(dir.to_path_buf()));
}

/// Where a wallet's own folders are, below the app home (`main`: the home itself).
pub fn wallet_root(id: &str) -> PathBuf {
    if id == MAIN {
        home().clone()
    } else {
        home().join("wallets").join(id)
    }
}

pub fn home() -> PathBuf {
    #[cfg(test)]
    if let Some(h) = TEST_HOME.with(|h| h.borrow().clone()) {
        return h;
    }
    #[cfg(test)]
    if std::env::var("ALM_HOME").map(|h| h.is_empty()).unwrap_or(true) {
        // a test that did not set a home must not reach the user's app folder
        return std::env::temp_dir().join("stable-alm-test-home");
    }
    real_home().clone()
}

fn real_home() -> &'static PathBuf {
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

/// The current wallet's config folder (pools.json, its settings.json).
pub fn config() -> PathBuf {
    wallet_root(&wallet()).join("config")
}
/// The current wallet's logs folder (capital, ledger, snapshots, executor log, pending transactions).
pub fn logs() -> PathBuf {
    wallet_root(&wallet()).join("logs")
}
pub fn data() -> PathBuf {
    home().join("data")
}
/// The current wallet's secrets folder (its private key).
pub fn secrets() -> PathBuf {
    let id = wallet();
    if id == MAIN {
        app_secrets()
    } else {
        app_secrets().join("wallets").join(id)
    }
}
pub fn app_config() -> PathBuf {
    home().join("config")
}
pub fn app_logs() -> PathBuf {
    home().join("logs")
}
pub fn app_secrets() -> PathBuf {
    std::env::var("ALM_SECRETS_DIR").map(PathBuf::from).unwrap_or_else(|_| home().join("secrets"))
}
/// The kill switch: one for the app, it pauses every wallet.
pub fn stop_file() -> PathBuf {
    std::env::var("ALM_STOP_FILE").map(PathBuf::from).unwrap_or_else(|_| app_config().join("STOP"))
}

/// One wallet's pause (Settings → Wallets): while it exists that wallet signs nothing, like the kill
/// switch does for all of them; it is still valued and its strategies still run as a dry run.
pub fn wallet_pause_file() -> PathBuf {
    config().join("PAUSED")
}

/// Why signing is not allowed right now (Pause / STOP file, or ALM_DISABLED=1), checked immediately
/// before every broadcast so a pause stops the next transaction, not only the next cycle.
pub fn execution_blocked() -> Option<String> {
    if stop_file().exists() {
        return Some("paused (kill switch is on)".into());
    }
    if wallet_pause_file().exists() {
        return Some("paused (this wallet is paused)".into());
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
        let d = app_secrets();
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
    for d in [config(), logs(), data(), secrets(), app_config(), app_logs()] {
        let _ = std::fs::create_dir_all(d);
    }
    harden_secrets();
    let pools = config().join("pools.json");
    if !pools.exists() {
        let _ = std::fs::write(pools, "[]\n");
    }
}
