//! The plan object and its lifecycle (plan section 3), in data/agent/plans.json (the last 300).
//!
//! ```text
//! proposed → validated → awaiting_approval → queued → executing → confirmed
//!               │               │              │          ├────► partially_completed
//!               ▼               ▼              ▼          ├────► failed
//!           rejected   expired / cancelled  expired      └────► superseded
//! ```
//!
//! A submission is keyed by (review_id, tool_use_id) before it is accepted, so a repeated call (a
//! retry, a restart) never makes a second plan. Approval binds to the plan's hash.

use serde_json::{json, Value};
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());
const FILE: &str = "plans.json";
const KEEP: usize = 300;

#[allow(dead_code)]
pub const STATES: &[&str] = &["proposed", "validated", "rejected", "awaiting_approval", "queued", "executing", "confirmed",
                              "partially_completed", "failed", "superseded", "expired", "cancelled"];

/// Allowed transitions.
pub fn can_move(from: &str, to: &str) -> bool {
    matches!((from, to),
        ("proposed", "validated") | ("proposed", "rejected")
        | ("validated", "awaiting_approval") | ("validated", "queued") | ("validated", "expired")
        | ("awaiting_approval", "queued") | ("awaiting_approval", "expired") | ("awaiting_approval", "cancelled") | ("awaiting_approval", "rejected")
        | ("queued", "executing") | ("queued", "expired") | ("queued", "cancelled") | ("queued", "superseded")
        | ("executing", "confirmed") | ("executing", "partially_completed") | ("executing", "failed") | ("executing", "superseded"))
}

pub fn terminal(state: &str) -> bool {
    matches!(state, "confirmed" | "partially_completed" | "failed" | "superseded" | "expired" | "cancelled" | "rejected")
}

fn load() -> Vec<Value> {
    super::read_json(FILE, json!([])).as_array().cloned().unwrap_or_default()
}

fn save(v: &[Value]) {
    let n = v.len();
    let _ = super::write_json(FILE, &json!(v[n.saturating_sub(KEEP)..]));
}

pub fn all() -> Vec<Value> {
    load()
}

pub fn get(id: &str) -> Option<Value> {
    load().into_iter().find(|p| p["plan_id"] == id)
}

/// The hash approval binds to: the plan's changes and bounds as submitted.
pub fn hash(changes: &Value, bounds: &Value) -> String {
    super::sha256_hex(&json!({"changes": changes, "requested_bounds": bounds}).to_string())
}

/// A canonical hash of a strategy object (keys sorted).
pub fn strategy_hash(s: &Value) -> String {
    fn canon(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                let mut o = serde_json::Map::new();
                for k in keys {
                    o.insert(k.clone(), canon(&m[k]));
                }
                Value::Object(o)
            }
            Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
            x => x.clone(),
        }
    }
    super::sha256_hex(&canon(s).to_string())[..16].to_string()
}

