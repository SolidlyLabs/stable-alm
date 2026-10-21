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
    /// ticks per range (1: one-tick ranges); the solve bins the span into ranges this wide
    pub width: i64,
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
            width: (n("width", 1.0) as i64).max(1),
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
         "p": {"hours": hours, "span": p.span.max(0), "every_hours": every_hours, "turnover": p.turnover.clamp(0.0, 1.0), "guard": p.guard.unwrap_or(0.0).max(0.0), "width": p.width.max(1)}, "in": {}}]})
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
            r[j] += m.sig_fee(i0 + k);
            osum[j] += m.l_others[i0 + k];
            cnt[j] += 1;
        }
    }
    let o: Vec<f64> = (0..n).map(|j| (osum[j] / cnt[j].max(1) as f64).max(1.0)).collect();
    if np_sum(&r) <= 0.0 {
        return None;
    }
    // a tick no swap paid at gets exactly 0 (o >= 1), so only the paid ones are computed; the zeros
    // stay in place so the sums are the same, to the bit, as over every tick
    let paid: Vec<usize> = (0..n).filter(|j| r[*j] > 0.0).collect();
    let mut buf = vec![0.0; n];
    let alloc = |buf: &mut [f64], lam: f64| {
        for &j in &paid {
            buf[j] = ((r[j] * o[j] / lam).sqrt() - o[j]).max(0.0);
        }
    };
    let (mut a, mut b) = (1e-30f64, 1e10f64);
    for _ in 0..200 {
        let mid = (a * b).sqrt();
        // a and b are neighbouring floats: every later step would leave them as they are
        if mid == a || mid == b {
            break;
        }
        alloc(&mut buf, mid);
        if np_sum(&buf) > budget_l {
            a = mid;
        } else {
            b = mid;
        }
    }
    alloc(&mut buf, b);
    let liq = buf;
    let s = np_sum(&liq);
    if s <= 0.0 {
        return None;
    }
    Some((lo, liq.iter().map(|x| x / s).collect()))
}

