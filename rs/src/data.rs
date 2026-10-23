//! Pool data for the live executor.
//!
//! Pool events (Swap/Mint/Burn) live in the shared event store, history.rs. Kept here per pool,
//! data/raw/<chain>/<pool>/:
//!   wallet_<addr>.jsonl  every Mint/Burn of the wallet in the pool (full history)
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
                // too large for the provider: pieces it accepts (10,000 blocks), then halves
                _ if b - a + 1 > CHUNK => ranges.extend((*a..=*b).step_by(CHUNK as usize).map(|x| (x, (x + CHUNK - 1).min(*b)))),
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
    let mut out = vec![];
    scan_jsonl(p, |e| out.push(e));
    out
}

/// Each row of an events file, a line at a time (rows that do not parse are skipped).
pub fn scan_jsonl(p: &PathBuf, mut f: impl FnMut(Ev)) {
    let Ok(file) = std::fs::File::open(p) else { return };
    for l in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        if let Ok(e) = serde_json::from_str(&l) {
            f(e)
        }
    }
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
    let dir = pool_dir(cid, &pool);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let now = crate::engine::now() as i64;
    match crate::history::import_legacy(cid, &pool) {
        Ok(n) if n > 0 => crate::info!("data", "{}: moved {n} stored events into the shared event store", &pool[..10]),
        Ok(_) => {}
        Err(e) => crate::warn!("data", "{}: old events file not imported ({e}); downloading instead", &pool[..10]),
    }
    if crate::history::coverage(cid, &pool).is_none() {
        crate::info!("data", "{}: no stored events; downloading the last {} days", &pool[..10], bootstrap_days);
    }
    let rep = crate::history::ensure(cid, std::slice::from_ref(&pool), now - bootstrap_days * 86400)?;
    // Wallet history: full scan once; after that the wallet's rows among the newly stored events.
    let wp = wallet_path(cid, &pool, &wallet);
    if !wp.exists() {
        // the processor's copy (one request) plus what the store holds past it; else a full RPC scan
        let w = match crate::history::wallet_events(cid, &pool, &wallet) {
            Some(Ok(mut w)) => {
                let have: HashSet<(i64, i64)> = w.iter().map(|e| (e.block, e.log_index)).collect();
                crate::history::scan_all(cid, &pool, |e| {
                    if e.kind != "swap" && e.owner == wallet && !have.contains(&(e.block, e.log_index)) {
                        w.push(e)
                    }
                });
                w.sort_by_key(|e| (e.block, e.log_index));
                w.dedup_by_key(|e| (e.block, e.log_index));
                w
            }
            r => {
                if let Some(Err(e)) = r {
                    crate::warn!("data", "{}: wallet history from the processor failed ({e}); scanning the chain", &pool[..10]);
                }
                let head = rpc::block_number(cid)? as i64;
                wallet_history(cid, &pool, &wallet, head)?
            }
        };
        crate::info!("data", "{}: wallet history {} mint/burn events", &pool[..10], w.len());
        write_jsonl(&wp, &w).map_err(|e| e.to_string())?;
    } else if rep.rows > 0 {
        let have: HashSet<(i64, i64)> = read_jsonl(&wp).into_iter().map(|e| (e.block, e.log_index)).collect();
        let from_ts = crate::chains::block_ts(cid, rep.from_block) - 3600;
        // a line at a time: the new rows can reach back weeks (a first download), and only the
        // wallet's own are kept
        let mut mine: Vec<Ev> = vec![];
        crate::history::scan(cid, &pool, from_ts, now + 3600, |e| {
            if e.kind != "swap" && e.owner == wallet && e.block >= rep.from_block && !have.contains(&(e.block, e.log_index)) {
                mine.push(e)
            }
        });
        mine.sort_by_key(|e| (e.block, e.log_index));
        mine.dedup_by_key(|e| (e.block, e.log_index));
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
        // the live replay keeps 10 days; a month leaves room
        match apr_history(cid, &pool, now - 30 * 86400) {
            Ok(_) => {}
            Err(e) => crate::warn!("data", "{}: processor history refresh failed: {e}", &pool[..10]),
        }
    }
    Ok(())
}