/// Check a submitted plan's shape (the schema is strict too; this is the code's own check).
pub fn shape(p: &Value) -> Result<(), String> {
    let changes = p["changes"].as_array().ok_or("changes must be a list")?;
    if changes.is_empty() {
        return Err("a plan needs at least one change (use hold for no change)".into());
    }
    if changes.len() > 12 {
        return Err("at most 12 changes per plan".into());
    }
    for (i, c) in changes.iter().enumerate() {
        let n = i + 1;
        match c["type"].as_str() {
            Some("strategy") => {
                if !c["pool"].as_str().map(super::is_addr).unwrap_or(false) || !c["strategy"].is_object() {
                    return Err(format!("change {n}: strategy needs pool and strategy"));
                }
            }
            Some("capital") => {
                if !c["pool"].as_str().map(super::is_addr).unwrap_or(false) || !c["target_usd"].as_f64().map(|x| x >= 0.0 && x.is_finite()).unwrap_or(false) {
                    return Err(format!("change {n}: capital needs pool and target_usd >= 0"));
                }
            }
            Some("add_pool") => {
                if !c["pool"].as_str().map(super::is_addr).unwrap_or(false) || c["chain_id"].as_u64().is_none() || !c["strategy"].is_object()
                    || !c["start_usd"].as_f64().map(|x| x > 0.0).unwrap_or(false) {
                    return Err(format!("change {n}: add_pool needs chain_id, pool, strategy and start_usd > 0"));
                }
            }
            Some("exit_pool") => {
                if !c["pool"].as_str().map(super::is_addr).unwrap_or(false) {
                    return Err(format!("change {n}: exit_pool needs pool"));
                }
            }
            _ => return Err(format!("change {n}: type is strategy, capital, add_pool or exit_pool")),
        }
        if c["thesis"].as_str().map(|s| s.trim().is_empty()).unwrap_or(true) {
            return Err(format!("change {n}: a thesis is required"));
        }
        let inv = &c["invalidation"];
        if !inv.is_object() || inv["metric"].as_str().is_none() || inv["op"].as_str().map(|o| o != "<" && o != ">").unwrap_or(true)
            || inv["threshold"].as_f64().is_none() || !matches!(inv["action"].as_str(), Some("exit_to_safe" | "revert_strategy" | "withdraw_to_unallocated" | "alert")) {
            return Err(format!("change {n}: invalidation needs metric, op (< or >), threshold and action"));
        }
        if !matches!(inv["metric"].as_str(), Some(m) if super::sched::METRICS.contains(&m)) {
            return Err(format!("change {n}: invalidation metric is one of {}", super::sched::METRICS.join(", ")));
        }
    }
    Ok(())
}

/// Store a submission once. Returns (plan, true) for a new one, (the stored one, false) for a repeat.
pub fn submit(review_id: &str, tool_use_id: &str, body: &Value, meta: &Value) -> Result<(Value, bool), String> {
    let _g = LOCK.lock().unwrap();
    let mut all = load();
    if let Some(p) = all.iter().find(|p| p["review_id"] == review_id && p["tool_use_id"] == tool_use_id) {
        return Ok((p.clone(), false));
    }
    shape(body)?;
    let mut changes = body["changes"].clone();
    // the strategy hash and the one it replaces, by code
    let pools = crate::settings::load_pools();
    for c in changes.as_array_mut().unwrap() {
        if c["strategy"].is_object() {
            c["strategy_hash"] = json!(strategy_hash(&c["strategy"]));
            if let Some(cur) = pools.iter().find(|p| p["pool"].as_str().map(|a| a.eq_ignore_ascii_case(c["pool"].as_str().unwrap_or(""))).unwrap_or(false)) {
                c["previous_strategy"] = cur["strategy"].clone();
                c["previous_strategy_hash"] = json!(strategy_hash(&cur["strategy"]));
            }
        }
        if let Some(p) = c["pool"].as_str().map(|s| s.to_lowercase()) {
            c["pool"] = json!(p);
        }
    }
    let bounds = body["requested_bounds"].clone();
    let id = format!("p{}-{}", super::now(), &super::sha256_hex(&format!("{review_id}{tool_use_id}"))[..6]);
    let p = json!({
        "plan_id": id, "review_id": review_id, "tool_use_id": tool_use_id, "created": super::now(), "state": "proposed",
        "snapshot_id": meta["snapshot_id"], "snapshot_ts": meta["snapshot_ts"], "account_version": meta["account_version"],
        "policy_version": POLICY_VERSION, "prompt_version": meta["prompt_version"], "model": meta["model"],
        "summary": body["summary"], "changes": changes, "requested_bounds": bounds, "plan_hash": hash(&body["changes"], &body["requested_bounds"]),
        "prices_at_snapshot": meta["prices"], "history": [{"ts": super::now(), "state": "proposed"}], "steps": [],
    });
    all.push(p.clone());
    save(&all);
    super::journal::record("plan_submitted", json!({"plan_id": id, "review_id": review_id, "changes": p["changes"], "summary": p["summary"]}));
    Ok((p, true))
}

pub const POLICY_VERSION: &str = "policy-v1";

