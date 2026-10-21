//! How much a pool's flow follows its liquidity: the elasticity β in fees ∝ liquidity^β, measured on
//! the pool's own recent swaps, for the backtest's fee model (backtest.rs: a position that deepens
//! the pool by a factor r draws r^β of the swap's real flow).
//!
//! β = 0 is flow that ignores depth (each LP's share shrinks as liquidity grows); β = 1 is flow in
//! proportion to depth (the fee per unit of liquidity is fixed, whatever the capital).
//!
//! The estimate, per hour over the last days of the replay: log fees against log active liquidity,
//! with the hour's price movement (big moves bring volume and move ranges) and day effects (busy and
//! quiet days) taken out first. Liquidity also follows volume (LPs come to busy pools), which biases
//! a plain regression upward, so where the wallet has positions in the pool their share of the
//! liquidity is the instrument: the ALM's ranges going in and out of the price change the depth for
//! reasons of their own, not because of demand. The error comes from a block bootstrap (6-hour
//! blocks keep the hours' correlation), and the estimate is shrunk toward a prior of 0.5 ± 0.3 by
//! its precision: a week of noisy hours moves the prior only as far as the data can carry it.

use crate::data::Market;
use serde_json::{json, Value};

const PRIOR: f64 = 0.5;
const PRIOR_SD: f64 = 0.3;
/// A first stage weaker than this (F statistic) is not an instrument worth trusting.
const MIN_F: f64 = 10.0;
const BOOT: usize = 300;
const BLOCK_H: usize = 6;

struct Hour {
    day: i64,
    fees: f64,
    liq: f64,
    share: f64,
    moved: f64,
}

fn hours(m: &Market, t0: i64, t1: i64) -> Vec<Hour> {
    let mut out: Vec<Hour> = vec![];
    let (mut cur, mut n, mut f, mut l, mut w, mut lo, mut hi) = (i64::MIN, 0usize, 0.0, 0.0, 0.0, i64::MAX, i64::MIN);
    let mut flush = |cur: i64, n: usize, f: f64, l: f64, w: f64, lo: i64, hi: i64, out: &mut Vec<Hour>| {
        if n > 0 && f > 0.0 && l > 0.0 {
            out.push(Hour { day: cur.div_euclid(86400), fees: f, liq: l / n as f64, share: (w / l).clamp(0.0, 1.0), moved: ((hi - lo).max(0) as f64).ln_1p() });
        }
    };
    let k0 = m.ts.partition_point(|t| *t < t0);
    for k in k0..m.ts.len() {
        if m.ts[k] >= t1 {
            break;
        }
        let h = m.ts[k].div_euclid(3600) * 3600;
        if h != cur {
            flush(cur, n, f, l, w, lo, hi, &mut out);
            (cur, n, f, l, w, lo, hi) = (h, 0, 0.0, 0.0, 0.0, i64::MAX, i64::MIN);
        }
        n += 1;
        f += m.fee[k];
        l += m.l_active[k];
        w += (m.l_active[k] - m.l_others[k]).max(0.0);
        lo = lo.min(m.tick[k]);
        hi = hi.max(m.tick[k]);
    }
    flush(cur, n, f, l, w, lo, hi, &mut out);
    out
}

/// Residuals of each column of `ys` after least squares on the controls `xs` (rows × k).
fn residualize(ys: &[Vec<f64>], xs: &[Vec<f64>]) -> Option<Vec<Vec<f64>>> {
    let k = xs.first()?.len();
    // normal equations X'X b = X'y, solved by Gaussian elimination with partial pivoting
    let mut a = vec![vec![0.0; k]; k];
    for x in xs {
        for i in 0..k {
            for j in 0..k {
                a[i][j] += x[i] * x[j];
            }
        }
    }
    let solve = |b: Vec<f64>| -> Option<Vec<f64>> {
        let mut m: Vec<Vec<f64>> = a.iter().cloned().zip(b).map(|(mut r, v)| { r.push(v); r }).collect();
        for c in 0..k {
            let p = (c..k).max_by(|i, j| m[*i][c].abs().partial_cmp(&m[*j][c].abs()).unwrap())?;
            if m[p][c].abs() < 1e-12 {
                return None;
            }
            m.swap(c, p);
            for r in 0..k {
                if r != c {
                    let f = m[r][c] / m[c][c];
                    for j in c..=k {
                        m[r][j] -= f * m[c][j];
                    }
                }
            }
        }
        Some((0..k).map(|i| m[i][k] / m[i][i]).collect())
    };
    ys.iter()
        .map(|y| {
            let b = solve((0..k).map(|i| xs.iter().zip(y).map(|(x, v)| x[i] * v).sum()).collect())?;
            Some(xs.iter().zip(y).map(|(x, v)| v - x.iter().zip(&b).map(|(a, c)| a * c).sum::<f64>()).collect())
        })
        .collect()
}

