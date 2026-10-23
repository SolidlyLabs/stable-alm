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
        // update.rs: check GitHub for a newer release signed by a whitelisted key (every 12 hours)
        "update_check": true,
        // the first-start wizard was finished (or skipped); it opens while this is false and no key is stored
        "onboarded": false,
    })
}

pub fn pool_defaults() -> Value {
    json!({
        "strategy": crate::ladder::as_graph(&crate::ladder::Params::from(&crate::ladder::plain_defaults()).unwrap()),
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

/// The settings each wallet has its own of (wallets.rs); the rest are the app's. The first wallet's
/// are in config/settings.json with the app's, as they always were; another wallet's are in its own
/// config/settings.json, merged over the app's.
pub const WALLET_KEYS: &[&str] = &["execute", "wallet", "claim", "sell_rewards", "funding_by_chain", "solid_lp", "parking",
                                   "idle_redeploy_pct", "resize_tolerance_pct", "retries"];

fn settings_raw() -> Value {
    let _g = LOCK.lock().unwrap();
    let root = read(&crate::paths::app_config().join("settings.json")).unwrap_or(json!({}));
    if crate::paths::wallet() == crate::paths::MAIN {
        return root;
    }
    let mut out = root.as_object().cloned().unwrap_or_default();
    for k in WALLET_KEYS {
        out.remove(*k);
    }
    let own = read(&crate::paths::config().join("settings.json")).unwrap_or(json!({}));
    for (k, v) in own.as_object().cloned().unwrap_or_default() {
        if WALLET_KEYS.contains(&k.as_str()) {
            out.insert(k, v);
        }
    }
    Value::Object(out)
}

pub fn load_settings() -> Value {
    let cur = settings_raw();
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
    let root_path = crate::paths::app_config().join("settings.json");
    if crate::paths::wallet() == crate::paths::MAIN {
        return write_atomic(&root_path, s);
    }
    // this wallet's own keys to its file, the app's to the app's file (the first wallet's keys there stay)
    let mut root = read(&root_path).and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let seen = merge(&defaults(), &Value::Object(root.clone()));
    let (mut own, mut changed) = (Map::new(), false);
    for (k, v) in s.as_object().cloned().unwrap_or_default() {
        if WALLET_KEYS.contains(&k.as_str()) {
            own.insert(k, v);
        } else if seen.get(&k) != Some(&v) {
            root.insert(k, v);
            changed = true;
        }
    }
    write_atomic(&crate::paths::config().join("settings.json"), &Value::Object(own))?;
    // the app's file is rewritten only when an app setting changed
    if changed {
        write_atomic(&root_path, &Value::Object(root))?;
    }
    Ok(())
}

pub fn load_pools() -> Vec<Value> {
    let raw = {
        let _g = LOCK.lock().unwrap();
        read(&crate::paths::config().join("pools.json")).unwrap_or(json!([]))
    };
    let mut migrated = vec![];
    let pools: Vec<Value> = raw.as_array().cloned().unwrap_or_default().iter().map(|p| {
        // the strategy is the pool's own (defaults are not merged into it)
        let mut m = merge(&pool_defaults(), p);
        if let Some(s) = p.get("strategy") {
            m["strategy"] = match crate::ladder::migrate(s) {
                Some(g) => {
                    migrated.push(m.clone());
                    g
                }
                None => s.clone(),
            };
        }
        m
    }).collect();
    if !migrated.is_empty() {
        migrate_pools(&raw, &pools, &migrated);
    }
    pools
}

/// Old plain ladders became node strategies with one ladder block (ladder.rs `migrate`): pools.json
/// is rewritten once (the old file kept as pools.json.v050.bak), and each such pool's next re-solve
/// stays where its last solve put it (the ladder's last_solve file seeds the node state).
fn migrate_pools(raw: &Value, pools: &[Value], migrated: &[Value]) {
    let dir = crate::paths::config();
    {
        let _g = LOCK.lock().unwrap();
        let bak = dir.join("pools.json.v050.bak");
        if !bak.exists() {
            let _ = write_atomic(&bak, raw);
        }
        let mut out = raw.as_array().cloned().unwrap_or_default();
        for (o, p) in out.iter_mut().zip(pools) {
            if o.get("strategy").is_some() {
                o["strategy"] = p["strategy"].clone();
            }
        }
        if let Err(e) = write_atomic(&dir.join("pools.json"), &Value::Array(out)) {
            crate::warn!("settings", "could not save the migrated pools: {e}");
            return;
        }
    }
    for p in migrated {
        let (cid, pool, name) = (p["chain_id"].as_u64().unwrap_or(crate::chains::BASE), p["pool"].as_str().unwrap_or(""), p["name"].as_str().unwrap_or(""));
        let ts = read(&crate::paths::logs().join(format!("last_solve_{name}.json"))).and_then(|v| v["ts"].as_i64()).unwrap_or(0);
        let mut st = crate::custom::load_state(cid, pool, true);
        if st.last_move == 0 && ts > 0 {
            st.last_move = ts;
            crate::custom::save_state(cid, pool, true, &st);
        }
        crate::info!("settings", "[{name}] the ladder is now a node strategy with one ladder block (same settings)");
    }
}

/// Held across a read-modify-write of pools.json (`update_pools`), so two writers (the worker's
/// agent threads, the dashboard) do not undo each other's change.
static POOLS_RMW: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Change the pools under one lock: `f` gets them as they are now and saves when it returns Ok.
pub fn update_pools<T>(f: impl FnOnce(&mut Vec<Value>) -> Result<T, String>) -> Result<T, String> {
    let _g = POOLS_RMW.lock().unwrap_or_else(|e| e.into_inner());
    let mut pools = load_pools();
    let r = f(&mut pools)?;
    save_pools(&pools)?;
    Ok(r)
}

/// A pool list sent by the dashboard, as it is saved: a pool the risk controller switched off
/// (`switched_off`) after the dashboard read the list stays off. Returns the names kept off.
pub fn keep_switched_off(current: &[Value], sent: &mut [Value]) -> Vec<String> {
    let mut kept = vec![];
    for p in sent.iter_mut().filter(|p| p["enabled"] == true) {
        let same = |q: &&Value| q["pool"].as_str().map(|a| a.eq_ignore_ascii_case(p["pool"].as_str().unwrap_or(""))).unwrap_or(false) && q["chain_id"] == p["chain_id"];
        let Some(c) = current.iter().find(same) else { continue };
        if c["enabled"] == false && !c["switched_off"].is_null() {
            if p["switched_off"]["at"] == c["switched_off"]["at"] {
                // switched on again by someone who saw it switched off
                if let Some(o) = p.as_object_mut() {
                    o.remove("switched_off");
                }
            } else {
                p["enabled"] = json!(false);
                p["switched_off"] = c["switched_off"].clone();
                kept.push(p["name"].as_str().unwrap_or("").to_string());
            }
        }
    }
    kept
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
    /// The agent's plan cost bound while one is in force (agent::exec::bounded): no swap costs more.
    pub plan_bound_bps: Option<f64>,
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
            plan_bound_bps: None,
        }
    }

    /// The cost limit of the swaps that must clear for the pool to work (funding, parking, a
    /// withdrawal's): the pool's limit, at least 10 bp, but never above the agent's plan bound.
    pub fn floored_cost(&self) -> f64 {
        let c = self.max_swap_cost_bps.max(10.0);
        self.plan_bound_bps.map(|b| c.min(b)).unwrap_or(c)
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
    match st["type"].as_str().unwrap_or("") {
        "ladder" => return Err(format!("{name}: the plain ladder is a node strategy now: a graph with one ladder block")),
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
            // the hedge block is backtested only for now: the live hedge
            // is not built, and a pool must not run a strategy whose result assumed one
            if st["nodes"].as_array().map(|a| a.iter().any(|n| n["kind"] == "hedge" || n["kind"] == "funding")).unwrap_or(false) {
                return Err(format!("{name}: the Hedge block runs in backtests only until the live hedge on Hyperliquid is built; remove it to run this strategy"));
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

#[cfg(test)]
mod migrate_tests {
    /// The plain-ladder migration on a copy of the app's folder: ALM_HOME must point at a copy (never
    /// the live one). cargo test -- --ignored migrate_on_a_copy --nocapture
    #[test]
    #[ignore]
    fn migrate_on_a_copy() {
        let home = std::env::var("ALM_HOME").expect("set ALM_HOME to a copy of the app folder");
        assert!(!home.to_lowercase().replace('\\', "/").trim_end_matches('/').ends_with("appdata/local/stablealm"), "not on the live folder");
        let pools = super::load_pools();
        for p in &pools {
            println!("{}: {}", p["name"], p["strategy"]);
            assert_ne!(p["strategy"]["type"], "ladder");
            let state = crate::custom::load_state(p["chain_id"].as_u64().unwrap_or(8453), p["pool"].as_str().unwrap(), true);
            println!("  state last_move {}", state.last_move);
        }
        assert!(crate::paths::config().join("pools.json.v050.bak").exists());
        let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(crate::paths::config().join("pools.json")).unwrap()).unwrap();
        assert!(saved.as_array().unwrap().iter().all(|p| p["strategy"]["type"] != "ladder"), "pools.json was rewritten");
        assert_eq!(super::load_pools(), pools, "a second load changes nothing");
    }
}

#[cfg(test)]
mod pool_save_tests {
    use super::*;

    #[test]
    fn a_stale_dashboard_save_does_not_switch_a_risk_switched_off_pool_on() {
        let off = json!({"name": "a", "pool": "0xA", "chain_id": 8453, "enabled": false, "switched_off": {"at": 100, "reason": "loss"}});
        // sent from a page loaded before the switch-off: it stays off
        let mut sent = vec![json!({"name": "a", "pool": "0xa", "chain_id": 8453, "enabled": true})];
        assert_eq!(keep_switched_off(&[off.clone()], &mut sent), vec!["a".to_string()]);
        assert_eq!(sent[0]["enabled"], false);
        assert_eq!(sent[0]["switched_off"]["at"], 100);
        // switched on by someone who saw it off: allowed, and the mark goes
        let mut sent = vec![json!({"name": "a", "pool": "0xa", "chain_id": 8453, "enabled": true, "switched_off": {"at": 100}})];
        assert!(keep_switched_off(&[off], &mut sent).is_empty());
        assert_eq!(sent[0]["enabled"], true);
        assert!(sent[0].get("switched_off").is_none());
        // a pool the user switched off is theirs to switch on
        let mut sent = vec![json!({"name": "b", "pool": "0xb", "chain_id": 8453, "enabled": true})];
        assert!(keep_switched_off(&[json!({"name": "b", "pool": "0xb", "chain_id": 8453, "enabled": false})], &mut sent).is_empty());
    }
}
