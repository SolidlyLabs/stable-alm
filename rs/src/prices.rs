//! Swaps through the Solidly UI meta-aggregator and USD prices from its quotes (cached 10 min;
//! USDC = 1). Token metadata cache.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const BASE: &str = "https://solidly.com/api/swap";

pub fn quote(cid: u64, tin: &str, tout: &str, amount: &str, din: u32, dout: u32) -> Result<Value, String> {
    quote_from(cid, tin, tout, amount, din, dout, None)
}

/// A quote from `aggregators` only (comma-separated ids), or from all of them.
pub fn quote_from(cid: u64, tin: &str, tout: &str, amount: &str, din: u32, dout: u32, aggregators: Option<&str>) -> Result<Value, String> {
    let only = aggregators.map(|a| format!("&aggregators={a}")).unwrap_or_default();
    crate::rpc::get_json(&format!(
        "{BASE}/quote/?chainId={cid}&tokenIn={tin}&tokenOut={tout}&amountIn={amount}&decimalsIn={din}&decimalsOut={dout}{only}"
    ))
}

/// The other aggregators that quoted in `q` (its candidates), besides `failed`: comma-separated ids,
/// None when there is none.
pub fn other_aggregators(q: &Value, failed: &str) -> Option<String> {
    let ids: Vec<String> = q["candidates"].as_array().into_iter().flatten()
        .filter_map(|c| c["aggregatorId"].as_str())
        .filter(|a| !a.eq_ignore_ascii_case(failed))
        .map(str::to_string)
        .collect();
    (!ids.is_empty()).then(|| ids.join(","))
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
    std::fs::read_to_string(crate::paths::app_config().join("routers.json"))
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
mod stale_price {
    use super::stale_ok;
    use std::time::Duration;

    #[test]
    fn a_price_from_the_last_hour_serves_when_it_cannot_be_refreshed() {
        assert_eq!(stale_ok(Some((1.0001, Duration::from_secs(1800)))), Some(1.0001));
        assert_eq!(stale_ok(Some((1.0001, Duration::from_secs(3700)))), None);
        assert_eq!(stale_ok(Some((0.0, Duration::from_secs(10)))), None);
        assert_eq!(stale_ok(None), None);
    }
}

#[cfg(test)]
mod other_aggregator_tests {
    use super::*;

    #[test]
    fn a_failed_build_is_quoted_again_by_the_other_aggregators() {
        let q = json!({"best": {"aggregatorId": "velora"},
                       "candidates": [{"aggregatorId": "velora"}, {"aggregatorId": "kyberswap"}, {"aggregatorId": "odos"}]});
        assert_eq!(other_aggregators(&q, "velora").as_deref(), Some("kyberswap,odos"));
        assert_eq!(other_aggregators(&json!({"candidates": [{"aggregatorId": "velora"}]}), "Velora"), None);
        assert_eq!(other_aggregators(&json!({}), "velora"), None);
    }
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

/// A pool token's price tied to its pair's: token → (the other token, how many of it one token is
/// worth, when the pool said so). Each token's USD quote is cached on its own for 10 minutes, so in a
/// fast move the two of a pair are read minutes apart (cbETH read 1.12 to 1.18 WETH while the pool
/// stood at 1.144), and the pool's value and its hold benchmark jumped with that.
static ANCHOR: Mutex<Option<HashMap<(u64, String), (String, f64, Instant)>>> = Mutex::new(None);
/// how long a pool's rate stands in for a quote (a cycle reads it again each minute or so)
const ANCHOR_S: u64 = 900;
/// a pool rate this far from the two quotes' own is not used (a thin or stuck pool)
const ANCHOR_MAX_GAP: f64 = 0.10;

/// Which of two tokens prices the other: the chain's USD stable, then the wrapped gas token. None
/// when neither is one of those (both are then quoted on their own).
fn anchor_rank(cid: u64, t: &str) -> u8 {
    let ch = crate::chains::get(cid);
    if t == ch.usd {
        0
    } else if t == ch.wnative.to_lowercase() {
        1
    } else {
        2
    }
}

/// The token of a pair priced from the other, with how many of the other one of it is worth, given
/// the pool's price `p0in1` (whole token1 per whole token0). None when neither token ranks above the
/// other or the rate is not a number.
pub fn pair_anchor(r0: u8, r1: u8, p0in1: f64) -> Option<(usize, f64)> {
    if !(p0in1.is_finite() && p0in1 > 0.0) || r0 == r1 {
        return None;
    }
    if r1 < r0 {
        Some((0, p0in1))
    } else {
        Some((1, 1.0 / p0in1))
    }
}

/// Ties the USD price of one of a pool's tokens to the other's through the pool's own price
/// (`p0in1`: whole token1 per whole token0), so both are valued from the same moment. The token
/// tied is the one that is not the USD stable (or, failing that, not the wrapped gas token).
pub fn anchor_pool(cid: u64, t0: &str, t1: &str, p0in1: f64) {
    let (t0, t1) = (t0.to_lowercase(), t1.to_lowercase());
    let Some((i, rate)) = pair_anchor(anchor_rank(cid, &t0), anchor_rank(cid, &t1), p0in1) else { return };
    let (tok, base) = if i == 0 { (t0, t1) } else { (t1, t0) };
    // checked against the quotes: a pool far off the market is not trusted to price its token
    if let (Ok(q), Ok(b)) = (quoted(cid, &tok), usd(cid, &base)) {
        if b > 0.0 && ((rate * b) / q - 1.0).abs() > ANCHOR_MAX_GAP {
            crate::warn!("prices", "the pool prices {tok} {:.1}% off its quote: quoted on its own", ((rate * b) / q - 1.0) * 100.0);
            ANCHOR.lock().unwrap().get_or_insert_with(HashMap::new).remove(&(cid, tok));
            return;
        }
    }
    ANCHOR.lock().unwrap().get_or_insert_with(HashMap::new).insert((cid, tok), (base, rate, Instant::now()));
}

/// USD per whole token: the chain's USD stable is 1; a pool token tied to its pair (anchor_pool) is
/// the pair token's price times the pool's rate; anything else is quoted.
pub fn usd(cid: u64, token: &str) -> Result<f64, String> {
    let t = token.to_lowercase();
    let a = ANCHOR.lock().unwrap().get_or_insert_with(HashMap::new).get(&(cid, t.clone())).cloned();
    if let Some((base, rate, at)) = a {
        if at.elapsed() < Duration::from_secs(ANCHOR_S) && base != t {
            if let Ok(b) = usd(cid, &base) {
                return Ok(b * rate);
            }
        }
    }
    quoted(cid, &t)
}

/// USD per whole token from a swap quote (cached 10 minutes; up to an hour while quotes fail).
fn quoted(cid: u64, token: &str) -> Result<f64, String> {
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
    let q = match quote(cid, &t, &usdc, &format!("1{}", "0".repeat(d as usize)), d, ud) {
        Ok(q) => q,
        Err(e) => {
            // the Solidly UI's API is down for a moment (a 502 from its proxy, say): a price read in the
            // last hour serves, so a pool's cycle does not stop over it
            let old = USD.lock().unwrap().as_ref().and_then(|m| m.get(&(cid, t.clone())).copied());
            if let Some(p) = stale_ok(old.map(|(p, at)| (p, at.elapsed()))) {
                crate::warn!("prices", "USD price of {t} not refreshed ({e}); using the one from the last hour");
                return Ok(p);
            }
            return Err(e);
        }
    };
    let best = &q["best"];
    let p = best["amountInUsd"].as_f64().or_else(|| best["amountInUsd"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0);
    let p = if p > 0.0 { p } else { num_out(best) / 10f64.powi(ud as i32) };
    if p <= 0.0 {
        return Err(format!("no USD price for {t}"));
    }
    USD.lock().unwrap().as_mut().unwrap().insert((cid, t), (p, Instant::now()));
    Ok(p)
}

/// A cached USD price still usable when it cannot be refreshed: up to an hour old.
fn stale_ok(cached: Option<(f64, Duration)>) -> Option<f64> {
    cached.filter(|(p, age)| *p > 0.0 && *age < Duration::from_secs(3600)).map(|(p, _)| p)
}

/// best.amountOut as f64 (raw units).
pub fn num_out(best: &Value) -> f64 {
    best["amountOut"].as_str().and_then(|s| s.parse().ok()).or_else(|| best["amountOut"].as_f64()).unwrap_or(0.0)
}

pub fn fnum(v: &Value) -> f64 {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::pair_anchor;

    #[test]
    fn the_pair_token_that_is_not_the_stable_or_weth_is_priced_from_the_other() {
        // cbETH (token0, unranked) / WETH (token1, the wrapped gas token): cbETH = 1.144 WETH
        assert_eq!(pair_anchor(2, 1, 1.144), Some((0, 1.144)));
        // USDC (token0, the stable) / USDT (token1): USDT = 1 / (USDT per USDC) USDC
        let (i, r) = pair_anchor(0, 2, 1.002).unwrap();
        assert_eq!(i, 1);
        assert!((r - 1.0 / 1.002).abs() < 1e-12);
        // neither ranks above the other, or no price: both quoted on their own
        assert_eq!(pair_anchor(2, 2, 1.0), None);
        assert_eq!(pair_anchor(2, 1, 0.0), None);
        assert_eq!(pair_anchor(2, 1, f64::NAN), None);
    }
}
