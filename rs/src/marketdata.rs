//! Hourly market series from free, keyless public sources, for the perp portfolio strategies
//! (`portfolio.rs`): a coin's own trend, volume and funding, and two flow signals.
//!
//! Series and their columns (rows are `t` = hour open, unix seconds):
//! - `perp`: Binance USD-M `<COIN>USDT` klines — open, high, low, close, base volume, quote volume.
//! - `funding`: Binance USD-M funding rate, at the hour the event falls in (every 8 h).
//! - `coinbase`: Coinbase `<COIN>-USD` hourly candles — open, high, low, close, volume.
//! - `lsr`: Bybit linear `<COIN>USDT` long/short account ratio (buy ratio), hourly, from 2021.
//!
//! Stored in `data/market/<series>_<COIN>.csv`. Only what is missing is downloaded: before the
//! first stored row once (a source with nothing earlier leaves a `.from` marker), and after the
//! last row up to the last closed hour.
use serde_json::Value;
use std::sync::Mutex;

pub const HOUR: i64 = 3600;
static ONE: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub t: i64,
    pub v: Vec<f64>,
}

fn cols(series: &str) -> Option<usize> {
    match series {
        "perp" => Some(6),
        "funding" | "lsr" => Some(1),
        "coinbase" => Some(5),
        _ => None,
    }
}

fn path(series: &str, coin: &str) -> std::path::PathBuf {
    crate::paths::data().join("market").join(format!("{series}_{coin}.csv"))
}

fn read(p: &std::path::Path, n: usize) -> Vec<Row> {
    let s = std::fs::read_to_string(p).unwrap_or_default();
    s.lines()
        .filter_map(|l| {
            let f: Vec<f64> = l.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (f.len() == n + 1).then(|| Row { t: f[0] as i64, v: f[1..].to_vec() })
        })
        .collect()
}

fn write(p: &std::path::Path, rows: &[Row]) -> Result<(), String> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let mut body = String::with_capacity(rows.len() * 64);
    for r in rows {
        body.push_str(&r.t.to_string());
        for x in &r.v {
            body.push(',');
            body.push_str(&x.to_string());
        }
        body.push('\n');
    }
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, p).map_err(|e| e.to_string())
}

fn num(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(f64::NAN)
}

