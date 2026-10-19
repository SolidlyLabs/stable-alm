//! The Liquidity Lens client (AGENT_PLAN section 6a), for now with **placeholder data only**.
//!
//! Every call has the shape the real one will have: the endpoint it maps to, the request it needs
//! (docs/LLENS_REQUESTS.md), and the cache in front of it (cache.rs, source "llens"). Until a
//! feature is integrated for real, the answer is generated: deterministic per token and hour, so a
//! page reads the same on every refresh, and plausible in size (stables deep, ETH tokens less so,
//! the rest thin). It is labelled `"placeholder": true` everywhere it goes.
//!
//! Placeholder data must never reach a decision: `decision_grade` is false for it, and nothing
//! but the Experimental tab reads this module yet. Everything here sits behind the experimental
//! features switch (`experimental.enabled`, off by default).
//!
//! Like the real service, prices are in the chain's canonical stable (USDC on Base), which is never
//! priced itself: asking for it answers `no-route`.

use serde_json::{json, Value};

use crate::cache::{self, Hit};

const SOURCE: &str = "llens";
const HOUR: i64 = 3600;

/// What the client does, from the settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Off,
    Placeholder,
}

pub fn mode() -> Mode {
    mode_of(&crate::settings::load_settings())
}

pub fn mode_of(s: &Value) -> Mode {
    let x = &s["experimental"];
    if !x["enabled"].as_bool().unwrap_or(false) {
        return Mode::Off;
    }
    match x["llens"].as_str().unwrap_or("placeholder") {
        "placeholder" => Mode::Placeholder,
        _ => Mode::Off,
    }
}

/// A token as the caller knows it: the price hint is the app's own USD price, which placeholder
/// data stays close to (so the numbers read sensibly); the real service ignores it.
#[derive(Clone, Debug)]
pub struct Token {
    pub chain: u64,
    pub address: String,
    pub symbol: String,
    pub usd_hint: Option<f64>,
}

/// How deep a token's markets are, for placeholder sizes.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Class {
    Stable,
    Eth,
    Other,
}

fn class(sym: &str) -> Class {
    let s = sym.to_uppercase();
    if crate::market_data::is_pegged(&s, "USDC", None) {
        Class::Stable
    } else if s.contains("ETH") {
        Class::Eth
    } else {
        Class::Other
    }
}

/// A number in [-1, 1] that depends only on the seed.
fn noise(seed: &str) -> f64 {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(seed.as_bytes());
    let x = u32::from_le_bytes([h[0], h[1], h[2], h[3]]) as f64 / u32::MAX as f64;
    2.0 * x - 1.0
}

fn hour_of(ts: i64) -> i64 {
    ts.div_euclid(HOUR) * HOUR
}

/// Only real, fresh Liquidity Lens data may inform a decision. Placeholder data never does.
/// (For the snapshot builder and the risk controller, which do not exist yet.)
#[allow(dead_code)]
pub fn decision_grade(v: &Value) -> bool {
    v["placeholder"] == json!(false) && v["stale"] != json!(true)
}

fn not_on() -> String {
    "Liquidity Lens is off: turn on experimental features in Settings".into()
}

/// The answer's envelope: where it would come from and what the cache did.
fn envelope(endpoint: &str, request: &str, live_in_llens: bool, hit: &Hit) -> Value {
    let mut v = json!({
        "source": SOURCE,
        "placeholder": true,
        "endpoint": endpoint,
        "request": request,
        "live_in_llens": live_in_llens,
        "data": hit.value,
    });
    for (k, x) in hit.meta().as_object().cloned().unwrap_or_default() {
        v[k] = x;
    }
    v
}

fn cached(key: &str, max_age_s: u64, endpoint: &str, request: &str, live: bool, make: impl FnOnce() -> Value) -> Result<Value, String> {
    if mode() == Mode::Off {
        return Err(not_on());
    }
    let hit = cache::global().get(SOURCE, key, max_age_s, || Ok(make()))?;
    Ok(envelope(endpoint, request, live, &hit))
}

fn is_quote_token(t: &Token) -> bool {
    t.address.to_lowercase() == crate::chains::get(t.chain).usd.to_lowercase()
}

/// Depth within 1% of the price, in USD, for a token at an hour (placeholder).
fn depth_1pct(t: &Token, hour: i64) -> f64 {
    let base = match class(&t.symbol) {
        Class::Stable => 2_000_000.0,
        Class::Eth => 800_000.0,
        Class::Other => 40_000.0,
    };
    base * (1.0 + 0.35 * noise(&format!("depth:{}:{}:{hour}", t.chain, t.address)))
}

