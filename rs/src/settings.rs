//! config/settings.json and config/pools.json, merged over the defaults.
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
        // where the dashboard listens (127.0.0.1); a port in use at startup moves to the next free one
        "ui_port": 8080,
        "claim": {"enabled": true, "every_days": 7},
        "sell_rewards": {"enabled": true, "tokens": [SOLID], "max_impact_pct": 3.0, "min_value_usd": 2.0},
        // chain id -> tokens that may be swapped to fund that chain's pools; a chain missing here uses
        // its USDC and USDT, an empty list turns funding off on that chain
        "funding_by_chain": {},
        "solid_lp": {"enabled": false, "pool": SOLID_WETH_POOL, "min_value_usd": 5.0, "max_price_dev_pct": 3.0,
                     "slippage_bps": 50.0, "max_swap_cost_bps": 50.0},
        "retries": {"attempts": 3, "backoff_s": 10},
        "idle_redeploy_pct": 2.0,
        "resize_tolerance_pct": 1.0,
        // parking.rs: tokens a strategy waits in earn in a whitelisted pegged pool meanwhile
        // (pools: [{chain_id, token, pool}]); each strategy opts in with "park": true
        "parking": {"enabled": false, "min_usd": 50.0, "range_pct": 0.5, "pools": []},
        // experimental.rs: the agent plan's groundwork, shown on the Experimental tab and used by
        // nothing that trades; llens: "placeholder" (generated data) or "off"
        "experimental": crate::agent::defaults(),
        // the first-start wizard was finished (or skipped); it opens while this is false and no key is stored
        "onboarded": false,
    })
}

pub fn pool_defaults() -> Value {
    json!({
        "strategy": {"type": "ladder", "lookback": 86400, "span": 3, "every": 72, "turnover": 0.4, "guard": 15},
        "slippage_bps": 2.0, "max_swap_cost_bps": 3.0, "max_spot_dev_ticks": 10, "max_actions_per_day": 5,
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
        // One list for all chains (older settings) held Base tokens: it becomes Base's list, and the
        // other chains start from their defaults.
        if let Some(old) = o.remove("funding_tokens") {
            if !o.contains_key("funding_by_chain") {
                o.insert("funding_by_chain".into(), json!({crate::chains::BASE.to_string(): old}));
            }
        }
    }
    let mut s = merge(&defaults(), &cur);
    let mut fb = serde_json::Map::new();
    for (k, v) in s["funding_by_chain"].as_object().cloned().unwrap_or_default() {
        fb.insert(k, Value::Array(clean_addrs(&v)));
    }
    s["funding_by_chain"] = Value::Object(fb);
    s
}

/// Lowercase, non-empty, de-duplicated addresses of a JSON list.
pub fn clean_addrs(v: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = vec![];
    for t in v.as_array().cloned().unwrap_or_default() {
        if let Some(a) = t.as_str().map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()) {
            if !out.iter().any(|o| o.as_str() == Some(a.as_str())) {
                out.push(Value::String(a));
            }
        }
    }
    out
}

/// Tokens that may be swapped to fund pools on chain `cid`.
pub fn funding_tokens(s: &Value, cid: u64) -> Vec<String> {
    match s["funding_by_chain"].get(cid.to_string()) {
        Some(v) => clean_addrs(v).iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        None => crate::chains::default_funding(cid),
    }
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

/// Allowed ranges of the numeric settings (path, min, max).
const SETTING_RANGES: &[(&str, f64, f64)] = &[
    ("interval_s", 30.0, 86400.0),
    ("ui_port", 1024.0, 65535.0),
    ("idle_redeploy_pct", 0.0, 100.0),
    ("resize_tolerance_pct", 0.0, 100.0),
    ("claim.every_days", 0.1, 365.0),
    ("sell_rewards.max_impact_pct", 0.0, 50.0),
    ("sell_rewards.min_value_usd", 0.0, 1e9),
    ("solid_lp.min_value_usd", 0.0, 1e9),
    ("solid_lp.max_price_dev_pct", 0.0, 50.0),
    ("solid_lp.slippage_bps", 0.0, 1000.0),
    ("solid_lp.max_swap_cost_bps", 0.0, 1000.0),
    ("retries.attempts", 1.0, 10.0),
    ("retries.backoff_s", 0.0, 600.0),
    ("parking.min_usd", 0.0, 1e9),
    ("parking.range_pct", 0.01, 20.0),
];

fn at<'a>(v: &'a Value, path: &str) -> &'a Value {
    path.split('.').fold(v, |a, k| &a[k])
}

