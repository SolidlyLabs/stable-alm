//! Pool data for the live executor.
//!
//! Storage per pool: data/raw/<chain>/<pool>/
//!   events.jsonl         Swap/Mint/Burn rows, append-only, pruned to the last 31 days
//!   wallet_<addr>.jsonl  every Mint/Burn of the wallet in the pool (full history)
//!   synced_to.txt        last block whose events are stored
//!   apr_hourly.json      processor hourly history (fees, volume, emissions)
//! The replay market is rebuilt each cycle from the last 10 days, as live_market() does.

use crate::chains;
use crate::rpc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::PathBuf;

pub const SWAP: &str = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67";
pub const MINT: &str = "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde";
pub const BURN: &str = "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c";
const CHUNK: i64 = 10_000;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Ev {
    pub block: i64,
    pub log_index: i64,
    pub ts: i64,
    pub kind: String,
    #[serde(default)]
    pub amount0: String,
    #[serde(default)]
    pub amount1: String,
    #[serde(default)]
    pub sqrt_price_x96: String,
    #[serde(default)]
    pub liquidity: String,
    #[serde(default)]
    pub tick: i64,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub tick_lower: i64,
    #[serde(default)]
    pub tick_upper: i64,
    #[serde(default)]
    pub amount: String,
}

pub fn pool_dir(cid: u64, pool: &str) -> PathBuf {
    crate::paths::data().join("raw").join(cid.to_string()).join(pool.to_lowercase())
}

fn s256(h: &str) -> String {
    let v = crate::eth::parse_uint(&format!("0x{h}"));
    let top = num_bigint::BigUint::from(1u8) << 255;
    if v >= top {
        let neg = (num_bigint::BigInt::from(1) << 256) - num_bigint::BigInt::from(v);
        format!("-{neg}")
    } else {
        v.to_string()
    }
}

fn s256_i(h: &str) -> i64 {
    s256(h).parse().unwrap_or(0)
}

pub fn decode(cid: u64, log: &Value) -> Ev {
    let t: Vec<&str> = log["topics"].as_array().unwrap().iter().map(|x| x.as_str().unwrap_or("")).collect();
    let d = log["data"].as_str().unwrap_or("0x").trim_start_matches("0x");
    let w: Vec<&str> = (0..d.len() / 64).map(|i| &d[i * 64..i * 64 + 64]).collect();
    let block = rpc::hex_u64(&log["blockNumber"]) as i64;
    let mut e = Ev { block, log_index: rpc::hex_u64(&log["logIndex"]) as i64, ts: chains::block_ts(cid, block), ..Default::default() };
    let u = |h: &str| crate::eth::parse_uint(&format!("0x{h}")).to_string();
    if t[0] == SWAP {
        e.kind = "swap".into();
        e.amount0 = s256(w[0]);
        e.amount1 = s256(w[1]);
        e.sqrt_price_x96 = u(w[2]);
        e.liquidity = u(w[3]);
        e.tick = s256_i(w[4]);
    } else {
        let off = if t[0] == MINT { 1 } else { 0 };
        e.kind = if t[0] == MINT { "mint" } else { "burn" }.into();
        e.owner = format!("0x{}", &t[1][26..]).to_lowercase();
        e.tick_lower = s256_i(t[2].trim_start_matches("0x"));
        e.tick_upper = s256_i(t[3].trim_start_matches("0x"));
        e.amount = u(w[off]);
        e.amount0 = u(w[off + 1]);
        e.amount1 = u(w[off + 2]);
    }
    e
}

fn get_logs(cid: u64, pool: &str, ranges: &[(i64, i64)], topics: &Value) -> Result<Vec<rpc::R>, String> {
    let calls: Vec<(&str, Value)> = ranges
        .iter()
        .map(|(a, b)| ("eth_getLogs", json!([{"address": pool, "fromBlock": format!("0x{a:x}"), "toBlock": format!("0x{b:x}"), "topics": topics}])))
        .collect();
    rpc::batch(cid, &calls)
}