/// `solve` over ranges `width` ticks wide: the span [c - span, c + span] cut into bins on the grid of
/// multiples of `width` (the bins at its ends reach past it), each bin's fees and others' liquidity summed (fees) and averaged (liquidity) over its
/// ticks. Liquidity spread over w ticks costs w times what one tick does, so the budget in liquidity
/// is the one-tick budget over w. Weights per bin, as value weights (every bin is equally wide).
pub fn solve_binned(m: &Market, p: &Params, i1: usize, budget_l: f64) -> Option<(i64, Vec<f64>)> {
    let k = p.width.max(1);
    let i0 = m.ts.partition_point(|t| *t < m.ts[i1] - p.lookback);
    let ticks = &m.tick[i0..=i1];
    if ticks.len() < 50 {
        return None;
    }
    let mut tf: Vec<f64> = ticks.iter().map(|t| *t as f64).collect();
    let c = median(&mut tf) as i64;
    // bins on one grid, multiples of `width` ticks, covering [c - span, c + span]
    let lo = (c - p.span).div_euclid(k) * k;
    let n = (c + p.span + 1 - lo + k - 1).div_euclid(k) as usize;
    let hi = lo + n as i64 * k;
    let (mut r, mut osum, mut cnt) = (vec![0.0; n], vec![0.0; n], vec![0usize; n]);
    for (q, t) in ticks.iter().enumerate() {
        if *t >= lo && *t < hi {
            let j = ((*t - lo) / k) as usize;
            r[j] += m.sig_fee(i0 + q);
            osum[j] += m.l_others[i0 + q];
            cnt[j] += 1;
        }
    }
    let o: Vec<f64> = (0..n).map(|j| (osum[j] / cnt[j].max(1) as f64).max(1.0)).collect();
    if np_sum(&r) <= 0.0 {
        return None;
    }
    let budget = budget_l / k as f64;
    let alloc = |lam: f64| -> Vec<f64> { (0..n).map(|j| if r[j] > 0.0 { ((r[j] * o[j] / lam).sqrt() - o[j]).max(0.0) } else { 0.0 }).collect() };
    let (mut a, mut b) = (1e-30f64, 1e10f64);
    for _ in 0..200 {
        let mid = (a * b).sqrt();
        if mid == a || mid == b {
            break;
        }
        if np_sum(&alloc(mid)) > budget {
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
    if p.width > 1 {
        return match solve_binned(m, p, i1, budget_l) {
            // too little history to solve: one range of the requested width, on the grid of
            // `width`-tick bins, around the price
            None => {
                let k = p.width;
                let lo = tick.div_euclid(k) * k;
                vec![(lo, lo + k, 1.0)]
            }
            Some((lo, w)) => {
                let top = w.iter().copied().fold(0.0f64, f64::max);
                let k = p.width;
                w.iter().enumerate().filter(|(_, x)| **x > 1e-3 * top && **x > 0.0).map(|(j, x)| (lo + j as i64 * k, lo + (j as i64 + 1) * k, *x)).collect()
            }
        };
    }
    match solve(m, p, i1, budget_l) {
        None => vec![(tick - 2, tick + 3, 1.0)],
        // ticks under a thousandth of the largest weight are dropped (relative, so a broad, even
        // allocation keeps its ticks instead of losing every one of them)
        Some((lo, w)) => {
            let top = w.iter().copied().fold(0.0f64, f64::max);
            w.iter().enumerate().filter(|(_, x)| **x > 1e-3 * top && **x > 0.0).map(|(j, x)| (lo + j as i64, lo + j as i64 + 1, *x)).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// solve as it was before the paid-ticks shortcut and the early stop: every tick, 200 steps
    fn solve_reference(m: &Market, p: &Params, i1: usize, budget_l: f64) -> Option<(i64, Vec<f64>)> {
        let i0 = m.ts.partition_point(|t| *t < m.ts[i1] - p.lookback);
        let ticks = &m.tick[i0..=i1];
        if ticks.len() < 50 {
            return None;
        }
        let mut tf: Vec<f64> = ticks.iter().map(|t| *t as f64).collect();
        let c = median(&mut tf) as i64;
        let (lo, hi) = (c - p.span, c + p.span + 1);
        let n = (hi - lo) as usize;
        let mut r = vec![0.0; n];
        let mut cnt = vec![0usize; n];
        let mut osum = vec![0.0; n];
        for (k, t) in ticks.iter().enumerate() {
            if *t >= lo && *t < hi {
                let j = (*t - lo) as usize;
                r[j] += m.sig_fee(i0 + k);
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

    #[test]
    fn the_fast_solve_gives_exactly_the_reference_weights() {
        // a wandering price with uneven fees and depth, solved at many points, narrow and wide spans
        let n = 20_000usize;
        let mut seed = 7u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut tick = vec![0i64; n];
        for i in 1..n {
            tick[i] = tick[i - 1] + ((rnd() - 0.5) * 9.0).round() as i64;
        }
        let fee: Vec<f64> = (0..n).map(|_| rnd() * 3.0).collect();
        let others: Vec<f64> = (0..n).map(|_| 1.0 + rnd() * 1e6).collect();
        let ts: Vec<i64> = (0..n as i64).map(|k| 1_000_000 + k * 30).collect();
        let m = Market { ts, tick: tick.clone(), sp: vec![1.0; n], fee, fee_sig: vec![], l_others: others.clone(), l_active: others, bar_end: vec![], bar_idx: vec![],
                         bar_tick: vec![], bar_hi: vec![], bar_lo: vec![], bar_fee: vec![], decimals: 6, px_scale: 1.0, liq_scale: 1.0, num1: true, bar_s: 300 };
        for span in [3, 40, 600, 6000] {
            let p = Params { lookback: 86400, span, every: 72, turnover: 0.4, guard: None, width: 1 };
            for i1 in (3000..n).step_by(997) {
                for budget in [1e3, 1e7, 1e11] {
                    assert_eq!(solve(&m, &p, i1, budget), solve_reference(&m, &p, i1, budget), "span {span}, swap {i1}, budget {budget}");
                }
            }
        }
    }

    #[test]
    fn a_wide_ladder_places_ranges_exactly_width_ticks_wide_on_one_grid() {
        // a price wandering over a few hundred ticks, fees and depth uneven
        let n = 8_000usize;
        let mut seed = 11u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut tick = vec![0i64; n];
        for i in 1..n {
            tick[i] = tick[i - 1] + ((rnd() - 0.5) * 7.0).round() as i64;
        }
        let fee: Vec<f64> = (0..n).map(|_| rnd() * 3.0).collect();
        let others: Vec<f64> = (0..n).map(|_| 1.0 + rnd() * 1e6).collect();
        let ts: Vec<i64> = (0..n as i64).map(|k| 1_000_000 + k * 30).collect();
        let m = Market { ts, tick: tick.clone(), sp: vec![1.0; n], fee, fee_sig: vec![], l_others: others.clone(), l_active: others, bar_end: vec![], bar_idx: vec![],
                         bar_tick: vec![], bar_hi: vec![], bar_lo: vec![], bar_fee: vec![], decimals: 6, px_scale: 1.0, liq_scale: 1.0, num1: true, bar_s: 300 };
        let i1 = n - 1;
        for width in [2i64, 10, 60] {
            let p = Params { lookback: 86400, span: 120, every: 72, turnover: 0.4, guard: None, width };
            let r = decide_at(&m, &p, 1e4, 1.0, tick[i1], i1);
            assert!(r.len() > 1, "width {width}: {r:?}");
            for (tl, tu, w) in &r {
                assert_eq!(tu - tl, width, "width {width}");
                assert_eq!(tl.rem_euclid(width), 0, "width {width}: on the grid of multiples of the width");
                assert!(*w > 0.0);
            }
            assert!(r.iter().map(|x| x.2).sum::<f64>() <= 1.0 + 1e-9);
            // inside the span around the median, rounded up to whole bins
            let mut tf: Vec<f64> = m.tick[m.ts.partition_point(|t| *t < m.ts[i1] - p.lookback)..=i1].iter().map(|t| *t as f64).collect();
            let c = median(&mut tf) as i64;
            assert!(r.iter().all(|(tl, tu, _)| *tl > c - p.span - width && *tu < c + p.span + 1 + width));
        }
        // too little history (or no fees) to solve: still one range exactly `width` wide, holding the price
        let few = Market { ts: m.ts[..20].to_vec(), tick: m.tick[..20].to_vec(), sp: vec![1.0; 20], fee: vec![0.0; 20], fee_sig: vec![], l_others: vec![1.0; 20], l_active: vec![1.0; 20],
                           bar_end: vec![], bar_idx: vec![], bar_tick: vec![], bar_hi: vec![], bar_lo: vec![], bar_fee: vec![], decimals: 6, px_scale: 1.0, liq_scale: 1.0, num1: true, bar_s: 300 };
        for width in [2i64, 7] {
            let p = Params { lookback: 86400, span: 120, every: 72, turnover: 0.4, guard: None, width };
            for t in [-13i64, 0, 5, 1001] {
                let r = decide_at(&few, &p, 1e4, 1.0, t, 19);
                assert_eq!(r.len(), 1);
                let (tl, tu, _) = r[0];
                assert!(tu - tl == width && tl <= t && t < tu && tl.rem_euclid(width) == 0, "width {width}, tick {t}: {r:?}");
            }
        }
        // width 1 is the per-tick ladder, unchanged
        let p1 = Params { lookback: 86400, span: 120, every: 72, turnover: 0.4, guard: None, width: 1 };
        let r1 = decide_at(&m, &p1, 1e4, 1.0, tick[i1], i1);
        let (lo, w) = solve(&m, &p1, i1, 1e4 * 1e6 / (1.0001f64.sqrt() - 1.0)).unwrap();
        let top = w.iter().copied().fold(0.0f64, f64::max);
        let want: Vec<(i64, i64, f64)> = w.iter().enumerate().filter(|(_, x)| **x > 1e-3 * top && **x > 0.0).map(|(j, x)| (lo + j as i64, lo + j as i64 + 1, *x)).collect();
        assert_eq!(r1, want);
    }

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
