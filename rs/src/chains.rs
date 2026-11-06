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
    /// Shown in the pools table (a view filter only).
    pub enabled: bool,
    /// Switched off (config/chains.json `off`): nothing on the chain is read or sent. Its pools are
    /// not run, valued or synced, its balances and rewards are not read, and the RPC layer refuses
    /// its requests. Only while none of its pools is on (set_off).
    pub off: bool,
    pub rpc: String,
    pub send_rpc: String,
    /// Most requests the RPC accepts in one batch (dRPC's free plan: 3).
    pub batch_size: usize,
    /// Requests per minute allowed to this chain's endpoints (None: 40 on dRPC's public endpoints,
    /// unlimited elsewhere; Some(0): unlimited).
    pub rate_per_min: Option<u32>,
    /// How transactions on this chain are priced: "low", "normal" (default) or "instant"
    /// (chain::GasSpeed).
    pub gas_speed: String,
    pub explorer: String,
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
    // RPCs default to dRPC's free public endpoints (batches of up to 3, log ranges up to 101 blocks,
    // though its refusal names 10,000); each chain's `rpc`, `send_rpc` and `batch_size` can be changed in the dashboard.
    json!({
        "8453": {"name": "Base", "enabled": true, "rpc": "https://base.drpc.org",
                 "send_rpc": "https://base.drpc.org", "batch_size": 3, "explorer": "https://basescan.org/",
                 "gas_symbol": "ETH", "min_gas": 0.0002,
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
               "gas_symbol": "ETH", "min_gas": 0.0002,
               "usd": "0x0b2c639c533813f4aa9d7837caf62653d097ff85",
               "wnative": "0x4200000000000000000000000000000000000006",
               "solid": "0x777cf5ba9c291a1a8f57ff14836f6f9dc5c0f9dd"},
        "1": {"name": "Ethereum", "enabled": false, "rpc": "https://eth.drpc.org",
              "send_rpc": "https://eth.drpc.org", "batch_size": 3, "explorer": "https://etherscan.io/",
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
    let user: Value = std::fs::read_to_string(crate::paths::app_config().join("chains.json"))
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
                off: v["off"].as_bool().unwrap_or(false),
                rpc: std::env::var(format!("ALM_RPC_URL_{id}"))
                    .or_else(|_| if id == BASE { std::env::var("ALM_RPC_URL") } else { Err(std::env::VarError::NotPresent) })
                    .unwrap_or_else(|_| v["rpc"].as_str().unwrap_or("").to_string())
                    .replace("{chain_id}", &id.to_string()),
                send_rpc: std::env::var(format!("ALM_SEND_RPC_URL_{id}"))
                    .or_else(|_| if id == BASE { std::env::var("ALM_SEND_RPC_URL") } else { Err(std::env::VarError::NotPresent) })
                    .unwrap_or_else(|_| v["send_rpc"].as_str().unwrap_or("").to_string()),
                batch_size: v["batch_size"].as_u64().unwrap_or(3).clamp(1, 100) as usize,
                rate_per_min: v["rate_per_min"].as_u64().map(|x| x.min(100_000) as u32),
                gas_speed: crate::chain::GasSpeed::parse(v["gas_speed"].as_str().unwrap_or("")).unwrap_or_default().name().into(),
                explorer: v["explorer"].as_str().unwrap_or("").to_string(),
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
        off: false,
        rpc: String::new(),
        send_rpc: String::new(),
        batch_size: 3,
        rate_per_min: None,
        gas_speed: "normal".into(),
        explorer: String::new(),
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

/// Show or hide a chain in the pools table (config/chains.json, `enabled`). Only a view filter: a
/// shown chain's other pools are listed and get the all-pools fee vs arbitrage figure. Balances,
/// funding and every configured pool work on every chain, shown or not.
pub fn set_enabled(id: u64, on: bool) -> Result<(), String> {
    if RETIRED.contains(&id) || !defaults().as_object().unwrap().contains_key(&id.to_string()) {
        return Err(format!("chain {id} is not supported"));
    }
    let p = crate::paths::app_config().join("chains.json");
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

/// Whether the chain is switched off.
pub fn is_off(id: u64) -> bool {
    get(id).off
}

/// The pools switched on (in any wallet) on chain `id`, by name.
fn pools_on(id: u64) -> Vec<String> {
    let mut out = vec![];
    for w in crate::wallets::ids() {
        crate::paths::scope(&w, || {
            for p in crate::settings::load_pools() {
                let c = crate::settings::PoolConfig::from(&p);
                if c.enabled && crate::worker::pool_chain(&p) == id {
                    out.push(c.name);
                }
            }
        });
    }
    out
}

/// Switch a chain off (or back on): config/chains.json `off`. Refused while a pool on it is on, in
/// any wallet: an off chain's pools are not run, so its positions would not be looked after.
pub fn set_off(id: u64, off: bool) -> Result<(), String> {
    if RETIRED.contains(&id) || !defaults().as_object().unwrap().contains_key(&id.to_string()) {
        return Err(format!("chain {id} is not supported"));
    }
    if off {
        let on = pools_on(id);
        if !on.is_empty() {
            return Err(format!("{} has pools switched on ({}): switch them off first", get(id).name, on.join(", ")));
        }
    }
    let p = crate::paths::app_config().join("chains.json");
    let mut user: Value = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    if !user.is_object() {
        user = json!({});
    }
    if !user[id.to_string()].is_object() {
        user[id.to_string()] = json!({});
    }
    user[id.to_string()]["off"] = json!(off);
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
    let p = crate::paths::app_config().join("chains.json");
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

/// Set how a chain's transactions are priced (config/chains.json, `gas_speed`): "low", "normal"
/// or "instant". Normal is the default and is stored as no entry.
pub fn set_gas_speed(id: u64, speed: &str) -> Result<(), String> {
    if RETIRED.contains(&id) || !defaults().as_object().unwrap().contains_key(&id.to_string()) {
        return Err(format!("chain {id} is not supported"));
    }
    let sp = crate::chain::GasSpeed::parse(speed).ok_or("gas_speed: expected low, normal or instant")?;
    // the app's chains.json, which every wallet reads (not the current wallet's folder)
    let p = crate::paths::app_config().join("chains.json");
    let mut user: Value = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    if !user.is_object() {
        user = json!({});
    }
    if !user[id.to_string()].is_object() {
        user[id.to_string()] = json!({});
    }
    let e = user[id.to_string()].as_object_mut().unwrap();
    if sp == crate::chain::GasSpeed::Normal {
        e.remove("gas_speed");
    } else {
        e.insert("gas_speed".into(), json!(sp.name()));
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
            .map(|c| json!({"id": c.id, "name": c.name, "enabled": c.enabled, "off": c.off, "explorer": c.explorer,
                            "gas_symbol": c.gas_symbol, "min_gas": c.min_gas_wei / 1e18, "usd": c.usd, "solid": c.solid,
                            "rpc": c.rpc, "send_rpc": c.send_rpc, "batch_size": c.batch_size,
                            "rate_per_min": c.rate_per_min, "gas_speed": c.gas_speed, "funding_default": default_funding(c.id), "rate_effective": crate::rpc::rate_for(&c, &c.rpc),
                            "rpc_default": defaults()[c.id.to_string()]["rpc"], "send_rpc_default": defaults()[c.id.to_string()]["send_rpc"]}))
            .collect(),
    )
}

// -- block timestamps -------------------------------------------------------------------------------
static ANCHOR: Mutex<Option<HashMap<u64, (i64, i64, f64)>>> = Mutex::new(None); // head block, head ts, s/block

/// A stretch of constant block time: its first block, that block's time, seconds per block.
type Stretch = (i64, i64, f64);

/// Changes of block time found while running (check_block_time), until a release knows them.
static FOUND: Mutex<Option<HashMap<u64, Vec<Stretch>>>> = Mutex::new(None);

/// The chain's block times where they are known exactly, oldest first; empty: estimated from the head.
fn stretches(id: u64) -> Vec<Stretch> {
    let mut s: Vec<Stretch> = if id == BASE {
        // 2 s from genesis, 0.2 s since 2026-11-05 18:00 UTC
        vec![(0, 1686789347, 2.0), (53556127, 1793901601, 0.2)]
    } else {
        vec![]
    };
    if let Some(f) = FOUND.lock().unwrap().as_ref().and_then(|m| m.get(&id)) {
        s.extend(f.iter().copied());
    }
    s
}

fn stretch_ts(s: &[Stretch], block: i64) -> i64 {
    let (b, t, bt) = s.iter().rev().find(|x| x.0 <= block).unwrap_or(&s[0]);
    t + (bt * (block - b) as f64) as i64
}

fn stretch_block(s: &[Stretch], ts: i64) -> i64 {
    let (b, t, bt) = s.iter().rev().find(|x| x.1 <= ts).unwrap_or(&s[0]);
    (b + ((ts - t) as f64 / bt).floor() as i64).max(0)
}

/// The first block of the chain's latest stretch of block time, where those are known.
pub fn last_block_time_change(id: u64) -> Option<i64> {
    stretches(id).last().map(|s| s.0).filter(|b| *b > 0)
}

/// (number, time) of a block, or the latest one.
fn header(id: u64, block: Option<i64>) -> Option<(i64, i64)> {
    let tag = block.map(|b| format!("0x{b:x}")).unwrap_or_else(|| "latest".into());
    let r = crate::rpc::batch_quick(id, &[("eth_getBlockByNumber", json!([tag, false]))]).ok()?;
    let h = r[0].as_ref().ok()?;
    h.get("number")?;
    Some((crate::rpc::hex_u64(&h["number"]) as i64, crate::rpc::hex_u64(&h["timestamp"]) as i64))
}

/// A stretch that began where the times stop fitting `s`, or None while they fit (or cannot be read).
fn find_change(id: u64, s: &[Stretch]) -> Option<Stretch> {
    let (head, head_ts) = header(id, None)?;
    if (stretch_ts(s, head) - head_ts).abs() <= 60 {
        return None;
    }
    let (mut lo, mut hi) = (s.last()?.0, head);
    while hi - lo > 1 {
        let m = (lo + hi) / 2;
        let (_, t) = header(id, Some(m))?;
        if (stretch_ts(s, m) - t).abs() <= 5 {
            lo = m;
        } else {
            hi = m;
        }
    }
    let (_, t) = header(id, Some(hi))?;
    if head - hi < 100 {
        return None; // too few blocks to measure yet: the next check
    }
    Some((hi, t, ((head_ts - t) as f64 / (head - hi) as f64 * 1000.0).round() / 1000.0))
}

/// Worker cycle: a chain's block time can change (Base's did, tenfold), and every time and range
/// derived from it would then be wrong without a word. Compared with the head at most every 10
/// minutes; a change is logged, used from then on, and what the store holds is put right. Also
/// repairs a store left by a version that did not know the latest change, once per run.
pub fn check_block_time(id: u64) {
    static CHECKED: Mutex<Option<HashMap<u64, std::time::Instant>>> = Mutex::new(None);
    let s = stretches(id);
    if s.is_empty() {
        return;
    }
    let first = {
        let mut g = CHECKED.lock().unwrap();
        let m = g.get_or_insert_with(HashMap::new);
        if m.get(&id).map(|t| t.elapsed().as_secs() < 600).unwrap_or(false) {
            return;
        }
        m.insert(id, std::time::Instant::now()).is_none()
    };
    let change = find_change(id, &s);
    if let Some(c) = change {
        crate::error!("chains", "{}: the block time changed at block {} to {} s; times are corrected, and a release should carry this", get(id).name, c.0, c.2);
        FOUND.lock().unwrap().get_or_insert_with(HashMap::new).entry(id).or_default().push(c);
    }
    if first || change.is_some() {
        if let Err(e) = crate::history::repair_timestamps(id) {
            crate::error!("history", "chain {id}: stored times not repaired: {e}");
        }
    }
}

/// Timestamp of a block: exact where the chain's block times are known (Base), else interpolated
/// from the head header and the average block time over the last ~100k blocks.
pub fn block_ts(id: u64, block: i64) -> i64 {
    let s = stretches(id);
    if !s.is_empty() {
        return stretch_ts(&s, block);
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

/// The block at time `ts`, by the same estimate block_ts uses (so the two agree).
pub fn block_at_ts(id: u64, ts: i64) -> i64 {
    let s = stretches(id);
    if !s.is_empty() {
        return stretch_block(&s, ts);
    }
    block_ts(id, 0); // anchors the estimate
    let a = ANCHOR.lock().unwrap();
    match a.as_ref().and_then(|m| m.get(&id)) {
        Some((hb, hts, spb)) => (hb - ((hts - ts) as f64 / spb).ceil() as i64).max(0),
        None => 0,
    }
}

/// Blocks per day at the chain's current block time (Base: 432,000).
pub fn blocks_per_day(id: u64) -> i64 {
    if let Some(s) = stretches(id).last() {
        return (86400.0 / s.2) as i64;
    }
    block_ts(id, 0);
    let a = ANCHOR.lock().unwrap();
    a.as_ref().and_then(|m| m.get(&id)).map(|(_, _, spb)| (86400.0 / spb) as i64).unwrap_or(43_200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_block_times_follow_its_change_from_2_s_to_0_2_s() {
        // real headers
        assert_eq!(block_ts(BASE, 51_000_000) - 1686789347, 102_000_000);
        assert_eq!(block_ts(BASE, 53556129), 1793901601);
        assert_eq!(block_ts(BASE, 53835614), 1793957498);
        assert_eq!(blocks_per_day(BASE), 432_000);
        for b in [1_000_000, 53_556_126, 53_556_127, 53_835_614] {
            assert!((block_at_ts(BASE, block_ts(BASE, b)) - b).abs() <= 5, "{b}");
        }
    }

    #[test]
    fn a_later_stretch_applies_from_its_first_block() {
        let s = [(0, 1000, 2.0), (100, 1200, 0.5)];
        assert_eq!(stretch_ts(&s, 99), 1198);
        assert_eq!(stretch_ts(&s, 100), 1200);
        assert_eq!(stretch_ts(&s, 110), 1205);
        assert_eq!(stretch_block(&s, 1198), 99);
        assert_eq!(stretch_block(&s, 1205), 110);
    }

    #[test]
    fn a_chain_switched_off_is_not_read_and_needs_its_pools_off_first() {
        let d = crate::wallets::tests::temp_home("chainoff");
        let sonic = 146;
        std::fs::create_dir_all(crate::paths::config()).unwrap();
        let pool = |on: bool| json!([{"name": "ws-anon", "pool": "0x00000000000000000000000000000000000000c1", "chain_id": sonic, "enabled": on}]);
        crate::settings::write_atomic(&crate::paths::config().join("pools.json"), &pool(true)).unwrap();
        let e = set_off(sonic, true).unwrap_err();
        assert!(e.contains("ws-anon"), "{e}");
        assert!(!is_off(sonic));
        // the pool switched off: the chain can go off, and nothing is asked of its RPC
        crate::settings::write_atomic(&crate::paths::config().join("pools.json"), &pool(false)).unwrap();
        set_off(sonic, true).unwrap();
        assert!(is_off(sonic) && !is_off(BASE));
        let e = crate::rpc::call(sonic, "eth_blockNumber", json!([])).unwrap_err();
        assert!(e.contains("switched off"), "{e}");
        assert_eq!(as_json().as_array().unwrap().iter().find(|c| c["id"] == sonic).unwrap()["off"], true);
        set_off(sonic, false).unwrap();
        assert!(!is_off(sonic));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_gas_speed_set_from_any_wallet_lands_in_the_app_wide_chains_file() {
        let d = crate::wallets::tests::temp_home("gasspeed");
        crate::paths::scope("second", || set_gas_speed(BASE, "instant")).unwrap();
        let read = |p: std::path::PathBuf| std::fs::read_to_string(p).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(json!({}));
        assert_eq!(read(crate::paths::app_config().join("chains.json"))["8453"]["gas_speed"], "instant");
        assert!(!crate::paths::scope("second", crate::paths::config).join("chains.json").exists());
        // normal is the default, stored as no entry
        set_gas_speed(BASE, "normal").unwrap();
        assert!(read(crate::paths::app_config().join("chains.json"))["8453"].get("gas_speed").is_none());
        assert!(set_gas_speed(BASE, "fast").is_err());
        let _ = std::fs::remove_dir_all(d);
    }
}
