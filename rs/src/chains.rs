//! Chain registry (docs/MULTICHAIN_PLAN.md): defaults per chain, overridden by config/chains.json.
//! Nothing else in the runtime hard-codes a chain.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

pub const BASE: u64 = 8453;
/// Chains Solidly no longer runs on (Fantom, retired 4 Oct 2026).
pub const RETIRED: [u64; 1] = [250];

#[derive(Clone, Debug)]
pub struct Chain {
    pub id: u64,
    pub name: String,
    pub enabled: bool,
    pub rpc: String,
    pub send_rpc: String,
    /// Most requests the RPC accepts in one batch (dRPC's free plan: 3).
    pub batch_size: usize,
    /// Requests per minute allowed to this chain's endpoints (None: 40 on dRPC's public endpoints,
    /// unlimited elsewhere; Some(0): unlimited).
    pub rate_per_min: Option<u32>,
    pub explorer: String,
    pub genesis_ts: Option<i64>,
    pub block_time_s: Option<f64>,
    pub gas_symbol: String,
    pub min_gas_wei: f64,
    pub usd: String,
    pub solid: String,
    pub solid_lp_pool: String,
    pub wnative: String,
}

fn defaults() -> Value {
    // The chains Solidly V3 runs on (Fantom was retired on 4 Oct 2026). SOLID has the same address
    // everywhere except Ethereum. `wnative` is the wrapped gas token, used to price gas in USD.
    // RPCs default to dRPC's free public endpoints (batches of up to 3, log ranges up to 10,000
    // blocks); each chain's `rpc`, `send_rpc` and `batch_size` can be changed in the dashboard.
    json!({
        "8453": {"name": "Base", "enabled": true, "rpc": "https://base.drpc.org",
                 "send_rpc": "https://base.drpc.org", "batch_size": 3, "explorer": "https://basescan.org/",
                 "block_time_s": 2, "genesis_ts": 1686789347, "gas_symbol": "ETH", "min_gas": 0.0002,
                 "usd": "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913",
                 "wnative": "0x4200000000000000000000000000000000000006",
                 "solid": "0x777cf5ba9c291a1a8f57ff14836f6f9dc5c0f9dd",
                 "solid_lp_pool": "0xd09f2ebaf365fc97a5bbdefc15703312165638e4"},
        "42161": {"name": "Arbitrum", "enabled": false, "rpc": "https://arbitrum.drpc.org",
                  "send_rpc": "https://arbitrum.drpc.org", "batch_size": 3, "explorer": "https://arbiscan.io/", "gas_symbol": "ETH",
                  "min_gas": 0.0002, "usd": "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
                  "wnative": "0x82af49447d8a07e3bd95bd0d56f35241523fbab1",
                  "solid": "0x777cf5ba9c291a1a8f57ff14836f6f9dc5c0f9dd"},
        "10": {"name": "Optimism", "enabled": false, "rpc": "https://optimism.drpc.org",
               "send_rpc": "https://optimism.drpc.org", "batch_size": 3, "explorer": "https://optimistic.etherscan.io/",
               "block_time_s": 2, "gas_symbol": "ETH", "min_gas": 0.0002,
               "usd": "0x0b2c639c533813f4aa9d7837caf62653d097ff85",
               "wnative": "0x4200000000000000000000000000000000000006",
               "solid": "0x777cf5ba9c291a1a8f57ff14836f6f9dc5c0f9dd"},
        "1": {"name": "Ethereum", "enabled": false, "rpc": "https://eth.drpc.org",
              "send_rpc": "https://eth.drpc.org", "batch_size": 3, "explorer": "https://etherscan.io/", "block_time_s": 12,
              "gas_symbol": "ETH", "min_gas": 0.003, "usd": "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
              "wnative": "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2",
              "solid": "0x777172d858dc1599914a1c4c6c9fc48c99a60990"},
        "146": {"name": "Sonic", "enabled": false, "rpc": "https://sonic.drpc.org",
                "send_rpc": "https://sonic.drpc.org", "batch_size": 3, "explorer": "https://sonicscan.org/", "gas_symbol": "S",
                "min_gas": 0.5, "usd": "0x29219dd400f2bf60e5a23d13be72b486d4038894",
                "wnative": "0x039e2fb66102314ce7b64ce5ce3e5183bc94ad38",
                "solid": "0x777cf5ba9c291a1a8f57ff14836f6f9dc5c0f9dd"}
    })
}

