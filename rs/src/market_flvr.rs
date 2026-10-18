//! Fee vs loss to arbitrage (F/LVR, trailing 7 days) for every pool of a chain, for Discover pairs.
//!
//! The swaps come from the shared event store (history.rs): a background refresh started by the
//! worker cycle keeps every pool of the chain covered for the last 7 days, in one download per
//! 10,000 blocks for all pools, and only the new blocks after that. Results are computed from the
//! stored swaps and kept until the next refresh. Configured pools keep their own figure (stored
//! events and the processor's hourly fee rate); here each swap pays the pool's current fee.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::data;

const WINDOW_S: i64 = 7 * 86400;

static RUNNING: AtomicBool = AtomicBool::new(false);
/// Refreshes completed per chain; a cached figure from an older generation is recomputed.
static GEN: Mutex<Option<HashMap<u64, u64>>> = Mutex::new(None);
static MEMO: Mutex<Option<HashMap<(u64, String), (u64, u64, Value)>>> = Mutex::new(None);

/// Start a background refresh of the chains' swaps unless one is already running.
pub fn refresh_in_background(chains: Vec<u64>, wallet: String) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        for cid in chains {
            if let Err(e) = refresh(cid, &wallet) {
                crate::warn!("market", "fee vs arb for all pools: refresh failed on chain {cid}: {e}");
            }
        }
        RUNNING.store(false, Ordering::SeqCst);
    });
}

fn refresh(cid: u64, wallet: &str) -> Result<(), String> {
    let pools: Vec<String> =
        crate::market_data::pools(cid, wallet)?.iter().filter_map(|p| p["pool"].as_str().map(|s| s.to_lowercase())).collect();
    if pools.is_empty() {
        return Ok(());
    }
    let from = crate::engine::now() as i64 - WINDOW_S - 3600;
    let rep = crate::history::ensure(cid, &pools, from)?;
    if rep.rows > 0 || rep.requests > 0 {
        crate::info!("market", "chain {cid}: {} pools' events stored up to block {} ({} new rows, {} requests)",
                     pools.len(), rep.to_block, rep.rows, rep.requests);
    }
    *GEN.lock().unwrap().get_or_insert_with(HashMap::new).entry(cid).or_insert(0) += 1;
    Ok(())
}

/// The pool's trailing-7-day F/LVR at its current fee (`fee_pips`, millionths), or None until the
/// pool's swaps have been downloaded. Token1 is the numeraire: the ratio is the same in either token.
pub fn flvr(cid: u64, pool: &str, d0: i32, d1: i32, fee_pips: f64) -> Option<Value> {
    let pool = pool.to_lowercase();
    let now = crate::engine::now() as i64;
    let cov = crate::history::coverage(cid, &pool)?;
    if cov.to_ts < now - 6 * 3600 || cov.from_ts > now - WINDOW_S + 6 * 3600 {
        return None; // not covering the window (yet)
    }
    let gen = GEN.lock().unwrap().get_or_insert_with(HashMap::new).get(&cid).copied().unwrap_or(0);
    let fee_key = fee_pips.to_bits();
    if let Some((g, f, v)) = MEMO.lock().unwrap().get_or_insert_with(HashMap::new).get(&(cid, pool.clone())) {
        if *g == gen && *f == fee_key {
            return Some(v.clone());
        }
    }
    let since = now - WINDOW_S;
    let sw: Vec<data::Ev> = crate::history::read(cid, &pool, since, now + 1).into_iter().filter(|e| e.kind == "swap").collect();
    let none = || json!({"ratio": null, "swaps": 0, "since": since, "basis": "current fee", "error": "no swaps in the last 7 days"});
    let v = if sw.is_empty() {
        none()
    } else {
        let rate = fee_pips / 1e6;
        let first = sw.iter().map(|e| e.ts).min().unwrap_or(since).div_euclid(3600) * 3600;
        let apr: HashMap<i64, (f64, f64)> = (0..=(WINDOW_S / 3600 + 1)).map(|h| (first + h * 3600, (rate, 0.0))).collect();
        match data::fee_vs_lvr(&sw, &apr, d0, d1, 1, since) {
            Some(x) => {
                let mut v = x.to_json(since);
                v["basis"] = json!("current fee");
                v
            }
            None => none(),
        }
    };
    MEMO.lock().unwrap().as_mut().unwrap().insert((cid, pool), (gen, fee_key, v.clone()));
    Some(v)
}