/// The processor's hourly history for the pool back to `since` (newest first), merged into the copy
/// kept in apr_hourly.json. The API pages newest first, so only what is newer than the copy is
/// read (or, when `since` lies before what the copy covers, down to `since`); a pool's whole life
/// is several pages a second each, and a 30-day backtest needs one.
pub fn apr_history(cid: u64, pool: &str, since: i64) -> Result<Vec<Value>, String> {
    let dir = pool_dir(cid, pool);
    let (path, from_path) = (dir.join("apr_hourly.json"), dir.join("apr_hourly.from"));
    let ts = |r: &Value| crate::prices::fnum(&r["timestamp"]) as i64;
    let have: Vec<Value> = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    // how far back the copy is complete (0: the pool's whole life); an older copy without the note
    // counts from its oldest hour
    let covered = std::fs::read_to_string(&from_path).ok().and_then(|s| s.trim().parse::<i64>().ok()).or_else(|| have.iter().map(ts).min());
    let newest = have.iter().map(ts).max();
    let stop_at = match (newest, covered) {
        (Some(n), Some(c)) if c <= since => n,
        _ => since,
    };
    let mut fresh = vec![];
    let mut offset = 0;
    let mut whole_life = false;
    loop {
        let url = format!("https://solidly.com/api/getPoolAprHistory/?chainId={cid}&poolId={pool}&interval=hour&limit={APR_PAGE}&offset={offset}");
        let h = rpc::get_json(&url)?["history"].as_array().cloned().unwrap_or_default();
        let n = h.len();
        let oldest = h.last().map(ts);
        fresh.extend(h);
        offset += n;
        if n < APR_PAGE {
            whole_life = true;
            break;
        }
        if oldest.map(|o| o <= stop_at).unwrap_or(true) {
            break;
        }
    }
    // newer rows replace older copies of the same hour (the last hour fills in as it completes)
    let mut by_ts: BTreeMap<i64, Value> = have.into_iter().map(|r| (ts(&r), r)).collect();
    for r in fresh {
        by_ts.insert(ts(&r), r);
    }
    let rows: Vec<Value> = by_ts.into_values().rev().collect();
    // the copy now runs without a gap from `since` (or from what it covered, when that is earlier)
    let reach = if whole_life { 0 } else { covered.map(|c| c.min(since)).unwrap_or(since) };
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&path, serde_json::to_string(&rows).unwrap());
    let _ = std::fs::write(&from_path, reach.to_string());
    Ok(rows)
}
const APR_PAGE: usize = 2400;

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

// -- the replay market -------------------------------------------------------------------------------
pub struct Market {
    pub ts: Vec<i64>,
    /// each swap's tick as it traded (execution, fees, valuation); the bars hold the cleaned signal
    pub tick: Vec<i64>,
    /// sqrt price after each swap, raw units (sqrtPriceX96 / 2^96), consistent with `tick`.
    pub sp: Vec<f64>,
    /// each swap's fee (numeraire) at its hour's final rate: what liquidity earned on it
    pub fee: Vec<f64>,
    /// the same at the rate known when it traded (the last completed hour's), for signals: the
    /// hour's rate is final only once the hour is over. Empty: `fee` (synthetic markets)
    pub fee_sig: Vec<f64>,
    pub l_others: Vec<f64>,
    /// the pool's active liquidity at each swap as it happened (the wallet's own included): the
    /// liquidity that earned the swap's fee
    pub l_active: Vec<f64>,
    pub bar_end: Vec<i64>,
    pub bar_idx: Vec<usize>,
    pub bar_tick: Vec<i64>,
    /// highest and lowest swap tick inside each bar (the close when no swap), and its fees
    pub bar_hi: Vec<i64>,
    pub bar_lo: Vec<i64>,
    pub bar_fee: Vec<f64>,
    pub decimals: i32,
    pub px_scale: f64,
    pub liq_scale: f64,
    /// token1 is the numeraire (fees are counted in it)
    pub num1: bool,
    /// bar length in seconds: BAR_S, or 60 for a node strategy that decides every minute
    pub bar_s: i64,
}

impl Market {
    /// Swap k's fee as a decision could know it then (`fee_sig`).
    pub fn sig_fee(&self, k: usize) -> f64 {
        self.fee_sig.get(k).copied().unwrap_or(self.fee[k])
    }
}

