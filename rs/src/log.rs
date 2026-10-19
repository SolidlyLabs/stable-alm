//! Logging as "<time> <LEVEL> <name>: <message>" (the original Python app's format) to the
//! console, logs/alm.log and (warnings and up) logs/errors.log, each rotated at 5 MB, 5 backups.
//! Hooks (the tray's notifications) see every warning and error.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Mutex;

#[derive(Clone, Copy, PartialEq, PartialOrd)]
pub enum Level {
    Debug,
    Info,
    Warning,
    Error,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
        }
    }
}

type Hook = Box<dyn Fn(Level, &str, &str) + Send + Sync>;
static LOCK: Mutex<()> = Mutex::new(());
static HOOKS: Mutex<Vec<Hook>> = Mutex::new(Vec::new());

pub fn add_hook(h: Hook) {
    HOOKS.lock().unwrap().push(h);
}

const MAX: u64 = 5 << 20;

fn rotate(path: &std::path::Path) {
    if std::fs::metadata(path).map(|m| m.len() < MAX).unwrap_or(true) {
        return;
    }
    for i in (1..5).rev() {
        let a = path.with_extension(format!("log.{i}"));
        let b = path.with_extension(format!("log.{}", i + 1));
        let _ = std::fs::rename(&a, &b);
    }
    let _ = std::fs::rename(path, path.with_extension("log.1"));
}

pub fn timestamp() -> String {
    // Local time is not in std; UTC with the same layout as Python's asctime (the logs have always been UTC).
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let (y, mo, d, h, mi, s) = civil(now.as_secs() as i64);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02},{:03}", now.subsec_millis())
}

/// UTC calendar fields for a unix time (Howard Hinnant's days-from-civil inverse).
pub fn civil(t: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = t.div_euclid(86400);
    let rem = t.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d, (rem / 3600) as u32, (rem % 3600 / 60) as u32, (rem % 60) as u32)
}

pub fn log(level: Level, name: &str, msg: &str) {
    let min = match std::env::var("ALM_LOG_LEVEL").unwrap_or_default().to_uppercase().as_str() {
        "DEBUG" => Level::Debug,
        "WARNING" => Level::Warning,
        "ERROR" => Level::Error,
        _ => Level::Info,
    };
    if level < min {
        return;
    }
    let line = format!("{} {:<7} alm.{}: {}\n", timestamp(), level.name(), name, msg);
    {
        let _g = LOCK.lock().unwrap();
        let _ = std::io::stdout().write_all(line.as_bytes());
        // tests never write into the real logs (stdout only)
        #[cfg(test)]
        return;
        #[allow(unreachable_code)]
        let dir = crate::paths::logs();
        let _ = std::fs::create_dir_all(&dir);
        let mut files = vec![dir.join("alm.log")];
        if level >= Level::Warning {
            files.push(dir.join("errors.log"));
        }
        for f in files {
            rotate(&f);
            if let Ok(mut h) = OpenOptions::new().create(true).append(true).open(&f) {
                let _ = h.write_all(line.as_bytes());
            }
        }
    }
    if level >= Level::Warning {
        for h in HOOKS.lock().unwrap().iter() {
            h(level, name, msg);
        }
    }
}

#[macro_export]
macro_rules! info { ($name:expr, $($a:tt)*) => { $crate::log::log($crate::log::Level::Info, $name, &format!($($a)*)) } }
#[macro_export]
macro_rules! warn { ($name:expr, $($a:tt)*) => { $crate::log::log($crate::log::Level::Warning, $name, &format!($($a)*)) } }
#[macro_export]
macro_rules! error { ($name:expr, $($a:tt)*) => { $crate::log::log($crate::log::Level::Error, $name, &format!($($a)*)) } }
#[macro_export]
macro_rules! debug { ($name:expr, $($a:tt)*) => { $crate::log::log($crate::log::Level::Debug, $name, &format!($($a)*)) } }
