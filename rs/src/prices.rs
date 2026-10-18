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

// -- what a swap transaction may do -------------------------------------------------------------------
// The aggregator API proposes the router, the spender and the calldata. None of that is trusted on its
// own: the router and spender must be known swap routers, and the transaction's token transfers (from a
// simulation before signing where the node supports it, and from the receipt afterwards) must only take
// the input token, up to the amount, and must deliver at least the minimum output to the wallet.

/// Swap routers the app may call or approve, per chain (lowercase). Others can be added in
/// config/routers.json as {"<chain id>": ["0x..."]}.
fn default_routers(cid: u64) -> Vec<&'static str> {
    let mut v = vec![
        "0x6131b5fae19ea4f9d964eac0408e4408b66337b5", // KyberSwap MetaAggregationRouterV2
        "0x6a000f20005980200259b80c5102003040001068", // Velora (ParaSwap) Augustus v6.2
        "0x111111125421ca6dc452d289314280a0f8842a65", // 1inch Aggregation Router v6
        "0x6352a56caadc4f1e25cd6c75970fa768a3304e64", // OpenOcean Exchange V2
    ];
    v.push(match cid {
        1 => "0xcf5540fffcdc3d510b18bfca6d2b9987b0772559",     // Odos Router V2
        10 => "0xca423977156bb05b13a2ba3b76bc5419e2fe9680",
        8453 => "0x19ceead7105607cd444f5ad10dd51356436095a1",
        42161 => "0xa669e7a0d4b3e4fa48af2de86bd4cd7126be4e13",
        _ => "",
    });
    v.into_iter().filter(|a| !a.is_empty()).collect()
}

pub fn router_allowed(cid: u64, addr: &str) -> bool {
    let a = addr.to_lowercase();
    if default_routers(cid).contains(&a.as_str()) {
        return true;
    }
    std::fs::read_to_string(crate::paths::config().join("routers.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v[cid.to_string()].as_array().cloned())
        .map(|l| l.iter().any(|x| x.as_str().map(|s| s.to_lowercase()) == Some(a.clone())))
        .unwrap_or(false)
}

/// An ERC-20 transfer (token, from, to, raw amount), lowercase addresses.
pub type Transfer = (String, String, String, f64);
pub const NATIVE: &str = "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

/// Transfer events in a list of logs (receipt or simulation).
pub fn transfers(logs: &[Value]) -> Vec<Transfer> {
    let topic = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
    let addr = |t: &Value| { let s = t.as_str().unwrap_or(""); format!("0x{}", &s[s.len().saturating_sub(40)..]).to_lowercase() };
    logs.iter()
        .filter_map(|l| {
            let t = l["topics"].as_array()?;
            if t.len() != 3 || t[0].as_str()?.to_lowercase() != topic {
                return None;
            }
            let amt = crate::eth::parse_uint(l["data"].as_str().unwrap_or("0x0")).to_string().parse::<f64>().unwrap_or(0.0);
            Some((l["address"].as_str()?.to_lowercase(), addr(&t[1]), addr(&t[2]), amt))
        })
        .collect()
}

/// Check a swap's transfers: the wallet may only send `tin` (at most `amount_in`, within rounding) and
/// must receive at least `min_out` of `tout`. Returns the amount of `tout` received.
pub fn check_swap(wallet: &str, tin: &str, amount_in: f64, tout: &str, min_out: f64, tr: &[Transfer]) -> Result<f64, String> {
    let (w, tin, tout) = (wallet.to_lowercase(), tin.to_lowercase(), tout.to_lowercase());
    let mut spent = 0.0;
    for (tok, from, _, amt) in tr.iter().filter(|t| t.1 == w && t.3 > 0.0) {
        if *tok != tin {
            return Err(format!("the swap would move {amt} of token {tok} out of the wallet (only {tin} may leave); from {from}"));
        }
        spent += amt;
    }
    if spent > amount_in * 1.000001 + 1.0 {
        return Err(format!("the swap would take {spent} of the input token, more than the {amount_in} requested"));
    }
    let got: f64 = tr.iter().filter(|t| t.0 == tout && t.2 == w && t.1 != w).map(|t| t.3).sum::<f64>()
        - tr.iter().filter(|t| t.0 == tout && t.1 == w && t.2 != w).map(|t| t.3).sum::<f64>();
    if got < min_out {
        return Err(format!("the swap would deliver {got} of the output token to the wallet, below the minimum {min_out}"));
    }
    Ok(got)
}

#[cfg(test)]
mod swap_checks {
    use super::*;
    const W: &str = "0x00000000000000000000000000000000000000aa";
    const A: &str = "0x00000000000000000000000000000000000000a1";
    const B: &str = "0x00000000000000000000000000000000000000b1";
    const X: &str = "0x00000000000000000000000000000000000000c1";
    const R: &str = "0x00000000000000000000000000000000000000ff";
    fn t(tok: &str, f: &str, to: &str, a: f64) -> Transfer {
        (tok.into(), f.into(), to.into(), a)
    }

    #[test]
    fn good_swap_passes() {
        let tr = vec![t(A, W, R, 100.0), t(B, R, W, 99.0)];
        assert_eq!(check_swap(W, A, 100.0, B, 98.0, &tr).unwrap(), 99.0);
    }

    #[test]
    fn foreign_token_outflow_is_refused() {
        let tr = vec![t(X, W, R, 5.0), t(B, R, W, 99.0)];
        assert!(check_swap(W, A, 100.0, B, 98.0, &tr).is_err());
    }

    #[test]
    fn overspend_and_short_output_are_refused() {
        assert!(check_swap(W, A, 100.0, B, 98.0, &[t(A, W, R, 200.0), t(B, R, W, 99.0)]).is_err());
        assert!(check_swap(W, A, 100.0, B, 98.0, &[t(A, W, R, 100.0), t(B, R, W, 1.0)]).is_err());
        assert!(check_swap(W, A, 100.0, B, 98.0, &[t(A, W, R, 100.0), t(B, R, X, 99.0)]).is_err());
    }

    #[test]
    fn unknown_router_is_refused() {
        assert!(router_allowed(8453, "0x6131B5fae19EA4f9D964eAc0408E4408b66337b5"));
        assert!(!router_allowed(8453, "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"));
    }
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
    let sk = crate::store::token_key(cid, &key.1);
    if let Some(v) = crate::store::get(&sk) {
        if let (Some(d), Some(s)) = (v["decimals"].as_u64(), v["symbol"].as_str()) {
            let m = Meta { decimals: d as u32, symbol: s.to_string() };
            META.lock().unwrap().as_mut().unwrap().insert(key, m.clone());
            return Ok(m);
        }
    }
    let (decimals, symbol) = crate::chain::erc20_meta(cid, &key.1)?;
    crate::store::merge(&sk, serde_json::json!({"decimals": decimals, "symbol": symbol}));
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