/// Move a plan to a new state (refused when the transition is not allowed). `extra` fields are merged in.
pub fn transition(id: &str, to: &str, why: &str, extra: Value) -> Result<Value, String> {
    let _g = LOCK.lock().unwrap();
    let mut all = load();
    let p = all.iter_mut().find(|p| p["plan_id"] == id).ok_or("no such plan")?;
    let from = p["state"].as_str().unwrap_or("").to_string();
    if !can_move(&from, to) {
        return Err(format!("plan {id} is {from}; it cannot become {to}"));
    }
    p["state"] = json!(to);
    if let (Some(h), true) = (p["history"].as_array_mut(), true) {
        h.push(json!({"ts": super::now(), "state": to, "why": why}));
    }
    for (k, v) in extra.as_object().cloned().unwrap_or_default() {
        p[k] = v;
    }
    let out = p.clone();
    save(&all);
    super::journal::record("plan_state", json!({"plan_id": id, "from": from, "to": to, "why": why}));
    Ok(out)
}

/// Update a plan's fields without a state change (its steps, notes).
pub fn update(id: &str, f: impl FnOnce(&mut Value)) -> Result<Value, String> {
    let _g = LOCK.lock().unwrap();
    let mut all = load();
    let p = all.iter_mut().find(|p| p["plan_id"] == id).ok_or("no such plan")?;
    f(p);
    let out = p.clone();
    save(&all);
    Ok(out)
}

/// Approve or reject a plan waiting for approval, bound to its hash. A repeated approval is a no-op.
pub fn decide(id: &str, plan_hash: &str, approve: bool, via: &str) -> Result<Value, String> {
    let p = get(id).ok_or("no such plan")?;
    if p["plan_hash"].as_str() != Some(plan_hash) {
        return Err("the plan changed since it was shown: not approved".into());
    }
    match (p["state"].as_str().unwrap_or(""), approve) {
        ("awaiting_approval", true) => {
            super::journal::record("approval", json!({"plan_id": id, "approved": true, "via": via}));
            transition(id, "queued", &format!("approved via {via}"), json!({"approved_via": via, "approved_at": super::now()}))
        }
        ("awaiting_approval", false) => {
            super::journal::record("approval", json!({"plan_id": id, "approved": false, "via": via}));
            transition(id, "rejected", &format!("rejected via {via}"), json!({}))
        }
        ("queued" | "executing" | "confirmed", true) => Ok(p),
        (s, _) => Err(format!("expired: the plan is {s}")),
    }
}

/// Void every pending approval (agent off, Telegram unlinked, a limit changed).
pub fn void_pending(why: &str) {
    for p in load().into_iter().filter(|p| p["state"] == "awaiting_approval") {
        let _ = transition(p["plan_id"].as_str().unwrap_or(""), "cancelled", why, json!({}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change() -> Value {
        json!({"type": "capital", "pool": "0x9ab7730b09ebd9ff2df70f06339bd289a1680a46", "target_usd": 100.0, "thesis": "t",
               "invalidation": {"metric": "zscore_72h", "source": "pool", "op": "<", "threshold": -2.5, "persist_minutes": 60, "action": "alert"}})
    }

    #[test]
    fn shapes() {
        assert!(shape(&json!({"changes": [change()]})).is_ok());
        assert!(shape(&json!({"changes": []})).is_err());
        let mut c = change();
        c["thesis"] = json!("");
        assert!(shape(&json!({"changes": [c]})).is_err());
        let mut c = change();
        c["invalidation"]["action"] = json!("buy_more");
        assert!(shape(&json!({"changes": [c]})).is_err());
    }

    #[test]
    fn transitions() {
        assert!(can_move("proposed", "validated"));
        assert!(can_move("awaiting_approval", "queued"));
        assert!(!can_move("confirmed", "executing"));
        assert!(!can_move("rejected", "queued"));
        assert!(terminal("superseded") && !terminal("queued"));
        assert_eq!(strategy_hash(&json!({"b": 1, "a": 2})), strategy_hash(&json!({"a": 2, "b": 1})));
    }
}
