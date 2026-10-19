//! Reference data (plan section 6, tier 1 and 2), each source read through the local cache
//! (cache.rs) so a repeat read within its age limit makes no request:
//! - **exchange mid prices** per token: Coinbase and Kraken, USD markets, mid of bid and ask (60 s);
//! - **token health**: a stablecoin's distance from $1; an LST's market ratio to ETH (its exchange
//!   rate source on Base is still to confirm, so the discount check reports "unknown");
//! - **price disagreement** between a pool and the exchanges, against its token class's threshold;
//! - **gas**: the wallet's native balance against the chain's minimum, and what a rebalance costs
//!   (the app's own last transactions × 3);
//! - **funding and open interest** for ETH and BTC (Hyperliquid), snapshotted hourly by the app
//!   itself (data/agent/funding.jsonl) so 24 h changes and a 30-day spread can be computed;
//! - **swap-cost scenarios**: an aggregator quote for an amount ("now") and for twice it ("thin").

use serde_json::{json, Value};

use crate::cache;

/// How a token behaves, for thresholds and exposure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Stable,
    Eth,
    Btc,
    Other,
}

pub fn class_of(sym: &str) -> Class {
    let s = sym.trim().to_uppercase();
    if crate::market_data::is_pegged(&s, "USDC", None) {
        Class::Stable
    } else if crate::market_data::is_pegged(&s, "WETH", None) || s == "ETH" || s.ends_with("ETH") {
        Class::Eth
    } else if crate::market_data::is_pegged(&s, "WBTC", None) || s.ends_with("BTC") {
        Class::Btc
    } else {
        Class::Other
    }
}

/// An LST or LRT: priced against ETH by an exchange rate, not 1:1.
pub fn is_lst(sym: &str) -> bool {
    let s = sym.trim().to_uppercase();
    class_of(&s) == Class::Eth && !matches!(s.as_str(), "ETH" | "WETH")
}

/// The exchange asset a token trades as.
fn cex_asset(sym: &str) -> String {
    let s = sym.trim().to_uppercase();
    match s.as_str() {
        "WETH" | "ETH" => "ETH".into(),
        "USDBC" | "USDC.E" | "USDC" => "USDC".into(),
        "USDT0" | "USDT" => "USDT".into(),
        "CBBTC" | "WBTC" | "TBTC" | "BTC" => "BTC".into(),
        _ => s,
    }
}

fn coinbase(asset: &str) -> Result<Value, String> {
    if asset == "USDC" {
        return Err("Coinbase has no USDC market (it treats USDC as USD)".into());
    }
    let v = crate::rpc::get_json(&format!("https://api.exchange.coinbase.com/products/{asset}-USD/ticker"))?;
    let (b, a) = (super::num(&v["bid"]), super::num(&v["ask"]));
    match (b, a) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => Ok(json!({"mid": (a + b) / 2.0, "spread_bp": (a - b) / ((a + b) / 2.0) * 1e4})),
        _ => Err(format!("Coinbase {asset}-USD: no bid and ask")),
    }
}

fn kraken(asset: &str) -> Result<Value, String> {
    let k = match asset {
        "BTC" => "XBT",
        "CBETH" => return Err("Kraken has no cbETH market".into()),
        a => a,
    };
    let v = crate::rpc::get_json(&format!("https://api.kraken.com/0/public/Ticker?pair={k}USD"))?;
    if let Some(e) = v["error"].as_array().filter(|e| !e.is_empty()) {
        return Err(format!("Kraken {k}USD: {}", e.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("; ")));
    }
    let r = v["result"].as_object().and_then(|m| m.values().next()).ok_or(format!("Kraken {k}USD: no result"))?;
    let (a, b) = (super::num(&r["a"][0]), super::num(&r["b"][0]));
    match (b, a) {
        (Some(b), Some(a)) if b > 0.0 && a > 0.0 => Ok(json!({"mid": (a + b) / 2.0, "spread_bp": (a - b) / ((a + b) / 2.0) * 1e4})),
        _ => Err(format!("Kraken {k}USD: no bid and ask")),
    }
}