static CACHE: Mutex<Option<HashMap<u64, Chain>>> = Mutex::new(None);

fn load_all() -> HashMap<u64, Chain> {
    let user: Value = std::fs::read_to_string(crate::paths::config().join("chains.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({}));
    let merged = crate::settings::merge(&defaults(), &user);
    let mut out = HashMap::new();
    for (k, v) in merged.as_object().unwrap() {
        let Ok(id) = k.parse::<u64>() else { continue };
        if RETIRED.contains(&id) {
            continue;
        }
        let s = |f: &str| v[f].as_str().unwrap_or("").to_lowercase();
        out.insert(
            id,
            Chain {
                id,
                name: v["name"].as_str().unwrap_or(k).to_string(),
                enabled: v["enabled"].as_bool().unwrap_or(false),
                rpc: std::env::var(format!("ALM_RPC_URL_{id}"))
                    .or_else(|_| if id == BASE { std::env::var("ALM_RPC_URL") } else { Err(std::env::VarError::NotPresent) })
                    .unwrap_or_else(|_| v["rpc"].as_str().unwrap_or("").to_string())
                    .replace("{chain_id}", &id.to_string()),
                send_rpc: std::env::var(format!("ALM_SEND_RPC_URL_{id}"))
                    .or_else(|_| if id == BASE { std::env::var("ALM_SEND_RPC_URL") } else { Err(std::env::VarError::NotPresent) })
                    .unwrap_or_else(|_| v["send_rpc"].as_str().unwrap_or("").to_string()),
                batch_size: v["batch_size"].as_u64().unwrap_or(3).clamp(1, 100) as usize,
                rate_per_min: v["rate_per_min"].as_u64().map(|x| x.min(100_000) as u32),
                explorer: v["explorer"].as_str().unwrap_or("").to_string(),
                genesis_ts: v["genesis_ts"].as_i64(),
                block_time_s: v["block_time_s"].as_f64(),
                gas_symbol: v["gas_symbol"].as_str().unwrap_or("ETH").to_string(),
                min_gas_wei: v["min_gas"].as_f64().unwrap_or(0.0002) * 1e18,
                usd: s("usd"),
                solid: s("solid"),
                solid_lp_pool: s("solid_lp_pool"),
                wnative: s("wnative"),
            },
        );
    }
    out
}

pub fn get(id: u64) -> Chain {
    let mut c = CACHE.lock().unwrap();
    let m = c.get_or_insert_with(load_all);
    m.get(&id).cloned().unwrap_or_else(|| Chain {
        id,
        name: format!("chain {id}"),
        enabled: false,
        rpc: String::new(),
        send_rpc: String::new(),
        batch_size: 3,
        rate_per_min: None,
        explorer: String::new(),
        genesis_ts: None,
        block_time_s: None,
        gas_symbol: "ETH".into(),
        min_gas_wei: 2e14,
        usd: String::new(),
        solid: String::new(),
        solid_lp_pool: String::new(),
        wnative: String::new(),
    })
}

pub fn all() -> Vec<Chain> {
    let mut c = CACHE.lock().unwrap();
    let mut v: Vec<Chain> = c.get_or_insert_with(load_all).values().cloned().collect();
    v.sort_by_key(|c| c.id);
    v
}

/// Turn a chain on or off (config/chains.json). Enabled chains are listed in the pools table and
/// get the all-pools fee vs arbitrage figure; a chain with configured pools is managed either way.
pub fn set_enabled(id: u64, on: bool) -> Result<(), String> {
    if RETIRED.contains(&id) || !defaults().as_object().unwrap().contains_key(&id.to_string()) {
        return Err(format!("chain {id} is not supported"));
    }
    let p = crate::paths::config().join("chains.json");
    let mut user: Value = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    if !user.is_object() {
        user = json!({});
    }
    if !user[id.to_string()].is_object() {
        user[id.to_string()] = json!({});
    }
    user[id.to_string()]["enabled"] = json!(on);
    crate::settings::write_atomic(&p, &user).map_err(|e| e.to_string())?;
    *CACHE.lock().unwrap() = None;
    Ok(())
}

/// Change a chain's RPC settings (config/chains.json): `rpc` (reads), `send_rpc` (signed
/// transactions), `batch_size`. An empty URL restores the default.
pub fn set_rpc(id: u64, rpc: Option<&str>, send_rpc: Option<&str>, batch_size: Option<u64>, rate_per_min: Option<Option<u64>>) -> Result<(), String> {
    let d = defaults();
    if RETIRED.contains(&id) || !d.as_object().unwrap().contains_key(&id.to_string()) {
        return Err(format!("chain {id} is not supported"));
    }
    let p = crate::paths::config().join("chains.json");
    let mut user: Value = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    if !user.is_object() {
        user = json!({});
    }
    if !user[id.to_string()].is_object() {
        user[id.to_string()] = json!({});
    }
    let e = &mut user[id.to_string()];
    for (k, v) in [("rpc", rpc), ("send_rpc", send_rpc)] {
        let Some(v) = v.map(str::trim) else { continue };
        if v.is_empty() {
            e.as_object_mut().unwrap().remove(k);
            continue;
        }
        if !(v.starts_with("https://") || v.starts_with("http://127.0.0.1") || v.starts_with("http://localhost")) || v.contains(char::is_whitespace) {
            return Err(format!("{k}: expected an https:// URL"));
        }
        e[k] = json!(v);
    }
    if let Some(b) = batch_size {
        if !(1..=100).contains(&b) {
            return Err("batch_size: expected 1 to 100".into());
        }
        e["batch_size"] = json!(b);
    }
    match rate_per_min {
        Some(Some(r)) if r > 100_000 => return Err("rate_per_min: expected 0 (no limit) to 100000".into()),
        Some(Some(r)) => e["rate_per_min"] = json!(r),
        Some(None) => {
            e.as_object_mut().unwrap().remove("rate_per_min");
        }
        None => {}
    }
    crate::settings::write_atomic(&p, &user).map_err(|e| e.to_string())?;
    *CACHE.lock().unwrap() = None;
    Ok(())
}

/// Check that an RPC answers for the right chain (the dashboard's "Test").
pub fn test_rpc(id: u64, url: &str) -> Result<Value, String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": []});
    let t = std::time::Instant::now();
    // a free endpoint may answer 429 while the app itself is busy on it: wait and ask again
    let mut tries = 0;
    let v: Value = loop {
        match crate::rpc::agent().post(url).timeout(std::time::Duration::from_secs(15)).send_json(body.clone()) {
            Ok(r) => break r.into_json().map_err(|e| e.to_string())?,
            Err(ureq::Error::Status(429, _)) if tries < 3 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(700 * tries));
            }
            Err(ureq::Error::Status(429, _)) => return Err("rate limited (HTTP 429) — the endpoint works but is busy; try again".into()),
            Err(e) => return Err(e.to_string()),
        }
    };
    let got = crate::rpc::hex_u64(&v["result"]);
    if got != id {
        return Err(format!("that RPC is for chain {got}, not {id}"));
    }
    Ok(json!({"ok": true, "ms": t.elapsed().as_millis() as u64}))
}

