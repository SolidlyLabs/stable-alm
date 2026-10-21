//! Positions tab: measured APR per position and a timeline of what happened to them.
//!
//! Sources, all public on solidly.com:
//! - `/api/getPositionEarnings`: per position episode (liquidity leaves 0 until it returns to 0) the
//!   fees earned so far, the time-weighted capital, the lifetime APR and trailing 1 h to 30 d fee
//!   sums. Fees are gross; the LP gets 80% (the Solidly UI's `LP_FEES_RATIO`), so every figure here
//!   is the LP's 80%.
//! - `/api/rewards/{wallet}`: cumulative emissions per pool and token (no history, so we sample it).
//! - `/api/history/{chain}/wallets/{owner}/events`: every mint and burn of the wallet.
//!
//! A background thread samples the first two every 15 minutes into `logs/position_apr.jsonl`; the
//! APR chart is the change between samples. Before there are samples, the trailing fee sums give a
//! coarse history (last hour, 1–2 h, 2–4 h, … 7–30 days back). The timeline adds the app's own
//! transactions (ledger), decisions (executor log) and range exits/entries (cycle snapshots).
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::Duration;

const LP_SHARE: f64 = 0.8;
const SAMPLE_S: u64 = 900;
const YEAR_S: f64 = 365.0 * 86400.0;

fn track_path() -> std::path::PathBuf {
    crate::paths::logs().join("position_apr.jsonl")
}

fn earnings_key(cid: u64, wallet: &str) -> String {
    format!("earnings_{cid}_{}", wallet.to_lowercase())
}

/// The wallet's position episodes on a chain, closed ones included, from the Solidly UI now (and
/// kept, in memory and on disk, for `earnings`).
fn fetch_earnings(cid: u64, wallet: &str) -> Result<Value, String> {
    let url = format!("https://solidly.com/api/getPositionEarnings?chainId={cid}&owner={}&includeClosed=true", wallet.to_lowercase());
    let v = crate::rpc::get_json(&url)?;
    let key = earnings_key(cid, wallet);
    crate::store::save_snapshot(&key, &v);
    crate::store::background_put(&key, v.clone());
    Ok(v)
}

/// The same for the page: the last copy at once (refreshed in the background after 2 minutes).
pub fn earnings(cid: u64, wallet: &str) -> Result<Value, String> {
    let key = earnings_key(cid, wallet);
    let w = wallet.to_string();
    crate::store::background(&key, 120, Duration::from_secs(3), || crate::store::load_snapshot(&key), move || fetch_earnings(cid, &w))
        .ok_or_else(|| "position earnings are still loading from the Solidly UI".to_string())
}

/// A token's USD price without waiting: the last one known, refreshed in the background.
fn usd_now(cid: u64, token: &str) -> Option<f64> {
    let t = token.to_lowercase();
    crate::store::background(&format!("usd_{cid}_{t}"), 600, Duration::ZERO, || None, move || crate::prices::usd(cid, &t).map(|p| json!(p)))?.as_f64()
}

/// An integer the processor writes as a number or a string.
fn int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// A timestamp as unix seconds (a number, a string of one, or ISO-8601).
fn ts_of(v: &Value) -> Option<u64> {
    int(v).map(|t| t as u64).or_else(|| v.as_str().and_then(chrono_ts))
}

fn chains() -> Vec<u64> {
    let mut ids: Vec<u64> = crate::settings::load_pools().iter().map(crate::worker::pool_chain).collect();
    ids.sort();
    ids.dedup();
    ids
}

/// Pool address → (name, chain) of the configured pools.
fn names() -> HashMap<String, (String, u64)> {
    crate::settings::load_pools()
        .iter()
        .filter_map(|p| Some((p["pool"].as_str()?.to_lowercase(), (p["name"].as_str()?.to_string(), crate::worker::pool_chain(p)))))
        .collect()
}

