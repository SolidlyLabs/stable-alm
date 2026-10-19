//! Experimental features (Settings → Experimental features, off by default): the groundwork of
//! the agent plan (docs/AGENT_PLAN.md), shown on the dashboard's Experimental tab and used by
//! nothing that trades.
//!
//! Today: the Liquidity Lens data the agent would read (placeholder data, llens.rs) for the
//! configured pools and their tokens, and the local cache's statistics (cache.rs).

use serde_json::{json, Value};

use crate::llens::{self, Token};

pub fn enabled(s: &Value) -> bool {
    s["experimental"]["enabled"].as_bool().unwrap_or(false)
}

/// Shown when a page asks for experimental data while the switch is off.
pub const OFF: &str = "experimental features are off (Settings → Experimental features)";

fn err(e: String) -> Value {
    json!({"error": e})
}

/// Everything the Experimental tab shows.
pub fn overview(s: &Value) -> Value {
    let now = crate::engine::now() as i64;
    let pools = crate::settings::load_pools();
    let mut tokens: Vec<Token> = vec![];
    let mut pool_rows = vec![];
    let mut chains: Vec<u64> = vec![];
    for p in &pools {
        let cid = crate::worker::pool_chain(p);
        let addr = p["pool"].as_str().unwrap_or("").to_lowercase();
        let name = p["name"].as_str().unwrap_or(&addr).to_string();
        if !chains.contains(&cid) {
            chains.push(cid);
        }
        let pair = crate::chain::pool_tokens(cid, &addr).ok();
        if let Some((a, b)) = &pair {
            for t in [a, b] {
                if !tokens.iter().any(|x| x.chain == cid && &x.address == t) {
                    let symbol = crate::prices::meta(cid, t).map(|m| m.symbol).unwrap_or_else(|_| t[..10.min(t.len())].to_string());
                    tokens.push(Token { chain: cid, address: t.clone(), symbol, usd_hint: crate::prices::usd(cid, t).ok() });
                }
            }
        }
        let traffic = llens::traffic(cid, &addr, now - 7 * 86_400, now);
        let summary = traffic.as_ref().ok().map(|t| {
            let rows = t["data"].as_array().cloned().unwrap_or_default();
            let vol: f64 = rows.iter().filter_map(|r| r["volume_usd"].as_f64()).sum();
            let w = |k: &str| if vol > 0.0 { rows.iter().map(|r| r[k].as_f64().unwrap_or(0.0) * r["volume_usd"].as_f64().unwrap_or(0.0)).sum::<f64>() / vol } else { 0.0 };
            json!({"volume_7d_usd": vol, "aggregator_share": w("aggregator_share"), "bot_share": w("bot_share"), "organic_share": w("organic_share"),
                   "hours": rows.len(), "fetched": t["fetched"], "stale": t["stale"]})
        });
        let competing = pair.as_ref().map(|(a, b)| llens::competing(cid, a, b).unwrap_or_else(err)).unwrap_or(Value::Null);
        pool_rows.push(json!({
            "chain_id": cid, "pool": addr, "name": name, "enabled": p["enabled"],
            "traffic": summary.unwrap_or_else(|| err(traffic.err().unwrap_or_default())),
            "competing": competing,
            "ranges": llens::ranges(cid, &addr).unwrap_or_else(err),
            "depth_value": llens::depth_value(cid, &addr).unwrap_or_else(err),
        }));
    }
    let token_rows: Vec<Value> = tokens
        .iter()
        .map(|t| {
            json!({
                "chain_id": t.chain, "address": t.address, "symbol": t.symbol, "usd_hint": t.usd_hint,
                "reference": llens::reference(t).unwrap_or_else(err),
                "depth": llens::depth(t).unwrap_or_else(err),
                "sell_quotes": llens::quotes(t, "sell", &[1e3, 1e4, 1e5]).unwrap_or_else(err),
                "health": llens::token_health(t, 1e4).unwrap_or_else(err),
            })
        })
        .collect();
    let candidates: Vec<Value> = chains.iter().map(|c| json!({"chain_id": c, "candidates": llens::candidates(*c).unwrap_or_else(err)})).collect();
    json!({
        "enabled": enabled(s),
        "llens": {"mode": format!("{:?}", llens::mode_of(s)).to_lowercase(), "placeholder": true},
        "tokens": token_rows,
        "pools": pool_rows,
        "chains": candidates,
        "cache": crate::cache::global().stats(now as u64),
    })
}
