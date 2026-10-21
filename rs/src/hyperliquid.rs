//! Hyperliquid perps, read side: the coin that hedges a token, its limits, and its hourly funding.
//!
//! Funding history (`fundingHistory`, hourly, from the coin's listing) is kept in
//! `data/hyperliquid/<COIN>_funding.jsonl` and in memory: only missing hours are downloaded, in
//! parallel windows (the hours before the first stored row when a backtest reaches further back, and
//! those after the last). The list of perps is kept in `meta.json`. A rate is the fraction of the position's notional paid each hour; positive means
//! longs pay shorts.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const INFO: &str = "https://api.hyperliquid.xyz/info";
/// fundingHistory returns at most this many rows a call
const PAGE: usize = 500;

/// The perp that hedges a token: wrapped and staked forms map to their base coin; a dollar stable
/// needs none (None). Other tokens hedge with a perp of their own symbol, when Hyperliquid lists it.
pub fn coin_for(symbol: &str) -> Option<String> {
    let s = symbol.to_uppercase();
    if ["USDC", "USDT", "USDC.E", "USDBC", "DAI", "USDS", "USDE", "FRAX", "LUSD", "GHO", "EURC"].contains(&s.as_str()) {
        return None;
    }
    Some(
        match s.as_str() {
            "WETH" | "ETH" | "CBETH" | "WSTETH" | "WEETH" | "RETH" | "EZETH" | "SUPEROETHB" => "ETH",
            "CBBTC" | "WBTC" | "TBTC" | "BTC" | "LBTC" => "BTC",
            // Sonic's wrapped and staked S
            "WS" | "STS" | "OS" => "S",
            other => other,
        }
        .to_string(),
    )
}

static META: Mutex<Option<(Instant, HashMap<String, f64>)>> = Mutex::new(None);

fn dir() -> std::path::PathBuf {
    crate::paths::data().join("hyperliquid")
}

/// Listed perps and their most leverage. Kept a day in memory and on disk (`meta.json`), so a
/// fresh start needs no request; when Hyperliquid cannot be reached a stored list of any age serves.
pub fn listed() -> Result<HashMap<String, f64>, String> {
    if let Some((t, m)) = META.lock().unwrap().as_ref() {
        if t.elapsed() < Duration::from_secs(86400) {
            return Ok(m.clone());
        }
    }
    let path = dir().join("meta.json");
    let disk: Option<(i64, HashMap<String, f64>)> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| Some((v["t"].as_i64()?, serde_json::from_value(v["lev"].clone()).ok()?)));
    let now = crate::engine::now() as i64;
    if let Some((t, m)) = disk.as_ref().filter(|(t, _)| now - t < 86400) {
        let age = Duration::from_secs((now - t).max(0) as u64);
        *META.lock().unwrap() = Some((Instant::now().checked_sub(age).unwrap_or_else(Instant::now), m.clone()));
        return Ok(m.clone());
    }
    let fetched = crate::rpc::post_json(INFO, &json!({"type": "meta"}), &[]).and_then(|v| {
        v["universe"]
            .as_array()
            .map(|u| {
                u.iter()
                    .filter(|a| a["isDelisted"].as_bool() != Some(true))
                    .filter_map(|a| Some((a["name"].as_str()?.to_string(), a["maxLeverage"].as_f64().unwrap_or(1.0))))
                    .collect::<HashMap<String, f64>>()
            })
            .ok_or_else(|| "Hyperliquid meta: no universe".to_string())
    });
    match (fetched, disk) {
        (Ok(m), _) => {
            let _ = std::fs::create_dir_all(dir());
            let _ = std::fs::write(&path, json!({"t": now, "lev": m}).to_string());
            *META.lock().unwrap() = Some((Instant::now(), m.clone()));
            Ok(m)
        }
        (Err(e), Some((_, m))) => {
            crate::warn!("hyperliquid", "meta unavailable ({e}); using the stored list");
            Ok(m)
        }
        (Err(e), None) => Err(e),
    }
}