fn fetch_single(cid: u64, pool: &str, a: i64, b: i64, topics: &Value) -> Result<Vec<Ev>, String> {
    match get_logs(cid, pool, &[(a, b)], topics)?.remove(0) {
        Ok(Value::Array(logs)) => Ok(logs.iter().map(|l| decode(cid, l)).collect()),
        r if b - a < 10 => Err(format!("getLogs failed for {a}-{b}: {r:?}")),
        _ => {
            let m = (a + b) / 2;
            let mut v = fetch_single(cid, pool, a, m, topics)?;
            v.extend(fetch_single(cid, pool, m + 1, b, topics)?);
            Ok(v)
        }
    }
}

pub fn fetch_range(cid: u64, pool: &str, start: i64, end: i64) -> Result<Vec<Ev>, String> {
    let topics = json!([[SWAP, MINT, BURN]]);
    let ranges: Vec<(i64, i64)> = (start..=end).step_by(CHUNK as usize).map(|b| (b, (b + CHUNK - 1).min(end))).collect();
    let mut rows = vec![];
    for part in ranges.chunks(10) {
        for ((a, b), r) in part.iter().zip(get_logs(cid, pool, part, &topics)?) {
            match r {
                Ok(Value::Array(logs)) => rows.extend(logs.iter().map(|l| decode(cid, l))),
                _ => {
                    let m = (a + b) / 2;
                    rows.extend(fetch_single(cid, pool, *a, m, &topics)?);
                    rows.extend(fetch_single(cid, pool, m + 1, *b, &topics)?);
                }
            }
        }
    }
    Ok(rows)
}

fn wallet_history(cid: u64, pool: &str, wallet: &str, head: i64) -> Result<Vec<Ev>, String> {
    let pad = format!("0x{}{}", "0".repeat(24), wallet.trim_start_matches("0x").to_lowercase());
    let topics = json!([[MINT, BURN], pad]);
    let step = 2_000_000i64;
    let mut ranges: Vec<(i64, i64)> = (0..=head).step_by(step as usize).map(|a| (a, (a + step - 1).min(head))).collect();
    let mut rows = vec![];
    while !ranges.is_empty() {
        let part: Vec<(i64, i64)> = ranges.drain(..ranges.len().min(10)).collect();
        for ((a, b), r) in part.iter().zip(get_logs(cid, pool, &part, &topics)?) {
            match r {
                Ok(Value::Array(logs)) => rows.extend(logs.iter().map(|l| decode(cid, l))),
                _ if b - a > 1000 => {
                    let m = (a + b) / 2;
                    ranges.push((*a, m));
                    ranges.push((m + 1, *b));
                }
                r => return Err(format!("wallet getLogs failed for {a}-{b}: {r:?}")),
            }
        }
    }
    Ok(rows)
}

pub fn read_jsonl(p: &PathBuf) -> Vec<Ev> {
    let Ok(f) = std::fs::File::open(p) else { return vec![] };
    std::io::BufReader::new(f).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str(&l).ok()).collect()
}

fn append_jsonl(p: &PathBuf, rows: &[Ev]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
    for r in rows {
        writeln!(f, "{}", serde_json::to_string(r).unwrap())?;
    }
    Ok(())
}

fn write_jsonl(p: &PathBuf, rows: &[Ev]) -> std::io::Result<()> {
    let tmp = p.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    append_jsonl(&tmp, rows)?;
    std::fs::rename(tmp, p)
}

pub fn wallet_path(cid: u64, pool: &str, wallet: &str) -> PathBuf {
    pool_dir(cid, pool).join(format!("wallet_{}.jsonl", wallet.to_lowercase()))
}