/// Cost in bp of trading `size_usd` (placeholder): a spread plus impact growing with size/depth.
fn cost_bp(t: &Token, size_usd: f64, hour: i64) -> f64 {
    let spread = match class(&t.symbol) {
        Class::Stable => 0.5,
        Class::Eth => 2.0,
        Class::Other => 15.0,
    };
    spread + 100.0 * size_usd / depth_1pct(t, hour)
}

fn spot(t: &Token, hour: i64) -> f64 {
    let base = t.usd_hint.unwrap_or(if class(&t.symbol) == Class::Stable { 1.0 } else { 0.0 });
    let wobble = match class(&t.symbol) {
        Class::Stable => 0.0004,
        Class::Eth => 0.004,
        Class::Other => 0.01,
    };
    base * (1.0 + wobble * noise(&format!("spot:{}:{}:{hour}", t.chain, t.address)))
}

fn tkey(t: &Token) -> String {
    format!("{}:{}", t.chain, t.address.to_lowercase())
}

// -- live in Liquidity Lens today ------------------------------------------------------------------

/// `/ext/oracle/reference`: time-weighted median price with a status (ok | deviation | stale |
/// no-route) and reasons. Placeholder.
pub fn reference(t: &Token) -> Result<Value, String> {
    let now = crate::engine::now() as i64;
    let h = hour_of(now);
    let quote = is_quote_token(t);
    let t2 = t.clone();
    cached(&format!("reference:{}", tkey(t)), 300, "/ext/oracle/reference", "live", true, move || {
        if quote {
            return json!({"status": "no-route", "reasons": ["the chain's quote token is never priced"], "price": null});
        }
        if t2.usd_hint.is_none() && class(&t2.symbol) != Class::Stable {
            return json!({"status": "no-route", "reasons": ["no price known for this token"], "price": null});
        }
        json!({"status": "ok", "reasons": [], "price": spot(&t2, h), "quote": "USDC", "window_s": 900, "state_age_ms": 4000})
    })
}

/// `/ext/oracle/batch`: quotes for each size in one direction, with the cost in bp. Placeholder.
pub fn quotes(t: &Token, side: &str, sizes_usd: &[f64]) -> Result<Value, String> {
    if !matches!(side, "buy" | "sell") {
        return Err("side is buy or sell".into());
    }
    let now = crate::engine::now() as i64;
    let h = hour_of(now);
    let (t2, side2, sizes) = (t.clone(), side.to_string(), sizes_usd.to_vec());
    let sz: Vec<String> = sizes.iter().map(|s| format!("{s:.0}")).collect();
    cached(&format!("quotes:{}:{side}:{}", tkey(t), sz.join(",")), 60, "/ext/oracle/batch", "R3", true, move || {
        let rows: Vec<Value> = sizes
            .iter()
            .map(|s| {
                let c = cost_bp(&t2, *s, h) * if side2 == "buy" { 1.0 } else { 1.08 };
                json!({"size_usd": s, "cost_bp": c, "route": ["placeholder pool A", "placeholder pool B"], "block": null})
            })
            .collect();
        json!({"side": side2, "quotes": rows})
    })
}

/// `/ext/oracle/depth`: the largest size within 1% and 2% of the price, per side. Placeholder.
pub fn depth(t: &Token) -> Result<Value, String> {
    let now = crate::engine::now() as i64;
    let h = hour_of(now);
    let t2 = t.clone();
    cached(&format!("depth:{}", tkey(t)), 600, "/ext/oracle/depth", "live", true, move || {
        let d1 = depth_1pct(&t2, h);
        let ladder: Vec<Value> = [1e3, 1e4, 1e5, 1e6, 1e7].iter().map(|s| json!({"size_usd": s, "cost_bp": cost_bp(&t2, *s, h)})).collect();
        json!({"buy": {"within_1pct_usd": d1, "within_2pct_usd": d1 * 1.9}, "sell": {"within_1pct_usd": d1 * 0.93, "within_2pct_usd": d1 * 1.8},
               "ladder": ladder})
    })
}