fn iso(t: i64) -> String {
    let d = t.div_euclid(86400);
    let s = t.rem_euclid(86400);
    let (y, m, dd) = civil(d);
    format!("{y:04}-{m:02}-{dd:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// One source page walk over [a, b] (unix seconds).
fn fetch(series: &str, coin: &str, a: i64, b: i64) -> Result<Vec<Row>, String> {
    let mut out: Vec<Row> = vec![];
    match series {
        "perp" => {
            let mut s = a;
            while s <= b {
                let url = format!("https://fapi.binance.com/fapi/v1/klines?symbol={coin}USDT&interval=1h&startTime={}&endTime={}&limit=1500", s * 1000, b * 1000);
                let rows = crate::rpc::get_json(&url)?.as_array().cloned().unwrap_or_default();
                if rows.is_empty() {
                    break;
                }
                for r in &rows {
                    out.push(Row { t: (num(&r[0]) / 1000.0) as i64, v: vec![num(&r[1]), num(&r[2]), num(&r[3]), num(&r[4]), num(&r[5]), num(&r[7])] });
                }
                let next = out.last().unwrap().t + HOUR;
                if next <= s {
                    break;
                }
                s = next;
            }
        }
        "funding" => {
            let mut s = a;
            while s <= b {
                let url = format!("https://fapi.binance.com/fapi/v1/fundingRate?symbol={coin}USDT&startTime={}&endTime={}&limit=1000", s * 1000, b * 1000);
                let rows = crate::rpc::get_json(&url)?.as_array().cloned().unwrap_or_default();
                if rows.is_empty() {
                    break;
                }
                for r in &rows {
                    let t = (num(&r["fundingTime"]) / 1000.0) as i64;
                    out.push(Row { t: t - t.rem_euclid(HOUR), v: vec![num(&r["fundingRate"])] });
                }
                let next = out.last().unwrap().t + HOUR;
                if next <= s || rows.len() < 1000 {
                    break;
                }
                s = next;
            }
        }
        "coinbase" => {
            // 300 candles a call; newest first
            let mut s = a - a.rem_euclid(HOUR);
            while s <= b {
                let e = (s + 299 * HOUR).min(b);
                let url = format!("https://api.exchange.coinbase.com/products/{coin}-USD/candles?granularity=3600&start={}&end={}", iso(s), iso(e));
                let rows = crate::rpc::get_json(&url)?.as_array().cloned().unwrap_or_default();
                for r in &rows {
                    out.push(Row { t: num(&r[0]) as i64, v: vec![num(&r[3]), num(&r[2]), num(&r[1]), num(&r[4]), num(&r[5])] });
                }
                s = e + HOUR;
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        }
        "lsr" => {
            let mut s = a.max(1_609_459_200); // Bybit serves this from 2021
            while s <= b {
                let e = (s + 499 * HOUR).min(b);
                let url = format!("https://api.bybit.com/v5/market/account-ratio?category=linear&symbol={coin}USDT&period=1h&startTime={}&endTime={}&limit=500", s * 1000, e * 1000);
                let v = crate::rpc::get_json(&url)?;
                if v["retCode"].as_i64().unwrap_or(-1) != 0 {
                    return Err(format!("Bybit: {}", v["retMsg"]));
                }
                for r in v["result"]["list"].as_array().cloned().unwrap_or_default() {
                    // the ratio is stamped at the end of its hour
                    let t = (num(&r["timestamp"]) / 1000.0) as i64 - HOUR;
                    out.push(Row { t, v: vec![num(&r["buyRatio"])] });
                }
                s = e + 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        _ => return Err(format!("series {series:?}")),
    }
    Ok(out)
}

/// Rows of `series` for `coin` covering [from, to] (unix seconds), fetched and stored as needed. A
/// source that cannot be reached serves what is stored, with the error when nothing covers the span.
pub fn get(series: &str, coin: &str, from: i64, to: i64) -> Result<Vec<Row>, String> {
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    let n = cols(series).ok_or(format!("series {series:?}"))?;
    let p = path(series, coin);
    let mut rows = read(&p, n);
    let now = crate::engine::now() as i64;
    let to = to.min(now);
    let mut err = None;
    let mut got = vec![];
    let floor_p = p.with_extension("from");
    let floor: Option<i64> = std::fs::read_to_string(&floor_p).ok().and_then(|s| s.trim().parse().ok());
    let first = rows.first().map(|r| r.t);
    if first.map(|f| from < f).unwrap_or(true) && floor.map(|fl| from < fl).unwrap_or(true) {
        match fetch(series, coin, from, first.map(|f| f - 1).unwrap_or(to)) {
            Ok(r) => {
                got.extend(r);
                let _ = std::fs::create_dir_all(p.parent().unwrap());
                let _ = std::fs::write(&floor_p, from.to_string());
            }
            Err(e) => err = Some(e),
        }
    }
    // funding comes every 8 h: ask again only after the next event is due
    let gap = if series == "funding" { 8 * HOUR } else { 2 * HOUR };
    if let Some(l) = rows.last().map(|r| r.t).filter(|l| *l + gap <= to) {
        match fetch(series, coin, l + HOUR, to) {
            Ok(r) => got.extend(r),
            Err(e) => err = err.or(Some(e)),
        }
    }
    if !got.is_empty() {
        rows.extend(got);
        rows.sort_by_key(|r| r.t);
        rows.dedup_by_key(|r| r.t);
        // an hour still open is not kept (funding and ratios are point events)
        if series == "perp" || series == "coinbase" {
            rows.retain(|r| r.t + HOUR <= now && r.v[3].is_finite() && r.v[3] > 0.0);
        }
        write(&p, &rows)?;
    }
    let out: Vec<Row> = rows.into_iter().filter(|r| r.t >= from - HOUR && r.t <= to).collect();
    match (out.is_empty(), err) {
        (true, Some(e)) => Err(e),
        (true, None) => Err(format!("no {series} data for {coin} in the span")),
        (false, _) => Ok(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_round_trip_and_dates_format() {
        let p = std::env::temp_dir().join(format!("alm_market_{}.csv", std::process::id()));
        let rows = vec![Row { t: 3600, v: vec![1.0, 2.5] }, Row { t: 7200, v: vec![-0.5, 3.0] }];
        write(&p, &rows).unwrap();
        assert_eq!(read(&p, 2), rows);
        let _ = std::fs::remove_file(&p);
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_795_000_000), "2026-11-18T11:06:40Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
    }
}
