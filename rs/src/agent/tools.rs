//! The agent's tools (plan section 8). Every tool is bounded: it reads, or it ends the review with a
//! decision that code then checks. Nothing here signs or moves funds.
//!
//! | Tool | Kind | What |
//! |---|---|---|
//! | get_pool | read | one pool's full snapshot block, its 24 h / 7 d / 30 d performance |
//! | get_price_history | read | hourly closes of a pool's tick from the local store (at most 14 days) |
//! | get_quote | market | the aggregator's swap cost for an amount (cached 60 s) |
//! | get_liquidity | market | Liquidity Lens data for a token or pool (placeholder data says so) |
//! | get_journal | read | the last decision records |
//! | backtest | research | a strategy on a pool against its current one and holding (slow) |
//! | web_search | research | the API's server-side search, at most `agent.searches_per_review` |
//! | write_note | memory | a note for the next reviews (kept 30) |
//! | hold | decision | no change, with the reason |
//! | escalate | decision | ask the user to look (an alert), with the reason |
//! | submit_plan | decision | a plan; the policy gate decides what happens to it |
//!
//! Routine reviews get no submit_plan.

use serde_json::{json, Value};

pub const DECISIONS: &[&str] = &["hold", "escalate", "submit_plan"];

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

fn change_schema() -> Value {
    obj(json!({
        "type": {"type": "string", "enum": ["strategy", "capital", "add_pool", "exit_pool"]},
        "pool": {"type": "string", "description": "the pool's 0x address"},
        "chain_id": {"type": ["integer", "null"], "description": "required for add_pool"},
        "target_usd": {"type": ["number", "null"], "description": "capital: the pool's capital after the change"},
        "start_usd": {"type": ["number", "null"], "description": "add_pool: its start capital"},
        "strategy_json": {"type": ["string", "null"], "description": "strategy and add_pool: the strategy object as JSON, in the Lab's format"},
        "name": {"type": ["string", "null"]},
        "thesis": {"type": "string", "description": "why, in one or two sentences, with the numbers it rests on"},
        "invalidation": obj(json!({
            "metric": {"type": "string", "enum": super::sched::METRICS},
            "source": {"type": "string"},
            "op": {"type": "string", "enum": ["<", ">"]},
            "threshold": {"type": "number"},
            "persist_minutes": {"type": "number"},
            "action": {"type": "string", "enum": ["exit_to_safe", "revert_strategy", "withdraw_to_unallocated", "alert"]},
        }), &["metric", "source", "op", "threshold", "persist_minutes", "action"]),
    }), &["type", "pool", "chain_id", "target_usd", "start_usd", "strategy_json", "name", "thesis", "invalidation"])
}

/// The tool list for a review (`routine`: no submit_plan).
pub fn definitions(x: &Value, routine: bool) -> Vec<Value> {
    let tool = |name: &str, desc: &str, schema: Value| json!({"name": name, "description": desc, "input_schema": schema, "strict": true});
    let mut t = vec![
        tool("get_pool", "One configured pool's full data: strategy, geometry, regime, activity, F/LVR, caps, performance (24 h, 7 d, 30 d).",
             obj(json!({"pool": {"type": "string"}}), &["pool"])),
        tool("get_price_history", "Hourly closes (ticks and price) of a pool from the local event store; at most 336 hours.",
             obj(json!({"pool": {"type": "string"}, "chain_id": {"type": "integer"}, "hours": {"type": "integer"}}), &["pool", "chain_id", "hours"])),
        tool("get_quote", "The aggregator's cost in bp of swapping a USD amount of one token into another (Solidly UI aggregator; cached 60 s).",
             obj(json!({"chain_id": {"type": "integer"}, "token_in": {"type": "string"}, "token_out": {"type": "string"}, "usd": {"type": "number"}}),
                 &["chain_id", "token_in", "token_out", "usd"])),
        tool("get_liquidity", "Liquidity Lens on-chain data: kind depth, quotes, token_health (a token) or traffic, competing, ranges, depth_value (a pool). Optional data that never outvotes the exchanges; placeholder data is labelled and not decision grade.",
             obj(json!({"kind": {"type": "string", "enum": ["depth", "quotes", "token_health", "traffic", "competing", "ranges", "depth_value"]},
                        "chain_id": {"type": "integer"}, "address": {"type": "string", "description": "the token (depth, quotes, token_health) or the pool"}}),
                 &["kind", "chain_id", "address"])),
        tool("get_journal", "The last decision records (reviews, plans, risk actions, approvals).",
             obj(json!({"n": {"type": "integer"}, "kinds": {"type": "array", "items": {"type": "string"}}}), &["n", "kinds"])),
        tool("backtest", "Backtest a strategy on a pool over the last N days (1-90) against its current strategy and holding, with the pool's costs. Takes minutes; use sparingly.",
             obj(json!({"chain_id": {"type": "integer"}, "pool": {"type": "string"}, "days": {"type": "integer"}, "strategy_json": {"type": "string"}, "capital_usd": {"type": "number"}}),
                 &["chain_id", "pool", "days", "strategy_json", "capital_usd"])),
        tool("write_note", "Keep a short note for the next reviews (what to watch, what was tried). Not a decision.",
             obj(json!({"text": {"type": "string"}}), &["text"])),
        tool("hold", "End the review with no change.", obj(json!({"reason": {"type": "string"}}), &["reason"])),
        tool("escalate", "End the review by asking the user to look at something.",
             obj(json!({"reason": {"type": "string"}, "severity": {"type": "string", "enum": ["normal", "severe"]}}), &["reason", "severity"])),
    ];
    if !routine {
        t.push(tool("submit_plan", "End the review with a plan. Code checks it against the limits and the strategy gate; it may be rejected or wait for approval.",
            obj(json!({
                "summary": {"type": "string"},
                "changes": {"type": "array", "items": change_schema()},
                "requested_bounds": obj(json!({"max_cost_bp": {"type": ["number", "null"]}, "plan_age_h": {"type": ["number", "null"]},
                                              "price_move_pct": {"type": ["number", "null"]}, "quote_age_s": {"type": ["number", "null"]}}),
                                        &["max_cost_bp", "plan_age_h", "price_move_pct", "quote_age_s"]),
            }), &["summary", "changes", "requested_bounds"])));
    }
    if x["agent"]["web_search"].as_bool().unwrap_or(true) && x["agent"]["searches_per_review"].as_f64().unwrap_or(5.0) >= 1.0 {
        t.push(json!({"type": x["agent"]["web_search_tool"].as_str().unwrap_or("web_search_20260209"), "name": "web_search",
                      "max_uses": x["agent"]["searches_per_review"].as_f64().unwrap_or(5.0) as u64}));
    }
    // the tool list is the cached prefix's end
    if let Some(last) = t.last_mut() {
        last["cache_control"] = json!({"type": "ephemeral"});
    }
    t
}

