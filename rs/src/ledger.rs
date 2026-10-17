//! Profit per chain, from the app's own transactions.
//!
//! On Solidly V3 everything an LP earns (swap fees and emissions) is paid through reward claims, and
//! every claim, swap, mint and burn is a transaction this app sent (logs/executor.jsonl, kind
//! "sent"). Each transaction's receipt is read once and cached in logs/ledger.json:
//!   - gas: gasUsed x effectiveGasPrice, plus the L1 data fee on OP-stack chains;
//!   - token movements in and out of the wallet (ERC-20 Transfer logs).
//! Per chain: earned = what the claims paid in + what is claimable now; swap cost = value given
//! away by swaps (out minus in); profit = earned - gas - swap cost. Tokens are valued at their price
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
            if d.get("ts").is_none() {
                d["ts"] = e["ts"].clone();
                changed = true;
            }
            continue;
        }
        let cid = e["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
        todo.entry(cid).or_default().push((h, e["what"].as_str().unwrap_or("").to_string(), e["ts"].clone()));
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
                        d["ts"] = ts.clone();
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
                let (inn, out_) = (val(true), val(false));
                if inn == 0.0 && out_ == 0.0 && !moves.is_empty() {
                    unpriced += 1;
                }
                swap_cost += out_ - inn;
            }
        }
        let claimable = crate::claim::claimable(cid, &wallet)["usd"].as_f64().unwrap_or(0.0);
        let gas_usd = native_usd.map(|p| gas * p);
        let earned = claimed + claimable;
        let profit = earned - gas_usd.unwrap_or(0.0) - swap_cost;
        out.insert(
            cid.to_string(),
            json!({"earned_usd": earned, "claimed_usd": claimed, "claimable_usd": claimable, "gas_native": gas,
                   "gas_symbol": ch.gas_symbol, "gas_usd": gas_usd, "swap_cost_usd": swap_cost, "profit_usd": profit,
                   "txs": txs, "claims": claims, "swaps": swaps, "unpriced_swaps": unpriced}),
        );
    }
    let v = Value::Object(out);
    *LAST.lock().unwrap() = Some((Instant::now(), v.clone()));
    v
}