/// A token's USD mid from each exchange (cached 60 s), and their median.
pub fn mids(sym: &str) -> Value {
    let asset = cex_asset(sym);
    let mut rows = vec![];
    let mut ok = vec![];
    // a source the risk controller marked suspect is shown but left out of the median for its hour
    let sus = super::risk::suspects();
    for (src, f) in [("coinbase", coinbase as fn(&str) -> Result<Value, String>), ("kraken", kraken)] {
        match cache::global().get(&format!("cex-{src}"), &asset, 60, || f(&asset)) {
            Ok(h) => {
                let age = super::now() - h.retrieved_at as i64;
                let suspect = sus.get(format!("{src}:{asset}")).is_some();
                if let Some(m) = h.value["mid"].as_f64() {
                    if !h.stale && !suspect {
                        ok.push(m);
                    }
                }
                rows.push(json!({"source": src, "mid": h.value["mid"], "spread_bp": h.value["spread_bp"], "age_s": age, "stale": h.stale, "error": h.error, "suspect": suspect}));
            }
            Err(e) => rows.push(json!({"source": src, "error": e})),
        }
    }
    ok.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = if ok.is_empty() { None } else if ok.len() % 2 == 1 { Some(ok[ok.len() / 2]) } else { Some((ok[ok.len() / 2 - 1] + ok[ok.len() / 2]) / 2.0) };
    json!({"asset": asset, "sources": rows, "fresh": ok.len(), "median": median})
}

/// A token's health from its exchange prices.
pub fn health(sym: &str, x: &Value) -> Value {
    let m = mids(sym);
    let med = m["median"].as_f64();
    let peg = x["risk"]["peg_pct"].as_f64().unwrap_or(1.0);
    match class_of(sym) {
        Class::Stable => match med {
            Some(p) => {
                let dev = (p - 1.0).abs() * 100.0;
                json!({"symbol": sym, "class": "stable", "healthy": dev <= peg, "peg_dev_pct": dev, "usd": p, "fresh_sources": m["fresh"], "mids": m})
            }
            None => json!({"symbol": sym, "class": "stable", "healthy": Value::Null, "reason": "no fresh exchange price", "mids": m}),
        },
        Class::Eth if is_lst(sym) => {
            let eth = mids("ETH")["median"].as_f64();
            let ratio = match (med, eth) {
                (Some(a), Some(b)) if b > 0.0 => Some(a / b),
                _ => None,
            };
            json!({"symbol": sym, "class": "lst", "healthy": Value::Null, "market_ratio_eth": ratio,
                   "reason": "exchange-rate source on this chain still to confirm: the discount against the rate is not checked", "mids": m})
        }
        c => json!({"symbol": sym, "class": format!("{c:?}").to_lowercase(), "healthy": true, "usd": med, "mids": m}),
    }
}

/// The disagreement threshold (bp) between a pool's tokens, by class.
pub fn disagree_bp(x: &Value, s0: &str, s1: &str) -> f64 {
    let r = &x["risk"];
    match (class_of(s0), class_of(s1)) {
        (Class::Stable, Class::Stable) => r["disagree_bp_stable"].as_f64().unwrap_or(30.0),
        (Class::Eth, Class::Eth) => r["disagree_bp_eth"].as_f64().unwrap_or(100.0),
        _ => r["disagree_bp_other"].as_f64().unwrap_or(200.0),
    }
}

/// The pool's price (token1 per token0) against the exchanges' ratio of the two tokens' USD mids.
pub fn pool_vs_cex(pool_price: f64, s0: &str, s1: &str) -> Value {
    let (m0, m1) = (mids(s0), mids(s1));
    match (m0["median"].as_f64(), m1["median"].as_f64()) {
        (Some(u0), Some(u1)) if u1 > 0.0 && pool_price > 0.0 => {
            let cex = u0 / u1;
            json!({"pool": pool_price, "cex": cex, "diff_bp": (pool_price / cex - 1.0) * 1e4, "fresh": m0["fresh"].as_u64().unwrap_or(0).min(m1["fresh"].as_u64().unwrap_or(0))})
        }
        _ => json!({"pool": pool_price, "cex": Value::Null, "diff_bp": Value::Null, "fresh": 0}),
    }
}