/// (β, first-stage F) on the hours `idx`: instrumental variables with the wallet share when `iv`,
/// else least squares.
fn fit(hs: &[Hour], idx: &[usize], iv: bool) -> Option<(f64, f64)> {
    let days: Vec<i64> = {
        let mut d: Vec<i64> = idx.iter().map(|i| hs[*i].day).collect();
        d.sort();
        d.dedup();
        d
    };
    // controls: a constant, the hour's movement, and a dummy per day but the first
    let xs: Vec<Vec<f64>> = idx.iter().map(|i| {
        let h = &hs[*i];
        let mut r = vec![1.0, h.moved];
        r.extend(days.iter().skip(1).map(|d| if *d == h.day { 1.0 } else { 0.0 }));
        r
    }).collect();
    let v: Vec<f64> = idx.iter().map(|i| hs[*i].fees.ln()).collect();
    let l: Vec<f64> = idx.iter().map(|i| hs[*i].liq.ln()).collect();
    let z: Vec<f64> = idx.iter().map(|i| hs[*i].share).collect();
    let r = residualize(&[v, l, z], &xs)?;
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    let (rv, rl, rz) = (&r[0], &r[1], &r[2]);
    if !iv {
        let d = dot(rl, rl);
        return (d > 1e-12).then(|| (dot(rv, rl) / d, f64::NAN));
    }
    let (zz, zl) = (dot(rz, rz), dot(rz, rl));
    if zz < 1e-12 || zl.abs() < 1e-12 {
        return None;
    }
    // first stage: rl on rz
    let g = zl / zz;
    let n = rl.len() as f64;
    let dof = (n - xs[0].len() as f64 - 1.0).max(1.0);
    let s2 = rl.iter().zip(rz).map(|(a, b)| (a - g * b).powi(2)).sum::<f64>() / dof;
    let f = g * g * zz / s2.max(1e-18);
    Some((dot(rv, rz) / zl, f))
}

