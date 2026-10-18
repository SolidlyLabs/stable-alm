//! One local store of pool events for everything that reads them: the live replay, the profit
//! ledger's pool prices, Fee/arb for every pool of a chain, and backtests. Downloaded once,
//! validated and normalized on the way in, then only topped up.
//!
//!   data/history/<chain>/<pool>/<YYYY-MM-DD>.jsonl   Swap/Mint/Burn rows of that UTC day,
//!                                                    sorted by (block, log index), no duplicates
//!   data/history/<chain>/<pool>/coverage.json        {from_block, to_block, from_ts, to_ts, source}
//!
//! Coverage is one contiguous block range per pool: every event of the pool inside it is stored, so
//! a reader never sees a silent gap. Rows follow the Swap/Mint/Burn events exactly (data::Ev): swap
//! amounts signed from the pool's side, mint/burn amounts and liquidity as positive sizes.
//!
//! Sources, in order:
//! 1. The Solidly data processor's public history (solidly.com/api/history, compressed in flight):
//!    one page set per chain and UTC day holds every pool's events, so all pools of a chain fill
//!    from the same download, and a top-up asks only for rows after the last one stored. Rows are
//!    used up to the block the processor has fully synced. Once per chain and run a sample range
//!    is compared with the chain's own logs; on a mismatch the processor is not used for that chain.
//! 2. RPC getLogs for the few minutes the processor has not synced yet (one request per 10,000
//!    blocks for up to 100 pools), and for everything when the processor is unreachable, except
//!    for large chain-wide requests (all pools of a chain), which then wait for the processor.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::data::{self, Ev};

const CHUNK: i64 = 10_000;
const ADDRS_PER_REQUEST: usize = 100;
const DAY: i64 = 86_400;
/// globIndex = block * 10^10 + log index, as the processor numbers events.
const GLOB: i128 = 10_000_000_000;
/// Blocks below the processor's synced position taken from a day still in progress.
const SYNC_MARGIN_BLOCKS: i64 = 64;
/// Requests for this many pools or more never fall back to a full RPC download.
const RPC_BACKFILL_MAX_POOLS: usize = 10;
const DEFAULT_HISTORY_API: &str = "https://solidly.com/api/history";

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Coverage {
    pub from_block: i64,
    pub to_block: i64,
    pub from_ts: i64,
    pub to_ts: i64,
    #[serde(default)]
    pub source: String,
}

/// One writer per chain at a time (the worker and a page request may both top up).
static LOCKS: Mutex<Option<HashMap<u64, std::sync::Arc<Mutex<()>>>>> = Mutex::new(None);

fn chain_lock(cid: u64) -> std::sync::Arc<Mutex<()>> {
    LOCKS.lock().unwrap().get_or_insert_with(HashMap::new).entry(cid).or_default().clone()
}

pub fn pool_dir(cid: u64, pool: &str) -> PathBuf {
    crate::paths::data().join("history").join(cid.to_string()).join(pool.to_lowercase())
}

pub fn coverage(cid: u64, pool: &str) -> Option<Coverage> {
    serde_json::from_str(&std::fs::read_to_string(pool_dir(cid, pool).join("coverage.json")).ok()?).ok()
}

fn save_coverage(cid: u64, pool: &str, c: &Coverage) -> Result<(), String> {
    let p = pool_dir(cid, pool).join("coverage.json");
    crate::settings::write_atomic(&p, &serde_json::to_value(c).unwrap()).map_err(|e| e.to_string())
}

