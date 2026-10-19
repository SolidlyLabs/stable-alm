//! Exposure and limits (plan section 5). The account is a list of holdings (token, USD, the pool it
//! sits in or none); every limit is a share of the account's total. A plan is checked on the state
//! it would leave (`project`), step by step and as a whole.
//!
//! - underlying: USD, ETH, BTC, other (refdata::class_of);
//! - issuer: who stands behind the token (Circle, Tether, Coinbase, Lido, …); the first healthy safe
//!   asset is not counted, so an all-USDC start is not "in breach";
//! - LST: tokens priced against ETH by an exchange rate (cbETH, wstETH, …);
//! - per pool and per chain.

use serde_json::{json, Value};
use std::collections::BTreeMap;

use super::refdata::{class_of, is_lst, Class};

#[derive(Clone, Debug, PartialEq)]
pub struct Holding {
    pub chain: u64,
    pub token: String,
    pub symbol: String,
    pub usd: f64,
    /// the pool whose capital it is (None: Unallocated)
    pub pool: Option<String>,
}

pub fn issuer(sym: &str) -> &'static str {
    match sym.trim().to_uppercase().as_str() {
        "USDC" | "USDBC" | "USDC.E" | "EURC" => "Circle",
        "USDT" | "USDT0" => "Tether",
        "DAI" | "USDS" | "SDAI" => "Sky",
        "CBETH" | "CBBTC" => "Coinbase",
        "WSTETH" | "STETH" => "Lido",
        "RETH" => "Rocket Pool",
        "WEETH" | "EETH" => "Ether.fi",
        "WBTC" => "BitGo",
        "TBTC" => "Threshold",
        "WETH" | "ETH" => "none",
        "USDE" | "SUSDE" => "Ethena",
        "FRAX" | "FRXETH" | "SFRXETH" => "Frax",
        "GHO" => "Aave",
        "LUSD" | "BOLD" => "Liquity",
        _ => "other",
    }
}