/// The estimate over [t1 - days, t1) of the replay, as JSON: beta (what the backtest uses),
/// raw (the data's own estimate), se, method, hours, first_stage_f, wallet_share.
pub fn estimate(m: &Market, t1: i64, days: i64) -> Value {
    let hs = hours(m, t1 - days * 86400, t1);
    let n = hs.len();
    let mean_share = if n > 0 { hs.iter().map(|h| h.share).sum::<f64>() / n as f64 } else { 0.0 };
    let var_share = if n > 0 { hs.iter().map(|h| (h.share - mean_share).powi(2)).sum::<f64>() / n as f64 } else { 0.0 };
    let prior = |why: &str| json!({"beta": PRIOR, "raw": null, "se": null, "method": "prior", "why": why, "hours": n, "wallet_share": mean_share});
    if n < 48 {
        return prior("fewer than 48 hours with swaps");
    }
    let all: Vec<usize> = (0..n).collect();
    let mut iv = mean_share > 0.01 && var_share.sqrt() > 0.01;
    let mut f_stat = f64::NAN;
    if iv {
        match fit(&hs, &all, true) {
            Some((_, f)) if f >= MIN_F => f_stat = f,
            Some((_, f)) => {
                f_stat = f;
                iv = false;
            }
            None => iv = false,
        }
    }
    let Some((raw, _)) = fit(&hs, &all, iv) else { return prior("liquidity barely changed in the window") };
    // moving-block bootstrap
    let mut seed = 0x9e37_79b9_7f4a_7c15u64 ^ n as u64;
    let mut rnd = |k: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % k as u64) as usize
    };
    let starts = n.saturating_sub(BLOCK_H) + 1;
    let mut bs = vec![];
    for _ in 0..BOOT {
        let mut idx = Vec::with_capacity(n);
        while idx.len() < n {
            let s = rnd(starts);
            idx.extend(s..(s + BLOCK_H).min(n));
        }
        idx.truncate(n);
        if let Some((b, _)) = fit(&hs, &idx, iv) {
            if b.is_finite() {
                bs.push(b);
            }
        }
    }
    if bs.len() < BOOT / 2 {
        return prior("the estimate was unstable under resampling");
    }
    let mean = bs.iter().sum::<f64>() / bs.len() as f64;
    let mut se = (bs.iter().map(|b| (b - mean).powi(2)).sum::<f64>() / (bs.len() - 1) as f64).sqrt();
    // without the instrument, liquidity following volume pushes the estimate up: count it as half
    // as informative
    if !iv {
        se *= 2.0;
    }
    let (wd, wp) = (1.0 / (se * se).max(1e-6), 1.0 / (PRIOR_SD * PRIOR_SD));
    let beta = ((raw.clamp(-1.0, 2.0) * wd + PRIOR * wp) / (wd + wp)).clamp(0.0, 1.0);
    json!({
        "beta": beta, "raw": raw, "se": se, "method": if iv { "instrument" } else { "regression" },
        "hours": n, "days": days, "first_stage_f": if f_stat.is_finite() { json!(f_stat) } else { Value::Null },
        "wallet_share": mean_share, "weight_of_data": wd / (wd + wp),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A week of swaps, an hour at a time: others' liquidity drifts, the wallet's switches in and out
    /// at random, and each hour's fees are scale × liquidity^beta × noise.
    fn market(beta: f64, wallet: bool, seed: u64) -> Market {
        let mut s = seed | 1;
        let mut r = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        let (mut ts, mut tick, mut fee, mut lo, mut la) = (vec![], vec![], vec![], vec![], vec![]);
        let mut others: f64 = 1e6;
        let mut w = 0.0;
        for h in 0..24 * 7 {
            others *= (0.08 * (r() - 0.5)).exp();
            if wallet && r() < 0.15 {
                w = if w > 0.0 { 0.0 } else { others * (0.5 + r()) };
            }
            let l = others + w;
            let hourly = 50.0 * (l / 1e6).powf(beta) * (0.6 * (r() - 0.5)).exp();
            for k in 0..6 {
                ts.push(1_700_000_000 + h * 3600 + k * 600);
                tick.push(((r() - 0.5) * 4.0) as i64);
                fee.push(hourly / 6.0);
                lo.push(others);
                la.push(l);
            }
        }
        let n = ts.len();
        Market { ts, tick, sp: vec![1.0; n], fee, fee_sig: vec![], l_others: lo, l_active: la, bar_end: vec![], bar_idx: vec![], bar_tick: vec![], bar_hi: vec![], bar_lo: vec![], bar_fee: vec![],
                 decimals: 6, px_scale: 1.0, liq_scale: 1.0, num1: true , bar_s: 300 }
    }

    #[test]
    fn the_instrument_recovers_the_elasticity() {
        for (b, seed) in [(0.2, 3), (0.8, 5)] {
            let m = market(b, true, seed);
            let e = estimate(&m, *m.ts.last().unwrap() + 1, 7);
            assert_eq!(e["method"], "instrument", "{e}");
            let raw = e["raw"].as_f64().unwrap();
            assert!((raw - b).abs() < 0.2, "{b}: {e}");
            // shrunk toward the prior, but not past it
            let beta = e["beta"].as_f64().unwrap();
            assert!((beta - 0.5).abs() <= (raw.clamp(0.0, 1.0) - 0.5).abs() + 1e-9, "{e}");
        }
    }

    /// The estimate on stored history (read only):
    /// ALM_WALLET=0x… cargo test --release elasticity::tests::stored -- --ignored --nocapture
    #[test]
    #[ignore]
    fn stored() {
        let w = std::env::var("ALM_WALLET").unwrap_or_default().to_lowercase();
        let now = crate::engine::now() as i64;
        for (cid, pool, d0, d1) in [(8453u64, "0x9ab7730b09ebd9ff2df70f06339bd289a1680a46", 6, 6), (8453, "0x74ad64bac8df96af2b63ea50c3d8840b79aa7a3b", 18, 18)] {
            let ev = crate::history::read(cid, pool, now - 31 * 86400, now + 3600);
            let apr = crate::data::load_apr(cid, pool);
            let Ok(m) = crate::data::build_market(ev, &w, &apr, d0, d1, 0) else { continue };
            for days in [7, 14, 30] {
                println!("{pool} {days}d {}", estimate(&m, *m.ts.last().unwrap() + 1, days));
            }
        }
    }

    #[test]
    fn without_a_wallet_it_regresses_and_trusts_itself_less() {
        let m = market(0.8, false, 9);
        let e = estimate(&m, *m.ts.last().unwrap() + 1, 7);
        assert_eq!(e["method"], "regression", "{e}");
        let short = market(0.8, true, 9);
        let e2 = estimate(&short, short.ts[0] + 20 * 3600, 7);
        assert_eq!(e2["method"], "prior");
        assert_eq!(e2["beta"], PRIOR);
    }
}