/// A submit_plan input as a plan body: strategies parsed, nulls dropped.
pub fn plan_body(input: &Value) -> Result<Value, String> {
    let mut changes = vec![];
    for (i, c) in input["changes"].as_array().ok_or("changes must be a list")?.iter().enumerate() {
        let mut o = serde_json::Map::new();
        for (k, v) in c.as_object().ok_or("a change must be an object")? {
            if v.is_null() {
                continue;
            }
            if k == "strategy_json" {
                let s: Value = serde_json::from_str(v.as_str().unwrap_or("")).map_err(|e| format!("change {}: strategy_json is not JSON: {e}", i + 1))?;
                if !s.is_object() {
                    return Err(format!("change {}: strategy_json must be an object", i + 1));
                }
                o.insert("strategy".into(), s);
            } else {
                o.insert(k.clone(), v.clone());
            }
        }
        changes.push(Value::Object(o));
    }
    let mut b = serde_json::Map::new();
    for (k, v) in input["requested_bounds"].as_object().into_iter().flatten() {
        if !v.is_null() {
            b.insert(k.clone(), v.clone());
        }
    }
    Ok(json!({"summary": input["summary"], "changes": changes, "requested_bounds": Value::Object(b)}))
}

fn pool_cfg(addr: &str) -> Option<Value> {
    crate::settings::load_pools().into_iter().find(|p| p["pool"].as_str().map(|a| a.eq_ignore_ascii_case(addr)).unwrap_or(false))
}

fn token_of(cid: u64, addr: &str) -> crate::llens::Token {
    let a = addr.to_lowercase();
    crate::llens::Token { chain: cid, symbol: crate::prices::meta(cid, &a).map(|m| m.symbol).unwrap_or_default(), usd_hint: crate::prices::usd(cid, &a).ok(), address: a }
}

