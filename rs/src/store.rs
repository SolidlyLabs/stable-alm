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

// -- slow values, refreshed in the background ---------------------------------------------------------

static BG: Mutex<Option<HashMap<String, (Instant, Value)>>> = Mutex::new(None);
static BG_BUSY: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

/// A value that is slow to get (it comes from the network), for a page that must answer at once.
/// Served from memory, or the first time from `seed` (a copy on disk, with the unix time it was
/// saved). When it is `ttl_s` or more old, `fetch` runs in a background thread, one at a time per
/// key, and a failed fetch keeps the old value. With nothing at all the caller waits up to `wait`
/// for the fetch and gets None if it has not finished; it keeps running and fills the cache.
pub fn background(key: &str, ttl_s: u64, wait: Duration, seed: impl FnOnce() -> Option<(Value, u64)>,
                  fetch: impl FnOnce() -> Result<Value, String> + Send + 'static) -> Option<Value> {
    run_background(key, ttl_s, wait, false, seed, fetch)
}

/// `background` for a value that should be current when looked at: an old one is served only when
/// the refresh takes longer than `wait`.
pub fn background_fresh(key: &str, ttl_s: u64, wait: Duration, fetch: impl FnOnce() -> Result<Value, String> + Send + 'static) -> Option<Value> {
    run_background(key, ttl_s, wait, true, || None, fetch)
}

