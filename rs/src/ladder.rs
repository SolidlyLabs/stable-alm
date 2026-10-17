//! The live strategy: Ladder (water-filling across single ticks) with the depeg guard, as in
//! alm/strategies.py. Only what the executor uses: one decision at the latest bar.

use crate::data::{median, rolling_median_trailing_bfill, Market};

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

    pub fn label(&self) -> String {
        let mut s = format!("ladder(lookback={},span={},every={},turnover={}", self.lookback, self.span, self.every, self.turnover);
        if let Some(g) = self.guard {
            s += &format!(",guard={}", g as i64);
        }
        s + ")"
    }
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

/// Distance of the latest bar tick from the 7-day (2016-bar) median; None without a guard.
pub fn guard_dev(m: &Market, p: &Params) -> Option<f64> {
    p.guard?;
    let med = rolling_median_trailing_bfill(&m.bar_tick, 2016, 12);
    let b = m.bar_tick.len().checked_sub(1)?;
    Some((m.bar_tick[b] as f64 - med[b]).abs())
}

pub fn frozen(m: &Market, p: &Params) -> bool {
    match (guard_dev(m, p), p.guard) {
        (Some(d), Some(g)) => d > g, // NaN (too little history) never freezes, as in pandas
        _ => false,
    }
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
    let budget_l = x * 10f64.powi(m.decimals) * sp / (1.0001f64.sqrt() - 1.0) / m.liq_scale;
    let i1 = *m.bar_idx.last().unwrap();
    match solve(m, p, i1, budget_l) {
        None => vec![(tick - 2, tick + 3, 1.0)],
        Some((lo, w)) => w.iter().enumerate().filter(|(_, x)| **x > 1e-3).map(|(j, x)| (lo + j as i64, lo + j as i64 + 1, *x)).collect(),
    }
}
