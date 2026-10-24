//! Profit per chain, from the app's own transactions.
//!
//! On Solidly V3 everything an LP earns (swap fees and emissions) is paid through reward claims, and
//! every claim, swap, mint and burn is a transaction this app sent (logs/executor.jsonl, kind
//! "sent"). Each transaction's receipt is read once and cached in logs/ledger.json:
//!   - gas: gasUsed x effectiveGasPrice, plus the L1 data fee on OP-stack chains;
//!   - token movements in and out of the wallet (ERC-20 Transfer logs).
//! Per chain: earned = what the claims paid in + what is claimable now + what is pending for the
//! next claim snapshot; swap cost = value given away by swaps, each against the small reference quote
//! of the same pair logged with it (older swaps: out minus in at the pool's price or the tokens' USD
//! prices), and never below zero: a swap pays fees, so a "gain" is the reference being off the
//! market, not the swap; IL = impermanent loss,
//! what the positions returned (or hold now) minus what the deposited tokens would be worth if
//! held, from the wallet's Mint/Burn history since the app's first transaction on the chain;
//! profit = earned - gas - swap cost - IL. Tokens are valued at their price
//! when the transaction was sent (from the nearest snapshot that held the token, logs/snapshots.jsonl),
//! else today's price. Price moves of the held tokens after that are not profit or loss here; they show
//! in the balance.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const TRANSFER: &str = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// per wallet (wallets.rs): the totals, when they were computed and how long the executor log was then
static LAST: Mutex<Option<std::collections::HashMap<String, (Instant, u64, Value)>>> = Mutex::new(None);

/// Forget the cached totals: a transaction was just sent, so the next read recomputes them rather
/// than show figures from before it for up to ten minutes.
pub fn invalidate() {
    if let Some(m) = LAST.lock().unwrap().as_mut() {
        m.remove(&crate::paths::wallet());
    }
}

fn path() -> std::path::PathBuf {
    crate::paths::logs().join("ledger.json")
}

/// Every transaction the ledger has read, by hash (as saved; no network).
pub fn transactions() -> Vec<(String, Value)> {
    let m: Map<String, Value> = std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    m.into_iter().collect()
}

fn hex_f64(v: &Value) -> f64 {
    v.as_str().map(|s| crate::eth::parse_uint(s).to_string().parse::<f64>().unwrap_or(0.0)).unwrap_or(0.0)
}

fn topic_addr(t: &Value) -> String {
    let s = t.as_str().unwrap_or("");
    format!("0x{}", &s[s.len().saturating_sub(40)..]).to_lowercase()
}

/// One receipt -> {gas_wei, moves: {token: signed raw amount}}.
fn digest(r: &Value, wallet: &str) -> Value {
    let gas = hex_f64(&r["gasUsed"]) * hex_f64(&r["effectiveGasPrice"]) + hex_f64(&r["l1Fee"]);
    let mut moves: HashMap<String, f64> = HashMap::new();
    for l in r["logs"].as_array().into_iter().flatten() {
        let t = l["topics"].as_array().cloned().unwrap_or_default();
        if t.len() != 3 || t[0].as_str().map(|s| s.to_lowercase()) != Some(TRANSFER.into()) {
            continue;
        }
        let token = l["address"].as_str().unwrap_or("").to_lowercase();
        let amt = hex_f64(&l["data"]);
        if topic_addr(&t[2]) == wallet {
            *moves.entry(token.clone()).or_default() += amt;
        }
        if topic_addr(&t[1]) == wallet {
            *moves.entry(token).or_default() -= amt;
        }
    }
    json!({"gas_wei": gas, "moves": moves, "status": r["status"]})
}

