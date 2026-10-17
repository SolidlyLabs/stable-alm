//! Fee vs loss to arbitrage (F/LVR, trailing 7 days) for every pool of a chain, for Discover pairs.
//!
//! One eth_getLogs per 10,000 blocks covers all of the chain's Solidly pools at once (Swap topic, the
//! pools' addresses as the filter). The last 7 days of swaps are kept in memory and topped up from
//! the last block seen, in a background thread started by the worker cycle, so a refresh costs one
//! call per cycle after a ~7-call backfill. Configured pools keep their own figure (stored events
//! and the processor's hourly fee rate); here each swap pays the pool's current fee.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::data::{self, Ev};

const WINDOW_S: i64 = 7 * 86400;
const CHUNK: i64 = 10_000;

#[derive(Default)]
struct ChainSwaps {
    last_block: i64,
    pools: HashSet<String>,
    swaps: HashMap<String, Vec<Ev>>,
}

static STORE: Mutex<Option<HashMap<u64, ChainSwaps>>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);

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
    let pools: HashSet<String> =
        crate::market_data::pools(cid, wallet)?.iter().filter_map(|p| p["pool"].as_str().map(|s| s.to_lowercase())).collect();
    if pools.is_empty() {
        return Ok(());
    }
    let head = crate::rpc::block_number(cid)? as i64;
    let window_blocks = crate::chains::blocks_per_day(cid) * 7;
    let (from, known) = {
        let mut g = STORE.lock().unwrap();
        let st = g.get_or_insert_with(HashMap::new).entry(cid).or_default();
        // A new pool has no history in the store yet: start the window over, so every pool covers 7 days.
        let restart = st.last_block == 0 || !pools.is_subset(&st.pools);
        (if restart { head - window_blocks } else { st.last_block + 1 }, !restart)
    };
    if from > head {
        return Ok(());
    }
    let addrs: Vec<&String> = pools.iter().collect();
    let mut fresh: Vec<(String, Ev)> = vec![];
    let mut a = from;
    while a <= head {
        let b = (a + CHUNK - 1).min(head);
        fetch(cid, &addrs, a, b, &mut fresh)?;
        a = b + 1;
    }
    let cutoff = crate::engine::now() as i64 - WINDOW_S;
    let mut g = STORE.lock().unwrap();
    let st = g.get_or_insert_with(HashMap::new).entry(cid).or_default();
    if !known {
        st.swaps.clear();
    }
    for (pool, e) in fresh {
        st.swaps.entry(pool).or_default().push(e);
    }
    for v in st.swaps.values_mut() {
        v.retain(|e| e.ts >= cutoff);
    }
    st.swaps.retain(|_, v| !v.is_empty());
    st.last_block = head;
    st.pools = pools;
    Ok(())
}

/// Swap logs for all `addrs` in [a, b]; a range the RPC refuses (too many results) is split in two.
fn fetch(cid: u64, addrs: &[&String], a: i64, b: i64, out: &mut Vec<(String, Ev)>) -> Result<(), String> {
    let p = json!([{"address": addrs, "fromBlock": format!("0x{a:x}"), "toBlock": format!("0x{b:x}"), "topics": [data::SWAP]}]);
    match crate::rpc::call(cid, "eth_getLogs", p)? {
        Ok(Value::Array(logs)) => {
            for l in &logs {
                let pool = l["address"].as_str().unwrap_or("").to_lowercase();
                out.push((pool, data::decode(cid, l)));
            }
            Ok(())
        }
        r if b - a < 10 => Err(format!("getLogs failed for {a}-{b}: {r:?}")),
        _ => {
            let m = (a + b) / 2;
            fetch(cid, addrs, a, m, out)?;
            fetch(cid, addrs, m + 1, b, out)
        }
    }
}

/// The pool's trailing-7-day F/LVR at its current fee (`fee_pips`, millionths), or None until the
/// chain's swaps have been downloaded. Token1 is the numeraire: the ratio is the same in either token.
pub fn flvr(cid: u64, pool: &str, d0: i32, d1: i32, fee_pips: f64) -> Option<Value> {
    let g = STORE.lock().unwrap();
    let st = g.as_ref()?.get(&cid)?;
    if st.last_block == 0 {
        return None;
    }
    let since = crate::engine::now() as i64 - WINDOW_S;
    let none = || json!({"ratio": null, "swaps": 0, "since": since, "basis": "current fee", "error": "no swaps in the last 7 days"});
    let Some(sw) = st.swaps.get(&pool.to_lowercase()) else { return Some(none()) };
    let rate = fee_pips / 1e6;
    let first = sw.iter().map(|e| e.ts).min()?.div_euclid(3600) * 3600;
    let apr: HashMap<i64, (f64, f64)> = (0..=(WINDOW_S / 3600 + 1)).map(|h| (first + h * 3600, (rate, 0.0))).collect();
    let v = match data::fee_vs_lvr(sw, &apr, d0, d1, 1, since) {
        Some(x) => {
            let mut v = x.to_json(since);
            v["basis"] = json!("current fee");
            v
        }
        None => none(),
    };
    Some(v)
}