/// Gas on a chain: the wallet's native balance against the minimum, and a rebalance's cost.
pub fn gas(cid: u64, wallet: &str) -> Value {
    let c = crate::chains::get(cid);
    let bal = crate::chain::eth_balance(cid, wallet).ok().map(|b| crate::tickmath::to_f64(&b));
    let ledger: Value = std::fs::read_to_string(crate::paths::logs().join("ledger.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    let mut txs: Vec<(i64, f64)> = ledger.as_object().into_iter().flat_map(|m| m.values())
        .filter(|d| d["chain_id"].as_u64() == Some(cid))
        .filter_map(|d| Some((d["ts"].as_i64().unwrap_or(0), d["gas_wei"].as_f64()?)))
        .collect();
    txs.sort_by_key(|t| -t.0);
    txs.truncate(30);
    let avg_wei = if txs.is_empty() { None } else { Some(txs.iter().map(|t| t.1).sum::<f64>() / txs.len() as f64) };
    let native_usd = if c.wnative.is_empty() { None } else { crate::prices::usd(cid, &c.wnative).ok() };
    let per_tx = avg_wei.zip(native_usd).map(|(w, p)| w / 1e18 * p);
    json!({"chain_id": cid, "native_balance": bal.map(|b| b / 1e18), "min_native": c.min_gas_wei / 1e18, "symbol": c.gas_symbol,
           "low": bal.map(|b| b < c.min_gas_wei), "tx_usd": per_tx, "rebalance_usd": per_tx.map(|x| x * 3.0), "sample_txs": txs.len()})
}

// -- funding and open interest ---------------------------------------------------------------------

fn hyperliquid() -> Result<Value, String> {
    let v = crate::rpc::post_json("https://api.hyperliquid.xyz/info", &json!({"type": "metaAndAssetCtxs"}), &[])?;
    let names = v[0]["universe"].as_array().ok_or("Hyperliquid: no universe")?;
    let ctx = v[1].as_array().ok_or("Hyperliquid: no asset contexts")?;
    let mut out = serde_json::Map::new();
    for (n, c) in names.iter().zip(ctx) {
        let name = n["name"].as_str().unwrap_or("");
        if name == "ETH" || name == "BTC" {
            let mark = super::num(&c["markPx"]).unwrap_or(0.0);
            out.insert(name.into(), json!({"funding": super::num(&c["funding"]), "oi_usd": super::num(&c["openInterest"]).map(|o| o * mark), "mark": mark}));
        }
    }
    if out.is_empty() {
        return Err("Hyperliquid: ETH and BTC not found".into());
    }
    Ok(Value::Object(out))
}

/// Take this hour's snapshot when it is missing (one request an hour at most).
pub fn snapshot_funding() -> Result<(), String> {
    let now = super::now();
    let last = super::tail("funding.jsonl", 1).last().and_then(|r| r["ts"].as_i64()).unwrap_or(0);
    if now / 3600 == last / 3600 {
        return Ok(());
    }
    let h = cache::global().get("hyperliquid", "metaAndAssetCtxs", 1800, hyperliquid)?;
    if h.stale {
        return Err(h.error.unwrap_or_else(|| "stale".into()));
    }
    let mut row = h.value.clone();
    row["ts"] = json!(now);
    super::append("funding.jsonl", &row);
    Ok(())
}

/// Funding and open interest now, their 24 h changes, and the 30-day spread of hourly funding
/// changes (what the trigger "a funding jump" is measured against).
pub fn funding() -> Value {
    let rows = super::tail("funding.jsonl", 24 * 31);
    let now = super::now();
    let mut out = serde_json::Map::new();
    for a in ["ETH", "BTC"] {
        let series: Vec<(i64, f64, f64)> = rows.iter()
            .filter_map(|r| Some((r["ts"].as_i64()?, r[a]["funding"].as_f64()?, r[a]["oi_usd"].as_f64().unwrap_or(0.0)))).collect();
        let Some(last) = series.last().copied() else {
            out.insert(a.into(), json!({"reason": "no snapshot yet"}));
            continue;
        };
        let day_ago = series.iter().rev().find(|r| r.0 <= last.0 - 86_400 + 1800).copied();
        let diffs: Vec<f64> = series.windows(2).map(|w| w[1].1 - w[0].1).collect();
        let sd = if diffs.len() > 24 {
            let m = diffs.iter().sum::<f64>() / diffs.len() as f64;
            Some((diffs.iter().map(|d| (d - m).powi(2)).sum::<f64>() / (diffs.len() - 1) as f64).sqrt())
        } else {
            None
        };
        let d_funding = day_ago.map(|d| last.1 - d.1);
        let oi_change = day_ago.filter(|d| d.2 > 0.0).map(|d| (last.2 / d.2 - 1.0) * 100.0);
        out.insert(a.into(), json!({
            "funding_hourly": last.1, "funding_annual_pct": last.1 * 24.0 * 365.0 * 100.0, "oi_usd": last.2, "age_s": now - last.0,
            "funding_change_24h": d_funding, "funding_change_sd_30d": sd, "oi_change_24h_pct": oi_change,
            "jump": match (d_funding, sd) { (Some(d), Some(s)) if s > 0.0 => d.abs() > 3.0 * s, _ => false } || oi_change.map(|o| o.abs() >= 10.0).unwrap_or(false),
            "hours": series.len(),
        }));
    }
    Value::Object(out)
}

// -- swap costs --------------------------------------------------------------------------------------

/// Cost in bp of swapping `usd` worth of `tin` into `tout` through the aggregator, valued at the two
/// tokens' USD prices (the aggregator's own figures when it gives them).
pub fn swap_cost(cid: u64, tin: &str, tout: &str, usd: f64) -> Result<Value, String> {
    let (mi, mo) = (crate::prices::meta(cid, tin)?, crate::prices::meta(cid, tout)?);
    let (ui, uo) = (crate::prices::usd(cid, tin)?, crate::prices::usd(cid, tout)?);
    if ui <= 0.0 || uo <= 0.0 || usd <= 0.0 {
        return Err("no USD price".into());
    }
    let amount = (usd / ui * 10f64.powi(mi.decimals as i32)).floor();
    let key = format!("{cid}:{tin}:{tout}:{amount:.0}");
    let h = cache::global().get("aggregator-quote", &key, 60, || {
        crate::prices::quote(cid, tin, tout, &format!("{amount:.0}"), mi.decimals, mo.decimals)
    })?;
    let out = crate::prices::num_out(&h.value["best"]) / 10f64.powi(mo.decimals as i32);
    let in_usd = super::num(&h.value["best"]["amountInUsd"]).unwrap_or(usd);
    let out_usd = super::num(&h.value["best"]["amountOutUsd"]).unwrap_or(out * uo);
    if out <= 0.0 || in_usd <= 0.0 {
        return Err("no route".into());
    }
    Ok(json!({"usd": usd, "cost_bp": (in_usd - out_usd) / in_usd * 1e4, "out": out, "age_s": super::now() - h.retrieved_at as i64, "stale": h.stale}))
}

/// The plan's swap-cost scenarios for an amount: now, and thin (twice the amount, its bp applied).
pub fn scenarios(cid: u64, tin: &str, tout: &str, usd: f64) -> Value {
    let now = swap_cost(cid, tin, tout, usd);
    let thin = swap_cost(cid, tin, tout, 2.0 * usd);
    json!({
        "now": now.as_ref().map(|v| v.clone()).unwrap_or_else(|e| json!({"error": e})),
        "thin": thin.as_ref().map(|v| v.clone()).unwrap_or_else(|e| json!({"error": e})),
        "cost_bp": now.as_ref().ok().and_then(|v| v["cost_bp"].as_f64()),
        "thin_bp": thin.as_ref().ok().and_then(|v| v["cost_bp"].as_f64()),
    })
}

/// The hourly funding snapshot while experimental features are on.
pub fn start() {
    std::thread::Builder::new().name("refdata".into()).spawn(|| loop {
        if super::enabled() {
            if let Err(e) = snapshot_funding() {
                crate::debug!("refdata", "funding snapshot: {e}");
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(300));
    }).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert_eq!(class_of("USDC"), Class::Stable);
        assert_eq!(class_of("usdt"), Class::Stable);
        assert_eq!(class_of("WETH"), Class::Eth);
        assert_eq!(class_of("cbETH"), Class::Eth);
        assert!(is_lst("cbETH") && !is_lst("WETH"));
        assert_eq!(class_of("cbBTC"), Class::Btc);
        assert_eq!(class_of("AERO"), Class::Other);
        let x = super::super::defaults();
        assert_eq!(disagree_bp(&x, "USDC", "USDT"), 30.0);
        assert_eq!(disagree_bp(&x, "cbETH", "WETH"), 100.0);
        assert_eq!(disagree_bp(&x, "AERO", "USDC"), 200.0);
    }
}