/// Fetch and cache receipts for every sent transaction not seen yet.
fn sync(wallet: &str) -> Map<String, Value> {
    let mut cache: Map<String, Value> =
        std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let mut todo: BTreeMap<u64, Vec<(String, String, Value)>> = BTreeMap::new();
    let mut changed = false;
    for e in crate::engine::log_tail(64 << 20) {
        if e["kind"] != "sent" {
            continue;
        }
        let Some(h) = e["hash"].as_str().map(|s| s.to_lowercase()) else { continue };
        if let Some(d) = cache.get_mut(&h) {
            for k in ["ts", "block", "pool", "ref"] {
                if d.get(k).map(|v| v.is_null()).unwrap_or(true) && !e[k].is_null() {
                    d[k] = e[k].clone();
                    changed = true;
                }
            }
            continue;
        }
        let cid = e["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
        todo.entry(cid).or_default().push((h, e["what"].as_str().unwrap_or("").to_string(), json!({"ts": e["ts"], "block": e["block"], "pool": e["pool"], "ref": e["ref"]})));
    }
    for (cid, txs) in todo {
        for part in txs.chunks(25) {
            let calls: Vec<(&str, Value)> = part.iter().map(|(h, _, _)| ("eth_getTransactionReceipt", json!([h]))).collect();
            let Ok(res) = crate::rpc::batch(cid, &calls) else { continue };
            for ((h, what, ts), r) in part.iter().zip(res) {
                if let Ok(r) = r {
                    if r.is_object() {
                        let mut d = digest(&r, wallet);
                        d["chain_id"] = json!(cid);
                        d["what"] = json!(what);
                        for k in ["ts", "block", "pool", "ref"] {
                            d[k] = ts[k].clone();
                        }
                        cache.insert(h.clone(), d);
                        changed = true;
                    }
                }
            }
        }
    }
    if changed {
        let _ = std::fs::write(path(), Value::Object(cache.clone()).to_string());
    }
    cache
}

/// Token prices over time, from the wallet balances in the snapshots: (chain, token) -> [(ts, usd)].
fn price_history() -> HashMap<(u64, String), Vec<(i64, f64)>> {
    let mut out: HashMap<(u64, String), Vec<(i64, f64)>> = HashMap::new();
    let Ok(text) = std::fs::read_to_string(crate::paths::logs().join("snapshots.jsonl")) else { return out };
    for l in text.lines() {
        let Ok(s) = serde_json::from_str::<Value>(l) else { continue };
        let ts = s["ts"].as_i64().unwrap_or(0);
        for t in s["tokens"].as_object().into_iter().flat_map(|m| m.values()) {
            let (Some(a), Some(amt), Some(u)) = (t["address"].as_str(), t["amount"].as_f64(), t["usd"].as_f64()) else { continue };
            if amt > 0.0 && u > 0.0 {
                let cid = t["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
                out.entry((cid, a.to_lowercase())).or_default().push((ts, u / amt));
            }
        }
    }
    out
}

/// The token's price nearest to `ts` (within 12 hours), else today's.
fn price_at(hist: &HashMap<(u64, String), Vec<(i64, f64)>>, cid: u64, token: &str, ts: Option<i64>) -> Option<f64> {
    if let (Some(ts), Some(v)) = (ts, hist.get(&(cid, token.to_string()))) {
        if let Some(&(t, p)) = v.iter().min_by_key(|(t, _)| (t - ts).abs()) {
            if (t - ts).abs() <= 12 * 3600 {
                return Some(p);
            }
        }
    }
    crate::prices::usd(cid, token).ok()
}

fn usd_of(hist: &HashMap<(u64, String), Vec<(i64, f64)>>, cid: u64, token: &str, raw: f64, ts: Option<i64>) -> Option<f64> {
    let m = crate::prices::meta(cid, token).ok()?;
    Some(raw / 10f64.powi(m.decimals as i32) * price_at(hist, cid, token, ts)?)
}

/// A configured pool's tokens and its price history (token1 per token0, whole tokens) from the
/// stored swaps, so values can be taken at the pool's own price rather than two separate USD quotes
/// (which disagree with each other by a few basis points and would show phantom gains).
struct PoolPx {
    t0: String,
    t1: String,
    u0: f64,
    u1: f64,
    swaps: Vec<(i64, f64)>, // (block, price) sorted by block
}

impl PoolPx {
    fn load(cid: u64, pool: &str) -> Option<PoolPx> {
        let (t0, t1) = crate::chain::pool_tokens(cid, pool).ok()?;
        let (d0, d1) = (crate::prices::meta(cid, &t0).ok()?.decimals as i32, crate::prices::meta(cid, &t1).ok()?.decimals as i32);
        let scale = 10f64.powi(d0 - d1);
        // a line at a time, keeping only (block, log index, price): every stored swap as an event
        // took several hundred MB each cycle
        let mut all: Vec<(i64, i64, f64)> = vec![];
        crate::history::scan_all(cid, pool, |e| {
            if e.kind == "swap" {
                all.push((e.block, e.log_index, 1.0001f64.powf(e.tick as f64) * scale))
            }
        });
        all.sort_by_key(|x| (x.0, x.1));
        all.dedup_by_key(|x| (x.0, x.1));
        let swaps: Vec<(i64, f64)> = all.into_iter().map(|(b, _, p)| (b, p)).collect();
        Some(PoolPx { t0, t1, u0: 10f64.powi(d0), u1: 10f64.powi(d1), swaps })
    }

    /// The pool price just before `block` (the mid price a transaction in that block traded against).
    fn before(&self, block: i64) -> Option<f64> {
        let i = self.swaps.partition_point(|x| x.0 < block);
        (i > 0).then(|| self.swaps[i - 1].1)
    }

    /// USD value of raw amounts at pool price `p`, with token1's USD price at `ts` setting the level.
    fn usd(&self, cid: u64, hist: &HashMap<(u64, String), Vec<(i64, f64)>>, a0: f64, a1: f64, p: f64, ts: Option<i64>) -> Option<f64> {
        Some((a0 / self.u0 * p + a1 / self.u1) * price_at(hist, cid, &self.t1, ts)?)
    }
}

/// A swap's cost (USD) against the reference quote logged with it (`r`, engine `reference_quote`):
/// what the tokens it gave would have bought at the small quote's rate, less what it got. None for a
/// swap without one (sent before 0.7.5) or whose tokens the receipt does not show.
fn ref_cost(r: &Value, moves: &Map<String, Value>, usd: &dyn Fn(&str, f64) -> Option<f64>) -> Option<f64> {
    let (tin, tout) = (r["token_in"].as_str()?.to_lowercase(), r["token_out"].as_str()?.to_lowercase());
    let small: f64 = r["in"].as_str()?.parse().ok()?;
    let small_out = r["out"].as_f64()?;
    let gave = -moves.get(&tin)?.as_f64()?;
    let got = moves.get(&tout)?.as_f64()?;
    if small <= 0.0 || gave <= 0.0 {
        return None;
    }
    usd(&tout, small_out / small * gave - got)
}

/// What a swap costs in the totals: never less than nothing. Every swap pays fees, so one that seems
/// to come out ahead was measured against a price off the market (a lagging USD price, a thin pool's
/// last trade), not a swap that gained.
fn swap_paid(cost_usd: f64) -> f64 {
    if cost_usd.is_finite() { cost_usd.max(0.0) } else { 0.0 }
}

#[cfg(test)]
mod swap_cost_tests {
    use super::*;

    #[test]
    fn a_swap_is_measured_against_its_reference_quote() {
        // the finished exit: 1.5430 WETH -> 3,596.50 USDC; the reference, 0.0154 WETH -> 35.92 USDC
        // (2,332.47 a WETH): it would have bought 3,599.07, so it cost $2.57
        let usdc = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
        let weth = "0x4200000000000000000000000000000000000006";
        let r = json!({"token_in": weth, "token_out": usdc, "in": "15400000000000000", "out": 35_920_038.0});
        let moves: Map<String, Value> = serde_json::from_value(json!({usdc: 3_596_499_369.0, weth: -1.5430253694104015e18})).unwrap();
        let usd = |t: &str, raw: f64| (t == usdc).then_some(raw / 1e6);
        let c = ref_cost(&r, &moves, &usd).unwrap();
        assert!((c - 2.57).abs() < 0.01, "{c}");
        // no reference (an older swap), or its tokens not moved: none
        assert!(ref_cost(&Value::Null, &moves, &usd).is_none());
        let other: Map<String, Value> = serde_json::from_value(json!({usdc: 10.0})).unwrap();
        assert!(ref_cost(&r, &other, &usd).is_none());
    }

    #[test]
    fn a_swap_never_counts_as_a_gain() {
        assert_eq!(swap_paid(-10.45), 0.0);
        assert_eq!(swap_paid(2.5), 2.5);
        assert_eq!(swap_paid(f64::NAN), 0.0);
    }
}

/// One pool's impermanent loss (USD, positive = loss) from the wallet's Mint/Burn events alone, in
/// block order: each withdrawal's tokens against the same share of what was deposited into that
/// range, both at the pool price just before it (`before`, by block); the liquidity still open by
/// the events against its deposits, both at `p_now`. `value(a0, a1, p, ts)` is the USD value of raw
/// amounts at pool price p; `held(tl, tu, l)` the raw amounts liquidity l holds at today's price.
///
/// Nothing here is read at a different moment from anything else: a withdrawal not in the events
/// yet (synced next cycle) leaves the position open in them too, so it is valued as still held, the
/// same number either way. Comparing the events with positions read on-chain now would count every
/// such lag as a loss (or a gain) of all the tokens in flight.
fn il_events(ev: &[crate::data::Ev], before: &dyn Fn(i64) -> Option<f64>, value: &dyn Fn(f64, f64, f64, Option<i64>) -> Option<f64>,
             held: &dyn Fn(i64, i64, f64) -> (f64, f64), p_now: f64) -> Option<f64> {
    let f = |s: &str| s.parse::<f64>().unwrap_or(0.0);
    let mut ev: Vec<&crate::data::Ev> = ev.iter().collect();
    ev.sort_by_key(|e| (e.block, e.log_index));
    let mut loss = 0.0;
    // (tl, tu) -> (liquidity, deposited token0, deposited token1) still in the position
    let mut basis: HashMap<(i64, i64), (f64, f64, f64)> = HashMap::new();
    for e in ev {
        let b = basis.entry((e.tick_lower, e.tick_upper)).or_default();
        if e.kind == "mint" {
            *b = (b.0 + f(&e.amount), b.1 + f(&e.amount0), b.2 + f(&e.amount1));
        } else if e.kind == "burn" && b.0 > 0.0 {
            let k = (f(&e.amount) / b.0).min(1.0);
            if let Some(p) = before(e.block) {
                let was = value(k * b.1, k * b.2, p, Some(e.ts))?;
                let got = value(f(&e.amount0), f(&e.amount1), p, Some(e.ts))?;
                loss += (was - got).max(0.0);
            }
            *b = (b.0 * (1.0 - k), b.1 * (1.0 - k), b.2 * (1.0 - k));
        }
    }
    for ((tl, tu), (l, b0, b1)) in basis {
        if l > 1.0 {
            let (a0, a1) = held(tl, tu, l);
            loss += (value(b0, b1, p_now, None)? - value(a0, a1, p_now, None)?).max(0.0);
        }
    }
    Some(loss)
}

/// Impermanent loss (USD, positive = loss) of the configured pools on a chain, for liquidity minted
/// at or after `since` (`il_events`, valued at the pool's own price: valued at one price, a range
/// position is never worth more than the tokens it started with, so this is never a gain).
fn il_cost(cid: u64, wallet: &str, hist: &HashMap<(u64, String), Vec<(i64, f64)>>, since: i64, px: &HashMap<String, PoolPx>) -> Option<f64> {
    let mut loss = 0.0;
    for (pool, pp) in px {
        let ev: Vec<crate::data::Ev> = crate::data::read_jsonl(&crate::data::wallet_path(cid, pool, wallet)).into_iter().filter(|e| e.ts >= since).collect();
        if ev.is_empty() {
            continue;
        }
        let (sp, _, _) = crate::chain::slot0(cid, pool).ok()?;
        let p_now = (crate::tickmath::to_f64(&sp) / 2f64.powi(96)).powi(2) * pp.u0 / pp.u1;
        let held = |tl: i64, tu: i64, l: f64| {
            let (a0, a1) = crate::tickmath::amounts_at(&sp, tl, tu, &num_bigint::BigUint::from(l.max(0.0) as u128));
            (crate::tickmath::to_f64(&a0), crate::tickmath::to_f64(&a1))
        };
        loss += il_events(&ev, &|b| pp.before(b), &|a0, a1, p, ts| pp.usd(cid, hist, a0, a1, p, ts), &held, p_now)?;
    }
    Some(loss)
}

/// Per-chain totals: {"<chain id>": {earned_usd, claimed_usd, claimable_usd, gas_usd, swap_cost_usd,
/// profit_usd, txs, claims, swaps, since (the first position event)}}. Recomputed at most every 10 minutes.
pub fn summary(wallet: &str, chains: &[u64]) -> Value {
    // a new transaction (the executor log grew) recomputes them once they are half a minute old
    let log_len = std::fs::metadata(crate::paths::logs().join("executor.jsonl")).map(|m| m.len()).unwrap_or(0);
    if let Some((t, n, v)) = LAST.lock().unwrap().get_or_insert_with(Default::default).get(&crate::paths::wallet()) {
        if t.elapsed() < Duration::from_secs(600) && (*n == log_len || t.elapsed() < Duration::from_secs(30)) {
            return v.clone();
        }
    }
    let wallet = wallet.to_lowercase();
    let cache = sync(&wallet);
    let hist = price_history();
    let mut out = Map::new();
    let mut ids: Vec<u64> = chains.to_vec();
    for d in cache.values() {
        if let Some(c) = d["chain_id"].as_u64() {
            if !ids.contains(&c) {
                ids.push(c);
            }
        }
    }
    for cid in ids {
        let ch = crate::chains::get(cid);
        let native_usd = if ch.wnative.is_empty() { None } else { crate::prices::usd(cid, &ch.wnative).ok() };
        let (mut gas, mut claimed, mut swap_cost) = (0.0, 0.0, 0.0);
        let (mut txs, mut claims, mut swaps, mut unpriced) = (0, 0, 0, 0);
        let px: HashMap<String, PoolPx> = crate::settings::load_pools()
            .iter()
            .filter(|p| crate::worker::pool_chain(p) == cid)
            .filter_map(|p| {
                let a = p["pool"].as_str()?.to_lowercase();
                PoolPx::load(cid, &a).map(|x| (a, x))
            })
            .collect();
        // per pool: gas and swap cost, all time and over the last 24 h and 7 days (engine caps, accounting)
        let now = crate::engine::now() as i64;
        let mut by_pool: HashMap<String, [f64; 7]> = HashMap::new();
        for d in cache.values().filter(|d| d["chain_id"].as_u64() == Some(cid)) {
            txs += 1;
            let (gas0, swap0) = (gas, swap_cost);
            gas += d["gas_wei"].as_f64().unwrap_or(0.0) / 1e18;
            let what = d["what"].as_str().unwrap_or("");
            let ts = d["ts"].as_i64();
            let moves = d["moves"].as_object().cloned().unwrap_or_default();
            let val = |pos: bool| -> f64 {
                moves
                    .iter()
                    .filter(|(_, v)| (v.as_f64().unwrap_or(0.0) > 0.0) == pos)
                    .filter_map(|(t, v)| usd_of(&hist, cid, t, v.as_f64().unwrap_or(0.0).abs(), ts))
                    .sum()
            };
            if what == "claim" {
                claims += 1;
                claimed += val(true);
            } else if what.starts_with("swap") || what.starts_with("sell") || what.contains("swap") {
                swaps += 1;
                // Against the reference quote logged with the swap (`ref_cost`). An older swap between
                // the pool's own tokens is measured against the pool's mid price just before it, other
                // older swaps (SOLID sales, funding) at the tokens' USD prices of the time.
                let pp = d["pool"].as_str().and_then(|a| px.get(&a.to_lowercase()));
                let own = pp.filter(|pp| moves.keys().all(|t| *t == pp.t0 || *t == pp.t1));
                let mid = own.and_then(|pp| d["block"].as_i64().and_then(|b| pp.before(b)).map(|p| (pp, p)));
                let c = if let Some(c) = ref_cost(&d["ref"], &moves, &|t, raw| usd_of(&hist, cid, t, raw, ts)) {
                    c
                } else if let Some((pp, p)) = mid {
                    let m = |t: &str| moves.get(t).and_then(|v| v.as_f64()).unwrap_or(0.0);
                    -pp.usd(cid, &hist, m(&pp.t0), m(&pp.t1), p, ts).unwrap_or(0.0)
                } else {
                    let (inn, out_) = (val(true), val(false));
                    if inn == 0.0 && out_ == 0.0 && !moves.is_empty() {
                        unpriced += 1;
                    }
                    out_ - inn
                };
                swap_cost += swap_paid(c);
            }
            if let Some(p) = d["pool"].as_str() {
                let g_usd = (gas - gas0) * native_usd.unwrap_or(0.0);
                let sc = swap_cost - swap0;
                let age = now - d["ts"].as_i64().unwrap_or(0);
                let e = by_pool.entry(p.to_lowercase()).or_default();
                e[0] += g_usd;
                e[1] += sc;
                e[2] += 1.0;
                if age <= 7 * 86_400 {
                    e[3] += g_usd;
                    e[4] += sc;
                }
                if age <= 86_400 {
                    e[5] += g_usd;
                    e[6] += sc;
                }
            }
        }
        let by_pool_json: Map<String, Value> = by_pool.iter().map(|(p, e)| (p.clone(), json!({
            "gas_usd": e[0], "swap_cost_usd": e[1], "txs": e[2], "gas_7d_usd": e[3], "swap_cost_7d_usd": e[4], "cost_7d_usd": e[3] + e[4],
            "gas_24h_usd": e[5], "swap_cost_24h_usd": e[6], "cost_24h_usd": e[5] + e[6]}))).collect();
        let rw = crate::claim::claimable(cid, &wallet);
        let (claimable, pending) = (rw["usd"].as_f64().unwrap_or(0.0), rw["pending_usd"].as_f64().unwrap_or(0.0));
        let gas_usd = native_usd.map(|p| gas * p);
        let since = cache.values().filter(|d| d["chain_id"].as_u64() == Some(cid)).filter_map(|d| d["ts"].as_i64()).min();
        let il = since.and_then(|t| il_cost(cid, &wallet, &hist, t, &px));
        let earned = claimed + claimable + pending;
        let profit = earned - gas_usd.unwrap_or(0.0) - swap_cost - il.unwrap_or(0.0);
        out.insert(
            cid.to_string(),
            json!({"earned_usd": earned, "since": since, "claimed_usd": claimed, "claimable_usd": claimable, "pending_usd": pending, "il_usd": il, "gas_native": gas,
                   "gas_symbol": ch.gas_symbol, "gas_usd": gas_usd, "swap_cost_usd": swap_cost, "profit_usd": profit,
                   "txs": txs, "claims": claims, "swaps": swaps, "unpriced_swaps": unpriced, "by_pool": by_pool_json}),
        );
    }
    let v = Value::Object(out);
    LAST.lock().unwrap().get_or_insert_with(Default::default).insert(crate::paths::wallet(), (Instant::now(), log_len, v.clone()));
    v
}

#[cfg(test)]
mod il_tests {
    use super::*;
    use crate::data::Ev;

    fn ev(block: i64, kind: &str, l: f64, a0: f64, a1: f64) -> Ev {
        Ev { block, ts: block, kind: kind.into(), amount: format!("{l}"), amount0: format!("{a0}"), amount1: format!("{a1}"), tick_lower: -100, tick_upper: 100, ..Default::default() }
    }

    /// Uniswap amounts of liquidity l on [tl, tu) at price p (token1 per token0, raw)
    fn amounts(tl: i64, tu: i64, l: f64, p: f64) -> (f64, f64) {
        let (sa, sb, s) = (1.0001f64.powf(tl as f64 / 2.0), 1.0001f64.powf(tu as f64 / 2.0), p.sqrt());
        let s = s.clamp(sa, sb);
        (l * (sb - s) / (s * sb), l * (s - sa))
    }

    fn value(a0: f64, a1: f64, p: f64, _: Option<i64>) -> Option<f64> {
        Some(a0 * p + a1)
    }

    /// IL with a constant pool price `before` every event and `p_now` today
    fn il(evs: &[Ev], p_then: f64, p_now: f64) -> f64 {
        il_events(evs, &|_| Some(p_then), &value, &|tl, tu, l| amounts(tl, tu, l, p_now), p_now).unwrap()
    }

    fn mint(block: i64, l: f64, p: f64) -> Ev {
        let (a0, a1) = amounts(-100, 100, l, p);
        ev(block, "mint", l, a0, a1)
    }

    fn burn(block: i64, l: f64, p: f64) -> Ev {
        let (a0, a1) = amounts(-100, 100, l, p);
        ev(block, "burn", l, a0, a1)
    }

    #[test]
    fn no_price_move_no_loss() {
        assert!(il(&[mint(1, 1e6, 1.0)], 1.0, 1.0).abs() < 1e-6);
        assert!(il(&[mint(1, 1e6, 1.0), burn(2, 1e6, 1.0)], 1.0, 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_price_move_is_a_loss_held_or_withdrawn_alike() {
        let p1 = 1.0001f64.powi(60);
        let open = il(&[mint(1, 1e6, 1.0)], p1, p1);
        let (d0, d1) = amounts(-100, 100, 1e6, 1.0);
        let (h0, h1) = amounts(-100, 100, 1e6, p1);
        assert!((open - ((d0 * p1 + d1) - (h0 * p1 + h1))).abs() < 1e-6 && open > 0.0, "{open}");
        // withdrawn at that price: the same loss
        let closed = il(&[mint(1, 1e6, 1.0), burn(2, 1e6, p1)], p1, p1);
        assert!((open - closed).abs() < 1e-6, "{open} vs {closed}");
    }

    #[test]
    fn a_withdrawal_not_synced_yet_changes_nothing() {
        // the cycle withdrew 30% at today's price; until its Burn is in the events, the position is
        // whole in them, and the number is the same as once it is
        let p1 = 1.0001f64.powi(30);
        let lagging = il(&[mint(1, 1e6, 1.0)], p1, p1);
        let synced = il(&[mint(1, 1e6, 1.0), burn(5, 3e5, p1)], p1, p1);
        assert!((lagging - synced).abs() < 1e-6, "{lagging} vs {synced}");
        // and a deposit not synced yet is not a loss of the tokens in flight either
        let before = il(&[mint(1, 1e6, 1.0)], p1, p1);
        let after = il(&[mint(1, 1e6, 1.0), mint(6, 5e5, p1)], p1, p1);
        assert!((before - after).abs() < 1e-6, "{before} vs {after}");
    }

    #[test]
    fn partial_withdrawals_take_their_share_of_the_deposits() {
        let p1 = 1.0001f64.powi(80);
        let one = il(&[mint(1, 1e6, 1.0), burn(2, 1e6, p1)], p1, p1);
        let parts = il(&[mint(1, 1e6, 1.0), burn(2, 4e5, p1), burn(3, 6e5, p1)], p1, p1);
        assert!((one - parts).abs() < 1e-6, "{one} vs {parts}");
        // a burn of more than is tracked (liquidity minted before the period) counts only what is
        assert!(il(&[burn(2, 4e5, p1)], p1, p1) == 0.0);
    }
}