/// Keep the pool's data current (sync_pool).
pub fn sync_pool(cid: u64, pool: &str, wallet: &str, bootstrap_days: i64) -> Result<(), String> {
    let (pool, wallet) = (pool.to_lowercase(), wallet.to_lowercase());
    let head = rpc::block_number(cid)? as i64;
    let dir = pool_dir(cid, &pool);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let bpd = chains::blocks_per_day(cid);
    let state = dir.join("synced_to.txt");
    let start = match std::fs::read_to_string(&state).ok().and_then(|s| s.trim().parse::<i64>().ok()) {
        Some(b) => b + 1,
        None => {
            crate::info!("data", "{}: no stored events; bootstrapping the last {} days", &pool[..10], bootstrap_days);
            head - bootstrap_days * bpd
        }
    };
    let evp = dir.join("events.jsonl");
    let mut new = vec![];
    if start <= head {
        new = fetch_range(cid, &pool, start, head)?;
        append_jsonl(&evp, &new).map_err(|e| e.to_string())?;
    }
    std::fs::write(&state, head.to_string()).map_err(|e| e.to_string())?;
    // Prune: keep 31 days of events (the replay reads 10).
    let keep_from = head - 31 * bpd;
    let first = std::fs::File::open(&evp)
        .ok()
        .and_then(|f| std::io::BufReader::new(f).lines().next())
        .and_then(|l| l.ok())
        .and_then(|l| serde_json::from_str::<Ev>(&l).ok());
    if first.map(|e| e.block < keep_from - bpd).unwrap_or(false) {
        let rows: Vec<Ev> = read_jsonl(&evp).into_iter().filter(|e| e.block >= keep_from).collect();
        write_jsonl(&evp, &rows).map_err(|e| e.to_string())?;
    }
    // Wallet history: full scan once, then the wallet's rows from new events.
    let wp = wallet_path(cid, &pool, &wallet);
    if !wp.exists() {
        let w = wallet_history(cid, &pool, &wallet, head)?;
        crate::info!("data", "{}: wallet history {} mint/burn events", &pool[..10], w.len());
        write_jsonl(&wp, &w).map_err(|e| e.to_string())?;
    } else {
        let mine: Vec<Ev> = new.into_iter().filter(|e| e.kind != "swap" && e.owner == wallet).collect();
        if !mine.is_empty() {
            append_jsonl(&wp, &mine).map_err(|e| e.to_string())?;
        }
    }
    // Processor hourly history, at most hourly.
    let ap = dir.join("apr_hourly.json");
    let stale = std::fs::metadata(&ap)
        .and_then(|m| m.modified())
        .map(|t| t.elapsed().map(|d| d.as_secs() > 3600).unwrap_or(true))
        .unwrap_or(true);
    if stale {
        match apr_history(cid, &pool) {
            Ok(h) => {
                let _ = std::fs::write(&ap, serde_json::to_string(&h).unwrap());
            }
            Err(e) => crate::warn!("data", "{}: processor history refresh failed: {e}", &pool[..10]),
        }
    }
    Ok(())
}

pub fn apr_history(cid: u64, pool: &str) -> Result<Vec<Value>, String> {
    let mut rows = vec![];
    let mut offset = 0;
    loop {
        let url = format!("https://solidly.com/api/getPoolAprHistory/?chainId={cid}&poolId={pool}&interval=hour&limit=2400&offset={offset}");
        let h = rpc::get_json(&url)?["history"].as_array().cloned().unwrap_or_default();
        let n = h.len();
        rows.extend(h);
        offset += n;
        if n < 2400 {
            break;
        }
    }
    Ok(rows)
}

/// (fees / volume clipped to [0, 1%], emissions USD) per hour timestamp.
pub fn load_apr(cid: u64, pool: &str) -> HashMap<i64, (f64, f64)> {
    let v: Vec<Value> = std::fs::read_to_string(pool_dir(cid, pool).join("apr_hourly.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let n = |x: &Value| crate::prices::fnum(x);
    let mut out = HashMap::new();
    for r in v {
        let ts = n(&r["timestamp"]) as i64;
        let vol = n(&r["volumeUSD"]);
        let rate = if vol == 0.0 { 0.0 } else { (n(&r["feesUSD"]) / vol).clamp(0.0, 0.01) };
        let rate = if rate.is_nan() { 0.0 } else { rate };
        out.entry(ts).or_insert((rate, n(&r["emissionsUSD"])));
    }
    out
}

// -- the replay market (live parts of alm/data.py Market) -------------------------------------------
pub struct Market {
    pub ts: Vec<i64>,
    pub tick: Vec<i64>,
    pub fee: Vec<f64>,
    pub l_others: Vec<f64>,
    pub bar_end: Vec<i64>,
    pub bar_idx: Vec<usize>,
    pub bar_tick: Vec<i64>,
    pub decimals: i32,
    pub px_scale: f64,
    pub liq_scale: f64,
}

/// numpy's round-half-to-even.
pub fn round_even(x: f64) -> f64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        r
    }
}