/// Run a read, market, research or memory tool; Err is returned to the model as an error result.
pub fn run(name: &str, input: &Value, worker: &crate::worker::Worker) -> Result<Value, String> {
    let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
    match name {
        "get_pool" => {
            let cfg = pool_cfg(&s("pool")).ok_or("not a configured pool")?;
            let cid = crate::worker::pool_chain(&cfg);
            let acct = worker.status.lock().unwrap()["account"]["pools"][cfg["name"].as_str().unwrap_or("")].clone();
            let mut f = super::features::pool(cid, &cfg, &acct);
            f["performance_30d"] = super::perf::pool(&s("pool"), 30 * 86_400);
            Ok(f)
        }
        "get_price_history" => {
            let cid = input["chain_id"].as_u64().ok_or("chain_id")?;
            let pool = s("pool").to_lowercase();
            let hours = input["hours"].as_i64().unwrap_or(48).clamp(1, 336);
            let now = super::now();
            let swaps: Vec<(i64, i64)> = crate::history::read(cid, &pool, now - hours * 3600 - 3600, now + 3600).into_iter()
                .filter(|e| e.kind == "swap").map(|e| (e.ts, e.tick)).collect();
            let (ts, c, h, l) = super::features::hourly_bars(&swaps, now - hours * 3600, now);
            let rows: Vec<Value> = ts.iter().enumerate().map(|(i, t)| json!([t, c[i], h[i], l[i]])).collect();
            Ok(json!({"columns": ["ts", "close_tick", "high_tick", "low_tick"], "rows": rows, "price_per_tick": "price = 1.0001^tick × 10^(dec0 − dec1)"}))
        }
        "get_quote" => {
            let usd = input["usd"].as_f64().unwrap_or(0.0);
            if !(1.0..=10_000_000.0).contains(&usd) {
                return Err("usd must be 1 to 10,000,000".into());
            }
            super::refdata::swap_cost(input["chain_id"].as_u64().ok_or("chain_id")?, &s("token_in").to_lowercase(), &s("token_out").to_lowercase(), usd)
        }
        "get_liquidity" => {
            let cid = input["chain_id"].as_u64().ok_or("chain_id")?;
            let a = s("address").to_lowercase();
            let now = super::now();
            match s("kind").as_str() {
                "depth" => crate::llens::depth(&token_of(cid, &a)),
                "quotes" => crate::llens::quotes(&token_of(cid, &a), "sell", &[1e3, 1e4, 1e5]),
                "token_health" => crate::llens::token_health(&token_of(cid, &a), 1e4),
                "traffic" => crate::llens::traffic(cid, &a, now - 7 * 86_400, now),
                "competing" => {
                    let (t0, t1) = crate::chain::pool_tokens(cid, &a)?;
                    crate::llens::competing(cid, &t0, &t1)
                }
                "ranges" => crate::llens::ranges(cid, &a),
                "depth_value" => crate::llens::depth_value(cid, &a),
                k => Err(format!("unknown kind {k}")),
            }
        }
        "get_journal" => {
            let kinds: Vec<String> = input["kinds"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(String::from)).collect();
            let k: Vec<&str> = kinds.iter().map(|s| s.as_str()).collect();
            Ok(json!(super::journal::recent(input["n"].as_u64().unwrap_or(20).clamp(1, 100) as usize, &k)))
        }
        "backtest" => {
            let strategy: Value = serde_json::from_str(&s("strategy_json")).map_err(|e| format!("strategy_json is not JSON: {e}"))?;
            let cid = input["chain_id"].as_u64().ok_or("chain_id")?;
            let pool = s("pool").to_lowercase();
            let days = input["days"].as_i64().unwrap_or(30).clamp(1, 90);
            let current = pool_cfg(&pool).map(|p| p["strategy"].clone());
            let wallet = crate::settings::load_settings()["wallet"].as_str().unwrap_or("").to_string();
            let compare = current.map(|c| json!([{"id": "current", "name": "Current", "strategy": c}])).unwrap_or(json!(false));
            let req = json!({"chain_id": cid, "pool": pool, "days": days, "capital_usd": input["capital_usd"].as_f64().unwrap_or(1000.0).max(10.0),
                             "strategy": strategy, "compare": compare, "hold": true});
            let id = crate::backtest::start(req, wallet)?;
            let until = std::time::Instant::now() + std::time::Duration::from_secs(900);
            loop {
                let j = crate::backtest::job(id).ok_or("the backtest disappeared")?;
                if j["state"] == "done" {
                    let r = &j["result"];
                    let brief = |v: &Value| json!({"pnl": v["pnl"], "vs_hold": v["vs_hodl"], "fees": v["fees"], "costs": v["costs"], "actions": v["actions"],
                                                  "time_in_range": v["time_in_range"], "spot_blocked_bars": v["spot_blocked_bars"]});
                    return Ok(json!({"days": r["span_days"], "capital": r["runs"]["main"]["start_value"], "new": brief(&r["runs"]["main"]),
                                     "current": r["runs"]["compare"].as_array().and_then(|a| a.first()).map(brief), "hold": brief(&r["runs"]["hold"]),
                                     "units": "the numeraire token of the pool"}));
                }
                if j["state"] == "error" {
                    return Err(j["error"].as_str().unwrap_or("backtest failed").into());
                }
                if std::time::Instant::now() > until {
                    return Err("the backtest took too long".into());
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
        "write_note" => {
            let t: String = s("text").chars().take(1000).collect();
            super::append("notes.jsonl", &json!({"ts": super::now(), "text": t}));
            let all = super::tail("notes.jsonl", 30);
            let _ = std::fs::write(super::dir().join("notes.jsonl"), all.iter().map(|v| v.to_string() + "\n").collect::<String>());
            Ok(json!({"ok": true}))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_bodies() {
        let i = json!({"summary": "s", "requested_bounds": {"max_cost_bp": 10.0, "plan_age_h": null},
                       "changes": [{"type": "strategy", "pool": "0xab", "chain_id": null, "strategy_json": "{\"type\":\"custom\",\"width\":10}", "thesis": "t"}]});
        let b = plan_body(&i).unwrap();
        assert_eq!(b["changes"][0]["strategy"]["width"], json!(10));
        assert!(b["changes"][0].get("chain_id").is_none());
        assert_eq!(b["requested_bounds"], json!({"max_cost_bp": 10.0}));
        assert!(plan_body(&json!({"changes": [{"strategy_json": "nope"}]})).is_err());
        let d = definitions(&super::super::defaults(), true);
        assert!(!d.iter().any(|t| t["name"] == "submit_plan"));
        assert!(d.last().unwrap()["cache_control"].is_object());
    }
}