/// Default funding tokens of a chain: its USDC and USDT (checked on-chain).
pub fn default_funding(id: u64) -> Vec<String> {
    let v: &[&str] = match id {
        1 => &["0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48", "0xdac17f958d2ee523a2206206994597c13d831ec7"],
        10 => &["0x0b2c639c533813f4aa9d7837caf62653d097ff85", "0x94b008aa00579c1307b0ef2c499ad98a8ce58e58"],
        146 => &["0x29219dd400f2bf60e5a23d13be72b486d4038894", "0x6047828dc181963ba44974801ff68e538da5eaf9"],
        8453 => &["0x833589fcd6edb6e08f4c7c32d4f71b54bda02913", "0xfde4c96c8593536e31f229ea8f37b2ada2699bb2"],
        42161 => &["0xaf88d065e77c8cc2239327c5edb3a432268e5831", "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9"],
        _ => &[],
    };
    v.iter().map(|s| s.to_string()).collect()
}

/// Registry for the UI.
pub fn as_json() -> Value {
    Value::Array(
        all()
            .into_iter()
            .map(|c| json!({"id": c.id, "name": c.name, "enabled": c.enabled, "explorer": c.explorer,
                            "gas_symbol": c.gas_symbol, "min_gas": c.min_gas_wei / 1e18, "usd": c.usd, "solid": c.solid,
                            "rpc": c.rpc, "send_rpc": c.send_rpc, "batch_size": c.batch_size,
                            "rate_per_min": c.rate_per_min, "funding_default": default_funding(c.id), "rate_effective": crate::rpc::rate_for(&c, &c.rpc),
                            "rpc_default": defaults()[c.id.to_string()]["rpc"], "send_rpc_default": defaults()[c.id.to_string()]["send_rpc"]}))
            .collect(),
    )
}