fn store(coin: &str) -> std::path::PathBuf {
    dir().join(format!("{coin}_funding.jsonl"))
}

/// The earliest time (ms) the store is known to cover: rows from then on, none missing before its
/// first row (a coin listed later has nothing earlier). `<COIN>_funding.from`.
fn covered_path(coin: &str) -> std::path::PathBuf {
    dir().join(format!("{coin}_funding.from"))
}

fn read_store(coin: &str) -> Vec<(i64, f64)> {
    let s = std::fs::read_to_string(store(coin)).unwrap_or_default();
    s.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|r| Some((r["t"].as_i64()?, r["r"].as_f64()?)))
        .collect()
}

/// Rows a window of this many hours stays under (a page is PAGE rows)
const WINDOW_H: i64 = 480;
/// Windows downloaded at once
const PARALLEL: usize = 4;

/// Every row of [a, b] (ms), in windows of WINDOW_H hours, PARALLEL at a time; a full page continues
/// within its window. The rows found, and the first error.
fn download(coin: &str, a: i64, b: i64) -> (Vec<(i64, f64)>, Option<String>) {
    let step = WINDOW_H * 3_600_000;
    let windows: Vec<(i64, i64)> = (0..).map(|k| a + k * step).take_while(|s| *s <= b).map(|s| (s, (s + step - 1).min(b))).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: Mutex<(Vec<(i64, f64)>, Option<String>)> = Mutex::new((vec![], None));
    std::thread::scope(|sc| {
        for _ in 0..PARALLEL.min(windows.len()) {
            sc.spawn(|| loop {
                let k = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(&(mut s, e)) = windows.get(k) else { break };
                loop {
                    match crate::rpc::post_json(INFO, &json!({"type": "fundingHistory", "coin": coin, "startTime": s, "endTime": e}), &[]) {
                        Ok(v) => {
                            let page: Vec<(i64, f64)> = v
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|r| Some((r["time"].as_i64()?, r["fundingRate"].as_str()?.parse::<f64>().ok()?)))
                                .collect();
                            let (n, last) = (page.len(), page.last().map(|r| r.0));
                            out.lock().unwrap().0.extend(page);
                            match last {
                                Some(t) if n >= PAGE && t < e => s = t + 1,
                                _ => break,
                            }
                        }
                        Err(err) => {
                            out.lock().unwrap().1.get_or_insert(err);
                            break;
                        }
                    }
                }
            });
        }
    });
    out.into_inner().unwrap()
}

/// Rows in memory per coin, as the store holds them, and when the store was last topped up
static ROWS: Mutex<Option<HashMap<String, (Vec<(i64, f64)>, Instant)>>> = Mutex::new(None);
/// One download at a time: two backtests asking for a coin together fetch it once
static DOWNLOAD: Mutex<()> = Mutex::new(());