/// `/ext/oracle/history`: hourly spot vs executable price for a size and side, with depth.
/// Placeholder, through the series cache (closed hours kept, then only topped up).
pub fn depth_history(t: &Token, side: &str, size_usd: f64, from: i64, to: i64) -> Result<Value, String> {
    if mode() == Mode::Off {
        return Err(not_on());
    }
    let t2 = t.clone();
    let sign = if side == "sell" { -1.0 } else { 1.0 };
    let hit = cache::global().series(SOURCE, &format!("history:{}:{side}:{size_usd:.0}", tkey(t)), HOUR, from, to, 600, |a, b| {
        Ok((a..b).step_by(HOUR as usize).map(|ts| {
            let s = spot(&t2, ts);
            let c = cost_bp(&t2, size_usd, ts);
            json!({"ts": ts, "spot": s, "exec": s * (1.0 + sign * c / 1e4), "gap_bp": c, "within_1pct_usd": depth_1pct(&t2, ts)})
        }).collect())
    })?;
    Ok(json!({"source": SOURCE, "placeholder": true, "endpoint": "/ext/oracle/history", "request": "live", "live_in_llens": true,
              "data": hit.rows, "fetched": hit.fetched, "stale": hit.stale, "error": hit.error, "filling": false}))
}

// -- requested (docs/LLENS_REQUESTS.md) ------------------------------------------------------------

/// R5: a pool's hourly aggregator / bot / organic share of volume. Placeholder.
pub fn traffic(chain: u64, pool: &str, from: i64, to: i64) -> Result<Value, String> {
    if mode() == Mode::Off {
        return Err(not_on());
    }
    let p = pool.to_lowercase();
    let hit = cache::global().series(SOURCE, &format!("traffic:{chain}:{p}"), HOUR, from, to, 3600, |a, b| {
        Ok((a..b).step_by(HOUR as usize).map(|ts| {
            let n = |k: &str| 0.5 + 0.5 * noise(&format!("traffic:{k}:{chain}:{p}:{ts}"));
            let (agg, bot) = (0.35 + 0.25 * n("a"), 0.15 + 0.3 * n("b"));
            let bot = bot.min(1.0 - agg);
            json!({"ts": ts, "volume_usd": 20_000.0 * (0.2 + n("v")), "aggregator_share": agg, "bot_share": bot, "organic_share": 1.0 - agg - bot})
        }).collect())
    })?;
    Ok(json!({"source": SOURCE, "placeholder": true, "endpoint": "/modules/volume-attribution/series", "request": "R5", "live_in_llens": false,
              "data": hit.rows, "fetched": hit.fetched, "stale": hit.stale, "error": hit.error}))
}

/// R6: every pool of the pair across DEXes on the chain, with liquidity near the price. Placeholder.
pub fn competing(chain: u64, token0: &str, token1: &str) -> Result<Value, String> {
    let (a, b) = (token0.to_lowercase(), token1.to_lowercase());
    let day = crate::engine::now() as i64 / 86_400;
    cached(&format!("competing:{chain}:{a}:{b}"), 3600, "pair-competition", "R6", false, move || {
        let pools: Vec<Value> = ["Placeholder DEX A", "Placeholder DEX B", "Placeholder DEX C"]
            .iter()
            .enumerate()
            .map(|(i, dex)| {
                let n = |k: &str| 0.5 + 0.5 * noise(&format!("comp:{k}:{chain}:{a}:{b}:{i}:{day}"));
                let tvl = 50_000.0 + 2_000_000.0 * n("tvl");
                let (fee_bp, fee_model) = [(1.0, "static"), (5.0, "dynamic"), (30.0, "static")][i];
                json!({"dex": dex, "pool": format!("0x{:040x}", i + 1), "fee_bp": fee_bp, "fee_model": fee_model,
                       "tvl_usd": tvl, "in_range_usd": {"0.1pct": tvl * 0.2 * n("r1"), "0.5pct": tvl * 0.5 * n("r2"), "1pct": tvl * 0.7 * n("r3")},
                       "fees_7d_usd": tvl * 0.002 * n("fees")})
            })
            .collect();
        json!({"pools": pools})
    })
}

/// R7: a token's spot vs executable gap now, its 24 h baseline, and a flag when it widens.
/// Built from the history above (as the real feed would be computed at head). Placeholder.
pub fn token_health(t: &Token, size_usd: f64) -> Result<Value, String> {
    let now = crate::engine::now() as i64;
    let h = depth_history(t, "sell", size_usd, now - 25 * HOUR, now)?;
    let gaps: Vec<f64> = h["data"].as_array().into_iter().flatten().filter_map(|r| r["gap_bp"].as_f64()).collect();
    let Some(last) = gaps.last().copied() else { return Err("no history yet".into()) };
    let base = gaps.iter().take(gaps.len().saturating_sub(1)).copied().sum::<f64>() / (gaps.len().saturating_sub(1).max(1)) as f64;
    Ok(json!({"source": SOURCE, "placeholder": true, "request": "R7", "live_in_llens": false, "size_usd": size_usd,
              "gap_bp": last, "baseline_24h_bp": base, "widening": base > 0.0 && last > 3.0 * base, "stale": h["stale"]}))
}