/// The newest cycle snapshot as written (with every position's value and whether it is in range).
fn last_snapshot() -> Value {
    let s = std::fs::read_to_string(crate::paths::logs().join("snapshots.jsonl")).unwrap_or_default();
    s.lines().rev().find_map(|l| serde_json::from_str(l).ok()).unwrap_or(Value::Null)
}

/// Value now of each of our positions, by (pool, tickLower, tickUpper), from the last cycle.
fn position_values(snap: &Value) -> HashMap<(String, i64, i64), (f64, bool)> {
    let by_name: HashMap<String, String> = names().into_iter().map(|(a, (n, _))| (n, a)).collect();
    let mut out = HashMap::new();
    for (name, p) in snap["pools"].as_object().cloned().unwrap_or_default() {
        let Some(addr) = by_name.get(&name) else { continue };
        for q in p["positions"].as_array().cloned().unwrap_or_default() {
            let (tl, tu) = (q["tl"].as_i64().unwrap_or(0), q["tu"].as_i64().unwrap_or(0));
            out.insert((addr.clone(), tl, tu), (q["usd"].as_f64().unwrap_or(0.0), q["in_range"].as_bool().unwrap_or(false)));
        }
    }
    out
}

/// Cumulative rewards per pool, in tokens: {pool: {token: amount}}, and the tokens' USD prices.
fn rewards_cumulative(cid: u64, wallet: &str) -> Result<(Value, Value), String> {
    let snap = crate::claim::snapshot(cid, wallet)?;
    let (mut by_pool, mut px) = (Map::new(), Map::new());
    for r in snap["rewards"].as_array().cloned().unwrap_or_default() {
        let (pool, token) = (r["pool"].as_str().unwrap_or("").to_lowercase(), r["token"].as_str().unwrap_or("").to_lowercase());
        let m = crate::prices::meta(cid, &token)?;
        let n = crate::prices::fnum;
        let cum = n(&r["pendingCumulative"]).max(n(&r["claimableCumulative"])) / 10f64.powi(m.decimals as i32);
        let e = by_pool.entry(pool).or_insert_with(|| json!({}));
        e[&token] = json!(e[&token].as_f64().unwrap_or(0.0) + cum);
        if let Ok(p) = crate::prices::usd(cid, &token) {
            px.insert(token, json!(p));
        }
    }
    Ok((Value::Object(by_pool), Value::Object(px)))
}

/// One sample of every chain: appended to position_apr.jsonl.
fn sample(wallet: &str) {
    let snap = last_snapshot();
    let vals = position_values(&snap);
    let mut f = match std::fs::OpenOptions::new().create(true).append(true).open(track_path()) {
        Ok(f) => f,
        Err(e) => return crate::warn!("positions", "cannot write position_apr.jsonl: {e}"),
    };
    for cid in chains() {
        let e = match fetch_earnings(cid, wallet) {
            Ok(e) => e,
            Err(e) => {
                crate::warn!("positions", "position earnings for chain {cid}: {e}");
                continue;
            }
        };
        let n = crate::prices::fnum;
        let ps: Vec<Value> = e["positions"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|p| {
                let key = (p["poolId"].as_str().unwrap_or("").to_lowercase(), int(&p["tickLower"]).unwrap_or(0), int(&p["tickUpper"]).unwrap_or(0));
                let usd = if p["closed"] == true { None } else { vals.get(&key).map(|v| v.0) };
                json!({"id": p["positionId"], "ep": p["episodeIndex"], "pool": key.0, "tl": key.1, "tu": key.2, "fees": n(&p["feesUSD"]),
                       "cap": n(&p["averageCapitalUSD"]), "usd": usd, "closed": p["closed"]})
            })
            .collect();
        let (rw, px) = rewards_cumulative(cid, wallet).unwrap_or_else(|e| {
            crate::warn!("positions", "rewards for chain {cid}: {e}");
            (Value::Null, Value::Null)
        });
        let deployed: Map<String, Value> = snap["pools"]
            .as_object()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter(|(_, p)| p["chain_id"].as_u64() == Some(cid))
            .filter_map(|(name, p)| names().into_iter().find(|(_, (n, c))| n == name && *c == cid).map(|(a, _)| (a, p["deployed_usd"].clone())))
            .collect();
        let row = json!({"ts": crate::engine::now(), "chain_id": cid, "p": ps, "rw": rw, "px": px, "dep": deployed});
        use std::io::Write;
        let _ = writeln!(f, "{row}");
    }
}

