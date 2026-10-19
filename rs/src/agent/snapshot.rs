//! The per-review snapshot (plan section 7): everything the model sees at the start of a review,
//! built by code, in blocks, kept under about 30k tokens (the long tails are left to the tools).
//!
//! | Block | What |
//! |---|---|
//! | account | capital per pool and Unallocated, exposure and the limits it breaks, gas per chain |
//! | pools | per configured pool: strategy, geometry, regime, activity, F/LVR, performance, caps |
//! | market | exchange mids and their health per token, funding and open interest |
//! | liquidity | Liquidity Lens reference prices per token (placeholder data says so) |
//! | candidates | the candidate pools with local history |
//! | safety | risk controller's last findings, locks, emergency, the write gate's heartbeat |
//! | plans | the last 10 plans and their states; the policy limits |
//! | memory | the agent's own notes (write_note), newest last |
//!
//! The snapshot's hash and the account version (a hash of pools.json and the capital book) are
//! stored with every plan made from it.

use serde_json::{json, Value};

const MAX_CHARS: usize = 110_000;

fn trim(v: &Value, max: usize) -> Value {
    let s = v.to_string();
    if s.len() <= max {
        return v.clone();
    }
    json!({"truncated": true, "chars": s.len(), "head": s.chars().take(max).collect::<String>()})
}

/// A hash of what a plan's changes are applied to (the pools' settings and the capital book).
pub fn account_version() -> String {
    let pools = crate::settings::load_pools();
    let live = crate::settings::load_settings()["execute"].as_bool().unwrap_or(false);
    let book = crate::capital::load(live).to_json();
    super::sha256_hex(&json!({"pools": pools, "book": book}).to_string())[..16].to_string()
}

fn tokens(cid: u64, pool: &str) -> Option<[(String, String); 2]> {
    let (t0, t1) = crate::chain::pool_tokens(cid, pool).ok()?;
    let s0 = crate::prices::meta(cid, &t0).ok()?.symbol;
    let s1 = crate::prices::meta(cid, &t1).ok()?.symbol;
    Some([(t0.to_lowercase(), s0), (t1.to_lowercase(), s1)])
}