pub fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// pandas Series.rolling(w, center=True, min_periods=1).median() on integer ticks.
fn rolling_median_centered(x: &[i64], w: usize) -> Vec<f64> {
    let half = w / 2;
    let n = x.len();
    // Ticks are integers in a narrow band: a counting window keeps this O(n * band).
    let mut out = vec![0.0; n];
    let mut window: BTreeMap<i64, usize> = BTreeMap::new();
    let (mut lo, mut hi) = (0usize, 0usize); // window = x[lo..hi)
    for i in 0..n {
        let a = i.saturating_sub(half);
        let b = (i + half + 1).min(n);
        while hi < b {
            *window.entry(x[hi]).or_default() += 1;
            hi += 1;
        }
        while lo < a {
            let c = window.get_mut(&x[lo]).unwrap();
            *c -= 1;
            if *c == 0 {
                window.remove(&x[lo]);
            }
            lo += 1;
        }
        out[i] = counted_median(&window, hi - lo);
    }
    out
}

fn counted_median(w: &BTreeMap<i64, usize>, n: usize) -> f64 {
    let kth = |k: usize| {
        let mut acc = 0;
        for (v, c) in w {
            acc += c;
            if acc > k {
                return *v as f64;
            }
        }
        0.0
    };
    if n % 2 == 1 {
        kth(n / 2)
    } else {
        (kth(n / 2 - 1) + kth(n / 2)) / 2.0
    }
}

/// pandas rolling(w, min_periods=mp).median() (trailing), then bfill.
pub fn rolling_median_trailing_bfill(x: &[i64], w: usize, mp: usize) -> Vec<f64> {
    let n = x.len();
    let mut out = vec![f64::NAN; n];
    let mut window: BTreeMap<i64, usize> = BTreeMap::new();
    for i in 0..n {
        *window.entry(x[i]).or_default() += 1;
        if i >= w {
            let c = window.get_mut(&x[i - w]).unwrap();
            *c -= 1;
            if *c == 0 {
                window.remove(&x[i - w]);
            }
        }
        let cnt = (i + 1).min(w);
        if cnt >= mp {
            out[i] = counted_median(&window, cnt);
        }
    }
    let mut next = f64::NAN;
    for i in (0..n).rev() {
        if out[i].is_nan() {
            out[i] = next;
        } else {
            next = out[i];
        }
    }
    out
}

fn f(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(0.0)
}