pub fn start() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(90));
        loop {
            // every wallet's positions, each into its own folder
            for id in crate::wallets::ids() {
                crate::paths::scope(&id, || {
                    let wallet = crate::settings::load_settings()["wallet"].as_str().unwrap_or("").to_string();
                    if !wallet.is_empty() {
                        sample(&wallet);
                    }
                });
            }
            std::thread::sleep(Duration::from_secs(SAMPLE_S));
        }
    });
}

fn read_samples(cid: u64, since: u64) -> Vec<Value> {
    let s = std::fs::read_to_string(track_path()).unwrap_or_default();
    s.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|r| r["chain_id"].as_u64() == Some(cid) && r["ts"].as_u64().unwrap_or(0) >= since).collect()
}

/// APR between samples at least an hour apart: the change in fees (LP share) over the capital.
/// `points` are (ts, fees so far, capital); a fall in fees (a new episode) restarts the window.
pub fn apr_series(points: &[(u64, f64, f64)]) -> Vec<(u64, f64)> {
    let mut out = vec![];
    let mut start: Option<(u64, f64)> = None;
    for &(ts, fees, cap) in points {
        match start {
            None => start = Some((ts, fees)),
            Some((_, f0)) if fees < f0 - 1e-9 => start = Some((ts, fees)),
            Some((t0, f0)) if ts >= t0 + 3600 => {
                if cap > 0.0 {
                    out.push((ts, LP_SHARE * (fees - f0) / cap * YEAR_S / (ts - t0) as f64 * 100.0));
                }
                start = Some((ts, fees));
            }
            _ => {}
        }
    }
    out
}

/// Coarse history from the trailing fee sums: (from, to, APR) for 0–1 h, 1–2 h, 2–4 h, 4–12 h,
/// 12–24 h, 1–3 d, 3–7 d and 7–30 days back, cut at the episode's start.
pub fn trailing(intervals: &[Value], cap: f64, now: u64, start: u64) -> Vec<(u64, u64, f64)> {
    const W: [(&str, u64); 8] = [("HOUR_1", 1), ("HOUR_2", 2), ("HOUR_4", 4), ("HOUR_12", 12), ("HOUR_24", 24), ("DAY_3", 72), ("DAY_7", 168), ("DAY_30", 720)];
    let fee = |k: &str| intervals.iter().find(|i| i["aggregationInterval"] == k).map(|i| crate::prices::fnum(&i["feesUSD"]));
    let mut out = vec![];
    let (mut prev_h, mut prev_f) = (0u64, 0.0);
    for (k, h) in W {
        let Some(f) = fee(k) else { continue };
        let (from, to) = (now.saturating_sub(h * 3600).max(start), now.saturating_sub(prev_h * 3600));
        if to > from && cap > 0.0 {
            out.push((from, to, LP_SHARE * (f - prev_f).max(0.0) / cap * YEAR_S / (to - from) as f64 * 100.0));
        }
        if now.saturating_sub(h * 3600) <= start {
            break;
        }
        (prev_h, prev_f) = (h, f);
    }
    out.reverse();
    out
}

/// Each pool's tick and exact price (token1 per token0, raw units, from sqrtPrice) in the last cycle,
/// by pool address.
fn pool_prices(snap: &Value) -> HashMap<String, (i64, f64)> {
    let by_name: HashMap<String, String> = names().into_iter().map(|(a, (n, _))| (n, a)).collect();
    let mut out = HashMap::new();
    for (name, p) in snap["pools"].as_object().cloned().unwrap_or_default() {
        let (Some(addr), Some(tick), Some(px)) = (by_name.get(&name), p["tick"].as_i64(), p["price"].as_f64()) else { continue };
        out.insert(addr.clone(), (tick, px));
    }
    out
}