/// The account's holdings from a cycle's account value (worker status "account") and the capital
/// status: positions are split into their two tokens; idle tokens are each pool's own or Unallocated.
pub fn holdings(acct: &Value, capital: &Value) -> Vec<Holding> {
    let mut out = vec![];
    let names: BTreeMap<String, String> = crate::settings::load_pools().iter()
        .filter_map(|p| Some((p["name"].as_str()?.to_string(), p["pool"].as_str()?.to_lowercase()))).collect();
    for (name, info) in acct["pools"].as_object().into_iter().flatten() {
        let cid = info["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
        let Some(addr) = names.get(name) else { continue };
        let Ok((t0, t1)) = crate::chain::pool_tokens(cid, addr) else { continue };
        let syms = [info["symbols"][0].as_str().unwrap_or("").to_string(), info["symbols"][1].as_str().unwrap_or("").to_string()];
        let (u0, u1) = (crate::prices::usd(cid, &t0).unwrap_or(0.0), crate::prices::usd(cid, &t1).unwrap_or(0.0));
        for p in info["positions"].as_array().into_iter().flatten() {
            let (a0, a1) = (p["amount0"].as_f64().unwrap_or(0.0), p["amount1"].as_f64().unwrap_or(0.0));
            out.push(Holding { chain: cid, token: t0.clone(), symbol: syms[0].clone(), usd: a0 * u0, pool: Some(addr.clone()) });
            out.push(Holding { chain: cid, token: t1.clone(), symbol: syms[1].clone(), usd: a1 * u1, pool: Some(addr.clone()) });
        }
    }
    for (cid, ch) in capital.as_object().into_iter().flatten() {
        let cid: u64 = cid.parse().unwrap_or(crate::chains::BASE);
        for p in ch["pools"].as_object().into_iter().flat_map(|m| m.values()) {
            let addr = p["pool"].as_str().unwrap_or("").to_lowercase();
            for t in p["idle"].as_array().into_iter().flatten() {
                out.push(Holding { chain: cid, token: t["address"].as_str().unwrap_or("").to_lowercase(), symbol: t["symbol"].as_str().unwrap_or("").into(),
                                   usd: t["usd"].as_f64().unwrap_or(0.0), pool: Some(addr.clone()) });
            }
        }
        for t in ch["unallocated"].as_array().into_iter().flatten() {
            out.push(Holding { chain: cid, token: t["address"].as_str().unwrap_or("").to_lowercase(), symbol: t["symbol"].as_str().unwrap_or("").into(),
                               usd: t["usd"].as_f64().unwrap_or(0.0), pool: None });
        }
    }
    out.retain(|h| h.usd.is_finite() && h.usd > 0.0);
    out
}

/// Shares of the total (fractions), by group.
pub fn summary(h: &[Holding], safe_excluded: &[String]) -> Value {
    let total: f64 = h.iter().map(|x| x.usd).sum();
    let mut by: BTreeMap<&str, BTreeMap<String, f64>> = BTreeMap::new();
    for x in h {
        let c = match class_of(&x.symbol) {
            Class::Stable => "USD",
            Class::Eth => "ETH",
            Class::Btc => "BTC",
            Class::Other => "other",
        };
        *by.entry("underlying").or_default().entry(c.into()).or_default() += x.usd;
        if !safe_excluded.iter().any(|s| s.eq_ignore_ascii_case(&x.token)) {
            *by.entry("issuer").or_default().entry(issuer(&x.symbol).into()).or_default() += x.usd;
        }
        if is_lst(&x.symbol) {
            *by.entry("lst").or_default().entry("LST".into()).or_default() += x.usd;
        }
        *by.entry("pool").or_default().entry(x.pool.clone().unwrap_or_else(|| "unallocated".into())).or_default() += x.usd;
        *by.entry("chain").or_default().entry(x.chain.to_string()).or_default() += x.usd;
        if class_of(&x.symbol) != Class::Stable {
            *by.entry("non_usd_token").or_default().entry(format!("{}:{}", x.chain, x.token)).or_default() += x.usd;
        }
    }
    let mut out = serde_json::Map::new();
    out.insert("total_usd".into(), json!(total));
    for (g, m) in by {
        let mut o = serde_json::Map::new();
        for (k, v) in m {
            o.insert(k, json!({"usd": v, "share": if total > 0.0 { v / total } else { 0.0 }}));
        }
        out.insert(g.into(), Value::Object(o));
    }
    Value::Object(out)
}

/// Every limit the summary breaks: [{limit, group, key, share, max}].
pub fn breaches(sum: &Value, limits: &Value) -> Vec<Value> {
    let l = |k: &str, d: f64| limits[k].as_f64().unwrap_or(d) / 100.0;
    let mut out = vec![];
    let mut check = |limit: &str, group: &str, key: Option<&str>, max: f64| {
        for (k, v) in sum[group].as_object().into_iter().flatten() {
            if key.map(|x| x == k).unwrap_or(true) && k != "unallocated" && k != "none" {
                let s = v["share"].as_f64().unwrap_or(0.0);
                if s > max + 1e-9 {
                    out.push(json!({"limit": limit, "group": group, "key": k, "share": s, "max": max}));
                }
            }
        }
    };
    check("eth_pct", "underlying", Some("ETH"), l("eth_pct", 50.0));
    check("btc_pct", "underlying", Some("BTC"), l("btc_pct", 25.0));
    check("other_pct", "underlying", Some("other"), l("other_pct", 10.0));
    check("issuer_pct", "issuer", None, l("issuer_pct", 75.0));
    check("lst_pct", "lst", None, l("lst_pct", 40.0));
    check("pool_pct", "pool", None, l("pool_pct", 75.0));
    check("chain_pct", "chain", None, l("chain_pct", 100.0));
    out
}

/// A capital move of a plan: USD from a pool (or Unallocated) to a pool (or Unallocated).
#[derive(Clone, Debug)]
pub struct Move {
    pub chain: u64,
    pub from: Option<String>,
    pub to: Option<String>,
    pub usd: f64,
}

/// The holdings after the moves: what leaves a pool leaves each of its tokens in proportion and
/// arrives as the safe asset (a release sells to USDC); what goes into a pool is split over its two
/// tokens like what it holds now, or half and half when it holds nothing (`tokens_of` gives a
/// pool's two tokens and symbols).
pub fn project(h: &[Holding], moves: &[Move], safe: &(String, String), tokens_of: &dyn Fn(u64, &str) -> Option<[(String, String); 2]>) -> Vec<Holding> {
    let mut out = h.to_vec();
    for m in moves {
        let mut take = m.usd;
        // what leaves
        let src: Vec<usize> = out.iter().enumerate().filter(|(_, x)| x.chain == m.chain && x.pool == m.from).map(|(i, _)| i).collect();
        let have: f64 = src.iter().map(|i| out[*i].usd).sum();
        if have > 0.0 {
            take = take.min(have);
            for i in &src {
                out[*i].usd -= take * out[*i].usd / have;
            }
        } else {
            continue;
        }
        // what arrives
        match &m.to {
            None => out.push(Holding { chain: m.chain, token: safe.0.clone(), symbol: safe.1.clone(), usd: take, pool: None }),
            Some(p) => {
                let cur: Vec<Holding> = out.iter().filter(|x| x.chain == m.chain && x.pool.as_deref() == Some(p.as_str()) && x.usd > 0.0).cloned().collect();
                let tot: f64 = cur.iter().map(|x| x.usd).sum();
                if tot > 0.0 {
                    for c in cur {
                        out.push(Holding { usd: take * c.usd / tot, ..c });
                    }
                } else if let Some([a, b]) = tokens_of(m.chain, p) {
                    out.push(Holding { chain: m.chain, token: a.0, symbol: a.1, usd: take / 2.0, pool: Some(p.clone()) });
                    out.push(Holding { chain: m.chain, token: b.0, symbol: b.1, usd: take / 2.0, pool: Some(p.clone()) });
                }
            }
        }
    }
    out.retain(|x| x.usd > 1e-9);
    out
}

/// The safe assets of a chain in order (its USDC, then USDT and the other funding stables).
pub fn safe_assets(cid: u64) -> Vec<String> {
    let mut v = vec![crate::chains::get(cid).usd.to_lowercase()];
    for t in crate::chains::default_funding(cid) {
        let t = t.to_lowercase();
        if !v.contains(&t) {
            v.push(t);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(tok: &str, sym: &str, usd: f64, pool: Option<&str>) -> Holding {
        Holding { chain: 8453, token: tok.into(), symbol: sym.into(), usd, pool: pool.map(String::from) }
    }

    #[test]
    fn an_all_usdc_start_is_not_an_issuer_breach() {
        let hs = vec![h("usdc", "USDC", 1000.0, None)];
        let s = summary(&hs, &["usdc".to_string()]);
        let b = breaches(&s, &super::super::defaults()["limits"]);
        assert!(b.iter().all(|x| x["limit"] != "issuer_pct"), "{b:?}");
        // counted, it would be
        let s = summary(&hs, &[]);
        assert!(breaches(&s, &super::super::defaults()["limits"]).iter().any(|x| x["limit"] == "issuer_pct"));
    }

    #[test]
    fn eth_and_lst_shares() {
        let hs = vec![h("usdc", "USDC", 400.0, None), h("cbeth", "cbETH", 300.0, Some("p")), h("weth", "WETH", 300.0, Some("p"))];
        let s = summary(&hs, &["usdc".into()]);
        assert!((s["underlying"]["ETH"]["share"].as_f64().unwrap() - 0.6).abs() < 1e-9);
        assert!((s["lst"]["LST"]["share"].as_f64().unwrap() - 0.3).abs() < 1e-9);
        let b = breaches(&s, &super::super::defaults()["limits"]);
        assert!(b.iter().any(|x| x["limit"] == "eth_pct"));
        assert!(!b.iter().any(|x| x["limit"] == "lst_pct"));
    }

    #[test]
    fn projecting_a_move_out_of_an_eth_pool() {
        let hs = vec![h("usdc", "USDC", 400.0, None), h("cbeth", "cbETH", 300.0, Some("p")), h("weth", "WETH", 300.0, Some("p"))];
        let safe = ("usdc".to_string(), "USDC".to_string());
        let after = project(&hs, &[Move { chain: 8453, from: Some("p".into()), to: None, usd: 200.0 }], &safe, &|_, _| None);
        let s = summary(&after, &["usdc".into()]);
        assert!((s["underlying"]["ETH"]["share"].as_f64().unwrap() - 0.4).abs() < 1e-9, "{s}");
        // into an empty pool: half and half
        let after = project(&hs, &[Move { chain: 8453, from: None, to: Some("q".into()), usd: 100.0 }], &safe,
                            &|_, _| Some([("a".into(), "USDT".into()), ("b".into(), "DAI".into())]));
        assert_eq!(after.iter().filter(|x| x.pool.as_deref() == Some("q")).count(), 2);
    }
}
