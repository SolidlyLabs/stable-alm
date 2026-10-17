//! config/settings.json and config/pools.json, merged over the same defaults as settings.py.
//! Kept as JSON values so the web UI can read and write them unchanged.

use serde_json::{json, Map, Value};
use std::sync::Mutex;

pub const USDC: &str = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
pub const SOLID: &str = "0x777cf5ba9c291a1a8f57ff14836f6f9dc5c0f9dd";
pub const SOLID_WETH_POOL: &str = "0xd09f2ebaf365fc97a5bbdefc15703312165638e4";

static LOCK: Mutex<()> = Mutex::new(());

pub fn defaults() -> Value {
    json!({
        "execute": false,
        "wallet": "",
        "interval_s": 300,
        "claim": {"enabled": true, "every_days": 7},
        "sell_rewards": {"enabled": true, "tokens": [SOLID], "max_impact_pct": 3.0, "min_value_usd": 2.0},
        "funding_tokens": [USDC],
        "solid_lp": {"enabled": false, "pool": SOLID_WETH_POOL, "min_value_usd": 5.0, "max_price_dev_pct": 3.0,
                     "slippage_bps": 50.0, "max_swap_cost_bps": 50.0},
        "retries": {"attempts": 3, "backoff_s": 10},
        "idle_redeploy_pct": 2.0,
        "resize_tolerance_pct": 1.0,
    })
}

pub fn pool_defaults() -> Value {
    json!({
        "strategy": {"type": "ladder", "lookback": 86400, "span": 3, "every": 72, "turnover": 0.4, "guard": 15},
        "slippage_bps": 2.0, "max_swap_cost_bps": 3.0, "max_spot_dev_ticks": 3, "max_actions_per_day": 5,
        "min_swap_value": 5.0, "numeraire": 0, "max_ranges": 5, "allocation_pct": 0.0, "enabled": false,
    })
}

pub fn merge(base: &Value, over: &Value) -> Value {
    let mut out = base.as_object().cloned().unwrap_or_default();
    if let Some(o) = over.as_object() {
        for (k, v) in o {
            let merged = match (base.get(k), v) {
                (Some(b @ Value::Object(_)), Value::Object(_)) => merge(b, v),
                _ => v.clone(),
            };
            out.insert(k.clone(), merged);
        }
    }
    Value::Object(out)
}

fn read(path: &std::path::Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub fn write_atomic(path: &std::path::Path, v: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap() + "\n")?;
    std::fs::rename(tmp, path)
}

pub fn load_settings() -> Value {
    let cur = {
        let _g = LOCK.lock().unwrap();
        read(&crate::paths::config().join("settings.json")).unwrap_or(json!({}))
    };
    let mut cur = cur;
    if let Some(o) = cur.as_object_mut() {
        if let Some(old) = o.remove("funding_token") {
            o.entry("funding_tokens").or_insert_with(|| if old.is_null() { json!([]) } else { json!([old]) });
        }
    }
    let mut s = merge(&defaults(), &cur);
    let ft: Vec<Value> = s["funding_tokens"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| t.as_str().filter(|x| !x.is_empty()).map(|x| Value::String(x.to_lowercase())))
        .collect();
    s["funding_tokens"] = Value::Array(ft);
    s
}

pub fn save_settings(s: &Value) -> std::io::Result<()> {
    let _g = LOCK.lock().unwrap();
    write_atomic(&crate::paths::config().join("settings.json"), s)
}

pub fn load_pools() -> Vec<Value> {
    let raw = {
        let _g = LOCK.lock().unwrap();
        read(&crate::paths::config().join("pools.json")).unwrap_or(json!([]))
    };
    raw.as_array().cloned().unwrap_or_default().iter().map(|p| merge(&pool_defaults(), p)).collect()
}

pub fn save_pools(pools: &[Value]) -> Result<(), String> {
    // Allocations are shares of each chain's own total (no bridging), so the check is per chain.
    let mut by: std::collections::BTreeMap<u64, f64> = Default::default();
    for p in pools.iter().filter(|p| p["enabled"].as_bool().unwrap_or(false)) {
        *by.entry(p["chain_id"].as_u64().unwrap_or(8453)).or_default() += p["allocation_pct"].as_f64().unwrap_or(0.0);
    }
    if let Some((c, total)) = by.iter().find(|(_, t)| **t > 100.0001) {
        return Err(format!("allocations of enabled pools on {} add up to {total:.1}% (> 100%)", crate::chains::get(*c).name));
    }
    let mut names: Vec<&str> = pools.iter().filter_map(|p| p["name"].as_str()).collect();
    let n = names.len();
    names.sort();
    names.dedup();
    if names.len() != n {
        return Err("pool names must be unique".into());
    }
    let _g = LOCK.lock().unwrap();
    write_atomic(&crate::paths::config().join("pools.json"), &Value::Array(pools.to_vec())).map_err(|e| e.to_string())
}

/// Typed view of one pool entry (what the executor needs).
#[derive(Clone, Debug)]
pub struct PoolConfig {
    pub name: String,
    pub pool: String,
    pub strategy: Map<String, Value>,
    pub slippage_bps: f64,
    pub max_spot_dev_ticks: i64,
    pub max_actions_per_day: i64,
    pub min_swap_value: f64,
    pub max_swap_cost_bps: f64,
    pub numeraire: i64,
    pub allocation_pct: f64,
    pub max_ranges: usize,
    pub enabled: bool,
}

impl PoolConfig {
    pub fn from(v: &Value) -> PoolConfig {
        let f = |k: &str, d: f64| v[k].as_f64().unwrap_or(d);
        PoolConfig {
            name: v["name"].as_str().unwrap_or("").to_string(),
            pool: v["pool"].as_str().unwrap_or("").to_lowercase(),
            strategy: v["strategy"].as_object().cloned().unwrap_or_default(),
            slippage_bps: f("slippage_bps", 2.0),
            max_spot_dev_ticks: f("max_spot_dev_ticks", 3.0) as i64,
            max_actions_per_day: f("max_actions_per_day", 5.0) as i64,
            min_swap_value: f("min_swap_value", 5.0),
            max_swap_cost_bps: f("max_swap_cost_bps", 3.0),
            numeraire: f("numeraire", 0.0) as i64,
            allocation_pct: f("allocation_pct", 0.0),
            max_ranges: f("max_ranges", 5.0) as usize,
            enabled: v["enabled"].as_bool().unwrap_or(false),
        }
    }
}

pub fn load_configs() -> Vec<PoolConfig> {
    load_pools().iter().map(PoolConfig::from).collect()
}