fn price_at(tick: i64, d0: i32, d1: i32) -> f64 {
    1.0001f64.powi(tick as i32) * 10f64.powi(d0 - d1)
}

/// The tab's data for every configured chain, built at most every 30 s (the building reads several log
/// files). An older one is rebuilt, and served only when the rebuild takes over 10 s; None when there
/// is none yet.
pub fn view(wallet: &str, days: u64) -> Option<Value> {
    let w = wallet.to_string();
    crate::store::background_fresh(&format!("positions_view_{}_{days}", w.to_lowercase()), 30, Duration::from_secs(10), move || Ok(build(&w, days)))
}

fn build(wallet: &str, days: u64) -> Value {
    let now = crate::engine::now();
    let since = now.saturating_sub(days * 86400);
    let snap = last_snapshot();
    let vals = position_values(&snap);
    let now_px = pool_prices(&snap);
    let names = names();
    let mut out = vec![];
    for cid in chains() {
        let e = match earnings(cid, wallet) {
            Ok(e) => e,
            Err(err) => {
                out.push(json!({"chain_id": cid, "error": err}));
                continue;
            }
        };
        let n = crate::prices::fnum;
        let samples = read_samples(cid, since.saturating_sub(3600));
        let mut positions = vec![];
        for p in e["positions"].as_array().cloned().unwrap_or_default() {
            let pool = p["poolId"].as_str().unwrap_or("").to_lowercase();
            let (tl, tu) = (int(&p["tickLower"]).unwrap_or(0), int(&p["tickUpper"]).unwrap_or(0));
            let (start, end) = (ts_of(&p["episodeStartTimestamp"]).unwrap_or(0), ts_of(&p["episodeEndTimestamp"]));
            let closed = p["closed"].as_bool().unwrap_or(false);
            if closed && end.unwrap_or(0) < since {
                continue;
            }
            // the value now belongs to the live episode only (a closed one may share its ticks)
            let now_val = if closed { None } else { vals.get(&(pool.clone(), tl, tu)).copied() };
            let (usd, in_range) = now_val.map(|(u, r)| (Some(u), Some(r))).unwrap_or((None, None));
            let cap = n(&p["averageCapitalUSD"]);
            let id = p["positionId"].as_str().unwrap_or("").to_string();
            let ep = p["episodeIndex"].clone();
            let pts: Vec<(u64, f64, f64)> = samples
                .iter()
                .filter_map(|r| {
                    let q = r["p"].as_array()?.iter().find(|q| q["id"] == id.as_str() && q["ep"] == ep)?.clone();
                    Some((r["ts"].as_u64()?, q["fees"].as_f64()?, q["usd"].as_f64().filter(|u| *u > 0.0).unwrap_or(q["cap"].as_f64().unwrap_or(0.0))))
                })
                .collect();
            let tokens = crate::chain::pool_tokens(cid, &pool).ok();
            let metas = tokens.as_ref().and_then(|(a, b)| Some((crate::prices::meta(cid, a).ok()?, crate::prices::meta(cid, b).ok()?)));
            let (sym, range, price_now) = match &metas {
                Some((m0, m1)) => {
                    let (d0, d1) = (m0.decimals as i32, m1.decimals as i32);
                    // the pool's price in the last cycle, for where it sits against an open range
                    let now = now_px.get(&pool).filter(|_| !closed).map(|(_, px)| px * 10f64.powi(d0 - d1));
                    (json!([m0.symbol, m1.symbol]), json!([price_at(tl, d0, d1), price_at(tu, d0, d1)]), now)
                }
                None => (Value::Null, Value::Null, None),
            };
            let tick_now = now_px.get(&pool).filter(|_| !closed).map(|(t, _)| *t);
            let intervals = p["intervals"].as_array().cloned().unwrap_or_default();
            let lp = |k: &str| intervals.iter().find(|i| i["aggregationInterval"] == k).map(|i| LP_SHARE * n(&i["feesUSD"]));
            let base = usd.filter(|u| *u > 0.0).unwrap_or(cap);
            positions.push(json!({
                "id": id, "episode": ep, "pool": pool, "name": names.get(&pool).map(|x| x.0.clone()), "symbols": sym, "tl": tl, "tu": tu, "prices": range,
                "price_now": price_now, "tick_now": tick_now, "price_ts": snap["ts"].as_u64().filter(|_| price_now.is_some()),
                "liquidity": p["liquidity"], "start": start, "end": end, "closed": closed, "usd": usd, "in_range": in_range,
                "fees_usd": LP_SHARE * n(&p["feesUSD"]), "deposited_usd": n(&p["depositedUSD"]), "avg_capital_usd": cap,
                "apr": LP_SHARE * n(&p["aprPercentage"]),
                "fees_24h": lp("HOUR_24"), "fees_7d": lp("DAY_7"), "fees_30d": lp("DAY_30"),
                "apr_24h": lp("HOUR_24").filter(|_| base > 0.0).map(|f| f / base * 365.0 * 100.0),
                "apr_7d": lp("DAY_7").filter(|_| base > 0.0).map(|f| f / base * 365.0 / 7.0 * 100.0),
                "series": apr_series(&pts).into_iter().filter(|x| x.0 >= since).map(|(t, a)| json!([t, a])).collect::<Vec<_>>(),
                "trailing": if closed { vec![] } else { trailing(&intervals, base, now, start).into_iter().filter(|x| x.1 >= since).map(|(a, b, c)| json!([a, b, c])).collect::<Vec<_>>() },
            }));
        }
        // rewards per pool: the change in cumulative emissions (at the newer sample's prices) over the
        // money deployed in the pool, between samples at least an hour apart
        let mut rewards: Map<String, Value> = Map::new();
        let pools: Vec<String> = samples.iter().filter_map(|r| r["rw"].as_object()).flat_map(|o| o.keys().cloned()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        for pool in pools {
            let mut pts = vec![];
            let mut start: Option<&Value> = None;
            for r in &samples {
                if r["rw"][&pool].is_null() {
                    continue;
                }
                match start {
                    None => start = Some(r),
                    Some(s0) if r["ts"].as_u64().unwrap_or(0) >= s0["ts"].as_u64().unwrap_or(0) + 3600 => {
                        let dt = (r["ts"].as_u64().unwrap_or(0) - s0["ts"].as_u64().unwrap_or(0)) as f64;
                        let gain: f64 = r["rw"][&pool]
                            .as_object()
                            .into_iter()
                            .flatten()
                            .map(|(t, a)| (a.as_f64().unwrap_or(0.0) - s0["rw"][&pool][t].as_f64().unwrap_or(0.0)).max(0.0) * r["px"][t].as_f64().unwrap_or(0.0))
                            .sum();
                        let dep = r["dep"][&pool].as_f64().unwrap_or(0.0);
                        if dep > 1.0 {
                            pts.push(json!([r["ts"], gain / dep * YEAR_S / dt * 100.0]));
                        }
                        start = Some(r);
                    }
                    _ => {}
                }
            }
            rewards.insert(pool, json!(pts.into_iter().filter(|x| x[0].as_u64().unwrap_or(0) >= since).collect::<Vec<_>>()));
        }
        out.push(json!({"chain_id": cid, "explorer": crate::chains::get(cid).explorer, "as_of": e["asOfTimestamp"], "positions": positions,
                        "rewards": rewards, "samples": samples.len()}));
    }
    json!({"now": now, "days": days, "lp_share": LP_SHARE, "chains": out, "events": events(wallet, since)})
}

/// ISO-8601 (as the processor writes it) to unix seconds.
fn chrono_ts(s: &str) -> Option<u64> {
    // YYYY-MM-DDTHH:MM:SS(.fff)Z
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let p = |a: usize, l: usize| s.get(a..a + l)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (p(0, 4)?, p(5, 2)?, p(8, 2)?, p(11, 2)?, p(14, 2)?, p(17, 2)?);
    // days from civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + h * 3600 + mi * 60 + se) as u64)
}

fn wallet_events_file(cid: u64, wallet: &str) -> std::path::PathBuf {
    crate::paths::data().join(format!("wallet_events_{cid}_{}.json", wallet.to_lowercase()))
}

/// The wallet's mints and burns from the Solidly UI's history: the copy on disk at once, topped up in
/// the background every 5 minutes.
fn wallet_events(cid: u64, wallet: &str) -> Vec<Value> {
    let file = wallet_events_file(cid, wallet);
    // the saved rows count as old, so the first look also tops them up
    let seed = || std::fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).map(|v| (v["rows"].clone(), 0));
    let w = wallet.to_string();
    let v = crate::store::background(&format!("wallet_events_{cid}_{}", wallet.to_lowercase()), 300, Duration::from_secs(3), seed, move || Ok(json!(top_up_wallet_events(cid, &w))));
    v.and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

/// Read the wallet's new history events from the Solidly UI into the file; returns every row.
fn top_up_wallet_events(cid: u64, wallet: &str) -> Vec<Value> {
    let Some(chain) = crate::history::blockchain_name(cid) else { return vec![] };
    let w = wallet.to_lowercase();
    let file = wallet_events_file(cid, wallet);
    let mut saved: Value = std::fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({"rows": [], "after": null}));
    let mut rows = saved["rows"].as_array().cloned().unwrap_or_default();
    let mut after = saved["after"].clone();
    for _ in 0..40 {
        let url = match int(&after) {
            Some(g) => format!("https://solidly.com/api/history/{chain}/wallets/{w}/events?after={g}"),
            None => format!("https://solidly.com/api/history/{chain}/wallets/{w}/events"),
        };
        let page = match crate::rpc::get_json(&url) {
            Ok(p) => p,
            Err(e) => {
                crate::warn!("positions", "wallet events for chain {cid}: {e}");
                break;
            }
        };
        let cols: Vec<String> = page["columns"].as_array().into_iter().flatten().filter_map(|c| c.as_str().map(String::from)).collect();
        let got = page["rows"].as_array().cloned().unwrap_or_default();
        for r in &got {
            let mut o = Map::new();
            for (i, c) in cols.iter().enumerate() {
                o.insert(c.clone(), r[i].clone());
            }
            after = o.get("g").cloned().unwrap_or(after.clone());
            rows.push(Value::Object(o));
        }
        if got.is_empty() || page["next"].is_null() {
            break;
        }
        after = page["next"].clone();
    }
    saved = json!({"rows": rows, "after": after});
    let _ = std::fs::write(&file, saved.to_string());
    rows
}

