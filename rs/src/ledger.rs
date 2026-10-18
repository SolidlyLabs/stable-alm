//! Profit per chain, from the app's own transactions.
//!
//! On Solidly V3 everything an LP earns (swap fees and emissions) is paid through reward claims, and
//! every claim, swap, mint and burn is a transaction this app sent (logs/executor.jsonl, kind
//! "sent"). Each transaction's receipt is read once and cached in logs/ledger.json:
//!   - gas: gasUsed x effectiveGasPrice, plus the L1 data fee on OP-stack chains;
//!   - token movements in and out of the wallet (ERC-20 Transfer logs).
//! Per chain: earned = what the claims paid in + what is claimable now + what is pending for the
//! next claim snapshot; swap cost = value given away by swaps (out minus in); IL = impermanent loss,
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

static LAST: Mutex<Option<(Instant, Value)>> = Mutex::new(None);

fn path() -> std::path::PathBuf {
    crate::paths::logs().join("ledger.json")
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
            for k in ["ts", "block", "pool"] {
                if d.get(k).map(|v| v.is_null()).unwrap_or(true) && !e[k].is_null() {
                    d[k] = e[k].clone();
                    changed = true;
                }
            }
            continue;
        }
        let cid = e["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
        todo.entry(cid).or_default().push((h, e["what"].as_str().unwrap_or("").to_string(), json!({"ts": e["ts"], "block": e["block"], "pool": e["pool"]})));
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
                        for k in ["ts", "block", "pool"] {
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
        let mut swaps: Vec<(i64, f64)> = crate::history::read_all(cid, pool)
            .into_iter()
            .filter(|e| e.kind == "swap")
            .map(|e| (e.block, 1.0001f64.powf(e.tick as f64) * scale))
            .collect();
        swaps.sort_by_key(|x| x.0);
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

/// Impermanent loss (USD, positive = loss) of the configured pools on a chain, for liquidity minted
/// at or after `since`: each withdrawal's tokens vs the same share of what was deposited into that
/// range, both at the pool price just before the withdrawal; open liquidity at the pool price now.
/// Valued at one price, a range position is never worth more than the tokens it started with, so
/// this is never a gain.
fn il_cost(cid: u64, wallet: &str, hist: &HashMap<(u64, String), Vec<(i64, f64)>>, since: i64, px: &HashMap<String, PoolPx>) -> Option<f64> {
    let mut loss = 0.0;
    for (pool, pp) in px {
        let mut ev: Vec<crate::data::Ev> = crate::data::read_jsonl(&crate::data::wallet_path(cid, pool, wallet)).into_iter().filter(|e| e.ts >= since).collect();
        if ev.is_empty() {
            continue;
        }
        ev.sort_by_key(|e| (e.block, e.log_index));
        let f = |s: &str| s.parse::<f64>().unwrap_or(0.0);
        // (tl, tu) -> (liquidity, deposited token0, deposited token1) still in the position
        let mut basis: HashMap<(i64, i64), (f64, f64, f64)> = HashMap::new();
        for e in &ev {
            let b = basis.entry((e.tick_lower, e.tick_upper)).or_default();
            if e.kind == "mint" {
                *b = (b.0 + f(&e.amount), b.1 + f(&e.amount0), b.2 + f(&e.amount1));
            } else if b.0 > 0.0 {
                let k = (f(&e.amount) / b.0).min(1.0);
                if let Some(p) = pp.before(e.block) {
                    let held = pp.usd(cid, hist, k * b.1, k * b.2, p, Some(e.ts))?;
                    let got = pp.usd(cid, hist, f(&e.amount0), f(&e.amount1), p, Some(e.ts))?;
                    loss += (held - got).max(0.0);
                }
                *b = (b.0 * (1.0 - k), b.1 * (1.0 - k), b.2 * (1.0 - k));
            }
        }
        let open: Vec<_> = basis.into_iter().filter(|(_, b)| b.0 > 1.0).collect();
        if open.is_empty() {
            continue;
        }
        let (sp, _, _) = crate::chain::slot0(cid, pool).ok()?;
        let p = (crate::tickmath::to_f64(&sp) / 2f64.powi(96)).powi(2) * pp.u0 / pp.u1;
        let live: HashMap<(i64, i64), num_bigint::BigUint> =
            crate::engine::wallet_positions(cid, pool, wallet).ok()?.into_iter().map(|(a, b, l)| ((a, b), l)).collect();
        for ((tl, tu), (l, b0, b1)) in open {
            let Some(cur) = live.get(&(tl, tu)) else { continue };
            // only the share of today's liquidity that the tracked deposits account for
            let k = (l / crate::tickmath::to_f64(cur)).min(1.0);
            let (a0, a1) = crate::tickmath::amounts_at(&sp, tl, tu, cur);
            let (a0, a1) = (crate::tickmath::to_f64(&a0) * k, crate::tickmath::to_f64(&a1) * k);
            loss += (pp.usd(cid, hist, b0, b1, p, None)? - pp.usd(cid, hist, a0, a1, p, None)?).max(0.0);
        }
    }
    Some(loss)
}

/// Per-chain totals: {"<chain id>": {earned_usd, claimed_usd, claimable_usd, gas_usd, swap_cost_usd,
/// profit_usd, txs, claims, swaps}}. Recomputed at most every 10 minutes.
pub fn summary(wallet: &str, chains: &[u64]) -> Value {
    if let Some((t, v)) = LAST.lock().unwrap().as_ref() {
        if t.elapsed() < Duration::from_secs(600) {
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
        for d in cache.values().filter(|d| d["chain_id"].as_u64() == Some(cid)) {
            txs += 1;
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
                // A swap between the pool's own tokens is measured against the pool's mid price just
                // before it: what the swap lost to fees and slippage. Other swaps (SOLID sales,
                // funding) are valued at the tokens' USD prices of the time.
                let pp = d["pool"].as_str().and_then(|a| px.get(&a.to_lowercase()));
                let own = pp.filter(|pp| moves.keys().all(|t| *t == pp.t0 || *t == pp.t1));
                let mid = own.and_then(|pp| d["block"].as_i64().and_then(|b| pp.before(b)).map(|p| (pp, p)));
                if let Some((pp, p)) = mid {
                    let m = |t: &str| moves.get(t).and_then(|v| v.as_f64()).unwrap_or(0.0);
                    swap_cost -= pp.usd(cid, &hist, m(&pp.t0), m(&pp.t1), p, ts).unwrap_or(0.0);
                } else {
                    let (inn, out_) = (val(true), val(false));
                    if inn == 0.0 && out_ == 0.0 && !moves.is_empty() {
                        unpriced += 1;
                    }
                    swap_cost += out_ - inn;
                }
            }
        }
        let rw = crate::claim::claimable(cid, &wallet);
        let (claimable, pending) = (rw["usd"].as_f64().unwrap_or(0.0), rw["pending_usd"].as_f64().unwrap_or(0.0));
        let gas_usd = native_usd.map(|p| gas * p);
        let since = cache.values().filter(|d| d["chain_id"].as_u64() == Some(cid)).filter_map(|d| d["ts"].as_i64()).min();
        let il = since.and_then(|t| il_cost(cid, &wallet, &hist, t, &px));
        let earned = claimed + claimable + pending;
        let profit = earned - gas_usd.unwrap_or(0.0) - swap_cost - il.unwrap_or(0.0);
        out.insert(
            cid.to_string(),
            json!({"earned_usd": earned, "claimed_usd": claimed, "claimable_usd": claimable, "pending_usd": pending, "il_usd": il, "gas_native": gas,
                   "gas_symbol": ch.gas_symbol, "gas_usd": gas_usd, "swap_cost_usd": swap_cost, "profit_usd": profit,
                   "txs": txs, "claims": claims, "swaps": swaps, "unpriced_swaps": unpriced}),
        );
    }
    let v = Value::Object(out);
    *LAST.lock().unwrap() = Some((Instant::now(), v.clone()));
    v
}
