//! The local cache in front of every external data source (AGENT_PLAN section 6, "Local cache
//! first"): data is downloaded on first use, kept on disk across restarts, and afterwards only
//! topped up, so a reader the cache can answer makes no request.
//!
//!   data/cache/ext/<source>/<key>.json    a value with its retrieval time (`get`)
//!   data/cache/ext/<source>/<key>.jsonl   a series of closed buckets, oldest first (`series`)
//!   data/cache/ext/<source>/<key>.cov     the contiguous range of buckets the series file covers
//!   data/cache/ext/stats.json             requests, cache hits and failures per source and UTC day
//!
//! - **Values** have an age limit. A fresh one is served from the cache; an older one is fetched
//!   again, and if the fetch fails, the old one is returned marked stale.
//! - **Series** keep closed buckets only (a bucket is closed once its end has passed). A read asks
//!   the source only for buckets the file does not cover: before its first or after its last. The
//!   newest end is topped up at most once per `topup_s`. Closed buckets are never pruned by age
//!   here: whoever owns the data decides (the plan: never while a backtest can use it).
//! - **A damaged file** (unreadable JSON, a row out of order) is dropped as a whole and that one
//!   piece is downloaded again; nothing is read from it.
//! - **Back-off**: after the second failure in a row a source is not asked again for 5 minutes,
//!   then 15, then 60. Cached data is still served meanwhile, marked stale.

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// A value read through the cache.
#[derive(Clone, Debug)]
pub struct Hit {
    pub value: Value,
    /// when the value was fetched from its source (unix seconds)
    pub retrieved_at: u64,
    /// served without asking the source
    pub from_cache: bool,
    /// older than its age limit (the source failed or is backing off)
    pub stale: bool,
    /// why a stale value was served
    pub error: Option<String>,
}

impl Hit {
    /// The cache's bookkeeping, to merge into what is shown or passed on.
    pub fn meta(&self) -> Value {
        json!({"retrieved_at": self.retrieved_at, "from_cache": self.from_cache, "stale": self.stale, "error": self.error})
    }
}

/// Rows of a series read through the cache, oldest first, each with a numeric "ts" (bucket start).
#[derive(Clone, Debug)]
pub struct SeriesHit {
    pub rows: Vec<Value>,
    /// requests made to the source for this read (0: served from disk)
    pub fetched: u32,
    pub stale: bool,
    pub error: Option<String>,
}

#[derive(Default)]
struct Backoff {
    failures: u32,
    until: u64,
}

#[derive(Default, Clone, Copy)]
struct Count {
    requests: u64,
    hits: u64,
    failures: u64,
}

pub struct Cache {
    dir: PathBuf,
    mem: Mutex<HashMap<(String, String), (Value, u64)>>,
    topped: Mutex<HashMap<(String, String), u64>>,
    backoff: Mutex<HashMap<String, Backoff>>,
    stats: Mutex<Option<HashMap<String, HashMap<String, Count>>>>,
}

const BACKOFF_S: [u64; 3] = [300, 900, 3600];
const STATS_DAYS: usize = 30;

/// The app's cache, under data/cache/ext/.
pub fn global() -> &'static Cache {
    static C: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    C.get_or_init(|| Cache::new(crate::paths::data().join("cache").join("ext")))
}

