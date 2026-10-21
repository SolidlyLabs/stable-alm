//! Price candles from free public sources, for strategies tested on a coin's own long history (the
//! ETH direction leg: years and regimes the pool replays do not reach).
//!
//! Sources, keyless: Binance spot klines (`<COIN>USDT`, from 2017) and Hyperliquid perp candles
//! (`candleSnapshot`: its latest 5,000 per interval). Kept in `data/candles/<source>_<COIN>_<iv>.csv`;
//! only what is missing (before the first stored candle, after the last) is downloaded, so a
//! backtest that needs history fetches it once by itself.
use serde_json::json;
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candle {
    /// open time, unix seconds
    pub t: i64,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    pub v: f64,
}

static ONE: Mutex<()> = Mutex::new(());

fn step_s(iv: &str) -> Option<i64> {
    match iv {
        "1h" => Some(3600),
        "4h" => Some(4 * 3600),
        "1d" => Some(86400),
        _ => None,
    }
}

fn path(source: &str, coin: &str, iv: &str) -> std::path::PathBuf {
    crate::paths::data().join("candles").join(format!("{source}_{coin}_{iv}.csv"))
}

fn read(p: &std::path::Path) -> Vec<Candle> {
    let s = std::fs::read_to_string(p).unwrap_or_default();
    s.lines()
        .filter_map(|l| {
            let f: Vec<f64> = l.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (f.len() == 6).then(|| Candle { t: f[0] as i64, o: f[1], h: f[2], l: f[3], c: f[4], v: f[5] })
        })
        .collect()
}

fn write(p: &std::path::Path, rows: &[Candle]) -> Result<(), String> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let body: String = rows.iter().map(|c| format!("{},{},{},{},{},{}\n", c.t, c.o, c.h, c.l, c.c, c.v)).collect();
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, p).map_err(|e| e.to_string())
}

fn f(v: &serde_json::Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(f64::NAN)
}

/// Binance klines over [a, b] (unix seconds), 1,000 a page.
fn binance(coin: &str, iv: &str, a: i64, b: i64) -> Result<Vec<Candle>, String> {
    let step = step_s(iv).ok_or("interval")?;
    let mut out = vec![];
    let mut s = a;
    while s <= b {
        let mut last_err = String::new();
        let mut page = None;
        for base in ["https://api.binance.com", "https://data-api.binance.vision"] {
            let url = format!("{base}/api/v3/klines?symbol={coin}USDT&interval={iv}&startTime={}&endTime={}&limit=1000", s * 1000, b * 1000);
            match crate::rpc::get_json(&url) {
                Ok(v) => {
                    page = Some(v);
                    break;
                }
                Err(e) => last_err = e,
            }
        }
        let page = page.ok_or(format!("Binance klines: {last_err}"))?;
        let rows = page.as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            break;
        }
        for r in &rows {
            let t = (f(&r[0]) / 1000.0) as i64;
            out.push(Candle { t, o: f(&r[1]), h: f(&r[2]), l: f(&r[3]), c: f(&r[4]), v: f(&r[5]) });
        }
        let next = out.last().map(|c| c.t + step).unwrap_or(b + 1);
        if next <= s {
            break;
        }
        s = next;
    }
    Ok(out)
}

/// Hyperliquid candles over [a, b]: the venue keeps only the latest 5,000 of an interval.
fn hyperliquid(coin: &str, iv: &str, a: i64, b: i64) -> Result<Vec<Candle>, String> {
    let step = step_s(iv).ok_or("interval")?;
    let mut out = vec![];
    let mut s = a;
    while s <= b {
        let v = crate::rpc::post_json("https://api.hyperliquid.xyz/info",
                                      &json!({"type": "candleSnapshot", "req": {"coin": coin, "interval": iv, "startTime": s * 1000, "endTime": b * 1000}}), &[])?;
        let rows = v.as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            break;
        }
        for r in &rows {
            out.push(Candle { t: (f(&r["t"]) / 1000.0) as i64, o: f(&r["o"]), h: f(&r["h"]), l: f(&r["l"]), c: f(&r["c"]), v: f(&r["v"]) });
        }
        let next = out.last().map(|c| c.t + step).unwrap_or(b + 1);
        if next <= s || rows.len() < 2 {
            break;
        }
        s = next;
    }
    Ok(out)
}

/// The candles of `coin` at interval `iv` ("1h", "4h", "1d") from `source` ("binance" or
/// "hyperliquid") covering [from, to] (unix seconds), fetched and stored as needed. A source that
/// cannot be reached serves what is stored, with the error when that does not cover the span.
pub fn get(source: &str, coin: &str, iv: &str, from: i64, to: i64) -> Result<Vec<Candle>, String> {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let step = step_s(iv).ok_or(format!("interval {iv:?}: 1h, 4h or 1d"))?;
    let fetch = |a: i64, b: i64| match source {
        "binance" => binance(coin, iv, a, b),
        "hyperliquid" => hyperliquid(coin, iv, a, b),
        _ => Err(format!("source {source:?}: binance or hyperliquid")),
    };
    let p = path(source, coin, iv);
    let mut rows = read(&p);
    let now = crate::engine::now() as i64;
    let to = to.min(now);
    let mut err = None;
    let mut got = vec![];
    // before the first stored candle (once: a source with nothing earlier leaves a marker)
    let floor_p = p.with_extension("from");
    let floor: Option<i64> = std::fs::read_to_string(&floor_p).ok().and_then(|s| s.trim().parse().ok());
    let first = rows.first().map(|c| c.t);
    if first.map(|f| from < f).unwrap_or(true) && floor.map(|fl| from < fl).unwrap_or(true) {
        match fetch(from, first.map(|f| f - 1).unwrap_or(to)) {
            Ok(r) => {
                got.extend(r);
                let _ = std::fs::create_dir_all(p.parent().unwrap());
                let _ = std::fs::write(&floor_p, from.to_string());
            }
            Err(e) => err = Some(e),
        }
    }
    // after the last stored candle, up to the last one that has closed
    if let Some(l) = rows.last().map(|c| c.t).filter(|l| *l + 2 * step <= to) {
        match fetch(l + step, to) {
            Ok(r) => got.extend(r),
            Err(e) => err = err.or(Some(e)),
        }
    }
    if !got.is_empty() {
        rows.extend(got);
        rows.sort_by_key(|c| c.t);
        rows.dedup_by_key(|c| c.t);
        // a candle still open is not kept
        rows.retain(|c| c.t + step <= now && c.c.is_finite() && c.c > 0.0);
        write(&p, &rows)?;
    }
    let out: Vec<Candle> = rows.into_iter().filter(|c| c.t >= from - step && c.t <= to).collect();
    match (out.is_empty(), err) {
        (true, Some(e)) => Err(e),
        (true, None) => Err(format!("{source}: no {coin} {iv} candles in the span")),
        (false, _) => Ok(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_candles_round_trip() {
        let p = std::env::temp_dir().join(format!("alm_candles_{}.csv", std::process::id()));
        let rows = vec![Candle { t: 3600, o: 1.0, h: 2.0, l: 0.5, c: 1.5, v: 10.0 }, Candle { t: 7200, o: 1.5, h: 1.6, l: 1.4, c: 1.55, v: 3.0 }];
        write(&p, &rows).unwrap();
        assert_eq!(read(&p), rows);
        let _ = std::fs::remove_file(&p);
    }
}
