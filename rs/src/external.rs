//! Other tokens' prices for node strategies (graph.rs `token_price`): a strategy on one pool may watch
//! another token, for example WETH's trend while it farms cbETH/WETH, or wS while it farms wS/Anon.
//!
//! A token's USD price comes from Solidly pools in the shared event store (history.rs): a pool of the
//! token against a dollar stable, or, when there is none, a pool against a paired token that has
//! one (BTC/WETH × WETH/USDC). Stables count as $1. Only pools with at least MIN_ACTIVE_USD of
//! active liquidity are used: a pool nobody routes through keeps an old price, which would read as
//! a calm market. A leg with no swap for STALE_S is stale too, and the price is NaN until it trades
//! again (a condition on it is false, a range on it waits).
//!
//! Prices are in ticks of the USD price (ln P / ln 1.0001, like every price in a graph), at the close
//! of each 5-minute bar of the strategy's own replay, with the bar's high and low on a direct route.
//!
//! Sync: the events are read from the local store. The worker keeps every pool of the chain current
//! there (market_flvr's chain-wide refresh, from the history server), so a live read mostly finds
//! them stored already; a pool the store does not cover over the bars (a backtest reaching further
//! back, a pool new to the route) is downloaded first, all of a token's pools in one go, and the
//! last minutes past the history server come from RPC only when the bars need them.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

pub const MIN_ACTIVE_USD: f64 = 500.0;
const STALE_S: i64 = 12 * 3600;