// -- block timestamps -------------------------------------------------------------------------------
static ANCHOR: Mutex<Option<HashMap<u64, (i64, i64, f64)>>> = Mutex::new(None); // head block, head ts, s/block

/// Timestamp of a block: exact on chains with a fixed block time since genesis (Base), else
/// interpolated from the head header and the average block time over the last ~100k blocks.
pub fn block_ts(id: u64, block: i64) -> i64 {
    let c = get(id);
    if let (Some(g), Some(bt)) = (c.genesis_ts, c.block_time_s) {
        return g + (bt * block as f64) as i64;
    }
    let mut a = ANCHOR.lock().unwrap();
    let m = a.get_or_insert_with(HashMap::new);
    if !m.contains_key(&id) {
        if let Some(x) = measure(id) {
            m.insert(id, x);
        }
    }
    match m.get(&id) {
        Some((hb, hts, spb)) => hts - ((*hb - block) as f64 * spb) as i64,
        None => 0,
    }
}

fn measure(id: u64) -> Option<(i64, i64, f64)> {
    let head = crate::rpc::block_number(id).ok()? as i64;
    let back = (head - 100_000).max(1);
    let r = crate::rpc::batch(
        id,
        &[
            ("eth_getBlockByNumber", json!([format!("0x{head:x}"), false])),
            ("eth_getBlockByNumber", json!([format!("0x{back:x}"), false])),
        ],
    )
    .ok()?;
    let ts = |x: &crate::rpc::R| x.as_ref().ok().map(|b| crate::rpc::hex_u64(&b["timestamp"]) as i64);
    let (t1, t0) = (ts(&r[0])?, ts(&r[1])?);
    Some((head, t1, (t1 - t0) as f64 / (head - back) as f64))
}

/// Blocks per day (Base: 43,200).
/// The block at time `ts`, by the same estimate block_ts uses (so the two agree).
pub fn block_at_ts(id: u64, ts: i64) -> i64 {
    let c = get(id);
    if let (Some(g), Some(bt)) = (c.genesis_ts, c.block_time_s) {
        return (((ts - g) as f64) / bt).floor().max(0.0) as i64;
    }
    block_ts(id, 0); // anchors the estimate
    let a = ANCHOR.lock().unwrap();
    match a.as_ref().and_then(|m| m.get(&id)) {
        Some((hb, hts, spb)) => (hb - ((hts - ts) as f64 / spb).ceil() as i64).max(0),
        None => 0,
    }
}

pub fn blocks_per_day(id: u64) -> i64 {
    let c = get(id);
    if let Some(bt) = c.block_time_s {
        return (86400.0 / bt) as i64;
    }
    block_ts(id, 0);
    let a = ANCHOR.lock().unwrap();
    a.as_ref().and_then(|m| m.get(&id)).map(|(_, _, spb)| (86400.0 / spb) as i64).unwrap_or(43_200)
}