pub fn day_name(ts: i64) -> String {
    let days = ts.div_euclid(DAY);
    // civil date from days since 1970-01-01 (Howard Hinnant's algorithm)
    let z = days + 719_468;
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

// -- validation --------------------------------------------------------------------------------------

fn is_addr(s: &str) -> bool {
    s.len() == 42 && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn uint(s: &str) -> Option<num_bigint::BigUint> {
    s.parse().ok()
}

/// A row the chain could have produced: the tick matches the price (exactly, as the pool computes
/// it), ranges are ordered, sizes positive. Anything else means a decoding or source error, and the
/// whole batch is refused rather than stored.
pub fn validate(e: &Ev) -> Result<(), String> {
    let at = || format!("block {} log {}", e.block, e.log_index);
    if e.block <= 0 || e.log_index < 0 || e.ts <= 0 {
        return Err(format!("{}: missing block, log index or time", at()));
    }
    match e.kind.as_str() {
        "swap" => {
            let sp = uint(&e.sqrt_price_x96).ok_or_else(|| format!("{}: bad sqrtPriceX96", at()))?;
            uint(&e.liquidity).ok_or_else(|| format!("{}: bad liquidity", at()))?;
            if !(-887_272..=887_272).contains(&e.tick) {
                return Err(format!("{}: tick {} out of range", at(), e.tick));
            }
            let lo = crate::tickmath::sqrt_ratio_at_tick(e.tick);
            let hi = crate::tickmath::sqrt_ratio_at_tick(e.tick + 1);
            // A swap that stops exactly on a tick boundary while the price falls leaves the pool at
            // the tick below with the price on the boundary: sqrtPrice == ratio(tick + 1).
            if sp < lo || sp > hi {
                return Err(format!("{}: tick {} does not match sqrtPriceX96", at(), e.tick));
            }
            if e.amount0.parse::<num_bigint::BigInt>().is_err() || e.amount1.parse::<num_bigint::BigInt>().is_err() {
                return Err(format!("{}: bad swap amounts", at()));
            }
        }
        "mint" | "burn" => {
            if e.tick_lower >= e.tick_upper || e.tick_lower < -887_272 || e.tick_upper > 887_272 {
                return Err(format!("{}: bad range [{}, {})", at(), e.tick_lower, e.tick_upper));
            }
            if !is_addr(&e.owner) {
                return Err(format!("{}: bad owner", at()));
            }
            for a in [&e.amount, &e.amount0, &e.amount1] {
                uint(a).ok_or_else(|| format!("{}: bad {} size", at(), e.kind))?;
            }
        }
        k => return Err(format!("{}: unknown event kind {k}", at())),
    }
    Ok(())
}

// -- files ----------------------------------------------------------------------------------------------

fn read_day(p: &PathBuf) -> Vec<Ev> {
    data::read_jsonl(p)
}

/// Merge rows into a day file: sorted by (block, log index), one row per event.
fn merge_day(p: &PathBuf, rows: Vec<Ev>) -> Result<(), String> {
    let mut all: BTreeMap<(i64, i64), Ev> = read_day(p).into_iter().map(|e| ((e.block, e.log_index), e)).collect();
    for e in rows {
        all.insert((e.block, e.log_index), e);
    }
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    for e in all.values() {
        writeln!(f, "{}", serde_json::to_string(e).unwrap()).map_err(|e| e.to_string())?;
    }
    f.sync_all().map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, p).map_err(|e| e.to_string())
}

fn store_rows(cid: u64, pool: &str, rows: Vec<Ev>) -> Result<(), String> {
    let mut by_day: BTreeMap<String, Vec<Ev>> = BTreeMap::new();
    for e in rows {
        by_day.entry(day_name(e.ts)).or_default().push(e);
    }
    let dir = pool_dir(cid, pool);
    for (day, rows) in by_day {
        merge_day(&dir.join(format!("{day}.jsonl")), rows)?;
    }
    Ok(())
}

/// The pool's stored events with from_ts <= ts < to_ts, in chain order.
pub fn read(cid: u64, pool: &str, from_ts: i64, to_ts: i64) -> Vec<Ev> {
    let dir = pool_dir(cid, pool);
    let mut out = vec![];
    let mut d = from_ts.div_euclid(DAY) * DAY;
    while d < to_ts {
        out.extend(read_day(&dir.join(format!("{}.jsonl", day_name(d)))).into_iter().filter(|e| e.ts >= from_ts && e.ts < to_ts));
        d += DAY;
    }
    // one row per event even where two sources filed it under neighbouring days (an RPC row's time
    // is the block time estimate, the processor's the real one)
    out.sort_by_key(|e| (e.block, e.log_index));
    out.dedup_by_key(|e| (e.block, e.log_index));
    out
}

/// Everything stored for the pool.
pub fn read_all(cid: u64, pool: &str) -> Vec<Ev> {
    match coverage(cid, pool) {
        Some(c) => read(cid, pool, c.from_ts - DAY, c.to_ts + DAY),
        None => vec![],
    }
}

// -- download -------------------------------------------------------------------------------------------

/// Swap/Mint/Burn logs of `addrs` in [a, b]; a range the RPC refuses for size is split in two.
fn fetch(cid: u64, addrs: &[String], a: i64, b: i64, out: &mut Vec<(String, Ev)>) -> Result<(), String> {
    let p = json!([{"address": addrs, "fromBlock": format!("0x{a:x}"), "toBlock": format!("0x{b:x}"),
                    "topics": [[data::SWAP, data::MINT, data::BURN]]}]);
    match crate::rpc::call(cid, "eth_getLogs", p)? {
        Ok(Value::Array(logs)) => {
            for l in &logs {
                let pool = l["address"].as_str().unwrap_or("").to_lowercase();
                out.push((pool, data::decode(cid, l)));
            }
            Ok(())
        }
        Err(e) if crate::rpc::is_rate_limit(&e) => Err(format!("getLogs rate limited for {a}-{b}: {e}")),
        r if b - a < 10 => Err(format!("getLogs failed for {a}-{b}: {r:?}")),
        _ => {
            let m = (a + b) / 2;
            fetch(cid, addrs, a, m, out)?;
            fetch(cid, addrs, m + 1, b, out)
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub requests: usize,
    pub rows: usize,
    pub from_block: i64,
    pub to_block: i64,
}

fn norm_pools(pools: &[String]) -> Vec<String> {
    let mut v: Vec<String> = pools.iter().map(|p| p.to_lowercase()).filter(|p| is_addr(p)).collect();
    v.sort();
    v.dedup();
    v
}

/// Make sure every pool in `pools` has its events from `from_ts` up to the chain head: full days
/// from the processor, the last minutes from RPC (see the module notes).
pub fn ensure(cid: u64, pools: &[String], from_ts: i64) -> Result<Report, String> {
    ensure_until(cid, pools, from_ts, None)
}

/// The store already holds the pool's events from `from_ts` to `to_ts`.
pub fn covers(cid: u64, pool: &str, from_ts: i64, to_ts: i64) -> bool {
    coverage(cid, pool).map(|c| c.from_ts <= from_ts && c.to_ts >= to_ts).unwrap_or(false)
}

/// Like ensure, for readers that need events only up to `to_ts`: pools the store already covers
/// are left alone, and the RPC top-up past the processor runs only for pools still short of `to_ts`
/// after the processor download (None: up to the chain head, as ensure).
pub fn ensure_until(cid: u64, pools: &[String], from_ts: i64, to_ts: Option<i64>) -> Result<Report, String> {
    let pools: Vec<String> = match to_ts {
        Some(t) => pools.iter().filter(|p| !covers(cid, p, from_ts, t)).cloned().collect(),
        None => pools.to_vec(),
    };
    if pools.is_empty() {
        return Ok(Report::default());
    }
    let lock = chain_lock(cid);
    let _g = lock.lock().unwrap();
    let pools = norm_pools(&pools);
    let mut rep = Report::default();
    let mut processor_ok = false;
    if processor_enabled(cid) {
        match ensure_processor(cid, &pools, from_ts) {
            Ok(r) => {
                processor_ok = true;
                rep = r;
                // the processor is checked against the chain at most every few hours per chain: the
                // check reads blocks over RPC, seconds on a slow chain, on every download otherwise
                static CHECKED: Mutex<Option<HashMap<u64, std::time::Instant>>> = Mutex::new(None);
                let due = CHECKED.lock().unwrap().get_or_insert_with(HashMap::new).get(&cid).map(|t| t.elapsed().as_secs() > 6 * 3600).unwrap_or(true);
                if rep.rows > 0 && due {
                    CHECKED.lock().unwrap().as_mut().unwrap().insert(cid, std::time::Instant::now());
                    if let Err(e) = spot_check(cid, &pools) {
                        crate::warn!("history", "chain {cid}: processor history does not match the chain ({e}); using RPC on this chain");
                        DISABLED.lock().unwrap().get_or_insert_with(Default::default).insert(cid);
                        processor_ok = false;
                    }
                }
            }
            Err(e) => crate::warn!("history", "chain {cid}: processor history unavailable ({e})"),
        }
    }
    let pools: Vec<String> = match to_ts {
        Some(t) => pools.into_iter().filter(|p| !covers(cid, p, from_ts, t)).collect(),
        None => pools,
    };
    let rpc_pools: Vec<String> = if !processor_ok && pools.len() >= RPC_BACKFILL_MAX_POOLS {
        // a chain-wide fill over public RPC would take hours and starve live trading: only top up
        // pools whose coverage already reaches back far enough
        pools.iter().filter(|p| coverage(cid, p).map(|c| c.from_ts <= from_ts + 6 * 3600).unwrap_or(false)).cloned().collect()
    } else {
        pools
    };
    let r = ensure_rpc(cid, &rpc_pools, from_ts)?;
    let from_block = match (rep.rows > 0, r.rows > 0) {
        (true, true) => rep.from_block.min(r.from_block),
        (true, false) => rep.from_block,
        _ => r.from_block,
    };
    Ok(Report { requests: rep.requests + r.requests, rows: rep.rows + r.rows, from_block, to_block: r.to_block })
}

/// The RPC download: every pool from `from_ts` (or right after its coverage) to the chain head.
fn ensure_rpc(cid: u64, pools: &[String], from_ts: i64) -> Result<Report, String> {
    let head = crate::rpc::block_number(cid)? as i64;
    // a small margin before from_ts: block times are estimates
    let target = (crate::chains::block_at_ts(cid, from_ts) - crate::chains::blocks_per_day(cid) / 48).max(1);
    let pools = norm_pools(pools);
    let mut cov: HashMap<String, Option<Coverage>> = pools.iter().map(|p| (p.clone(), coverage(cid, p))).collect();
    let start_of = |c: &Option<Coverage>| rpc_start(c.as_ref(), target, from_ts);
    let need: Vec<String> = pools.iter().filter(|p| start_of(&cov[*p]) <= head).cloned().collect();
    let mut rep = Report { from_block: head + 1, to_block: head, ..Default::default() };
    if need.is_empty() {
        return Ok(rep);
    }
    let x = need.iter().map(|p| start_of(&cov[p])).min().unwrap();
    rep.from_block = x;
    let mut a = x;
    while a <= head {
        let b = (a + CHUNK - 1).min(head);
        let mut got: Vec<(String, Ev)> = vec![];
        for group in need.chunks(ADDRS_PER_REQUEST) {
            fetch(cid, group, a, b, &mut got)?;
            rep.requests += 1;
        }
        for (_, e) in &got {
            validate(e)?;
        }
        rep.rows += got.len();
        let mut by_pool: HashMap<String, Vec<Ev>> = HashMap::new();
        for (p, e) in got {
            by_pool.entry(p).or_default().push(e);
        }
        let tb = crate::chains::block_ts(cid, b);
        for p in &need {
            store_rows(cid, p, by_pool.remove(p).unwrap_or_default())?;
            // coverage grows only while it stays one contiguous range
            let c = cov.get_mut(p).unwrap();
            let next = match c.take() {
                None => Coverage { from_block: x, to_block: b, from_ts: crate::chains::block_ts(cid, x), to_ts: tb, source: "rpc".into() },
                Some(c) if b + 1 >= c.from_block && x <= c.to_block + 1 => Coverage {
                    from_block: c.from_block.min(x),
                    to_block: c.to_block.max(b),
                    from_ts: c.from_ts.min(crate::chains::block_ts(cid, x)),
                    to_ts: c.to_ts.max(tb),
                    source: c.source,
                },
                Some(c) => c, // backfill not joined up yet
            };
            save_coverage(cid, p, &next)?;
            *c = Some(next);
        }
        a = b + 1;
    }
    rep.to_block = head;
    Ok(rep)
}

/// Where the RPC download of a pool starts: right after its coverage when that reaches back to
/// `from_ts`, else at `target` (the block of `from_ts`). Coverage reaches back when its first block
/// or its first time does: a processor backfill over days without a single event of the pool (before
/// it was deployed) moves the first time back but has no event to move the first block to.
fn rpc_start(c: Option<&Coverage>, target: i64, from_ts: i64) -> i64 {
    match c {
        Some(c) if c.from_block <= target || c.from_ts <= from_ts => c.to_block + 1,
        _ => target,
    }
}

// -- the processor ------------------------------------------------------------------------------------

static DISABLED: Mutex<Option<HashSet<u64>>> = Mutex::new(None);
/// What a running processor download is doing, per chain (shown by the backtest's status).
static PROGRESS: Mutex<Option<HashMap<u64, String>>> = Mutex::new(None);

pub fn progress(cid: u64) -> Option<String> {
    PROGRESS.lock().unwrap().get_or_insert_with(HashMap::new).get(&cid).cloned()
}

fn set_progress(cid: u64, s: Option<String>) {
    let mut g = PROGRESS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    match s {
        Some(s) => m.insert(cid, s),
        None => m.remove(&cid),
    };
}
static CHECKED: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

pub fn blockchain_name(cid: u64) -> Option<&'static str> {
    Some(match cid {
        1 => "ethereum",
        10 => "optimism",
        146 => "sonic",
        250 => "fantom",
        8453 => "base",
        42161 => "arbitrum",
        _ => return None,
    })
}

/// The history API; ALM_HISTORY_API overrides it ("off" for RPC only).
fn api_base() -> String {
    std::env::var("ALM_HISTORY_API").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| DEFAULT_HISTORY_API.into())
}

fn processor_enabled(cid: u64) -> bool {
    blockchain_name(cid).is_some() && api_base() != "off" && !DISABLED.lock().unwrap().get_or_insert_with(Default::default).contains(&cid)
}

/// GET a history page (gzip in flight); a 429/503 waits as long as the server asks, up to a minute.
fn api_get(path: &str) -> Result<Value, String> {
    // the site serves its routes with a trailing slash (anything else is a redirect)
    let (route, query) = path.split_once('?').map(|(r, q)| (r, format!("?{q}"))).unwrap_or((path, String::new()));
    let url = format!("{}/{}/{query}", api_base().trim_end_matches('/'), route.trim_end_matches('/'));
    for _ in 0..4 {
        match crate::rpc::agent().get(&url).set("Accept", "application/json").call() {
            Ok(r) => return r.into_json().map_err(|e| format!("{path}: {e}")),
            Err(ureq::Error::Status(c @ (429 | 503), r)) => {
                let wait = r.header("Retry-After").and_then(|s| s.parse::<u64>().ok()).unwrap_or(10).clamp(1, 60);
                crate::info!("history", "processor history busy (HTTP {c}); retrying in {wait}s");
                std::thread::sleep(std::time::Duration::from_secs(wait));
            }
            Err(ureq::Error::Status(c, r)) => {
                return Err(format!("{path}: HTTP {c}: {}", r.into_string().unwrap_or_default().chars().take(200).collect::<String>()))
            }
            Err(e) => return Err(format!("{path}: {e}")),
        }
    }
    Err(format!("{path}: still busy after retries"))
}

#[derive(Debug, Clone, Copy)]
struct Head {
    synced_ts: i64,
    /// Every event up to this block is in the processor.
    synced_block: i64,
}

fn head(chain: &str) -> Result<Head, String> {
    let v = api_get(&format!("{chain}/head"))?;
    let g: i128 = v["syncedGlobIndex"].as_str().and_then(|s| s.parse().ok()).ok_or("head without syncedGlobIndex")?;
    let synced_ts = v["syncedTs"].as_i64().ok_or("head without syncedTs")?;
    // the synced block itself may still be partly processed
    Ok(Head { synced_ts, synced_block: (g / GLOB) as i64 - 1 })
}

fn abs(s: &str) -> String {
    s.trim_start_matches('-').to_string()
}

/// One processor row (columns as the page names them) as (pool, event).
fn row_to_ev(cols: &HashMap<String, usize>, r: &Value, pool: Option<&str>) -> Result<(String, Ev), String> {
    let get = |k: &str| cols.get(k).map(|&i| &r[i]).unwrap_or(&Value::Null);
    let s = |k: &str| get(k).as_str().map(String::from).unwrap_or_default();
    let g: i128 = s("g").parse().map_err(|_| "row without globIndex".to_string())?;
    let (block, log_index) = ((g / GLOB) as i64, (g % GLOB) as i64);
    let kind = match s("k").as_str() {
        "S" => "swap",
        "M" => "mint",
        "B" => "burn",
        k => return Err(format!("block {block} log {log_index}: unknown event kind {k}")),
    };
    let mut e = Ev { block, log_index, ts: get("ts").as_i64().unwrap_or(0), kind: kind.into(), tick: get("tick").as_i64().unwrap_or(0), ..Default::default() };
    if kind == "swap" {
        e.amount0 = s("a0");
        e.amount1 = s("a1");
        e.sqrt_price_x96 = s("sqrtP");
        e.liquidity = s("liq");
    } else {
        // the processor signs a burn's amounts negative; the store keeps sizes, as the event does
        e.amount0 = abs(&s("a0"));
        e.amount1 = abs(&s("a1"));
        e.amount = abs(&s("dl"));
        e.owner = s("owner").to_lowercase();
        e.tick_lower = get("tl").as_i64().unwrap_or(0);
        e.tick_upper = get("tu").as_i64().unwrap_or(0);
        e.tick = 0;
    }
    Ok((pool.map(String::from).unwrap_or_else(|| s("pool").to_lowercase()), e))
}

/// Up to this many pools are read pool by pool (the processor's per-pool query is indexed and
/// small); more are read chain-wide, every pool of the chain in one stream. The lab picking one
/// pool on a busy chain would otherwise download every other pool's swaps too, and a chain-wide
/// month can run past the processor's per-query time limit (it answers HTTP 500).
const POOL_ROUTE_MAX: usize = 4;

/// Which pools a download serves: `Some` reads them one by one, `None` the whole chain.
fn only(pools: &[String]) -> Option<&[String]> {
    (pools.len() <= POOL_ROUTE_MAX).then_some(pools)
}

/// The pools' rows (or every pool's) of a chain's day after `after_block`, and the day's status.
fn chain_day(chain: &str, only: Option<&[String]>, day: &str, after_block: i64, rep: &mut Report) -> Result<(Vec<(String, Ev)>, String), String> {
    chain_days(chain, only, day, 1, after_block, rep)
}

/// Days per request when the processor takes ranges (?days=N, up to 31). A range is still paged
/// by row count, so a quiet stretch of days costs one request instead of one per day: the
/// processor limits uncached requests per client, which is what paces a long download.
const RANGE_DAYS: i64 = 31;
/// Range requests in flight at once when reading pool by pool.
const PARALLEL_RANGES: usize = 3;
/// 0: not tried yet, 1: ranges work, 2: the server refused them (single days from then on).
static RANGES: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// `n` days of every pool's rows from `day` in one request where the server takes ranges; None when
/// it does not (the caller reads single days).
fn chain_range(chain: &str, only: Option<&[String]>, day: &str, n: i64, after_block: i64, rep: &mut Report) -> Result<Option<(Vec<(String, Ev)>, String)>, String> {
    use std::sync::atomic::Ordering;
    if n <= 1 || RANGES.load(Ordering::Relaxed) == 2 {
        return Ok(None);
    }
    match chain_days(chain, only, day, n, after_block, rep) {
        Ok(r) => {
            RANGES.store(1, Ordering::Relaxed);
            Ok(Some(r))
        }
        Err(e) if RANGES.load(Ordering::Relaxed) == 0 && (e.contains("unknown query parameter") || e.contains("days must")) => {
            crate::info!("history", "the history server reads one day per request");
            RANGES.store(2, Ordering::Relaxed);
            Ok(None)
        }
        // a range too heavy for the processor's query time limit: this stretch goes day by day
        Err(e) if e.contains("HTTP 500") || e.contains("HTTP 504") => {
            crate::info!("history", "{chain}: {n} days from {day} in one request was refused; reading them day by day");
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn chain_days(chain: &str, only: Option<&[String]>, day: &str, n: i64, after_block: i64, rep: &mut Report) -> Result<(Vec<(String, Ev)>, String), String> {
    let Some(pools) = only else { return stream(chain, None, day, n, after_block, rep) };
    let mut out = vec![];
    let mut status = String::new();
    for p in pools {
        let (rows, st) = stream(chain, Some(p), day, n, after_block, rep)?;
        out.extend(rows);
        status = st;
    }
    out.sort_by_key(|(_, e)| (e.block, e.log_index));
    Ok((out, status))
}

/// One pool's (or the chain's) pages of `n` days from `day`.
fn stream(chain: &str, pool: Option<&str>, day: &str, n: i64, after_block: i64, rep: &mut Report) -> Result<(Vec<(String, Ev)>, String), String> {
    let mut after: i128 = if after_block > 0 { (after_block as i128 + 1) * GLOB - 1 } else { 0 };
    let mut out = vec![];
    let mut status;
    let route = match pool {
        Some(p) => format!("{chain}/pools/{p}/events/{day}"),
        None => format!("{chain}/events/{day}"),
    };
    loop {
        let mut q = vec![];
        if after > 0 {
            q.push(format!("after={after}"));
        }
        if n > 1 {
            q.push(format!("days={n}"));
        }
        let q = if q.is_empty() { String::new() } else { format!("?{}", q.join("&")) };
        let v = match api_get(&format!("{route}{q}")) {
            // a pool the processor does not list (not a Solidly pool, or too new) has no rows
            Err(e) if pool.is_some() && e.contains("HTTP 404") => return Ok((vec![], "final".into())),
            r => r?,
        };
        if n > 1 && v["days"].as_i64().unwrap_or(1) != n {
            return Err(format!("{day}: asked for {n} days, the server answered {}", v["days"]));
        }
        rep.requests += 1;
        status = v["status"].as_str().unwrap_or("").to_string();
        let cols: HashMap<String, usize> =
            v["columns"].as_array().ok_or("page without columns")?.iter().enumerate().filter_map(|(i, c)| c.as_str().map(|c| (c.to_string(), i))).collect();
        for r in v["rows"].as_array().ok_or("page without rows")? {
            out.push(row_to_ev(&cols, r, pool)?);
        }
        match v["next"].as_str().and_then(|s| s.parse::<i128>().ok()) {
            Some(n) if n > after => after = n,
            _ => break,
        }
    }
    Ok((out, status))
}

/// Full days from the processor for every pool, up to its synced block less a margin.
fn ensure_processor(cid: u64, pools: &[String], from_ts: i64) -> Result<Report, String> {
    let r = ensure_processor_days(cid, pools, from_ts);
    set_progress(cid, None);
    r
}

fn ensure_processor_days(cid: u64, pools: &[String], from_ts: i64) -> Result<Report, String> {
    let chain = blockchain_name(cid).ok_or("chain not in the processor")?;
    let h = head(chain)?;
    let cap = h.synced_block - SYNC_MARGIN_BLOCKS;
    let mut rep = Report { from_block: i64::MAX, ..Default::default() };
    // per pool: its coverage (kept when it reaches back to from_ts), the first day to read, and the
    // last block already stored
    let mut cov: HashMap<String, Option<Coverage>> = HashMap::new();
    let mut start: HashMap<String, (i64, i64)> = HashMap::new();
    // coverage that starts too late: download only the days before it, then join the two
    let late: Vec<(String, Coverage)> =
        pools.iter().filter_map(|p| coverage(cid, p).filter(|c| c.from_ts > from_ts).map(|c| (p.clone(), c))).collect();
    if !late.is_empty() {
        let first = from_ts.div_euclid(DAY) * DAY;
        let last = late.iter().map(|(_, c)| c.from_ts.div_euclid(DAY) * DAY).max().unwrap();
        let mut d = first;
        let mut day_min: HashMap<i64, i64> = HashMap::new();
        let mut got: HashMap<String, Vec<Ev>> = HashMap::new();
        let mut complete = true;
        while d <= last {
            set_progress(cid, Some(format!("earlier history: {} ({} of {} days)", day_name(d), (d - first) / DAY + 1, (last - first) / DAY + 1)));
            let n = ((last - d) / DAY + 1).min(RANGE_DAYS);
            let lp: Vec<String> = late.iter().map(|(p, _)| p.clone()).collect();
            let (rows, status, step) = match chain_range(chain, only(&lp), &day_name(d), n, 0, &mut rep)? {
                Some((r, st)) => (r, st, n),
                None => {
                    let (r, st) = chain_day(chain, only(&lp), &day_name(d), 0, &mut rep)?;
                    (r, st, 1)
                }
            };
            if !(status == "final" || status == "complete") {
                complete = false;
                break;
            }
            for (_, e) in &rows {
                let day = e.ts.div_euclid(DAY) * DAY;
                let b = day_min.entry(day).or_insert(e.block);
                *b = (*b).min(e.block);
            }
            for (p, e) in rows {
                if let Some((_, c)) = late.iter().find(|(lp, _)| *lp == p) {
                    if e.ts < c.from_ts.div_euclid(DAY) * DAY + DAY {
                        validate(&e)?;
                        got.entry(p).or_default().push(e);
                    }
                }
            }
            d += step * DAY;
        }
        if complete {
            for (p, c) in &late {
                let rows = got.remove(p).unwrap_or_default();
                rep.rows += rows.len();
                store_rows(cid, p, rows)?;
                // the downloaded days run through the day where the old coverage began
                let fb = (first..=c.from_ts.div_euclid(DAY) * DAY).step_by(DAY as usize).find_map(|d| day_min.get(&d).copied()).unwrap_or_else(|| crate::chains::block_at_ts(cid, first));
                rep.from_block = rep.from_block.min(fb);
                save_coverage(cid, p, &Coverage { from_block: fb.min(c.from_block), from_ts: first, ..c.clone() })?;
            }
        }
    }
    for p in pools {
        let c = coverage(cid, p).filter(|c| c.from_ts <= from_ts);
        let s = match &c {
            Some(c) if c.to_block >= cap => continue, // nothing new the processor has
            // an hour back: an RPC coverage end carries an estimated time, and the block cursor
            // keeps rows already stored from being counted twice
            Some(c) => ((c.to_ts - 3600).div_euclid(DAY) * DAY, c.to_block),
            None => (from_ts.div_euclid(DAY) * DAY, 0),
        };
        start.insert(p.clone(), s);
        cov.insert(p.clone(), c);
    }
    let Some(first) = start.values().map(|s| s.0).min() else {
        // nothing to top up (a backfill may have run)
        if rep.from_block == i64::MAX {
            rep.from_block = 0;
        }
        return Ok(rep);
    };
    let mut d = first;
    let days_total = (h.synced_ts - first) / DAY + 1;
    // days fetched ahead as one range (only days the processor has synced past), with each range's status
    let mut ahead: HashMap<i64, Vec<(String, Ev)>> = HashMap::new();
    let mut ahead_st: HashMap<i64, String> = HashMap::new();
    let put = |ahead: &mut HashMap<i64, Vec<(String, Ev)>>, ahead_st: &mut HashMap<i64, String>, d: i64, n: i64, rows: Vec<(String, Ev)>, st: String| {
        for k in 0..n {
            ahead.insert(d + k * DAY, vec![]);
            ahead_st.insert(d + k * DAY, st.clone());
        }
        for (p, e) in rows {
            let day = e.ts.div_euclid(DAY) * DAY;
            ahead.entry(day.max(d)).or_default().push((p, e));
        }
    };
    if only(pools).is_some() {
        // pool by pool the ranges are small queries: fetch the period's ranges a few at a time
        // instead of one after another (the processor's per-client budget allows it)
        let mut ranges = vec![];
        let mut x = first;
        loop {
            let whole = (h.synced_ts - x) / DAY - 1;
            if whole < 2 {
                break;
            }
            let n = whole.min(RANGE_DAYS);
            ranges.push((x, n, start.values().filter(|s| s.0 < x + n * DAY).map(|s| s.1).min().unwrap_or(0)));
            x += n * DAY;
        }
        if ranges.len() > 1 {
            set_progress(cid, Some(format!("history: {} months from {}", ranges.len(), day_name(first))));
            for batch in ranges.chunks(PARALLEL_RANGES) {
                let got: Vec<(i64, i64, Result<Option<(Vec<(String, Ev)>, String)>, String>, usize)> = std::thread::scope(|sc| {
                    let hs: Vec<_> = batch.iter().map(|&(d0, n, after)| sc.spawn(move || {
                        let mut r = Report::default();
                        let x = chain_range(chain, only(pools), &day_name(d0), n, after, &mut r);
                        (d0, n, x, r.requests)
                    })).collect();
                    hs.into_iter().map(|h| h.join().unwrap_or((0, 0, Err("history download thread failed".into()), 0))).collect()
                });
                for (d0, n, r, reqs) in got {
                    rep.requests += reqs;
                    if let Some((rows, st)) = r? {
                        put(&mut ahead, &mut ahead_st, d0, n, rows, st);
                    }
                }
            }
        }
    }
    while d <= h.synced_ts {
        set_progress(cid, Some(format!("history: {} ({} of {} days)", day_name(d), (d - first) / DAY + 1, days_total)));
        let whole_days = (h.synced_ts - d) / DAY - 1; // complete days from d on, keeping the last synced day single
        if !ahead.contains_key(&d) && whole_days >= 2 {
            let n = whole_days.min(RANGE_DAYS);
            // the cursor serves every pool that starts within the range
            let after = start.values().filter(|s| s.0 < d + n * DAY).map(|s| s.1).min().unwrap_or(0);
            if let Some((rows, st)) = chain_range(chain, only(pools), &day_name(d), n, after, &mut rep)? {
                put(&mut ahead, &mut ahead_st, d, n, rows, st);
            }
        }
        let after = start.values().filter(|s| s.0 <= d).map(|s| s.1).min().unwrap_or(0);
        let (rows, status) = match ahead.remove(&d) {
            Some(r) => (r, ahead_st.remove(&d).unwrap_or_default()),
            None => chain_day(chain, only(pools), &day_name(d), after, &mut rep)?,
        };
        let whole = status == "final" || status == "complete";
        // up to this block the day is complete: all of it, or up to the synced position
        let day_max = rows.iter().map(|(_, e)| e.block).max().unwrap_or(0);
        let upto = if whole { day_max } else { day_max.min(cap) };
        let day_min = rows.iter().map(|(_, e)| e.block).min();
        let mut mine: HashMap<String, Vec<Ev>> = HashMap::new();
        for (p, e) in rows {
            if e.block > upto {
                continue;
            }
            if let Some(&(sd, sb)) = start.get(&p) {
                if sd <= d && e.block > sb {
                    validate(&e)?;
                    mine.entry(p).or_default().push(e);
                }
            }
        }
        if upto > 0 {
            let upto_ts = crate::chains::block_ts(cid, upto);
            for (p, &(sd, sb)) in &start {
                if sd > d || upto <= sb {
                    continue;
                }
                let rows = mine.remove(p).unwrap_or_default();
                rep.rows += rows.len();
                store_rows(cid, p, rows)?;
                let c = cov.get_mut(p).unwrap();
                let next = match c.take() {
                    Some(c) => Coverage { to_block: c.to_block.max(upto), to_ts: c.to_ts.max(upto_ts), ..c },
                    None => {
                        let fb = day_min.unwrap_or(upto);
                        rep.from_block = rep.from_block.min(fb);
                        Coverage { from_block: fb, to_block: upto, from_ts: d, to_ts: upto_ts, source: "processor".into() }
                    }
                };
                save_coverage(cid, p, &next)?;
                *c = Some(next);
            }
            for s in start.values_mut() {
                if s.0 <= d {
                    s.1 = s.1.max(upto);
                }
            }
        }
        if !whole {
            break;
        }
        d += DAY;
    }
    if rep.from_block == i64::MAX {
        rep.from_block = 0;
    }
    rep.to_block = cap;
    Ok(rep)
}

/// The wallet's Mint/Burn rows in the pool, its whole history, from the processor (None when the
/// processor does not serve this chain).
pub fn wallet_events(cid: u64, pool: &str, wallet: &str) -> Option<Result<Vec<Ev>, String>> {
    if !processor_enabled(cid) {
        return None;
    }
    let chain = blockchain_name(cid)?;
    let (pool, wallet) = (pool.to_lowercase(), wallet.to_lowercase());
    if !is_addr(&wallet) {
        return Some(Ok(vec![]));
    }
    Some((|| {
        let mut out = vec![];
        let mut after: i128 = 0;
        loop {
            let q = if after > 0 { format!("?after={after}") } else { String::new() };
            let v = api_get(&format!("{chain}/wallets/{wallet}/events{q}"))?;
            let cols: HashMap<String, usize> =
                v["columns"].as_array().ok_or("page without columns")?.iter().enumerate().filter_map(|(i, c)| c.as_str().map(|c| (c.to_string(), i))).collect();
            for r in v["rows"].as_array().ok_or("page without rows")? {
                // the wallet route has no price columns; owner is the wallet
                let mut r = r.clone();
                let mut cols = cols.clone();
                cols.insert("owner".into(), r.as_array().map(|a| a.len()).unwrap_or(0));
                r.as_array_mut().ok_or("bad row")?.push(json!(wallet));
                let (p, e) = row_to_ev(&cols, &r, None)?;
                if p == pool {
                    validate(&e)?;
                    out.push(e);
                }
            }
            match v["next"].as_str().and_then(|s| s.parse::<i128>().ok()) {
                Some(n) if n > after => after = n,
                _ => break,
            }
        }
        Ok(out)
    })())
}

/// Once per chain and run: the chain's own logs for a sample of blocks the processor filled must
/// be exactly the stored rows (same events, same fields).
fn spot_check(cid: u64, pools: &[String]) -> Result<(), String> {
    if !CHECKED.lock().unwrap().get_or_insert_with(Default::default).insert(cid) {
        return Ok(());
    }
    let covered: Vec<(String, Coverage)> =
        pools.iter().filter_map(|p| coverage(cid, p).filter(|c| c.source == "processor").map(|c| (p.clone(), c))).take(ADDRS_PER_REQUEST).collect();
    let Some(b) = covered.iter().map(|(_, c)| c.to_block).min() else { return Ok(()) };
    let a = (b - 300).max(covered.iter().map(|(_, c)| c.from_block).max().unwrap_or(b));
    if a >= b {
        return Ok(());
    }
    let names: Vec<String> = covered.iter().map(|(p, _)| p.clone()).collect();
    let mut on_chain: Vec<(String, Ev)> = vec![];
    fetch(cid, &names, a, b, &mut on_chain)?;
    // every field but the time: RPC rows carry the block time estimate, processor rows the real one
    let line = |p: &str, e: &Ev| format!("{p} {}", serde_json::to_string(&Ev { ts: 0, ..e.clone() }).unwrap());
    let mut want: Vec<String> = on_chain.iter().map(|(p, e)| line(p, e)).collect();
    let (ta, tb) = (crate::chains::block_ts(cid, a) - 3600, crate::chains::block_ts(cid, b) + 3600);
    let mut have: Vec<String> =
        names.iter().flat_map(|p| read(cid, p, ta, tb).into_iter().filter(|e| e.block >= a && e.block <= b).map(|e| line(p, &e)).collect::<Vec<_>>()).collect();
    want.sort();
    have.sort();
    if want != have {
        let differ = want.iter().filter(|w| !have.contains(w)).take(1).cloned().collect::<Vec<_>>();
        return Err(format!("blocks {a}-{b}: {} on chain, {} stored; first difference {:?}", want.len(), have.len(), differ));
    }
    crate::info!("history", "chain {cid}: processor history matches the chain on blocks {a}-{b} ({} events)", want.len());
    Ok(())
}

/// First run after the store was introduced: take over the pool's old events file
/// (data/raw/<chain>/<pool>/events.jsonl, complete from its first row to synced_to.txt), validated
/// like any download. The old file is left in place.
pub fn import_legacy(cid: u64, pool: &str) -> Result<usize, String> {
    if coverage(cid, pool).is_some() {
        return Ok(0);
    }
    let dir = data::pool_dir(cid, pool);
    let Some(to_block) = std::fs::read_to_string(dir.join("synced_to.txt")).ok().and_then(|s| s.trim().parse::<i64>().ok()) else {
        return Ok(0);
    };
    let mut rows = data::read_jsonl(&dir.join("events.jsonl"));
    rows.retain(|e| e.block <= to_block);
    let Some(from_block) = rows.iter().map(|e| e.block).min() else { return Ok(0) };
    for e in &rows {
        validate(e)?;
    }
    let n = rows.len();
    let lock = chain_lock(cid);
    let _g = lock.lock().unwrap();
    store_rows(cid, pool, rows)?;
    save_coverage(cid, pool, &Coverage {
        from_block, to_block, from_ts: crate::chains::block_ts(cid, from_block), to_ts: crate::chains::block_ts(cid, to_block), source: "rpc (imported)".into(),
    })?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_before_a_pool_existed_do_not_send_it_to_rpc() {
        // USDC/USDT: first event on block 30,218,057 (2025-05-14); the processor's empty days before
        // it moved the coverage's first time back to 2025-04-04
        let c = Coverage { from_block: 30_218_057, to_block: 52_783_267, from_ts: 1_743_724_800, to_ts: 1_792_355_881, source: "processor".into() };
        // a backtest wanting 2025-04-19 on: covered by time, only the blocks after the coverage are new
        assert_eq!(rpc_start(Some(&c), 29_500_000, 1_745_020_800), 52_783_268);
        // wanting 2025-03-01: earlier than the coverage by time and by block, from the target on
        assert_eq!(rpc_start(Some(&c), 27_000_000, 1_740_787_200), 27_000_000);
        // a coverage reaching back by block is enough too, and no coverage starts at the target
        assert_eq!(rpc_start(Some(&Coverage { from_ts: 1_790_000_000, ..c.clone() }), 31_000_000, 1_745_020_800), 52_783_268);
        assert_eq!(rpc_start(None, 31_000_000, 1_745_020_800), 31_000_000);
    }

    fn swap(block: i64, li: i64, tick: i64) -> Ev {
        Ev {
            block, log_index: li, ts: 1_792_000_000 + block, kind: "swap".into(), amount0: "-5".into(), amount1: "5".into(),
            sqrt_price_x96: crate::tickmath::sqrt_ratio_at_tick(tick).to_string(), liquidity: "1000".into(), tick, ..Default::default()
        }
    }

    #[test]
    fn day_names() {
        assert_eq!(day_name(0), "1970-01-01");
        assert_eq!(day_name(1_792_195_200), "2026-10-17");
        assert_eq!(day_name(1_792_195_199), "2026-10-16");
        assert_eq!(day_name(951_782_400), "2000-02-29");
        assert_eq!(day_name(951_868_799), "2000-02-29");
    }

    #[test]
    fn validation_catches_what_the_chain_cannot_produce() {
        assert!(validate(&swap(10, 0, 17)).is_ok());
        let mut e = swap(10, 0, 17);
        e.tick = 18; // price says 17
        assert!(validate(&e).is_err());
        let mut m = Ev { block: 5, log_index: 1, ts: 1, kind: "mint".into(), owner: format!("0x{}", "1".repeat(40)),
                         tick_lower: 10, tick_upper: 12, amount: "7".into(), amount0: "1".into(), amount1: "2".into(), ..Default::default() };
        assert!(validate(&m).is_ok());
        m.tick_upper = 10;
        assert!(validate(&m).is_err());
        m.tick_upper = 12;
        m.amount = "-7".into();
        assert!(validate(&m).is_err());
        let mut k = swap(1, 0, 0);
        k.kind = "flash".into();
        assert!(validate(&k).is_err());
    }

    #[test]
    fn a_swap_ending_on_a_tick_boundary_is_valid() {
        // Base, USDC/USDT pool, block 52289156 log 1901: tick 7 with the price exactly at tick 8's boundary
        let mut e = swap(52_289_156, 1901, 7);
        e.sqrt_price_x96 = "79259858533276714757314932306".into();
        assert_eq!(crate::tickmath::sqrt_ratio_at_tick(8).to_string(), e.sqrt_price_x96);
        assert!(validate(&e).is_ok());
        e.sqrt_price_x96 = (crate::tickmath::sqrt_ratio_at_tick(8) + num_bigint::BigUint::from(1u8)).to_string();
        assert!(validate(&e).is_err(), "one past the boundary belongs to tick 8");
    }

    #[test]
    fn processor_rows_become_store_rows() {
        let cols: HashMap<String, usize> = ["g", "ts", "pool", "k", "tick", "sqrtP", "liq", "a0", "a1", "owner", "tl", "tu", "dl", "tx"]
            .iter()
            .enumerate()
            .map(|(i, c)| (c.to_string(), i))
            .collect();
        let pool = format!("0x{}", "Ab".repeat(20));
        let sp = crate::tickmath::sqrt_ratio_at_tick(7).to_string();
        let swap = json!(["522891560000001901", 1_792_195_300, pool, "S", 7, sp, "1000", "-5", "6", null, null, null, null, "0x1"]);
        let (p, e) = row_to_ev(&cols, &swap, None).unwrap();
        assert_eq!(p, pool.to_lowercase());
        assert_eq!((e.block, e.log_index, e.kind.as_str(), e.amount0.as_str(), e.amount1.as_str()), (52_289_156, 1901, "swap", "-5", "6"));
        assert!(validate(&e).is_ok());
        let owner = format!("0x{}", "1".repeat(40));
        let burn = json!(["522891570000000003", 1_792_195_301, pool, "B", 7, sp, "1000", "-30", "-70", owner, -10, 10, "-100", "0x2"]);
        let (_, b) = row_to_ev(&cols, &burn, None).unwrap();
        assert_eq!((b.kind.as_str(), b.amount.as_str(), b.amount0.as_str(), b.amount1.as_str()), ("burn", "100", "30", "70"));
        assert_eq!((b.tick_lower, b.tick_upper, b.block, b.log_index, b.tick), (-10, 10, 52_289_157, 3, 0));
        assert!(validate(&b).is_ok());
        let bad = json!(["1", 1, pool, "X", 0, "1", "1", "0", "0", null, null, null, null, null]);
        assert!(row_to_ev(&cols, &bad, None).is_err());
    }

    #[test]
    fn day_files_stay_sorted_and_unique() {
        let dir = std::env::temp_dir().join(format!("alm-hist-test-{}", std::process::id()));
        let p = dir.join("d.jsonl");
        merge_day(&p, vec![swap(20, 1, 1), swap(10, 0, 1)]).unwrap();
        merge_day(&p, vec![swap(15, 3, 1), swap(20, 1, 1)]).unwrap();
        let rows = read_day(&p);
        assert_eq!(rows.iter().map(|e| (e.block, e.log_index)).collect::<Vec<_>>(), vec![(10, 0), (15, 3), (20, 1)]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