/// The coin's hourly funding from `from` (unix seconds): {hour start: rate}. Served from memory;
/// the store is topped up first (at most every 10 minutes) with only the hours it lacks: those after
/// its last row, and those before its first when `from` is earlier than it covers. A failed download
/// keeps what is stored.
pub fn funding(coin: &str, from: i64) -> Result<HashMap<i64, f64>, String> {
    let _one = DOWNLOAD.lock().unwrap_or_else(|e| e.into_inner());
    let from_ms = from * 1000;
    let covered: i64 = std::fs::read_to_string(covered_path(coin)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(i64::MAX);
    let cached = ROWS.lock().unwrap().get_or_insert_with(HashMap::new).get(coin).cloned();
    let fresh = cached.as_ref().map(|(_, t)| t.elapsed() < Duration::from_secs(600)).unwrap_or(false);
    let mut rows = cached.map(|(r, _)| r).unwrap_or_else(|| read_store(coin));
    let mut err = None;
    if !fresh || from_ms < covered {
        let now_ms = crate::engine::now() as i64 * 1000;
        let first = rows.first().map(|r| r.0);
        let mut got = vec![];
        let mut prepended = false;
        // the hours before what is stored (or all of them, from an empty store)
        if from_ms < covered && first.map(|f| f > from_ms).unwrap_or(true) {
            let (r, e) = download(coin, from_ms, first.map(|f| f - 1).unwrap_or(now_ms));
            prepended = first.is_some() && !r.is_empty();
            got.extend(r);
            match e {
                Some(e) => err = Some(e),
                None => {
                    let _ = std::fs::create_dir_all(dir());
                    let _ = std::fs::write(covered_path(coin), from_ms.min(covered).to_string());
                }
            }
        }
        // the hours after the last stored row
        if let Some(l) = rows.last().map(|r| r.0).filter(|l| *l < now_ms - 3_600_000 && !fresh) {
            let (r, e) = download(coin, l + 1, now_ms);
            got.extend(r);
            err = err.or(e);
        }
        if !got.is_empty() {
            let had = rows.len();
            rows.extend(got);
            rows.sort_by_key(|r| r.0);
            rows.dedup_by_key(|r| r.0);
            let _ = std::fs::create_dir_all(dir());
            if prepended || had == 0 {
                let body: String = rows.iter().map(|(t, r)| format!("{}\n", json!({"t": t, "r": r}))).collect();
                std::fs::write(store(coin), body).map_err(|e| e.to_string())?;
            } else {
                let mut f = std::fs::OpenOptions::new().create(true).append(true).open(store(coin)).map_err(|e| e.to_string())?;
                for (t, r) in &rows[had..] {
                    let _ = writeln!(f, "{}", json!({"t": t, "r": r}));
                }
            }
        }
        if let Some(e) = &err {
            if rows.is_empty() {
                return Err(format!("Hyperliquid funding for {coin}: {e}"));
            }
            crate::warn!("hyperliquid", "funding download for {coin} failed, using what is stored: {e}");
        }
        ROWS.lock().unwrap().get_or_insert_with(HashMap::new).insert(coin.to_string(), (rows.clone(), Instant::now()));
    }
    Ok(rows.into_iter().filter(|r| r.0 / 1000 >= from - 3600).map(|(t, r)| ((t / 1000).div_euclid(3600) * 3600, r)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_map_to_their_perp() {
        assert_eq!(coin_for("WETH").as_deref(), Some("ETH"));
        assert_eq!(coin_for("cbETH").as_deref(), Some("ETH"));
        assert_eq!(coin_for("cbBTC").as_deref(), Some("BTC"));
        assert_eq!(coin_for("AERO").as_deref(), Some("AERO"));
        assert_eq!(coin_for("wS").as_deref(), Some("S"));
        assert_eq!(coin_for("USDC"), None);
    }

    /// A year of S funding into an empty store, then the same again from memory, then two years
    /// (only the earlier year is fetched). Network; run with ALM_HOME set to a scratch folder.
    #[test]
    #[ignore]
    fn funding_downloads_once_and_only_what_is_missing() {
        let home = std::env::var("ALM_HOME").expect("set ALM_HOME to a scratch folder");
        let live = std::env::var("LOCALAPPDATA").map(|l| format!("{l}\\StableALM").to_lowercase()).unwrap_or_default();
        assert!(!live.is_empty() && !home.to_lowercase().replace('/', "\\").starts_with(&live), "ALM_HOME is the live folder");
        let now = crate::engine::now() as i64;
        let t = Instant::now();
        let a = funding("S", now - 365 * 86400).unwrap();
        let cold = t.elapsed();
        let t = Instant::now();
        let b = funding("S", now - 365 * 86400).unwrap();
        let warm = t.elapsed();
        let t = Instant::now();
        let c = funding("S", now - 730 * 86400).unwrap();
        let back = t.elapsed();
        eprintln!("cold {cold:?} ({} h), warm {warm:?}, two years {back:?} ({} h)", a.len(), c.len());
        assert!(a.len() > 300 * 24 && a == b && c.len() >= a.len());
        assert!(warm < Duration::from_millis(50));
        let stored = read_store("S");
        assert!(stored.windows(2).all(|w| w[0].0 < w[1].0), "the store is sorted and has no repeats");
    }
}