/// Reject settings with numbers outside their range (or not numbers at all).
pub fn validate_settings(s: &Value) -> Result<(), String> {
    for (k, lo, hi) in SETTING_RANGES {
        let v = at(s, k);
        if v.is_null() {
            continue;
        }
        let Some(x) = v.as_f64().filter(|x| x.is_finite()) else { return Err(format!("{k} must be a number")) };
        if x < *lo || x > *hi {
            return Err(format!("{k} must be between {lo} and {hi} (got {x})"));
        }
    }
    for e in s["parking"]["pools"].as_array().cloned().unwrap_or_default() {
        let addr = |k: &str| e[k].as_str().map(|a| a.len() == 42 && a.starts_with("0x") && a[2..].chars().all(|c| c.is_ascii_hexdigit())).unwrap_or(false);
        if e["chain_id"].as_u64().is_none() || !addr("token") || !addr("pool") {
            return Err("parking.pools: each entry needs a chain_id, a token address and a pool address".into());
        }
        if !matches!(e["strategy"].as_str(), None | Some("fixed") | Some("recentre")) {
            return Err("parking.pools: strategy is fixed or recentre".into());
        }
        for (k, lo, hi) in [("range_pct", 0.01, 20.0), ("recentre_after_min", 0.0, 10080.0)] {
            if !e[k].is_null() && !e[k].as_f64().map(|x| x >= lo && x <= hi).unwrap_or(false) {
                return Err(format!("parking.pools: {k} must be between {lo} and {hi}"));
            }
        }
    }
    crate::agent::validate(&s["experimental"])?;
    if let Some(w) = s["wallet"].as_str() {
        if !w.is_empty() && !(w.len() == 42 && w.starts_with("0x") && w[2..].chars().all(|c| c.is_ascii_hexdigit())) {
            return Err("wallet must be an address (0x + 40 hex digits)".into());
        }
    }
    Ok(())
}

/// Reject pool settings out of range: allocation, limits and strategy parameters.
pub fn validate_pool(p: &Value) -> Result<(), String> {
    let name = p["name"].as_str().unwrap_or("?");
    let check = |v: &Value, k: &str, lo: f64, hi: f64| -> Result<(), String> {
        if v.is_null() {
            return Ok(());
        }
        let Some(x) = v.as_f64().filter(|x| x.is_finite()) else { return Err(format!("{name}: {k} must be a number")) };
        if x < lo || x > hi { Err(format!("{name}: {k} must be between {lo} and {hi} (got {x})")) } else { Ok(()) }
    };
    for (k, lo, hi) in [("allocation_pct", 0.0, 100.0), ("slippage_bps", 0.0, 500.0), ("max_swap_cost_bps", 0.0, 1000.0),
                        ("max_spot_dev_ticks", 1.0, 100000.0), ("max_actions_per_day", 0.0, 1000.0), ("min_swap_value", 0.0, 1e12),
                        ("max_ranges", 1.0, 50.0)] {
        check(&p[k], k, lo, hi)?;
    }
    let st = &p["strategy"];
    match st["type"].as_str().unwrap_or("ladder") {
        "ladder" => {
            for (k, lo, hi) in [("lookback", 3600.0, 30.0 * 86400.0), ("span", 1.0, 100000.0), ("every", 1.0, 100000.0),
                                ("turnover", 0.0, 1.0), ("guard", 0.0, 100000.0)] {
                check(&st[k], k, lo, hi)?;
            }
        }
        "custom" => {
            for (k, lo, hi) in [("width", 1.0, 1_774_544.0), ("skew", 0.0, 1.0), ("offset_up", -1_000_000.0, 1_000_000.0),
                                ("offset_down", -1_000_000.0, 1_000_000.0), ("buffer_lo", -1_000_000.0, 1_000_000.0),
                                ("buffer_hi", -1_000_000.0, 1_000_000.0), ("delay_min", 0.0, 10080.0), ("cooldown_min", 0.0, 43200.0),
                                ("exit_delay_min", 0.0, 10080.0), ("guard", 0.0, 100000.0)] {
                check(&st[k], k, lo, hi)?;
            }
            let spacing = p["tick_spacing"].as_i64().unwrap_or(1);
            crate::custom::Rules::from(st.as_object().ok_or(format!("{name}: strategy must be an object"))?, spacing)
                .map_err(|e| format!("{name}: {e}"))?;
        }
        "graph" => {
            if serde_json::to_string(st).map(|s| s.len()).unwrap_or(0) > 64 * 1024 {
                return Err(format!("{name}: node strategy too large"));
            }
            let spacing = p["tick_spacing"].as_i64().unwrap_or(1);
            crate::graph::Graph::parse(st.as_object().ok_or(format!("{name}: strategy must be an object"))?, spacing)
                .map_err(|e| format!("{name}: {e}"))?;
        }
        t => return Err(format!("{name}: unknown strategy type {t}")),
    }
    Ok(())
}

pub fn load_configs() -> Vec<PoolConfig> {
    load_pools().iter().map(PoolConfig::from).collect()
}
