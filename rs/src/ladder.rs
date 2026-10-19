//! The ladder (water-filling across single ticks) with the depeg guard: the solve behind the
//! node strategies' ladder block (graph.rs). One decision at the latest bar (checked against the
//! original Python version in parity.rs).
//!
//! There is no plain ladder strategy type any more: an old one ({"type": "ladder"}, in pools.json, a
//! saved strategy or a request) is migrated to a node strategy with one ladder block and the same
//! settings (`migrate`).

use crate::data::{median, Market};
#[cfg(test)]
use crate::data::{rolling_median_trailing, week_bars};
use serde_json::{json, Map, Value};

#[derive(Clone, Debug)]
pub struct Params {
    pub lookback: i64,
    pub span: i64,
    pub every: i64,
    pub turnover: f64,
    pub guard: Option<f64>,
}

impl Params {
    pub fn from(s: &serde_json::Map<String, serde_json::Value>) -> Result<Params, String> {
        if s.get("type").and_then(|t| t.as_str()).unwrap_or("ladder") != "ladder" {
            return Err("the Rust runtime supports only the ladder strategy".into());
        }
        let n = |k: &str, d: f64| s.get(k).and_then(|v| v.as_f64()).unwrap_or(d);
        Ok(Params {
            lookback: n("lookback", 86400.0) as i64,
            span: n("span", 3.0) as i64,
            every: n("every", 72.0) as i64,
            turnover: n("turnover", 0.4),
            guard: s.get("guard").and_then(|v| v.as_f64()).filter(|g| *g != 0.0),
        })
    }

    /// A pool's ladder as adding the pool sets it: 3 ticks (at least 3 spacings) and the depeg guard
    /// on a pegged pair; 60 spacings and no guard on any other.
    pub fn for_pool(spacing: i64, pegged: bool) -> Params {
        let d = Params::from(&plain_defaults()).unwrap();
        if pegged {
            Params { span: d.span.max(3 * spacing), ..d }
        } else {
            Params { span: 60 * spacing, guard: None, ..d }
        }
    }
}

/// What the old plain ladder's settings defaulted to (pool defaults, settings.rs, before v0.5.1).
pub fn plain_defaults() -> Map<String, Value> {
    json!({"type": "ladder", "lookback": 86400, "span": 3, "every": 72, "turnover": 0.4, "guard": 15}).as_object().unwrap().clone()
}

/// The ladder as a node strategy: one ladder block with these settings (span and guard in ticks).
/// The block's limits apply: lookback 1 to 168 hours, re-solve every 0.1 to 168 hours.
pub fn as_graph(p: &Params) -> Value {
    let hours = (p.lookback as f64 / 3600.0).clamp(1.0, 168.0);
    let every_hours = (p.every as f64 * crate::data::BAR_S as f64 / 3600.0).clamp(0.1, 168.0);
    json!({"type": "graph", "name": "Ladder", "nodes": [
        {"id": "lad", "kind": "ladder", "x": 960, "y": 60, "unit": "ticks",
         "p": {"hours": hours, "span": p.span.max(0), "every_hours": every_hours, "turnover": p.turnover.clamp(0.0, 1.0), "guard": p.guard.unwrap_or(0.0).max(0.0)}, "in": {}}]})
}

/// An old plain ladder as the same node strategy; None for any other strategy. A setting it leaves
/// out takes the old pool default (`plain_defaults`, which the app merged in when loading pools).
pub fn migrate(s: &Value) -> Option<Value> {
    let o = s.as_object()?;
    if o.get("type").and_then(|t| t.as_str()).unwrap_or("ladder") != "ladder" {
        return None;
    }
    let mut m = plain_defaults();
    for (k, v) in o {
        m.insert(k.clone(), v.clone());
    }
    m.insert("type".into(), json!("ladder"));
    Params::from(&m).ok().map(|p| as_graph(&p))
}

/// The strategy, an old plain ladder migrated.
pub fn normalized(s: &Value) -> Value {
    migrate(s).unwrap_or_else(|| s.clone())
}

/// numpy's pairwise summation (np.sum of a float array), for identical weights.
pub fn np_sum(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        let mut r = 0.0;
        for x in a {
            r += x;
        }
        r
    } else if n <= 128 {
        let mut r = [a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]];
        let mut i = 8;
        while i < n - n % 8 {
            for j in 0..8 {
                r[j] += a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        np_sum(&a[..n2]) + np_sum(&a[n2..])
    }
}