#[derive(Clone, Debug)]
pub struct Leg {
    pub pool: String,
    pub symbol: String,
    /// the priced token is the pool's token0
    pub is0: bool,
    /// decimals of the priced token and of the other one
    pub d_tok: i32,
    pub d_other: i32,
    pub active_usd: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Series {
    pub close: Vec<f64>,
    pub hi: Vec<f64>,
    pub lo: Vec<f64>,
    /// when the swap behind each close happened (0: none)
    pub at: Vec<i64>,
}

fn is_usd(sym: &str) -> bool {
    crate::market_data::is_pegged(sym, "USDC", None)
}

fn wallet() -> String {
    crate::settings::load_settings()["wallet"].as_str().unwrap_or("").to_lowercase()
}

/// The pools that price `token` on chain `cid` in USD, hop by hop: one hop (pools against dollar
/// stables) or two (pools against a paired token X, then X's pools against stables). Every pool of a
/// hop with enough active liquidity is used, so the route follows liquidity as it moves between
/// pools: it is worked out again whenever the price is loaded. Empty for a stable itself.
pub fn route(cid: u64, token: &str) -> Result<Vec<Vec<Leg>>, String> {
    let token = token.to_lowercase();
    let pools = crate::market_data::pools(cid, &wallet())?;
    let ok: Vec<&Value> = pools.iter().filter(|p| p["active_usd"].as_f64().unwrap_or(0.0) >= MIN_ACTIVE_USD).collect();
    let addr = |t: &Value| t["address"].as_str().unwrap_or("").to_lowercase();
    let sym = |t: &Value| t["symbol"].as_str().unwrap_or("").to_string();
    let dec = |t: &Value| t["decimals"].as_i64().unwrap_or(18) as i32;
    for p in &pools {
        for t in [&p["token0"], &p["token1"]] {
            if addr(t) == token && is_usd(&sym(t)) {
                return Ok(vec![]);
            }
        }
    }
    let leg = |p: &Value, of: &str| -> Option<(Leg, Value)> {
        let (t0, t1) = (&p["token0"], &p["token1"]);
        let (me, other, is0) = if addr(t0) == of { (t0, t1, true) } else if addr(t1) == of { (t1, t0, false) } else { return None };
        Some((Leg { pool: p["pool"].as_str().unwrap_or("").to_string(), symbol: format!("{}/{}", sym(t0), sym(t1)), is0, d_tok: dec(me), d_other: dec(other),
                    active_usd: p["active_usd"].as_f64().unwrap_or(0.0) }, other.clone()))
    };
    let to_usd = |of: &str| -> Vec<Leg> {
        let mut v: Vec<Leg> = ok.iter().filter_map(|p| leg(p, of)).filter(|(_, o)| is_usd(&sym(o))).map(|x| x.0).collect();
        v.sort_by(|a, b| b.active_usd.total_cmp(&a.active_usd));
        v
    };
    let direct = to_usd(&token);
    if !direct.is_empty() {
        return Ok(vec![direct]);
    }
    // through a paired token X with its own stable pools: the X whose thinner hop is deepest
    let mut by_x: HashMap<String, Vec<Leg>> = HashMap::new();
    for (l, other) in ok.iter().filter_map(|p| leg(p, &token)) {
        if !is_usd(&sym(&other)) {
            by_x.entry(addr(&other)).or_default().push(l);
        }
    }
    let mut best: Option<(f64, Vec<Vec<Leg>>)> = None;
    for (x, mut h1) in by_x {
        let h2 = to_usd(&x);
        if h2.is_empty() {
            continue;
        }
        h1.sort_by(|a, b| b.active_usd.total_cmp(&a.active_usd));
        let depth = h1.iter().map(|l| l.active_usd).sum::<f64>().min(h2.iter().map(|l| l.active_usd).sum::<f64>());
        if best.as_ref().map(|b| depth > b.0).unwrap_or(true) {
            best = Some((depth, vec![h1, h2]));
        }
    }
    best.map(|b| b.1).ok_or_else(|| {
        let thin: Vec<String> = pools.iter().filter_map(|p| leg(p, &token)).map(|(l, _)| format!("{} (${:.0} active)", l.symbol, l.active_usd)).collect();
        if thin.is_empty() {
            format!("no Solidly pool on chain {cid} holds token {token}")
        } else {
            format!("no pool with at least ${MIN_ACTIVE_USD:.0} of active liquidity prices this token in dollars, directly or through one paired token (its pools: {})", thin.join(", "))
        }
    })
}

/// Every token on chain `cid` a strategy can watch, with its route (for the lab's token picker).
pub fn sources(cid: u64) -> Result<Value, String> {
    let pools = crate::market_data::pools(cid, &wallet())?;
    let mut seen: HashMap<String, Value> = HashMap::new();
    for p in &pools {
        for t in [&p["token0"], &p["token1"]] {
            let a = t["address"].as_str().unwrap_or("").to_lowercase();
            if a.is_empty() || seen.contains_key(&a) {
                continue;
            }
            let v = match route(cid, &a) {
                Ok(hops) => {
                    // a route is as deep as its thinner hop
                    let depth = hops.iter().map(|h| h.iter().map(|l| l.active_usd).sum::<f64>()).fold(f64::INFINITY, f64::min);
                    json!({"address": a, "symbol": t["symbol"], "logo": t["logo"], "ok": true, "stable": hops.is_empty(),
                           "active_usd": if depth.is_finite() { json!(depth) } else { Value::Null },
                           "route": hops.iter().map(|h| h.iter().map(|l| json!({"pool": l.pool, "symbol": l.symbol, "active_usd": l.active_usd})).collect::<Vec<_>>()).collect::<Vec<_>>()})
                }
                Err(e) => json!({"address": a, "symbol": t["symbol"], "logo": t["logo"], "ok": false, "error": e}),
            };
            seen.insert(a, v);
        }
    }
    let mut out: Vec<Value> = seen.into_values().collect();
    out.sort_by(|a, b| b["ok"].as_bool().cmp(&a["ok"].as_bool()).then(a["symbol"].as_str().cmp(&b["symbol"].as_str())));
    Ok(json!({"chain_id": cid, "min_active_usd": MIN_ACTIVE_USD, "tokens": out}))
}

/// One leg's price of the token in the other token, in ticks, at each bar close (NaN before its
/// first swap or when stale), with the bar's high and low.
fn leg_series(cid: u64, l: &Leg, bar_end: &[i64]) -> Result<Series, String> {
    let (from, to) = span(bar_end);
    let mut sw: Vec<(i64, i64, i64)> = crate::history::read(cid, &l.pool, from, to + 1).into_iter().filter(|e| e.kind == "swap").map(|e| (e.ts, e.block * 100_000 + e.log_index, e.tick)).collect();
    sw.sort_by_key(|x| x.1);
    sw.dedup_by_key(|x| x.1);
    Ok(from_swaps(&sw.iter().map(|x| (x.0, x.2)).collect::<Vec<_>>(), l, bar_end))
}

/// The swaps a leg's bars read: from a stale window before the first bar to the last one.
fn span(bar_end: &[i64]) -> (i64, i64) {
    (bar_end[0] - STALE_S - 3600, *bar_end.last().unwrap())
}

/// How far short of the last bar the store may end before the pools are topped up: the worker's
/// refresh keeps them about this current, and a minute of missing swaps only delays a close.
const SYNC_SLACK_S: i64 = 15 * 60;

/// Every pool of the route stored over the bars: one download for those that are not.
fn sync(cid: u64, hops: &[Vec<Leg>], bar_end: &[i64]) -> Result<(), String> {
    let (from, to) = span(bar_end);
    let mut pools: Vec<String> = hops.iter().flatten().map(|l| l.pool.to_lowercase()).collect();
    pools.sort();
    pools.dedup();
    crate::history::ensure_until(cid, &pools, from, Some(to - SYNC_SLACK_S)).map(|_| ())
}

/// The bar series of one leg from its swaps (time, tick), in order.
fn from_swaps(sw: &[(i64, i64)], l: &Leg, bar_end: &[i64]) -> Series {
    let n = bar_end.len();
    let c = (10f64).ln() / 1.0001f64.ln();
    let shift = (l.d_tok - l.d_other) as f64 * c;
    let price = |tick: i64| if l.is0 { tick as f64 + shift } else { -(tick as f64) + shift };
    let mut s = Series { close: vec![f64::NAN; n], hi: vec![f64::NAN; n], lo: vec![f64::NAN; n], at: vec![0; n] };
    let mut k = 0;
    let mut last: Option<(i64, f64)> = None;
    for j in 0..n {
        let (mut h, mut lo) = (f64::NEG_INFINITY, f64::INFINITY);
        while k < sw.len() && sw[k].0 <= bar_end[j] {
            let p = price(sw[k].1);
            h = h.max(p);
            lo = lo.min(p);
            last = Some((sw[k].0, p));
            k += 1;
        }
        if let Some((t, p)) = last {
            if bar_end[j] - t <= STALE_S {
                s.close[j] = p;
                s.hi[j] = if h.is_finite() { h } else { p };
                s.lo[j] = if lo.is_finite() { lo } else { p };
                s.at[j] = t;
            }
        }
    }
    s
}

/// One hop from all its pools: at each bar the price of the pool that traded last (the freshest
/// price; arbitrage keeps the pools together), with that pool's high and low.
fn combine(legs: Vec<Series>) -> Series {
    let n = legs.first().map(|s| s.close.len()).unwrap_or(0);
    let mut out = Series { close: vec![f64::NAN; n], hi: vec![f64::NAN; n], lo: vec![f64::NAN; n], at: vec![0; n] };
    for j in 0..n {
        let mut best: Option<&Series> = None;
        for s in &legs {
            if s.close[j].is_finite() && best.map(|b| s.at[j] > b.at[j]).unwrap_or(true) {
                best = Some(s);
            }
        }
        if let Some(b) = best {
            out.close[j] = b.close[j];
            out.at[j] = b.at[j];
            out.hi[j] = b.hi[j];
            out.lo[j] = b.lo[j];
        }
    }
    out
}

static CACHE: Mutex<Option<Vec<((u64, String, i64, i64, usize), Series)>>> = Mutex::new(None);

/// The token's USD price series over the bars `bar_end` (cached: a backtest's variants share it).
pub fn load(cid: u64, token: &str, bar_end: &[i64]) -> Result<Series, String> {
    if bar_end.is_empty() {
        return Ok(Series::default());
    }
    let key = (cid, token.to_lowercase(), bar_end[0], *bar_end.last().unwrap(), bar_end.len());
    if let Some(s) = CACHE.lock().unwrap().get_or_insert_with(Vec::new).iter().find(|(k, _)| *k == key).map(|(_, s)| s.clone()) {
        return Ok(s);
    }
    let hops = route(cid, token)?;
    sync(cid, &hops, bar_end)?;
    let n = bar_end.len();
    let mut s = Series { close: vec![0.0; n], hi: vec![0.0; n], lo: vec![0.0; n], at: vec![0; n] };
    for (k, hop) in hops.iter().enumerate() {
        let h = combine(hop.iter().map(|l| leg_series(cid, l, bar_end)).collect::<Result<Vec<_>, _>>()?);
        // token in X, plus X in dollars: the second hop adds its close (its own range is X's, not the token's)
        for j in 0..n {
            s.close[j] += h.close[j];
            s.hi[j] += if k == 0 { h.hi[j] } else { h.close[j] };
            s.lo[j] += if k == 0 { h.lo[j] } else { h.close[j] };
        }
    }
    let mut g = CACHE.lock().unwrap();
    let list = g.as_mut().unwrap();
    list.push((key, s.clone()));
    if list.len() > 8 {
        list.remove(0);
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_take_the_last_swap_and_go_stale_without_swaps() {
        // WETH (18 decimals) as token0 against USDC (6): tick t is 1.0001^t * 1e12 USDC per WETH
        let l = Leg { pool: String::new(), symbol: String::new(), is0: true, d_tok: 18, d_other: 6, active_usd: 1e4 };
        let bars: Vec<i64> = (1..=200).map(|k| k * 300).collect();
        let sw = vec![(10, -198_000), (250, -197_990), (290, -198_010), (600, -197_000)];
        let s = from_swaps(&sw, &l, &bars);
        let usd = |t: f64| 1.0001f64.powf(t);
        assert!((usd(s.close[0]) - 2500.0).abs() / 2500.0 < 0.05, "about $2,500 a WETH: {}", usd(s.close[0]));
        let c = (10f64).ln() / 1.0001f64.ln();
        assert!((s.close[0] - (-198_010.0 + 12.0 * c)).abs() < 1e-6, "the bar's last swap");
        assert!((s.hi[0] - s.lo[0] - 20.0).abs() < 1e-6, "the bar's high and low");
        assert_eq!(s.close[2], s.close[1], "no swap: the last price, high = low");
        let stale = bars.iter().position(|b| b - 600 > STALE_S).unwrap();
        assert!(s.close[stale - 1].is_finite() && s.close[stale].is_nan(), "12 hours without a swap is stale");
        // the same pool with WETH as token1: the price is the inverse
        let l1 = Leg { is0: false, d_tok: 18, d_other: 6, ..l.clone() };
        let s1 = from_swaps(&[(10, 198_000)], &l1, &bars[..1]);
        assert!((s1.close[0] - (-198_000.0 + 12.0 * c)).abs() < 1e-6);
    }

    #[test]
    fn a_hop_takes_the_pool_that_traded_last() {
        let l = Leg { pool: String::new(), symbol: String::new(), is0: true, d_tok: 6, d_other: 6, active_usd: 1e4 };
        let bars: Vec<i64> = (1..=10).map(|k| k * 300).collect();
        let a = from_swaps(&[(10, 100), (1000, 110)], &l, &bars);
        let b = from_swaps(&[(500, 105)], &l, &bars);
        let h = combine(vec![a, b]);
        assert_eq!(h.close[0], 100.0, "only the first pool has traded");
        assert_eq!(h.close[1], 105.0, "the second traded later");
        assert_eq!(h.close[3], 110.0, "the first traded again");
        let empty = from_swaps(&[], &l, &bars);
        assert!(combine(vec![empty]).close.iter().all(|x| x.is_nan()));
    }
}