/// Build the market from events (already filtered to the window, plus the wallet's full history).
pub fn build_market(events: Vec<Ev>, wallet: &str, apr: &HashMap<i64, (f64, f64)>, d0: i32, d1: i32, numeraire: i64) -> Result<Market, String> {
    let mut seen = HashSet::new();
    let mut ev: Vec<Ev> = events.into_iter().filter(|e| seen.insert((e.block, e.log_index))).collect();
    ev.sort_by_key(|e| (e.block, e.log_index));
    let px_scale = 10f64.powi(d0 - d1);
    let liq_scale = 10f64.powf((d0 + d1) as f64 / 2.0);
    let sw: Vec<(usize, &Ev)> = ev.iter().enumerate().filter(|(_, e)| e.kind == "swap").collect();
    if sw.is_empty() {
        return Err("no swaps in the replay window yet".into());
    }
    let raw_tick: Vec<i64> = sw.iter().map(|(_, e)| e.tick).collect();
    let med = rolling_median_centered(&raw_tick, 51);
    let mut tick = Vec::with_capacity(sw.len());
    let mut sp = Vec::with_capacity(sw.len());
    for (k, (_, e)) in sw.iter().enumerate() {
        let m = round_even(med[k]);
        if (raw_tick[k] as f64 - m).abs() > 20.0 {
            tick.push(m as i64);
            sp.push(1.0001f64.powf((m + 0.5) / 2.0));
        } else {
            tick.push(raw_tick[k]);
            sp.push(f(&e.sqrt_price_x96) / 2f64.powi(96));
        }
    }
    // Wallet liquidity active at each swap's (post) tick.
    let lp: Vec<(usize, &Ev)> = ev.iter().enumerate().filter(|(_, e)| e.kind != "swap" && e.owner == wallet).collect();
    let mut positions: Vec<((i64, i64), f64)> = vec![];
    let mut j = 0;
    let mut l_wallet = vec![0.0; sw.len()];
    for (k, (idx, _)) in sw.iter().enumerate() {
        while j < lp.len() && lp[j].0 < *idx {
            let e = lp[j].1;
            let key = (e.tick_lower, e.tick_upper);
            let sign = if e.kind == "mint" { 1.0 } else { -1.0 };
            match positions.iter_mut().find(|(k2, _)| *k2 == key) {
                Some(p) => p.1 += sign * f(&e.amount),
                None => positions.push((key, sign * f(&e.amount))),
            }
            j += 1;
        }
        let t = tick[k];
        l_wallet[k] = positions.iter().filter(|((tl, tu), v)| *tl <= t && t < *tu && *v > 0.0).map(|(_, v)| *v).sum();
    }
    let ts: Vec<i64> = sw.iter().map(|(_, e)| e.ts).collect();
    let mut fee = Vec::with_capacity(sw.len());
    let mut l_others = Vec::with_capacity(sw.len());
    for (k, (_, e)) in sw.iter().enumerate() {
        let a0 = f(&e.amount0) / 10f64.powi(d0);
        let a1 = f(&e.amount1) / 10f64.powi(d1);
        let p = sp[k] * sp[k] * px_scale;
        let in0 = if a0 > 0.0 { a0 } else { 0.0 };
        let in1 = if a1 > 0.0 { a1 } else { 0.0 };
        let amount_in = if numeraire == 0 { in0 + in1 / p } else { in0 * p + in1 };
        let hour = ts[k].div_euclid(3600) * 3600;
        fee.push(amount_in * apr.get(&hour).map(|x| x.0).unwrap_or(0.0));
        l_others.push((f(&e.liquidity) - l_wallet[k]).max(0.0) / liq_scale);
    }
    let bs = 300i64;
    let t0 = (ts[0].div_euclid(bs) + 1) * bs;
    let last = *ts.last().unwrap();
    let bar_end: Vec<i64> = (0..).map(|i| t0 + i * bs).take_while(|t| *t < last).collect();
    let bar_idx: Vec<usize> = bar_end.iter().map(|b| ts.partition_point(|t| t <= b) - 1).collect();
    let bar_tick = bar_idx.iter().map(|i| tick[*i]).collect();
    Ok(Market { ts, tick, fee, l_others, bar_end, bar_idx, bar_tick, decimals: d0, px_scale, liq_scale })
}

/// live_market(): the last `days` of swaps plus the wallet's full Mint/Burn history.
pub fn live_market(cid: u64, pool: &str, wallet: &str, d0: i32, d1: i32, numeraire: i64, days: i64) -> Result<Market, String> {
    let head = rpc::block_number(cid)? as i64;
    let since = head - days * chains::blocks_per_day(cid);
    let dir = pool_dir(cid, pool);
    let mut ev: Vec<Ev> = read_jsonl(&dir.join("events.jsonl")).into_iter().filter(|e| e.block >= since || e.kind != "swap").collect();
    ev.extend(read_jsonl(&wallet_path(cid, pool, wallet)));
    if ev.is_empty() {
        return Err(format!("no event data for pool {pool}; run the data bootstrap first"));
    }
    build_market(ev, &wallet.to_lowercase(), &load_apr(cid, pool), d0, d1, numeraire)
}

/// (tl, tu) of the wallet's ranges from its Mint/Burn history.
pub fn history_ranges(cid: u64, pool: &str, wallet: &str) -> HashSet<(i64, i64)> {
    read_jsonl(&wallet_path(cid, pool, wallet)).into_iter().map(|e| (e.tick_lower, e.tick_upper)).collect()
}

// -- fee vs loss to arbitrage (research/volatile/fee_vs_lvr.py, reports/VOLATILE.md) -----------------
/// Per unit of at-the-money liquidity over a window: F = Σ fee_i / L_i over swaps (fee_i = amount in,
/// in the numeraire, × the hour's processor fee rate; L_i = the Swap event's active liquidity), and
/// LVR = Σ r² · √P / 4 over 1-hour bars (1/√P when token0 is the numeraire). F/LVR well above 1 means
/// the pool's flow is fee-paying; toward 2 or below it is arbitrage. A monitor only: trading ignores it.
#[derive(Clone, Debug, Default)]
pub struct Flvr {
    pub f: f64,
    pub lvr: f64,
    pub swaps: usize,
    pub hours: usize,
}

