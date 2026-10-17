//! Swaps through the Solidly UI meta-aggregator (alm/executor/aggregator.py) and USD prices from its
//! quotes (alm/executor/prices.py, cached 10 min; USDC = 1). Token metadata cache.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const BASE: &str = "https://solidly.com/api/swap";

pub fn quote(cid: u64, tin: &str, tout: &str, amount: &str, din: u32, dout: u32) -> Result<Value, String> {
    crate::rpc::get_json(&format!(
        "{BASE}/quote/?chainId={cid}&tokenIn={tin}&tokenOut={tout}&amountIn={amount}&decimalsIn={din}&decimalsOut={dout}"
    ))
}

pub fn build(cid: u64, best: &Value, wallet: &str, slippage: f64) -> Result<Value, String> {
    let body = json!({"aggregatorId": best["aggregatorId"], "chainId": cid, "raw": best["raw"], "sender": wallet,
                      "recipient": wallet, "slippage": slippage, "deadlineMinutes": 5});
    let built = crate::rpc::post_json(&format!("{BASE}/build/"), &body, &[])?;
    if built["calldata"].as_str().unwrap_or("").is_empty() {
        return Err(format!("aggregator build failed: {built}"));
    }
    let value = built["value"].as_str().map(crate::eth::parse_uint).unwrap_or_default();
    if value != num_bigint::BigUint::default() || built["value"].as_u64().unwrap_or(0) != 0 {
        return Err("aggregator route wants native ETH value; refusing".into());
    }
    Ok(built)
}

#[derive(Clone)]
pub struct Meta {
    pub decimals: u32,
    pub symbol: String,
}

static META: Mutex<Option<HashMap<(u64, String), Meta>>> = Mutex::new(None);
static USD: Mutex<Option<HashMap<(u64, String), (f64, Instant)>>> = Mutex::new(None);

pub fn meta(cid: u64, token: &str) -> Result<Meta, String> {
    let key = (cid, token.to_lowercase());
    if let Some(m) = META.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        return Ok(m.clone());
    }
    let (decimals, symbol) = crate::chain::erc20_meta(cid, &key.1)?;
    let m = Meta { decimals, symbol };
    META.lock().unwrap().as_mut().unwrap().insert(key, m.clone());
    Ok(m)
}

pub fn usd(cid: u64, token: &str) -> Result<f64, String> {
    let t = token.to_lowercase();
    let usdc = crate::chains::get(cid).usd;
    if t == usdc {
        return Ok(1.0);
    }
    if let Some((p, at)) = USD.lock().unwrap().get_or_insert_with(HashMap::new).get(&(cid, t.clone())) {
        if at.elapsed() < Duration::from_secs(600) {
            return Ok(*p);
        }
    }
    let d = meta(cid, &t)?.decimals;
    let ud = meta(cid, &usdc)?.decimals;
    let q = quote(cid, &t, &usdc, &format!("1{}", "0".repeat(d as usize)), d, ud)?;
    let best = &q["best"];
    let p = best["amountInUsd"].as_f64().or_else(|| best["amountInUsd"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0);
    let p = if p > 0.0 { p } else { num_out(best) / 10f64.powi(ud as i32) };
    if p <= 0.0 {
        return Err(format!("no USD price for {t}"));
    }
    USD.lock().unwrap().as_mut().unwrap().insert((cid, t), (p, Instant::now()));
    Ok(p)
}

/// best.amountOut as f64 (raw units).
pub fn num_out(best: &Value) -> f64 {
    best["amountOut"].as_str().and_then(|s| s.parse().ok()).or_else(|| best["amountOut"].as_f64()).unwrap_or(0.0)
}

pub fn fnum(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0)
}
