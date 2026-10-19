//! Safety locks and the emergency state (plan section 4), in data/agent/locks.json.
//!
//! A lock covers a pool or a whole chain: no deposits, plan steps or engine deployments there;
//! withdrawals and exits are allowed. The risk controller sets it and marks its condition cleared
//! when the condition goes away; it lifts when the condition has cleared **and** the user confirms,
//! or when the user's timer (risk.lock_hours, 0 = never) runs out after it cleared.
//!
//! The emergency state lists the pools an emergency exit is working on; plan steps that are not
//! part of it are refused by the write gate while it lasts.

use serde_json::{json, Value};
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());
const FILE: &str = "locks.json";

fn load() -> Value {
    let v = super::read_json(FILE, json!({"locks": [], "emergency": null}));
    if v["locks"].is_array() { v } else { json!({"locks": [], "emergency": null}) }
}

fn save(v: &Value) {
    let _ = super::write_json(FILE, v);
}

pub fn all() -> Value {
    load()
}

fn covers(l: &Value, cid: u64, pool: Option<&str>) -> bool {
    if l["chain_id"].as_u64() != Some(cid) {
        return false;
    }
    match (l["pool"].as_str(), pool) {
        (None, _) => true,
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        (Some(_), None) => false,
    }
}

/// The active lock on a pool (or its chain), if any.
pub fn active(cid: u64, pool: Option<&str>) -> Option<Value> {
    load()["locks"].as_array()?.iter().find(|l| l["lifted"].is_null() && covers(l, cid, pool)).cloned()
}

/// Set (or keep) a lock for a condition; returns true when it is new.
pub fn set(cid: u64, pool: Option<&str>, condition: &str, reason: &str) -> bool {
    let _g = LOCK.lock().unwrap();
    let mut v = load();
    let now = super::now();
    let list = v["locks"].as_array_mut().unwrap();
    if let Some(l) = list.iter_mut().find(|l| l["lifted"].is_null() && l["condition"] == condition && l["chain_id"].as_u64() == Some(cid) && l["pool"].as_str() == pool) {
        l["reason"] = json!(reason);
        l["cleared_at"] = Value::Null;
        l["last_seen"] = json!(now);
        save(&v);
        return false;
    }
    list.push(json!({"id": format!("{now}-{}", list.len()), "chain_id": cid, "pool": pool, "condition": condition, "reason": reason,
                     "since": now, "last_seen": now, "cleared_at": null, "lifted": null}));
    save(&v);
    true
}

/// The controller saw a condition clear: the lock waits for the user (or the timer).
pub fn cleared(cid: u64, pool: Option<&str>, condition: &str, lock_hours: f64) {
    let _g = LOCK.lock().unwrap();
    let mut v = load();
    let now = super::now();
    let mut changed = false;
    for l in v["locks"].as_array_mut().unwrap().iter_mut() {
        if l["lifted"].is_null() && l["condition"] == condition && l["chain_id"].as_u64() == Some(cid) && l["pool"].as_str() == pool {
            if l["cleared_at"].is_null() {
                l["cleared_at"] = json!(now);
                changed = true;
            }
            let since = l["cleared_at"].as_i64().unwrap_or(now);
            if lock_hours > 0.0 && (now - since) as f64 >= lock_hours * 3600.0 {
                l["lifted"] = json!({"at": now, "by": "timer"});
                changed = true;
            }
        }
    }
    if changed {
        save(&v);
    }
}

/// The user lifts a lock whose condition has cleared. Refused while the condition holds.
pub fn confirm(id: &str) -> Result<(), String> {
    let _g = LOCK.lock().unwrap();
    let mut v = load();
    let now = super::now();
    let l = v["locks"].as_array_mut().unwrap().iter_mut().find(|l| l["id"] == id && l["lifted"].is_null()).ok_or("no such active lock")?;
    if l["cleared_at"].is_null() {
        return Err(format!("the condition still holds: {}", l["reason"].as_str().unwrap_or("")));
    }
    l["lifted"] = json!({"at": now, "by": "user"});
    save(&v);
    Ok(())
}

// -- emergency state --------------------------------------------------------------------------------

pub fn emergency() -> Option<Value> {
    let v = load()["emergency"].clone();
    (!v.is_null()).then_some(v)
}

pub fn start_emergency(cid: u64, pools: &[String], reason: &str) {
    let _g = LOCK.lock().unwrap();
    let mut v = load();
    let now = super::now();
    let mut list: Vec<String> = v["emergency"]["pools"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    for p in pools {
        if !list.contains(p) {
            list.push(p.clone());
        }
    }
    v["emergency"] = json!({"chain_id": cid, "pools": list, "since": v["emergency"]["since"].as_i64().unwrap_or(now), "reason": reason});
    save(&v);
}

pub fn end_emergency() {
    let _g = LOCK.lock().unwrap();
    let mut v = load();
    if !v["emergency"].is_null() {
        let mut hist = v["history"].as_array().cloned().unwrap_or_default();
        let mut e = v["emergency"].clone();
        e["ended"] = json!(super::now());
        hist.push(e);
        let n = hist.len();
        v["history"] = json!(hist[n.saturating_sub(50)..]);
        v["emergency"] = Value::Null;
        save(&v);
    }
}

/// Lifted locks older than 30 days are dropped from the file.
pub fn tidy() {
    let _g = LOCK.lock().unwrap();
    let mut v = load();
    let now = super::now();
    let before = v["locks"].as_array().map(|a| a.len()).unwrap_or(0);
    v["locks"].as_array_mut().unwrap().retain(|l| l["lifted"]["at"].as_i64().map(|t| now - t < 30 * 86_400).unwrap_or(true));
    if v["locks"].as_array().map(|a| a.len()).unwrap_or(0) != before {
        save(&v);
    }
}