/// Build the snapshot: {snapshot_id, ts, hash, account_version, prices, blocks}.
pub fn build(worker: &crate::worker::Worker) -> Value {
    let x = super::settings();
    let now = super::now();
    let st = worker.status.lock().unwrap().clone();
    let wallet = crate::settings::load_settings()["wallet"].as_str().unwrap_or("").to_string();
    let pools = crate::settings::load_pools();
    let mut prices = serde_json::Map::new();
    let mut symbols: Vec<String> = vec![];
    let mut chains: Vec<u64> = vec![];
    let mut tok_list: Vec<crate::llens::Token> = vec![];
    let mut pool_rows = vec![];
    for p in &pools {
        let cid = crate::worker::pool_chain(p);
        let addr = p["pool"].as_str().unwrap_or("").to_lowercase();
        if !chains.contains(&cid) {
            chains.push(cid);
        }
        if let Some(ts) = tokens(cid, &addr) {
            for (t, s) in ts {
                let px = crate::prices::usd(cid, &t).ok();
                if let Some(u) = px {
                    prices.insert(format!("{cid}:{t}"), json!(u));
                }
                if !symbols.contains(&s) {
                    symbols.push(s.clone());
                }
                if !tok_list.iter().any(|k| k.chain == cid && k.address == t) {
                    tok_list.push(crate::llens::Token { chain: cid, address: t, symbol: s, usd_hint: px });
                }
            }
        }
        let name = p["name"].as_str().unwrap_or("");
        let acct = st["account"]["pools"][name].clone();
        let mut f = super::features::pool(cid, p, &acct);
        f["capital_usd"] = json!(super::exec::capital_of(worker, &addr));
        f["caps"] = super::gate::caps(cid, &addr, super::exec::capital_of(worker, &addr).unwrap_or(0.0), p["max_actions_per_day"].as_i64().unwrap_or(5), &x);
        f["settings"] = json!({"max_swap_cost_bps": p["max_swap_cost_bps"], "slippage_bps": p["slippage_bps"], "max_spot_dev_ticks": p["max_spot_dev_ticks"],
                               "max_actions_per_day": p["max_actions_per_day"], "allocation_pct": p["allocation_pct"]});
        // what leaving would cost: half the capital sold to the safe asset, now and in a thin market
        if let (Some(cap), Some([(t0, _), (t1, _)])) = (super::exec::capital_of(worker, &addr).filter(|c| *c > 10.0), tokens(cid, &addr)) {
            let safe = super::exposure::safe_assets(cid).into_iter().next().unwrap_or_default();
            let sell = if t0 == safe { t1 } else { t0 };
            if sell != safe {
                f["exit_cost"] = super::refdata::scenarios(cid, &sell, &safe, cap / 2.0);
            }
        }
        if let Some((pp, s0, s1)) = super::risk::pool_price(cid, &addr) {
            f["pool_vs_cex"] = super::refdata::pool_vs_cex(pp, &s0, &s1);
        }
        pool_rows.push(f);
    }
    let holdings = super::exposure::holdings(&st["account"], &st["capital"]);
    let safe: Vec<String> = chains.iter().filter_map(|c| super::exposure::safe_assets(*c).into_iter().next()).collect();
    let sum = super::exposure::summary(&holdings, &safe);
    let account = json!({
        "capital": st["capital"], "exposure": sum, "breaches": super::exposure::breaches(&sum, &x["limits"]),
        "gas": chains.iter().map(|c| super::refdata::gas(*c, &wallet)).collect::<Vec<_>>(),
        "execute": crate::settings::load_settings()["execute"], "paused": crate::paths::execution_blocked(),
    });
    let market = json!({
        "mids": symbols.iter().map(|s| { let m = super::refdata::mids(s); json!({"symbol": s, "mids": m, "health": super::refdata::health(s, &x)}) }).collect::<Vec<_>>(),
        "funding": super::refdata::funding(),
    });
    let liquidity = if crate::llens::mode() == crate::llens::Mode::Off {
        json!({"off": true})
    } else {
        json!({"note": "Liquidity Lens is optional on-chain data; it never outvotes the exchanges; placeholder data is not decision grade",
               "reference": tok_list.iter().map(|t| json!({"symbol": t.symbol, "chain_id": t.chain, "ref": crate::llens::reference(t).unwrap_or_else(|e| json!({"error": e}))})).collect::<Vec<_>>()})
    };
    let candidates: Vec<Value> = super::history_sync::candidates(&wallet).into_iter().map(|(cid, pool, name)| {
        let cov = crate::history::coverage(cid, &pool).map(|c| (c.to_ts - c.from_ts) as f64 / 86_400.0);
        json!({"chain_id": cid, "pool": pool, "name": name, "history_days": cov})
    }).collect();
    let safety = json!({
        "risk_last": super::risk::last(), "locks": super::locks::all(), "emergency": super::locks::emergency(),
        "heartbeat_age_s": super::gate::heartbeat_age(), "risk_mode": x["risk"]["mode"], "suspect_sources": super::risk::suspects(),
    });
    let plans: Vec<Value> = super::plan::all().into_iter().rev().take(10).map(|p| json!({
        "plan_id": p["plan_id"], "created": p["created"], "state": p["state"], "summary": p["summary"],
        "changes": p["changes"].as_array().map(|a| a.iter().map(|c| json!({"type": c["type"], "pool": c["pool"], "target_usd": c["target_usd"], "invalidation": c["invalidation"]})).collect::<Vec<_>>()),
        "failures": p["failures"], "invalidated": p["invalidated"],
    })).collect();
    let memory: Vec<Value> = super::tail("notes.jsonl", 30);
    let now_utc = { let (y, mo, d, h, mi, _) = crate::log::civil(now); format!("{y}-{mo:02}-{d:02} {h:02}:{mi:02} UTC") };
    let blocks = json!({
        "now": now, "now_utc": now_utc,
        "account": trim(&account, 20_000),
        "pools": trim(&json!(pool_rows), 45_000),
        "market": trim(&market, 12_000),
        "liquidity": trim(&liquidity, 8_000),
        "candidates": trim(&json!(candidates), 6_000),
        "safety": trim(&safety, 8_000),
        "plans": trim(&json!({"recent": plans, "limits": x["limits"], "execution": x["execution"], "mode": x["agent"]["mode"]}), 8_000),
        "memory": trim(&json!(memory), 6_000),
    });
    let mut body = blocks;
    if body.to_string().len() > MAX_CHARS {
        body["pools"] = trim(&body["pools"], 25_000);
    }
    let hash = super::sha256_hex(&body.to_string());
    json!({"snapshot_id": format!("s{now}-{}", &hash[..8]), "ts": now, "hash": hash, "account_version": account_version(),
           "prices": Value::Object(prices), "blocks": body})
}
