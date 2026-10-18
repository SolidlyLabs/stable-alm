//! Things that do not change once known, kept on disk across restarts (`data/cache/`):
//! - `static.json`: token symbol, name, decimals and logo URL; a pool's tokens and tick spacing;
//! - `logos/`: token logo images, fetched once and then served by the app itself;
//! - `pools_<chain>_<wallet>.json`: the last pool list per chain, shown at once after a restart while
//!   a fresh copy loads (and kept on screen when the source is down).

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

static KV: Mutex<Option<Map<String, Value>>> = Mutex::new(None);

fn dir() -> PathBuf {
    crate::paths::data().join("cache")
}

fn kv_path() -> PathBuf {
    dir().join("static.json")
}

fn with_kv<T>(f: impl FnOnce(&mut Map<String, Value>) -> T) -> T {
    let mut g = KV.lock().unwrap();
    let m = g.get_or_insert_with(|| {
        std::fs::read_to_string(kv_path()).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| v.as_object().cloned()).unwrap_or_default()
    });
    f(m)
}

/// A stored fact, e.g. `meta:8453:0x…`.
pub fn get(key: &str) -> Option<Value> {
    with_kv(|m| m.get(key).cloned())
}

/// Store a fact; the file is rewritten only when something changed.
pub fn put(key: &str, v: Value) {
    let snapshot = with_kv(|m| {
        if m.get(key) == Some(&v) {
            return None;
        }
        m.insert(key.to_string(), v);
        Some(Value::Object(m.clone()))
    });
    if let Some(all) = snapshot {
        let _ = std::fs::create_dir_all(dir());
        let _ = crate::settings::write_atomic(&kv_path(), &all);
    }
}

/// Merge fields into a stored object (keeps fields already known).
pub fn merge(key: &str, fields: Value) {
    let mut cur = get(key).filter(|v| v.is_object()).unwrap_or_else(|| Value::Object(Map::new()));
    for (k, v) in fields.as_object().cloned().unwrap_or_default() {
        if !v.is_null() && !(v.is_string() && v.as_str() == Some("")) {
            cur[k] = v;
        }
    }
    put(key, cur);
}

pub fn token_key(cid: u64, addr: &str) -> String {
    format!("token:{cid}:{}", addr.to_lowercase())
}

// -- the last pool list per chain ---------------------------------------------------------------------

fn snapshot_path(name: &str) -> PathBuf {
    dir().join(format!("{name}.json"))
}

pub fn save_snapshot(name: &str, v: &Value) {
    let _ = std::fs::create_dir_all(dir());
    let _ = crate::settings::write_atomic(&snapshot_path(name), &serde_json::json!({"saved": crate::engine::now(), "data": v}));
}

/// (value, saved at) of a snapshot written by `save_snapshot`.
pub fn load_snapshot(name: &str) -> Option<(Value, u64)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(snapshot_path(name)).ok()?).ok()?;
    Some((v["data"].clone(), v["saved"].as_u64().unwrap_or(0)))
}

// -- logos --------------------------------------------------------------------------------------------

const MAX_LOGO_BYTES: usize = 512 * 1024;
static LOGO_FAIL: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

fn ext_for(ctype: &str) -> Option<&'static str> {
    match ctype.split(';').next().unwrap_or("").trim() {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "image/svg+xml" => Some("svg"),
        _ => None,
    }
}

fn ctype_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/svg+xml",
    }
}

/// The logo of a token as (bytes, content type): from disk, or fetched once from the logo URL the
/// pool list gave for it (only that URL: the app never fetches an address a page asks for).
pub fn logo(cid: u64, addr: &str) -> Result<(Vec<u8>, &'static str), String> {
    let addr = addr.to_lowercase();
    if !(addr.len() == 42 && addr.starts_with("0x") && addr[2..].chars().all(|c| c.is_ascii_hexdigit())) {
        return Err("bad address".into());
    }
    let base = dir().join("logos").join(format!("{cid}_{addr}"));
    for ext in ["png", "jpg", "webp", "gif", "svg"] {
        if let Ok(b) = std::fs::read(base.with_extension(ext)) {
            return Ok((b, ctype_for(ext)));
        }
    }
    let key = format!("{cid}:{addr}");
    if let Some(t) = LOGO_FAIL.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        if t.elapsed() < Duration::from_secs(3600) {
            return Err("logo unavailable".into());
        }
    }
    let res = (|| -> Result<(Vec<u8>, &'static str), String> {
        let url = get(&token_key(cid, &addr)).and_then(|v| v["logo_src"].as_str().map(String::from)).ok_or("no logo known")?;
        let url = if url.starts_with('/') { format!("https://solidly.com{url}") } else { url };
        if !url.starts_with("https://") {
            return Err("logo URL is not https".into());
        }
        let resp = crate::rpc::agent().get(&url).timeout(Duration::from_secs(15)).call().map_err(|e| e.to_string())?;
        let ext = ext_for(resp.content_type()).ok_or("not an image")?;
        let mut buf = vec![];
        use std::io::Read;
        resp.into_reader().take(MAX_LOGO_BYTES as u64 + 1).read_to_end(&mut buf).map_err(|e| e.to_string())?;
        if buf.len() > MAX_LOGO_BYTES || buf.is_empty() {
            return Err("logo too large or empty".into());
        }
        let _ = std::fs::create_dir_all(base.parent().unwrap());
        let tmp = base.with_extension("tmp");
        if std::fs::write(&tmp, &buf).is_ok() {
            let _ = std::fs::rename(&tmp, base.with_extension(ext));
        }
        Ok((buf, ctype_for(ext)))
    })();
    if res.is_err() {
        LOGO_FAIL.lock().unwrap().as_mut().unwrap().insert(key, Instant::now());
    }
    res
}
