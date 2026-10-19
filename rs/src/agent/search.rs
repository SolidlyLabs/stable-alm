//! The search ledger (plan section 9 and section 17, 9a): code records every strategy the agent
//! tries on a pool, so "found by search" is counted rather than declared by the plan.
//!
//! | Source | Recorded when | Configurations |
//! |---|---|---|
//! | backtest | the `backtest` tool runs a strategy | 1 |
//! | optimize | the `optimize` tool runs the auto optimizer | the backtests it ran |
//! | gate | the strategy gate checks a submitted strategy | 1 |
//!
//! data/agent/search.jsonl, one line each: {ts, chain_id, pool, source, strategy_hash, configs}.
//! The gate flags a pool with more than `FLAG_CANDIDATES` distinct strategies in `WINDOW_DAYS`: the
//! winner of many tries looks better than it is, so the plan waits for approval.

use serde_json::{json, Value};

const FILE: &str = "search.jsonl";
pub const WINDOW_DAYS: i64 = 7;
pub const FLAG_CANDIDATES: usize = 5;

pub fn record(cid: u64, pool: &str, source: &str, strategy: &Value, configs: u64) {
    super::append(FILE, &json!({"ts": super::now(), "chain_id": cid, "pool": pool.to_lowercase(), "source": source,
                                "strategy_hash": super::plan::strategy_hash(strategy), "configs": configs.max(1)}));
}

/// Distinct strategies and configurations tried on `pool` since `since` (and in all, on any pool).
pub fn stats_of(rows: &[Value], pool: &str, since: i64) -> Value {
    let pool = pool.to_lowercase();
    let mut hashes: Vec<&str> = vec![];
    let (mut configs, mut all_configs) = (0u64, 0u64);
    for r in rows {
        let n = r["configs"].as_u64().unwrap_or(1);
        all_configs += n;
        if r["ts"].as_i64().unwrap_or(0) < since || !r["pool"].as_str().map(|p| p.eq_ignore_ascii_case(&pool)).unwrap_or(false) {
            continue;
        }
        configs += n;
        if let Some(h) = r["strategy_hash"].as_str() {
            if !hashes.contains(&h) {
                hashes.push(h);
            }
        }
    }
    json!({"candidates": hashes.len(), "configs": configs, "days": WINDOW_DAYS, "all_time_configs": all_configs,
           "found_by_search": hashes.len() > FLAG_CANDIDATES})
}

/// The pool's search over the last `WINDOW_DAYS`.
pub fn stats(pool: &str) -> Value {
    stats_of(&super::tail(FILE, 20_000), pool, super::now() - WINDOW_DAYS * 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_distinct_strategies_in_the_window() {
        let row = |ts: i64, pool: &str, h: &str, n: u64| json!({"ts": ts, "pool": pool, "strategy_hash": h, "configs": n});
        let rows = vec![row(100, "0xA", "h1", 1), row(200, "0xa", "h1", 1), row(200, "0xa", "h2", 40), row(50, "0xa", "h3", 1),
                        row(200, "0xb", "h4", 1), row(300, "0xa", "h5", 1), row(300, "0xa", "h6", 1), row(300, "0xa", "h7", 1), row(300, "0xa", "h8", 1)];
        let s = stats_of(&rows, "0xA", 100);
        assert_eq!(s["candidates"], json!(6), "h1 h2 h5 h6 h7 h8; h3 is too old, h4 another pool");
        assert_eq!(s["configs"], json!(46));
        assert_eq!(s["all_time_configs"], json!(48));
        assert_eq!(s["found_by_search"], json!(true));
        assert_eq!(stats_of(&rows, "0xb", 0)["found_by_search"], json!(false));
    }
}