/// R8: ranked range candidates for a pool (`lp-research`). Placeholder.
pub fn ranges(chain: u64, pool: &str) -> Result<Value, String> {
    let p = pool.to_lowercase();
    let day = crate::engine::now() as i64 / 86_400;
    cached(&format!("ranges:{chain}:{p}"), 86_400, "lp-research", "R8", false, move || {
        let rows: Vec<Value> = [0.1f64, 0.25, 0.5, 1.0, 2.0]
            .iter()
            .map(|w| {
                let n = 0.5 + 0.5 * noise(&format!("ranges:{chain}:{p}:{w}:{day}"));
                json!({"width_pct": w, "expected_fee_apr": (0.25 / w).min(1.5) * (0.6 + 0.8 * n), "in_range_share": (0.35 + 0.6 * w / 2.0).min(0.99)})
            })
            .collect();
        json!({"candidates": rows, "pinned_block": null, "fee_model": "placeholder"})
    })
}

/// R9: what added capital would earn in a pool (the depth-threshold curve). Placeholder.
pub fn depth_value(chain: u64, pool: &str) -> Result<Value, String> {
    let p = pool.to_lowercase();
    let day = crate::engine::now() as i64 / 86_400;
    cached(&format!("depth_value:{chain}:{p}"), 86_400, "depth-threshold curve", "R9", false, move || {
        let n = 0.5 + 0.5 * noise(&format!("dv:{chain}:{p}:{day}"));
        let pts: Vec<Value> = [10e3, 50e3, 100e3, 500e3, 1e6]
            .iter()
            .map(|c: &f64| {
                let share = 1.0 - (-c / (400e3 * (0.5 + n))).exp();
                json!({"capital_usd": c, "volume_share": share, "fees_per_year_usd": share * 150_000.0})
            })
            .collect();
        json!({"points": pts})
    })
}

/// R10: candidate pairs with routed demand and no deep pool. Placeholder.
pub fn candidates(chain: u64) -> Result<Value, String> {
    let day = crate::engine::now() as i64 / 86_400;
    cached(&format!("candidates:{chain}"), 86_400, "pair-discovery", "R10", false, move || {
        let rows: Vec<Value> = (0..3)
            .map(|i| {
                let n = 0.5 + 0.5 * noise(&format!("cand:{chain}:{i}:{day}"));
                json!({"pair": format!("PLACEHOLDER{}/USDC", i + 1), "routed_volume_7d_usd": 50_000.0 + 900_000.0 * n, "deepest_pool_tvl_usd": 5_000.0 * n})
            })
            .collect();
        json!({"pairs": rows})
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(sym: &str, usd: Option<f64>) -> Token {
        Token { chain: 8453, address: format!("0x{:0>40}", sym.len()), symbol: sym.into(), usd_hint: usd }
    }

    #[test]
    fn off_unless_experimental_is_on() {
        assert_eq!(mode_of(&json!({})), Mode::Off);
        assert_eq!(mode_of(&json!({"experimental": {"enabled": false, "llens": "placeholder"}})), Mode::Off);
        assert_eq!(mode_of(&json!({"experimental": {"enabled": true}})), Mode::Placeholder);
        assert_eq!(mode_of(&json!({"experimental": {"enabled": true, "llens": "off"}})), Mode::Off);
    }

    #[test]
    fn placeholder_data_is_never_decision_grade() {
        assert!(!decision_grade(&json!({"placeholder": true, "stale": false})));
        assert!(!decision_grade(&json!({"data": 1})));
        assert!(!decision_grade(&json!({"placeholder": false, "stale": true})));
        assert!(decision_grade(&json!({"placeholder": false, "stale": false})));
    }

    #[test]
    fn placeholder_numbers_are_stable_and_plausible() {
        let usdt = tok("USDT", Some(1.0));
        let weth = tok("WETH", Some(3000.0));
        let h = 1_000 * HOUR;
        assert_eq!(depth_1pct(&weth, h), depth_1pct(&weth, h));
        assert!(depth_1pct(&usdt, h) > depth_1pct(&weth, h));
        assert!(cost_bp(&weth, 1e6, h) > cost_bp(&weth, 1e4, h));
        let p = spot(&weth, h);
        assert!((p / 3000.0 - 1.0).abs() < 0.005, "{p}");
        assert!((spot(&usdt, h) - 1.0).abs() < 0.001);
    }
}