impl Flvr {
    /// None when the price did not move over the window (no loss to arbitrage at all).
    pub fn ratio(&self) -> Option<f64> {
        if self.lvr > 0.0 { Some(self.f / self.lvr) } else { None }
    }

    pub fn to_json(&self, since: i64) -> Value {
        json!({"ratio": self.ratio(), "f": self.f, "lvr": self.lvr, "swaps": self.swaps, "hours": self.hours, "since": since})
    }
}

/// F/LVR from the swaps at or after `since` (events may be unsorted, with duplicates).
pub fn fee_vs_lvr(events: &[Ev], apr: &HashMap<i64, (f64, f64)>, d0: i32, d1: i32, numeraire: i64, since: i64) -> Option<Flvr> {
    let mut seen = HashSet::new();
    let mut sw: Vec<&Ev> = events.iter().filter(|e| e.kind == "swap" && e.ts >= since && seen.insert((e.block, e.log_index))).collect();
    if sw.is_empty() {
        return None;
    }
    sw.sort_by_key(|e| (e.block, e.log_index));
    let px_scale = 10f64.powi(d0 - d1);
    let liq_scale = 10f64.powf((d0 + d1) as f64 / 2.0);
    let ts: Vec<i64> = sw.iter().map(|e| e.ts).collect();
    let p: Vec<f64> = sw.iter().map(|e| 1.0001f64.powf(e.tick as f64) * px_scale).collect();
    let mut out = Flvr { swaps: sw.len(), ..Default::default() };
    for (k, e) in sw.iter().enumerate() {
        let l = f(&e.liquidity) / liq_scale;
        if l <= 0.0 {
            continue;
        }
        let in0 = (f(&e.amount0) / 10f64.powi(d0)).max(0.0);
        let in1 = (f(&e.amount1) / 10f64.powi(d1)).max(0.0);
        let amount_in = if numeraire == 0 { in0 + in1 / p[k] } else { in0 * p[k] + in1 };
        let rate = apr.get(&(ts[k].div_euclid(3600) * 3600)).map(|x| x.0).unwrap_or(0.0);
        out.f += amount_in * rate / l;
    }
    let last = *ts.last().unwrap();
    let mut b = (ts[0].div_euclid(3600) + 1) * 3600;
    let mut prev: Option<f64> = None;
    while b < last {
        let pb = p[ts.partition_point(|t| *t <= b) - 1];
        let lp = pb.ln();
        let r = prev.map(|x| lp - x).unwrap_or(0.0);
        let root = if numeraire == 1 { pb.sqrt() } else { 1.0 / pb.sqrt() };
        out.lvr += r * r * root / 4.0;
        out.hours += 1;
        prev = Some(lp);
        b += 3600;
    }
    Some(out)
}

static FLVR: std::sync::Mutex<Option<HashMap<(u64, String), (i64, Value)>>> = std::sync::Mutex::new(None);

/// The pool's trailing-7-day F/LVR from its stored events, recomputed at most hourly.
pub fn pool_flvr(cid: u64, pool: &str, d0: i32, d1: i32, numeraire: i64) -> Value {
    let key = (cid, pool.to_lowercase());
    let now = crate::engine::now() as i64;
    if let Some((t, v)) = FLVR.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        if now - t < 3600 {
            return v.clone();
        }
    }
    let since = now - 7 * 86400;
    let ev = read_jsonl(&pool_dir(cid, &key.1).join("events.jsonl"));
    let v = match fee_vs_lvr(&ev, &load_apr(cid, &key.1), d0, d1, numeraire, since) {
        Some(x) => x.to_json(since),
        None => json!({"ratio": null, "swaps": 0, "since": since, "error": "no swaps stored for the last 7 days"}),
    };
    FLVR.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, (now, v.clone()));
    v
}

/// The last computed F/LVR of a configured pool (for Discover pairs), without recomputing.
pub fn cached_flvr(cid: u64, pool: &str) -> Option<Value> {
    FLVR.lock().unwrap().as_ref()?.get(&(cid, pool.to_lowercase())).map(|x| x.1.clone())
}