/// A file name for a key: readable characters kept, the rest replaced, and a hash suffix so two
/// keys never share a file.
fn file_key(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let clean: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(80).collect();
    let h = Sha256::digest(key.as_bytes());
    format!("{clean}-{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3])
}

fn day_of(ts: u64) -> String {
    // days since the epoch → yyyy-mm-dd (civil from days, Howard Hinnant's algorithm)
    let z = (ts / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

impl Cache {
    pub fn new(dir: PathBuf) -> Cache {
        Cache { dir, mem: Mutex::new(HashMap::new()), topped: Mutex::new(HashMap::new()), backoff: Mutex::new(HashMap::new()), stats: Mutex::new(None) }
    }

    fn path(&self, source: &str, key: &str, ext: &str) -> PathBuf {
        self.dir.join(file_key(source)).join(format!("{}.{ext}", file_key(key)))
    }

    // -- statistics ---------------------------------------------------------------------------------

    fn count(&self, source: &str, now: u64, f: impl FnOnce(&mut Count), persist: bool) {
        let snapshot = {
            let mut g = self.stats.lock().unwrap();
            let all = g.get_or_insert_with(|| self.load_stats());
            f(all.entry(source.to_string()).or_default().entry(day_of(now)).or_default());
            for days in all.values_mut() {
                if days.len() > STATS_DAYS {
                    let mut keys: Vec<String> = days.keys().cloned().collect();
                    keys.sort();
                    for k in &keys[..keys.len() - STATS_DAYS] {
                        days.remove(k);
                    }
                }
            }
            persist.then(|| Self::stats_json(all))
        };
        if let Some(v) = snapshot {
            let _ = std::fs::create_dir_all(&self.dir);
            let _ = crate::settings::write_atomic(&self.dir.join("stats.json"), &v);
        }
    }

    fn load_stats(&self) -> HashMap<String, HashMap<String, Count>> {
        let v: Value = std::fs::read_to_string(self.dir.join("stats.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
        let mut out = HashMap::new();
        for (src, days) in v.as_object().cloned().unwrap_or_default() {
            let mut m = HashMap::new();
            for (d, c) in days.as_object().cloned().unwrap_or_default() {
                let n = |k: &str| c[k].as_u64().unwrap_or(0);
                m.insert(d, Count { requests: n("requests"), hits: n("hits"), failures: n("failures") });
            }
            out.insert(src, m);
        }
        out
    }

    fn stats_json(all: &HashMap<String, HashMap<String, Count>>) -> Value {
        let mut out = Map::new();
        for (src, days) in all {
            let mut m = Map::new();
            for (d, c) in days {
                m.insert(d.clone(), json!({"requests": c.requests, "hits": c.hits, "failures": c.failures}));
            }
            out.insert(src.clone(), Value::Object(m));
        }
        Value::Object(out)
    }

    /// Per source: today's requests, hits, failures and hit rate, the last 7 days' totals, the
    /// back-off state and the bytes on disk.
    pub fn stats(&self, now: u64) -> Value {
        let all = {
            let mut g = self.stats.lock().unwrap();
            g.get_or_insert_with(|| self.load_stats()).clone()
        };
        let today = day_of(now);
        let week: Vec<String> = (0..7).map(|i| day_of(now.saturating_sub(i * 86_400))).collect();
        let back = self.backoff.lock().unwrap();
        let mut rows = vec![];
        let mut names: Vec<&String> = all.keys().collect();
        names.sort();
        for src in names {
            let days = &all[src];
            let t = days.get(&today).copied().unwrap_or_default();
            let mut w = Count::default();
            for d in &week {
                if let Some(c) = days.get(d) {
                    w.requests += c.requests;
                    w.hits += c.hits;
                    w.failures += c.failures;
                }
            }
            let rate = |c: Count| if c.requests + c.hits == 0 { Value::Null } else { json!(c.hits as f64 / (c.requests + c.hits) as f64) };
            let b = back.get(src);
            rows.push(json!({
                "source": src,
                "today": {"requests": t.requests, "hits": t.hits, "failures": t.failures, "hit_rate": rate(t)},
                "week": {"requests": w.requests, "hits": w.hits, "failures": w.failures, "hit_rate": rate(w)},
                "backing_off_until": b.filter(|b| b.until > now).map(|b| b.until),
                "bytes": dir_bytes(&self.dir.join(file_key(src))),
            }));
        }
        json!(rows)
    }

    // -- back-off -----------------------------------------------------------------------------------

    fn backing_off(&self, source: &str, now: u64) -> Option<u64> {
        self.backoff.lock().unwrap().get(source).filter(|b| b.until > now).map(|b| b.until)
    }

    fn failed(&self, source: &str, now: u64) {
        {
            let mut g = self.backoff.lock().unwrap();
            let b = g.entry(source.to_string()).or_default();
            b.failures += 1;
            if b.failures >= 2 {
                b.until = now + BACKOFF_S[((b.failures - 2) as usize).min(BACKOFF_S.len() - 1)];
            }
        }
        self.count(source, now, |c| c.failures += 1, true);
    }

    fn succeeded(&self, source: &str, now: u64) {
        self.backoff.lock().unwrap().remove(source);
        self.count(source, now, |c| c.requests += 1, true);
    }

    // -- values -------------------------------------------------------------------------------------

    fn stored(&self, source: &str, key: &str) -> Option<(Value, u64)> {
        let k = (source.to_string(), key.to_string());
        if let Some(v) = self.mem.lock().unwrap().get(&k) {
            return Some(v.clone());
        }
        let p = self.path(source, key, "json");
        let s = std::fs::read_to_string(&p).ok()?;
        match serde_json::from_str::<Value>(&s) {
            Ok(v) if v["retrieved_at"].is_u64() => {
                let e = (v["value"].clone(), v["retrieved_at"].as_u64().unwrap());
                self.mem.lock().unwrap().insert(k, e.clone());
                Some(e)
            }
            _ => {
                // damaged: drop it, the caller fetches it again
                let _ = std::fs::remove_file(&p);
                None
            }
        }
    }

    fn store(&self, source: &str, key: &str, value: &Value, at: u64) {
        let p = self.path(source, key, "json");
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = crate::settings::write_atomic(&p, &json!({"retrieved_at": at, "value": value}));
        self.mem.lock().unwrap().insert((source.to_string(), key.to_string()), (value.clone(), at));
    }

    /// `key` of `source`, from the cache when younger than `max_age_s`, else from `fetch`.
    pub fn get(&self, source: &str, key: &str, max_age_s: u64, fetch: impl FnOnce() -> Result<Value, String>) -> Result<Hit, String> {
        self.get_at(source, key, max_age_s, crate::engine::now(), fetch)
    }

    pub fn get_at(&self, source: &str, key: &str, max_age_s: u64, now: u64, fetch: impl FnOnce() -> Result<Value, String>) -> Result<Hit, String> {
        let old = self.stored(source, key);
        if let Some((v, at)) = &old {
            if now.saturating_sub(*at) < max_age_s {
                self.count(source, now, |c| c.hits += 1, false);
                return Ok(Hit { value: v.clone(), retrieved_at: *at, from_cache: true, stale: false, error: None });
            }
        }
        let stale = |e: String| match &old {
            Some((v, at)) => Ok(Hit { value: v.clone(), retrieved_at: *at, from_cache: true, stale: true, error: Some(e) }),
            None => Err(e),
        };
        if let Some(until) = self.backing_off(source, now) {
            return stale(format!("{source} is not asked again until {until} after repeated failures"));
        }
        match fetch() {
            Ok(v) => {
                self.succeeded(source, now);
                self.store(source, key, &v, now);
                Ok(Hit { value: v, retrieved_at: now, from_cache: false, stale: false, error: None })
            }
            Err(e) => {
                self.failed(source, now);
                stale(e)
            }
        }
    }

    // -- series -------------------------------------------------------------------------------------

    /// The stored rows and the bucket range they cover, or None (nothing stored, or damaged and
    /// dropped).
    fn load_series(&self, source: &str, key: &str) -> Option<(Vec<Value>, i64, i64)> {
        let (pj, pc) = (self.path(source, key, "jsonl"), self.path(source, key, "cov"));
        let cov: Value = serde_json::from_str(&std::fs::read_to_string(&pc).ok()?).ok()?;
        let (Some(a), Some(b)) = (cov["from"].as_i64(), cov["to"].as_i64()) else {
            self.drop_series(source, key);
            return None;
        };
        let text = std::fs::read_to_string(&pj).unwrap_or_default();
        let mut rows = vec![];
        let mut last = i64::MIN;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                self.drop_series(source, key);
                return None;
            };
            let Some(ts) = v["ts"].as_i64().filter(|t| *t > last && *t >= a && *t < b) else {
                self.drop_series(source, key);
                return None;
            };
            last = ts;
            rows.push(v);
        }
        Some((rows, a, b))
    }

    fn drop_series(&self, source: &str, key: &str) {
        crate::warn!("cache", "{source} {key}: damaged cache file dropped; it is downloaded again");
        let _ = std::fs::remove_file(self.path(source, key, "jsonl"));
        let _ = std::fs::remove_file(self.path(source, key, "cov"));
    }

    fn save_series(&self, source: &str, key: &str, rows: &[Value], from: i64, to: i64) {
        let (pj, pc) = (self.path(source, key, "jsonl"), self.path(source, key, "cov"));
        if let Some(d) = pj.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = pj.with_extension("jsonl.tmp");
        let ok = (|| -> std::io::Result<()> {
            let mut f = std::fs::File::create(&tmp)?;
            for r in rows {
                writeln!(f, "{r}")?;
            }
            f.sync_all()?;
            std::fs::rename(&tmp, &pj)
        })();
        if ok.is_ok() {
            let _ = crate::settings::write_atomic(&pc, &json!({"from": from, "to": to}));
        }
    }

    /// Buckets of `bucket_s` seconds between `from` and `to` (bucket starts, `to` exclusive).
    /// Closed buckets come from the cache when it covers them; the rest from `fetch(a, b)`, which
    /// returns the rows with "ts" in [a, b). The open bucket (not yet ended) is never stored.
    pub fn series(&self, source: &str, key: &str, bucket_s: i64, from: i64, to: i64, topup_s: u64,
                  fetch: impl FnMut(i64, i64) -> Result<Vec<Value>, String>) -> Result<SeriesHit, String> {
        self.series_at(source, key, bucket_s, from, to, topup_s, crate::engine::now(), fetch)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn series_at(&self, source: &str, key: &str, bucket_s: i64, from: i64, to: i64, topup_s: u64, now: u64,
                     mut fetch: impl FnMut(i64, i64) -> Result<Vec<Value>, String>) -> Result<SeriesHit, String> {
        let align = |t: i64| t.div_euclid(bucket_s) * bucket_s;
        let (from, to) = (align(from), align(to.min(now as i64)) + bucket_s);
        // closed buckets end at or before now
        let closed_to = align(now as i64 - bucket_s) + bucket_s;
        let (mut rows, mut cf, mut ct) = self.load_series(source, key).unwrap_or((vec![], 0, 0));
        let empty = ct <= cf;
        let mut wanted: Vec<(i64, i64)> = vec![];
        if empty {
            wanted.push((from, to));
        } else {
            if from < cf {
                wanted.push((from, cf));
            }
            let tk = (source.to_string(), key.to_string());
            let recently = self.topped.lock().unwrap().get(&tk).map(|t| now.saturating_sub(*t) < topup_s).unwrap_or(false);
            if to > ct && !recently {
                wanted.push((ct, to));
            }
        }
        let mut fetched = 0;
        let mut open: Vec<Value> = vec![];
        let mut error = None;
        let backing = self.backing_off(source, now);
        for (a, b) in wanted {
            if let Some(until) = backing {
                error = Some(format!("{source} is not asked again until {until} after repeated failures"));
                break;
            }
            match fetch(a, b) {
                Ok(new) => {
                    fetched += 1;
                    self.succeeded(source, now);
                    if b > ct {
                        self.topped.lock().unwrap().insert((source.to_string(), key.to_string()), now);
                    }
                    let mut keep: Vec<Value> = vec![];
                    for r in new {
                        match r["ts"].as_i64() {
                            Some(t) if t >= a && t < b && t < closed_to => keep.push(r),
                            Some(t) if t >= a && t < b => open.push(r),
                            _ => {}
                        }
                    }
                    let (na, nb) = (a, b.min(closed_to));
                    if nb > na {
                        // merge: the new range touches or overlaps the covered one, or replaces it
                        if empty_or_disjoint(cf, ct, na, nb) {
                            rows = keep;
                            cf = na;
                            ct = nb;
                        } else {
                            rows.retain(|r| r["ts"].as_i64().map(|t| t < na || t >= nb).unwrap_or(false));
                            rows.extend(keep);
                            rows.sort_by_key(|r| r["ts"].as_i64().unwrap_or(0));
                            rows.dedup_by_key(|r| r["ts"].as_i64().unwrap_or(0));
                            cf = cf.min(na);
                            ct = ct.max(nb);
                        }
                        self.save_series(source, key, &rows, cf, ct);
                    }
                }
                Err(e) => {
                    self.failed(source, now);
                    error = Some(e);
                    break;
                }
            }
        }
        if fetched == 0 && error.is_none() {
            self.count(source, now, |c| c.hits += 1, false);
        }
        if rows.is_empty() && open.is_empty() {
            if let Some(e) = &error {
                return Err(e.clone());
            }
        }
        let mut out: Vec<Value> = rows.into_iter().filter(|r| r["ts"].as_i64().map(|t| t >= from && t < to).unwrap_or(false)).collect();
        out.extend(open);
        Ok(SeriesHit { rows: out, fetched, stale: error.is_some(), error })
    }
}

/// True when [na, nb) cannot be joined to the covered [cf, ct) without a gap (or nothing is covered).
fn empty_or_disjoint(cf: i64, ct: i64, na: i64, nb: i64) -> bool {
    ct <= cf || nb < cf || na > ct
}

fn dir_bytes(p: &std::path::Path) -> u64 {
    std::fs::read_dir(p).map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn tmp(name: &str) -> Cache {
        let d = std::env::temp_dir().join(format!("alm-cache-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        Cache::new(d)
    }

    #[test]
    fn value_is_fetched_once_then_served_until_it_ages() {
        let c = tmp("value");
        let n = Cell::new(0);
        let f = || { n.set(n.get() + 1); Ok(json!(n.get())) };
        assert!(!c.get_at("s", "k", 60, 1000, f).unwrap().from_cache);
        let h = c.get_at("s", "k", 60, 1030, || panic!("should be cached")).unwrap();
        assert!(h.from_cache && !h.stale && h.value == json!(1));
        let h = c.get_at("s", "k", 60, 1061, || Ok(json!(2))).unwrap();
        assert!(!h.from_cache && h.value == json!(2));
    }

    #[test]
    fn a_restart_reads_the_disk() {
        let c = tmp("restart");
        c.get_at("s", "k", 600, 1000, || Ok(json!({"a": 1}))).unwrap();
        let c2 = Cache::new(c.dir.clone());
        let h = c2.get_at("s", "k", 600, 1100, || panic!("should come from disk")).unwrap();
        assert_eq!(h.value, json!({"a": 1}));
    }

    #[test]
    fn a_failure_serves_the_old_value_stale_and_backs_off() {
        let c = tmp("fail");
        c.get_at("s", "k", 10, 1000, || Ok(json!(1))).unwrap();
        let h = c.get_at("s", "k", 10, 1100, || Err("down".into())).unwrap();
        assert!(h.stale && h.value == json!(1));
        let h = c.get_at("s", "k", 10, 1101, || Err("down".into())).unwrap();
        assert!(h.stale);
        // two failures in a row: no request for 5 minutes
        let h = c.get_at("s", "k", 10, 1102, || panic!("backing off")).unwrap();
        assert!(h.stale && h.error.unwrap().contains("not asked again"));
        assert!(c.get_at("s", "other", 10, 1103, || panic!("backing off")).is_err());
        let h = c.get_at("s", "k", 10, 1101 + 301, || Ok(json!(3))).unwrap();
        assert!(!h.stale && h.value == json!(3));
    }

    #[test]
    fn a_damaged_value_file_is_fetched_again() {
        let c = tmp("damaged");
        c.get_at("s", "k", 600, 1000, || Ok(json!(1))).unwrap();
        std::fs::write(c.path("s", "k", "json"), "{not json").unwrap();
        let c2 = Cache::new(c.dir.clone());
        let h = c2.get_at("s", "k", 600, 1001, || Ok(json!(2))).unwrap();
        assert!(!h.from_cache && h.value == json!(2));
    }

    fn rows(a: i64, b: i64) -> Vec<Value> {
        (a..b).step_by(3600).map(|t| json!({"ts": t, "v": t / 3600})).collect()
    }

    #[test]
    fn series_downloads_once_then_only_tops_up() {
        let c = tmp("series");
        let h = 3600;
        let calls = std::cell::RefCell::new(vec![]);
        let f = |a: i64, b: i64| { calls.borrow_mut().push((a, b)); Ok(rows(a, b)) };
        // now = 100.5 h: buckets 0..100 closed, 100 open
        let now = 100 * h as u64 + 1800;
        let r = c.series_at("s", "k", h, 0, now as i64, 600, now, f).unwrap();
        assert_eq!(r.rows.len(), 101); // 100 closed + the open one
        assert_eq!(calls.borrow().len(), 1);
        // the same read again within topup_s: nothing asked
        let r = c.series_at("s", "k", h, 0, now as i64, 600, now + 60, |_, _| panic!("cached")).unwrap();
        assert_eq!(r.rows.len(), 100);
        assert_eq!(r.fetched, 0);
        // later: only the new buckets are asked for
        let now2 = now + 3 * h as u64;
        let mut asked = vec![];
        let r = c.series_at("s", "k", h, 0, now2 as i64, 600, now2, |a, b| { asked.push((a, b)); Ok(rows(a, b)) }).unwrap();
        assert_eq!(asked, vec![(100 * h, 104 * h)]);
        assert_eq!(r.rows.len(), 104);
        // an earlier start: only the part before the covered range
        let mut asked = vec![];
        c.series_at("s", "k", h, -5 * h, now2 as i64, 600, now2 + 1, |a, b| { asked.push((a, b)); Ok(rows(a, b)) }).unwrap();
        assert_eq!(asked, vec![(-5 * h, 0)]);
        // a restart keeps all of it
        let c2 = Cache::new(c.dir.clone());
        let r = c2.series_at("s", "k", h, -5 * h, 103 * h, 600, now2 + 2, |a, b| { assert!(a >= 103 * h, "{a}"); Ok(rows(a, b)) }).unwrap();
        assert_eq!(r.rows.first().unwrap()["ts"], json!(-5 * h));
    }

    #[test]
    fn a_damaged_series_is_dropped_and_downloaded_again() {
        let c = tmp("series-damaged");
        let h = 3600;
        let now = 10 * h as u64;
        c.series_at("s", "k", h, 0, now as i64, 0, now, |a, b| Ok(rows(a, b))).unwrap();
        let p = c.path("s", "k", "jsonl");
        let text = std::fs::read_to_string(&p).unwrap().replacen("\"v\"", "\"v", 1);
        std::fs::write(&p, text).unwrap();
        let mut asked = vec![];
        let r = c.series_at("s", "k", h, 0, now as i64, 0, now + 1, |a, b| { asked.push((a, b)); Ok(rows(a, b)) }).unwrap();
        assert_eq!(asked, vec![(0, 11 * h)]);
        assert_eq!(r.rows.len(), 11);
    }

    #[test]
    fn stats_count_requests_and_hits() {
        let c = tmp("stats");
        c.get_at("s", "k", 60, 1000, || Ok(json!(1))).unwrap();
        c.get_at("s", "k", 60, 1001, || Ok(json!(1))).unwrap();
        c.get_at("s", "k", 60, 1002, || Ok(json!(1))).unwrap();
        let st = c.stats(1003);
        assert_eq!(st[0]["today"]["requests"], json!(1));
        assert_eq!(st[0]["today"]["hits"], json!(2));
        assert_eq!(day_of(0), "1970-01-01");
        assert_eq!(day_of(1_760_832_000), "2025-10-19");
    }
}