fn run_background(key: &str, ttl_s: u64, wait: Duration, wait_when_old: bool, seed: impl FnOnce() -> Option<(Value, u64)>,
                  fetch: impl FnOnce() -> Result<Value, String> + Send + 'static) -> Option<Value> {
    let mut cur = BG.lock().unwrap().get_or_insert_with(HashMap::new).get(key).cloned();
    if cur.is_none() {
        if let Some((v, saved)) = seed() {
            let age = crate::engine::now().saturating_sub(saved);
            let at = Instant::now().checked_sub(Duration::from_secs(age)).unwrap_or_else(Instant::now);
            BG.lock().unwrap().as_mut().unwrap().insert(key.to_string(), (at, v.clone()));
            cur = Some((at, v));
        }
    }
    let stale = cur.as_ref().map(|(t, _)| t.elapsed() >= Duration::from_secs(ttl_s)).unwrap_or(true);
    if stale && BG_BUSY.lock().unwrap().get_or_insert_with(Default::default).insert(key.to_string()) {
        let k = key.to_string();
        std::thread::spawn(move || {
            match fetch() {
                Ok(v) => background_put(&k, v),
                Err(e) => crate::warn!("cache", "{k}: refresh failed, keeping the old copy ({e})"),
            }
            BG_BUSY.lock().unwrap().as_mut().unwrap().remove(&k);
        });
    }
    if !(stale && wait_when_old) {
        if let Some((_, v)) = cur.clone() {
            return Some(v);
        }
    }
    let had = cur.as_ref().map(|c| c.0);
    let until = Instant::now() + wait;
    loop {
        // busy first: a fetch that finishes in between has already stored its value
        let busy = refreshing(key);
        if let Some((t, v)) = BG.lock().unwrap().as_ref().and_then(|m| m.get(key).cloned()) {
            if Some(t) != had {
                return Some(v);
            }
        }
        if Instant::now() >= until || !busy {
            return cur.map(|c| c.1);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What `background` holds for a key (however old), without fetching.
pub fn background_peek(key: &str) -> Option<Value> {
    BG.lock().unwrap().as_ref().and_then(|m| m.get(key).map(|x| x.1.clone()))
}

/// Put a value fetched elsewhere (e.g. by a background sampler) where `background` serves it.
pub fn background_put(key: &str, v: Value) {
    BG.lock().unwrap().get_or_insert_with(HashMap::new).insert(key.to_string(), (Instant::now(), v));
}

/// True while a background fetch for the key runs.
pub fn refreshing(key: &str) -> bool {
    BG_BUSY.lock().unwrap().as_ref().map(|s| s.contains(key)).unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn wait_idle(key: &str) {
        let until = Instant::now() + Duration::from_secs(5);
        while refreshing(key) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn background_serves_the_old_copy_at_once_and_refreshes_it_once() {
        let key = "test_bg_stale";
        let calls = Arc::new(AtomicUsize::new(0));
        let old = || Some((json!("old"), crate::engine::now() - 3600));
        let fetch = |c: Arc<AtomicUsize>| move || {
            c.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
            Ok(json!("new"))
        };
        let t = Instant::now();
        assert_eq!(background(key, 60, Duration::from_secs(5), old, fetch(calls.clone())), Some(json!("old")));
        // a second look while the refresh runs: the old copy again, and no second fetch
        assert_eq!(background(key, 60, Duration::from_secs(5), old, fetch(calls.clone())), Some(json!("old")));
        assert!(t.elapsed() < Duration::from_millis(150), "did not wait for the network");
        wait_idle(key);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // fresh now: served without a fetch
        assert_eq!(background(key, 60, Duration::from_secs(5), old, fetch(calls.clone())), Some(json!("new")));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // a max age of 0 forces a refresh (a token the list does not have yet)
        background(key, 0, Duration::ZERO, old, fetch(calls.clone()));
        wait_idle(key);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn background_with_nothing_saved_waits_a_little_then_gives_up() {
        let quick = background("test_bg_cold_quick", 60, Duration::from_secs(5), || None, || {
            std::thread::sleep(Duration::from_millis(30));
            Ok(json!(1))
        });
        assert_eq!(quick, Some(json!(1)));
        let t = Instant::now();
        let slow = background("test_bg_cold_slow", 60, Duration::from_millis(50), || None, || {
            std::thread::sleep(Duration::from_millis(300));
            Ok(json!(2))
        });
        assert_eq!(slow, None);
        assert!(t.elapsed() < Duration::from_millis(250));
        // the fetch carried on and fills the cache for the next look
        wait_idle("test_bg_cold_slow");
        assert_eq!(background_peek("test_bg_cold_slow"), Some(json!(2)));
    }

    #[test]
    fn background_fresh_waits_for_the_refresh_unless_it_is_slow() {
        let n = Arc::new(AtomicUsize::new(0));
        let fetch = |n: Arc<AtomicUsize>, ms: u64| move || {
            std::thread::sleep(Duration::from_millis(ms));
            Ok(json!(n.fetch_add(1, Ordering::SeqCst)))
        };
        let key = "test_bg_fresh";
        assert_eq!(background_fresh(key, 0, Duration::from_secs(5), fetch(n.clone(), 10)), Some(json!(0)));
        // old (max age 0): the caller gets the new value, not the old one
        assert_eq!(background_fresh(key, 0, Duration::from_secs(5), fetch(n.clone(), 10)), Some(json!(1)));
        // a slow refresh: the old value after the wait
        let t = Instant::now();
        assert_eq!(background_fresh(key, 0, Duration::from_millis(50), fetch(n.clone(), 300)), Some(json!(1)));
        assert!(t.elapsed() < Duration::from_millis(250));
        wait_idle(key);
        // young enough: no refresh, no wait
        assert_eq!(background_fresh(key, 60, Duration::from_secs(5), fetch(n.clone(), 10)), Some(json!(2)));
        assert_eq!(n.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn background_keeps_the_old_copy_when_the_refresh_fails() {
        let key = "test_bg_fail";
        let old = || Some((json!("old"), 0));
        assert_eq!(background(key, 60, Duration::ZERO, old, || Err("down".into())), Some(json!("old")));
        wait_idle(key);
        assert_eq!(background_peek(key), Some(json!("old")));
        // nothing saved and the fetch fails: no waiting out the whole timeout
        let t = Instant::now();
        assert_eq!(background("test_bg_fail_cold", 60, Duration::from_secs(5), || None, || Err("down".into())), None);
        assert!(t.elapsed() < Duration::from_secs(1));
    }
}