/// Range exits and entries of every pool, from the cycle snapshots.
fn range_changes(since: u64) -> Vec<Value> {
    let s = std::fs::read_to_string(crate::paths::logs().join("snapshots.jsonl")).unwrap_or_default();
    let by_name: HashMap<String, (String, u64)> = names().into_iter().map(|(a, (n, c))| (n, (a, c))).collect();
    let mut last: HashMap<String, bool> = HashMap::new();
    let mut out = vec![];
    for l in s.lines() {
        // most lines are older than the window: check the timestamp before parsing the whole line
        let Ok(r) = serde_json::from_str::<Value>(l) else { continue };
        let ts = r["ts"].as_u64().unwrap_or(0);
        for (name, p) in r["pools"].as_object().cloned().unwrap_or_default() {
            if p["positions"].as_array().map(|a| a.is_empty()).unwrap_or(true) {
                continue;
            }
            let now_in = p["in_range"].as_bool().unwrap_or(false);
            if let Some(was) = last.insert(name.clone(), now_in) {
                if was != now_in && ts >= since {
                    let (pool, cid) = by_name.get(&name).cloned().unwrap_or_default();
                    out.push(json!({"ts": ts, "chain_id": cid, "kind": if now_in { "range_enter" } else { "range_exit" }, "pool": pool, "name": name, "tick": p["tick"]}));
                }
            }
        }
    }
    out
}