/// A week of this market's bars.
pub fn week_bars(m: &Market) -> usize {
    (7 * 86400 / m.bar_s.max(1)) as usize
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

/// Decision bar length of the replay market, and a week of such bars.
pub const BAR_S: i64 = 300;
pub const WEEK_BARS: usize = (7 * 86400 / BAR_S) as usize;

/// pandas rolling(w, min_periods=mp).median() (trailing): each value uses x[..=i] only, NaN until
/// mp values are in the window (never backfilled from later values).
pub fn rolling_median_trailing(x: &[i64], w: usize, mp: usize) -> Vec<f64> {
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
    out
}

/// The signal price: each swap's tick with spikes (a sandwich's front-run, a fat-finger trade)
/// replaced by the trailing median of the last 51 swaps. A spike is a swap farther from that median
/// than 20 ticks and than 6 median absolute deviations of the window, so a pool that is moving
/// keeps its moves; and three such swaps in a row on the same side are a new level, kept as they
/// are. Trailing: a swap's value never depends on later swaps (live reads the same).
/// Before Oct 2026 every swap more than 20 ticks from the median was replaced: right for a stable
/// pool, but on a volatile one it swapped 78% of a year's swaps (AERO/WETH) for a lagging median.
pub fn spike_filter(tick: &[i64]) -> Vec<i64> {
    const W: usize = 51;
    let med = rolling_median_trailing(tick, W, 1);
    let (mut run, mut side) = (0usize, 0i8);
    let mut dev: Vec<f64> = Vec::with_capacity(W);
    (0..tick.len()).map(|k| {
        let m = med[k];
        dev.clear();
        dev.extend(tick[k.saturating_sub(W - 1)..=k].iter().map(|t| (*t as f64 - m).abs()));
        let mad = median(&mut dev);
        let d = tick[k] as f64 - m;
        if d.abs() <= (6.0 * mad).max(20.0) {
            run = 0;
            return tick[k];
        }
        let s = if d > 0.0 { 1 } else { -1 };
        if s == side { run += 1 } else { (run, side) = (1, s) }
        if run >= 3 { tick[k] } else { round_even(m) as i64 }
    }).collect()
}

fn f(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(0.0)
}

/// Build the market from events (already filtered to the window, plus the wallet's full history).
pub fn build_market(events: Vec<Ev>, wallet: &str, apr: &HashMap<i64, (f64, f64)>, d0: i32, d1: i32, numeraire: i64) -> Result<Market, String> {
    build_market_bars(events, wallet, apr, d0, d1, numeraire, BAR_S)
}

/// build_market() on bars of `bar_s` seconds.
pub fn build_market_bars(events: Vec<Ev>, wallet: &str, apr: &HashMap<i64, (f64, f64)>, d0: i32, d1: i32, numeraire: i64, bar_s: i64) -> Result<Market, String> {
    build_market_asof(events, wallet, apr, d0, d1, numeraire, bar_s, None)
}

/// build_market_bars() with the time the events are complete to (`as_of`, the store's coverage):
/// every bar that has closed by then, swaps or none, so live has the bars a replay of the same
/// time has (bars once ended only before the last swap, and live, which has no later swap, lagged
/// behind the replay in a quiet pool). None: through the last swap.
#[allow(clippy::too_many_arguments)]
pub fn build_market_asof(events: Vec<Ev>, wallet: &str, apr: &HashMap<i64, (f64, f64)>, d0: i32, d1: i32, numeraire: i64, bar_s: i64,
                         as_of: Option<i64>) -> Result<Market, String> {
    let mut seen = HashSet::new();
    let mut ev: Vec<Ev> = events.into_iter().filter(|e| seen.insert((e.block, e.log_index))).collect();
    ev.sort_by_key(|e| (e.block, e.log_index));
    let px_scale = 10f64.powi(d0 - d1);
    let liq_scale = 10f64.powf((d0 + d1) as f64 / 2.0);
    let sw: Vec<(usize, &Ev)> = ev.iter().enumerate().filter(|(_, e)| e.kind == "swap").collect();
    if sw.is_empty() {
        return Err("no swaps in the replay window yet".into());
    }
    // every swap at the price it traded at: that is where our ranges were active, earned its fee,
    // and where a trade of ours could fill. The cleaned price (a swap far from the trailing median
    // of 51 replaced by it: spikes such as sandwiches) is for signals only, the bars below, which
    // strategies read; using it to execute and value let a replay trade at a price that was gone
    // (Oct 2026 review)
    // a swap through an empty pool (no liquidity left in range after it) moves the price as far as
    // the next liquidity, up to the tick limit, for any amount: nobody could trade there, so the
    // price stays where the pool last had depth (a 0.008 wS swap once took wS/Anon to tick -887272,
    // valuing a replay at 10^42)
    let (mut tick, mut sp) = (Vec::with_capacity(sw.len()), Vec::with_capacity(sw.len()));
    for (k, (_, e)) in sw.iter().enumerate() {
        if k > 0 && e.liquidity.trim() == "0" {
            tick.push(tick[k - 1]);
            sp.push(sp[k - 1]);
            continue;
        }
        let x = f(&e.sqrt_price_x96) / 2f64.powi(96);
        tick.push(e.tick);
        sp.push(if x > 0.0 && x.is_finite() { x } else { 1.0001f64.powf(e.tick as f64 / 2.0) });
    }
    let clean = spike_filter(&tick);
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
    let mut fee_sig = Vec::with_capacity(sw.len());
    let mut l_others = Vec::with_capacity(sw.len());
    let mut l_active = Vec::with_capacity(sw.len());
    let mut sig_rate = 0.0;
    for (k, (_, e)) in sw.iter().enumerate() {
        let a0 = f(&e.amount0) / 10f64.powi(d0);
        let a1 = f(&e.amount1) / 10f64.powi(d1);
        let p = sp[k] * sp[k] * px_scale;
        let in0 = if a0 > 0.0 { a0 } else { 0.0 };
        let in1 = if a1 > 0.0 { a1 } else { 0.0 };
        let amount_in = if numeraire == 0 { in0 + in1 / p } else { in0 * p + in1 };
        let hour = ts[k].div_euclid(3600) * 3600;
        fee.push(amount_in * apr.get(&hour).map(|x| x.0).unwrap_or(0.0));
        if let Some(r) = apr.get(&(hour - 3600)) {
            sig_rate = r.0;
        }
        fee_sig.push(amount_in * sig_rate);
        l_others.push((f(&e.liquidity) - l_wallet[k]).max(0.0) / liq_scale);
        l_active.push(f(&e.liquidity).max(0.0) / liq_scale);
    }
    let bs = bar_s;
    let t0 = (ts[0].div_euclid(bs) + 1) * bs;
    let last = *ts.last().unwrap();
    let upto = as_of.filter(|a| *a >= last).unwrap_or(last - 1);
    let bar_end: Vec<i64> = (0..).map(|i| t0 + i * bs).take_while(|t| *t <= upto).collect();
    let bar_idx: Vec<usize> = bar_end.iter().map(|b| ts.partition_point(|t| t <= b) - 1).collect();
    // the bars are the signal: cleaned closes, highs and lows
    let bar_tick: Vec<i64> = bar_idx.iter().map(|i| clean[*i]).collect();
    let (bar_hi, bar_lo, bar_fee) = bar_ranges(&clean, &fee_sig, &bar_idx);
    Ok(Market { ts, tick, sp, fee, fee_sig, l_others, l_active, bar_end, bar_idx, bar_tick, bar_hi, bar_lo, bar_fee, decimals: d0, px_scale, liq_scale, num1: numeraire == 1, bar_s })
}

/// Per bar: the high and low swap tick in (previous bar, this bar] (the close when none), and the fees.
pub fn bar_ranges(tick: &[i64], fee: &[f64], bar_idx: &[usize]) -> (Vec<i64>, Vec<i64>, Vec<f64>) {
    let (mut hi, mut lo, mut fs) = (Vec::with_capacity(bar_idx.len()), Vec::with_capacity(bar_idx.len()), Vec::with_capacity(bar_idx.len()));
    let mut prev: Option<usize> = None;
    for &i in bar_idx {
        let from = prev.map(|p| p + 1).unwrap_or(i);
        let (mut h, mut l, mut f) = (tick[i], tick[i], 0.0);
        if from <= i {
            for k in from..=i {
                h = h.max(tick[k]);
                l = l.min(tick[k]);
                f += fee[k];
            }
        }
        hi.push(h);
        lo.push(l);
        fs.push(f);
        prev = Some(i);
    }
    (hi, lo, fs)
}

/// live_market(): the last `days` of swaps plus the wallet's full Mint/Burn history.
pub fn live_market(cid: u64, pool: &str, wallet: &str, d0: i32, d1: i32, numeraire: i64, days: i64) -> Result<Market, String> {
    live_market_bars(cid, pool, wallet, d0, d1, numeraire, days, BAR_S)
}

/// live_market() on bars of `bar_s` seconds (60 for a strategy that decides every minute).
#[allow(clippy::too_many_arguments)]
pub fn live_market_bars(cid: u64, pool: &str, wallet: &str, d0: i32, d1: i32, numeraire: i64, days: i64, bar_s: i64) -> Result<Market, String> {
    let now = crate::engine::now() as i64;
    let since = now - days * 86400;
    // swaps of the replay window; mints and burns as far back as the store goes (up to 31 days)
    // (filtered as read: the older swaps are most of the 31 days)
    let mut ev: Vec<Ev> = vec![];
    crate::history::scan(cid, pool, now - 31 * 86400, now + 3600, |e| {
        if e.ts >= since || e.kind != "swap" {
            ev.push(e)
        }
    });
    ev.sort_by_key(|e| (e.block, e.log_index));
    ev.dedup_by_key(|e| (e.block, e.log_index));
    ev.extend(read_jsonl(&wallet_path(cid, pool, wallet)));
    if ev.is_empty() {
        return Err(format!("no event data for pool {pool}; run the data bootstrap first"));
    }
    let as_of = crate::history::coverage(cid, pool).map(|c| c.to_ts.min(now));
    build_market_asof(ev, &wallet.to_lowercase(), &load_apr(cid, pool), d0, d1, numeraire, bar_s, as_of)
}

/// (tl, tu) of the wallet's ranges from its Mint/Burn history.
pub fn history_ranges(cid: u64, pool: &str, wallet: &str) -> HashSet<(i64, i64)> {
    read_jsonl(&wallet_path(cid, pool, wallet)).into_iter().map(|e| (e.tick_lower, e.tick_upper)).collect()
}

// -- fee vs loss to arbitrage (reports/VOLATILE.md) ---------------------------------------------------------
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
    let ev = crate::history::read(cid, &key.1, since, now + 3600);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spike_filter_drops_spikes_and_keeps_moves() {
        // calm around 0, a two-swap sandwich spike of 300, then calm again: the spike goes
        let mut x: Vec<i64> = (0..200).map(|k| (k % 5) - 2).collect();
        x[100] = 300;
        x[101] = 298;
        let c = spike_filter(&x);
        assert!(c[100].abs() <= 2 && c[101].abs() <= 2, "{} {}", c[100], c[101]);
        assert_eq!(&c[..100], &x[..100]);
        // a steady trend of 10 ticks a swap: every swap is kept
        let tr: Vec<i64> = (0..400).map(|k| k * 10).collect();
        assert_eq!(spike_filter(&tr), tr);
        // a jump to a new level that holds: kept from its third swap on
        let lv: Vec<i64> = (0..200).map(|k| if k < 100 { (k % 3) - 1 } else { 500 + (k % 3) - 1 }).collect();
        let c = spike_filter(&lv);
        assert!(c[100].abs() <= 1 && c[101].abs() <= 1);
        assert!(c[102..].iter().all(|t| *t >= 498), "{:?}", &c[100..106]);
    }

    #[test]
    fn a_swap_through_an_empty_pool_keeps_the_last_price() {
        let sq = |t: i64| format!("{:.0}", 1.0001f64.powf(t as f64 / 2.0) * 2f64.powi(96));
        let swap = |i: i64, tick: i64, liq: &str| Ev {
            block: i, ts: 1_000_000 + i * 600, kind: "swap".into(), amount0: "1000".into(), amount1: "-1000".into(),
            sqrt_price_x96: sq(tick), liquidity: liq.into(), tick, ..Default::default()
        };
        // the pool emptied: a dust swap goes to the tick limit, then liquidity comes back near where it was
        let ev = vec![swap(1, -26800, "1000000"), swap(2, -26900, "1000000"), swap(3, -887272, "0"), swap(4, -26950, "900000")];
        let m = build_market(ev, "", &HashMap::new(), 18, 18, 0).unwrap();
        assert_eq!(m.tick, vec![-26800, -26900, -26900, -26950]);
        assert_eq!(m.sp[2], m.sp[1]);
        // a pool's very first swap has no earlier price: it stands as it is
        let m = build_market(vec![swap(1, -100, "0"), swap(2, -110, "5")], "", &HashMap::new(), 18, 18, 0).unwrap();
        assert_eq!(m.tick, vec![-100, -110]);
    }

    #[test]
    fn trailing_median_never_looks_ahead() {
        let a: Vec<i64> = (0..300).map(|i| (i * 7 % 13) - 6).collect();
        let mut b = a.clone();
        for x in &mut b[150..] {
            *x += 500;
        }
        let (ma, mb) = (rolling_median_trailing(&a, 51, 1), rolling_median_trailing(&b, 51, 1));
        assert_eq!(ma[..150], mb[..150]);
        assert!(rolling_median_trailing(&a, WEEK_BARS, 12)[..11].iter().all(|x| x.is_nan()));
    }
}