/// Distance of the latest bar tick from the 7-day (2016-bar) median; None without a guard (the
/// ladder block's guard, graph.rs, computes the same at every bar).
#[cfg(test)]
pub fn guard_dev(m: &Market, p: &Params) -> Option<f64> {
    p.guard?;
    let med = rolling_median_trailing(&m.bar_tick, week_bars(m), 12);
    let b = m.bar_tick.len().checked_sub(1)?;
    Some((m.bar_tick[b] as f64 - med[b]).abs())
}

/// (lowest tick, weights) over [c - span, c + span] from the trailing `lookback`, or None.
pub fn solve(m: &Market, p: &Params, i1: usize, budget_l: f64) -> Option<(i64, Vec<f64>)> {
    let i0 = m.ts.partition_point(|t| *t < m.ts[i1] - p.lookback);
    let ticks = &m.tick[i0..=i1];
    if ticks.len() < 50 {
        return None;
    }
    let mut tf: Vec<f64> = ticks.iter().map(|t| *t as f64).collect();
    let c = median(&mut tf) as i64; // int(np.median(...)): truncates toward zero
    let (lo, hi) = (c - p.span, c + p.span + 1);
    let n = (hi - lo) as usize;
    let mut r = vec![0.0; n];
    let mut cnt = vec![0usize; n];
    let mut osum = vec![0.0; n];
    for (k, t) in ticks.iter().enumerate() {
        if *t >= lo && *t < hi {
            let j = (*t - lo) as usize;
            r[j] += m.fee[i0 + k];
            osum[j] += m.l_others[i0 + k];
            cnt[j] += 1;
        }
    }
    let o: Vec<f64> = (0..n).map(|j| (osum[j] / cnt[j].max(1) as f64).max(1.0)).collect();
    if np_sum(&r) <= 0.0 {
        return None;
    }
    let alloc = |lam: f64| -> Vec<f64> { (0..n).map(|j| ((r[j] * o[j] / lam).sqrt() - o[j]).max(0.0)).collect() };
    let (mut a, mut b) = (1e-30f64, 1e10f64);
    for _ in 0..200 {
        let mid = (a * b).sqrt();
        if np_sum(&alloc(mid)) > budget_l {
            a = mid;
        } else {
            b = mid;
        }
    }
    let liq = alloc(b);
    let s = np_sum(&liq);
    if s <= 0.0 {
        return None;
    }
    Some((lo, liq.iter().map(|x| x / s).collect()))
}

/// The ladder's decision at the latest bar for a fresh state (the executor re-solves on its own
/// schedule): ranges (tl, tu, weight). x = budget in token0 units, sp = sqrt price (float).
pub fn decide(m: &Market, p: &Params, x: f64, sp: f64, tick: i64) -> Vec<(i64, i64, f64)> {
    decide_at(m, p, x, sp, tick, *m.bar_idx.last().unwrap())
}

/// The same decision as of swap `i1` (the backtester's bar), from the swaps up to it only.
pub fn decide_at(m: &Market, p: &Params, x: f64, sp: f64, tick: i64, i1: usize) -> Vec<(i64, i64, f64)> {
    let budget_l = x * 10f64.powi(m.decimals) * sp / (1.0001f64.sqrt() - 1.0) / m.liq_scale;
    match solve(m, p, i1, budget_l) {
        None => vec![(tick - 2, tick + 3, 1.0)],
        Some((lo, w)) => w.iter().enumerate().filter(|(_, x)| **x > 1e-3).map(|(j, x)| (lo + j as i64, lo + j as i64 + 1, *x)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_old_plain_ladder_becomes_one_ladder_block_with_the_same_settings() {
        for (old, guard) in [(json!({"type": "ladder", "lookback": 86400, "span": 3, "every": 72, "turnover": 0.4, "guard": 15}), Some(15.0)),
                             (json!({"type": "ladder", "span": 120, "guard": null}), None),
                             (json!({"span": 7}), Some(15.0))] {
            let g = migrate(&old).expect("a ladder");
            let p = crate::graph::Graph::parse(g.as_object().unwrap(), 60).unwrap().ladder().unwrap();
            let want = Params::from(&{
                let mut m = plain_defaults();
                m.extend(old.as_object().unwrap().clone());
                m.insert("type".into(), json!("ladder"));
                m
            }).unwrap();
            assert_eq!((p.lookback, p.span, p.every, p.turnover, p.guard), (want.lookback, want.span, want.every, want.turnover, guard), "{old}");
        }
        assert!(migrate(&json!({"type": "graph", "nodes": []})).is_none());
        assert!(migrate(&json!({"type": "custom", "width": 10})).is_none());
        let p = Params::for_pool(10, false);
        assert_eq!((p.span, p.guard), (600, None));
        assert_eq!(Params::for_pool(10, true).span, 30);
    }
}