/// Every event since `since`, newest first: mints and burns (Solidly UI history), the app's other
/// transactions (claims, swaps, approvals: the ledger), its decisions (executor log) and range changes.
fn events(wallet: &str, since: u64) -> Vec<Value> {
    let names = names();
    let mut out: Vec<Value> = vec![];
    let mut dec: HashMap<(u64, String), (i32, i32, Value)> = HashMap::new();
    for cid in chains() {
        for r in wallet_events(cid, wallet) {
            let ts = ts_of(&r["ts"]).unwrap_or(0);
            if ts < since {
                continue;
            }
            let pool = r["pool"].as_str().unwrap_or("").to_lowercase();
            let key = (cid, pool.clone());
            if !dec.contains_key(&key) {
                let m = crate::chain::pool_tokens(cid, &pool).ok().and_then(|(a, b)| Some((crate::prices::meta(cid, &a).ok()?, crate::prices::meta(cid, &b).ok()?)));
                if let Some((m0, m1)) = m {
                    dec.insert(key.clone(), (m0.decimals as i32, m1.decimals as i32, json!([m0.symbol, m1.symbol])));
                }
            }
            let (d0, d1, sym) = dec.get(&key).cloned().unwrap_or((18, 18, Value::Null));
            let n = crate::prices::fnum;
            out.push(json!({"ts": ts, "chain_id": cid, "kind": if r["k"] == "M" { "mint" } else { "burn" }, "pool": pool,
                            "name": names.get(&pool).map(|x| x.0.clone()), "tl": int(&r["tl"]), "tu": int(&r["tu"]), "symbols": sym,
                            "amount0": n(&r["a0"]).abs() / 10f64.powi(d0), "amount1": n(&r["a1"]).abs() / 10f64.powi(d1), "tx": r["tx"]}));
        }
    }
    for (tx, d) in crate::ledger::transactions() {
        let ts = d["ts"].as_u64().unwrap_or(0);
        let what = d["what"].as_str().unwrap_or("");
        if ts < since || what.starts_with("mint") || what.starts_with("burn") {
            continue;
        }
        let cid = d["chain_id"].as_u64().unwrap_or(8453);
        let pool = d["pool"].as_str().unwrap_or("").to_lowercase();
        let kind = if what == "claim" { "claim" } else if what.contains("swap") || what.starts_with("sell") { "swap" } else { "tx" };
        let moves: Vec<Value> = d["moves"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(t, v)| {
                let m = crate::prices::meta(cid, t).ok()?;
                let a = v.as_f64()? / 10f64.powi(m.decimals as i32);
                Some(json!({"symbol": m.symbol, "amount": a, "usd": usd_now(cid, t).map(|p| p * a)}))
            })
            .collect();
        out.push(json!({"ts": ts, "chain_id": cid, "kind": kind, "what": what, "pool": pool, "name": names.get(&pool).map(|x| x.0.clone()),
                        "moves": moves, "gas": d["gas_wei"].as_f64().unwrap_or(0.0) / 1e18, "ok": d["status"] == "0x1", "tx": tx}));
    }
    for e in crate::engine::log_tail(4_000_000) {
        let ts = e["ts"].as_u64().unwrap_or(0);
        let k = e["kind"].as_str().unwrap_or("");
        let keep = (k == "rebalance" && e["live"] == true) || k == "error" || k == "park" || k == "park_handover" || k == "capital" || k == "solid_lp";
        if ts < since || !keep {
            continue;
        }
        let pool = e["pool"].as_str().unwrap_or("").to_lowercase();
        let mut v = e.clone();
        v["kind"] = json!(if k == "rebalance" { "decision" } else { k });
        v["name"] = json!(names.get(&pool).map(|x| x.0.clone()));
        out.push(v);
    }
    out.extend(range_changes(since));
    out.sort_by(|a, b| b["ts"].as_u64().cmp(&a["ts"].as_u64()));
    out.truncate(600);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apr_between_samples_an_hour_apart_and_a_new_episode_restarts() {
        // $1000 earning $1 gross over 2 hours: 0.8 × 1/1000 × (year / 2 h)
        let s = apr_series(&[(0, 0.0, 1000.0), (1800, 0.3, 1000.0), (7200, 1.0, 1000.0), (9000, 0.0, 1000.0), (12600, 0.5, 1000.0)]);
        assert_eq!(s.len(), 2);
        assert!((s[0].1 - 0.8 * 0.001 * YEAR_S / 7200.0 * 100.0).abs() < 1e-9);
        assert_eq!(s[1].0, 12600);
        assert!((s[1].1 - 0.8 * 0.0005 * YEAR_S / 3600.0 * 100.0).abs() < 1e-9);
    }

    #[test]
    fn trailing_sums_become_buckets_cut_at_the_episode_start() {
        let iv: Vec<Value> = [("HOUR_1", 1.0), ("HOUR_2", 1.5), ("HOUR_4", 2.0), ("HOUR_12", 4.0), ("HOUR_24", 6.0), ("DAY_3", 9.0)]
            .iter()
            .map(|(k, f)| json!({"aggregationInterval": k, "feesUSD": f.to_string()}))
            .collect();
        let now = 100 * 3600;
        let b = trailing(&iv, 1000.0, now, now - 10 * 3600);
        // 0–1 h, 1–2 h, 2–4 h, then 4–10 h (the episode started 10 h ago); nothing older
        assert_eq!(b.len(), 4);
        assert_eq!(b[0], (now - 10 * 3600, now - 4 * 3600, 0.8 * 2.0 / 1000.0 * YEAR_S / (6.0 * 3600.0) * 100.0));
        assert_eq!(b[3].1, now);
    }

    /// On a copy of the app folder (never the live one), with the network:
    /// ALM_HOME=<copy> cargo test -- --ignored view_on_a_copy --nocapture
    #[test]
    #[ignore]
    fn view_on_a_copy() {
        let home = std::env::var("ALM_HOME").expect("set ALM_HOME to a copy of the app folder");
        assert!(!home.to_lowercase().replace('\\', "/").trim_end_matches('/').ends_with("appdata/local/stablealm"), "not on the live folder");
        let wallet = crate::settings::load_settings()["wallet"].as_str().unwrap().to_string();
        for (i, days) in [7u64, 7, 30].iter().enumerate() {
            let t = std::time::Instant::now();
            let _ = view(&wallet, *days);
            println!("view({days}) call {i}: {:.2} s", t.elapsed().as_secs_f64());
        }
        sample(&wallet);
        let v = build(&wallet, 30);
        std::fs::write(std::path::Path::new(&home).join("positions_view.json"), v.to_string()).unwrap();
        for c in v["chains"].as_array().unwrap() {
            println!("chain {} error {} samples {}", c["chain_id"], c["error"], c["samples"]);
            for p in c["positions"].as_array().unwrap() {
                println!(
                    "  {} [{},{}) closed {} usd {} in_range {} fees {:.4} apr {:.2} apr_24h {} trailing {} series {}",
                    p["name"], p["tl"], p["tu"], p["closed"], p["usd"], p["in_range"], p["fees_usd"].as_f64().unwrap(), p["apr"].as_f64().unwrap(),
                    p["apr_24h"], p["trailing"].as_array().unwrap().len(), p["series"].as_array().unwrap().len()
                );
            }
            println!("  rewards pools {:?}", c["rewards"].as_object().unwrap().keys().collect::<Vec<_>>());
        }
        let ev = v["events"].as_array().unwrap();
        let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
        for e in ev {
            *kinds.entry(e["kind"].as_str().unwrap_or("?").to_string()).or_default() += 1;
        }
        println!("events {} {:?}", ev.len(), kinds);
        assert!(ev.iter().any(|e| e["kind"] == "mint"));
    }

    #[test]
    fn processor_timestamps_parse() {
        assert_eq!(chrono_ts("2026-10-20T00:00:00.000Z"), Some(1792454400));
        assert_eq!(chrono_ts("1970-01-01T00:00:01Z"), Some(1));
    }
}
