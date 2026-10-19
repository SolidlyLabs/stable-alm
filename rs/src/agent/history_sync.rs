//! The 45-day local store (plan section 6): the strategy gate evaluates over up to 45 days plus
//! lookbacks of up to 370 h, so the managed pools and about 8 candidates keep 61 days of swaps in the
//! shared event store (history.rs), filled from the Solidly UI data processor (RPC only for the
//! minutes it has not synced). Managed pools are checked every 6 hours, candidates daily; a pool
//! already covered asks for nothing. The event store prunes nothing, so backtests keep everything.
//!
//! What each pass did is kept in data/agent/history_sync.json, with the size on disk per pool.

use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const DAYS: i64 = 61;
const MANAGED_EVERY_S: i64 = 6 * 3600;
const CANDIDATES_EVERY_S: i64 = 24 * 3600;
pub const CANDIDATES: usize = 8;

fn dir_bytes(p: &std::path::Path) -> u64 {
    std::fs::read_dir(p).map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum()).unwrap_or(0)
}

/// The candidate pools: the market list's pegged pairs with the best 24 h APR on the chains already
/// used, not configured, with at least $10k TVL.
pub fn candidates(wallet: &str) -> Vec<(u64, String, String)> {
    let conf: Vec<(u64, String)> = crate::settings::load_pools().iter()
        .map(|p| (crate::worker::pool_chain(p), p["pool"].as_str().unwrap_or("").to_lowercase())).collect();
    let chains: Vec<u64> = conf.iter().map(|c| c.0).collect();
    let Ok(rows) = crate::market_data::shortlist(wallet, &conf) else { return vec![] };
    let mut rows: Vec<&Value> = rows.iter()
        .filter(|r| chains.contains(&r["chain_id"].as_u64().unwrap_or(0)))
        .filter(|r| !conf.iter().any(|c| Some(c.1.as_str()) == r["pool"].as_str()))
        // pegged pairs (what the app's strategies are built for) with real liquidity
        .filter(|r| r["pegged"] == true && r["tvl_usd"].as_f64().unwrap_or(0.0) >= 10_000.0)
        .collect();
    let apr = |r: &Value| r["processor_apr_24h"].as_f64().unwrap_or(0.0);
    rows.sort_by(|a, b| apr(b).partial_cmp(&apr(a)).unwrap_or(std::cmp::Ordering::Equal));
    rows.into_iter().take(CANDIDATES).filter_map(|r| {
        let name = format!("{}/{}", r["token0"]["symbol"].as_str().unwrap_or("?"), r["token1"]["symbol"].as_str().unwrap_or("?"));
        Some((r["chain_id"].as_u64()?, r["pool"].as_str()?.to_lowercase(), name))
    }).collect()
}

/// One pass: what is due is topped up to 61 days.
pub fn pass(force: bool) -> Value {
    let now = super::now();
    let mut state = super::read_json("history_sync.json", json!({}));
    let wallet = crate::settings::load_settings()["wallet"].as_str().unwrap_or("").to_lowercase();
    let mut groups: BTreeMap<(u64, bool), Vec<(String, String)>> = BTreeMap::new();
    for p in crate::settings::load_pools() {
        let addr = p["pool"].as_str().unwrap_or("").to_lowercase();
        groups.entry((crate::worker::pool_chain(&p), true)).or_default().push((addr, p["name"].as_str().unwrap_or("").to_string()));
    }
    for (cid, addr, name) in candidates(&wallet) {
        groups.entry((cid, false)).or_default().push((addr, name));
    }
    let from = now - DAYS * 86_400;
    for ((cid, managed), pools) in groups {
        let every = if managed { MANAGED_EVERY_S } else { CANDIDATES_EVERY_S };
        let due: Vec<String> = pools.iter()
            .filter(|(a, _)| force || now - state[a]["at"].as_i64().unwrap_or(0) >= every)
            .map(|(a, _)| a.clone()).collect();
        if due.is_empty() {
            continue;
        }
        let r = crate::history::ensure_until(cid, &due, from, None);
        for (a, name) in &pools {
            if !due.contains(a) {
                continue;
            }
            let cov = crate::history::coverage(cid, a);
            state[a] = json!({
                "chain_id": cid, "name": name, "managed": managed, "at": now,
                "ok": r.is_ok(), "error": r.as_ref().err(), "requests": r.as_ref().ok().map(|x| x.requests),
                "from_ts": cov.as_ref().map(|c| c.from_ts), "to_ts": cov.as_ref().map(|c| c.to_ts),
                "days": cov.as_ref().map(|c| (c.to_ts - c.from_ts) as f64 / 86_400.0),
                "bytes": dir_bytes(&crate::history::pool_dir(cid, a)),
            });
        }
    }
    let _ = super::write_json("history_sync.json", &state);
    state
}

pub fn status() -> Value {
    super::read_json("history_sync.json", json!({}))
}

pub fn start() {
    std::thread::Builder::new().name("history-sync".into()).spawn(|| {
        // the worker's first cycle comes first
        std::thread::sleep(std::time::Duration::from_secs(120));
        loop {
            if super::enabled() {
                let _ = std::panic::catch_unwind(|| pass(false));
            }
            std::thread::sleep(std::time::Duration::from_secs(1800));
        }
    }).ok();
}
