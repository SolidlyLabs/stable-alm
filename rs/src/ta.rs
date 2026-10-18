//! Indicator series for node strategies (graph.rs), computed over whole bar arrays.
//!
//! Every function returns one value per bar, computed from that bar and earlier ones only (no
//! look-ahead), and NaN while it warms up or while an input is NaN. Pitfalls handled here:
//! - Warm-up: an indicator is NaN until its whole window holds real values, and a recursive
//!   smoother (EMA, Wilder) starts from the first real value instead of carrying a NaN forever.
//! - Seeding: EMA and Wilder's smoothing start from the simple average of their first `n` values
//!   (the usual definition), so early values do not lean on one bar.
//! - Flat prices: pegged pools often trade at one tick for hours, so every ratio with a range or a
//!   deviation in the denominator (stochastic, CCI, ADX, z-score, RSI) has a neutral value there
//!   instead of dividing by zero.
//! - Rounding drift: rolling sums are kept relative to a reference value and recomputed exactly
//!   every few thousand bars, so a 90-day backtest of thousands of 5-minute bars does not drift.
//! - Ticks: prices are ticks (log prices, 1 tick = 0.01%), which can be zero or negative, so a rate
//!   of change is a difference of ticks, never a ratio.
//! - Look-ahead: the Donchian channel uses the bars before the current one (a breakout above it is
//!   possible), and Ichimoku's leading spans at a bar are the ones computed `shift` bars earlier;
//!   the lagging span (the close shifted back) would look ahead and is not offered.

use std::collections::VecDeque;

pub type S = Vec<f64>;

fn nan(n: usize) -> S {
    vec![f64::NAN; n]
}

/// x shifted by `k` bars: out[i] = x[i - k].
pub fn lag(x: &[f64], k: usize) -> S {
    (0..x.len()).map(|i| if i >= k { x[i - k] } else { f64::NAN }).collect()
}

/// Rolling sum and sum of squares over the last `n` values (NaN while any of them is NaN), kept
/// relative to a reference value and recomputed exactly every `RESYNC` bars.
fn rolling(x: &[f64], n: usize, squares: bool) -> (S, S) {
    const RESYNC: usize = 4096;
    let len = x.len();
    let (mut sum, mut sq) = (nan(len), nan(len));
    let r = x.iter().copied().find(|v| v.is_finite()).unwrap_or(0.0);
    let (mut s, mut s2) = (0.0, 0.0);
    let mut last_nan: Option<usize> = None;
    for i in 0..len {
        if !x[i].is_finite() {
            last_nan = Some(i);
        }
        let d = x[i] - r;
        if x[i].is_finite() {
            s += d;
            s2 += d * d;
        }
        if i >= n && x[i - n].is_finite() {
            let o = x[i - n] - r;
            s -= o;
            s2 -= o * o;
        }
        if i % RESYNC == RESYNC - 1 {
            // exact re-sum of the window
            let from = (i + 1).saturating_sub(n);
            s = x[from..=i].iter().filter(|v| v.is_finite()).map(|v| v - r).sum();
            if squares {
                s2 = x[from..=i].iter().filter(|v| v.is_finite()).map(|v| (v - r) * (v - r)).sum();
            }
        }
        if i + 1 >= n && last_nan.map(|k| k + n <= i).unwrap_or(true) {
            sum[i] = s + r * n as f64;
            if squares {
                sq[i] = s2; // relative to r: what stdev needs
            }
        }
    }
    (sum, sq)
}

pub fn sma(x: &[f64], n: usize) -> S {
    rolling(x, n, false).0.into_iter().map(|s| s / n as f64).collect()
}

/// Population standard deviation over the last `n` values.
pub fn stdev(x: &[f64], n: usize) -> S {
    let r = x.iter().copied().find(|v| v.is_finite()).unwrap_or(0.0);
    let (sum, sq) = rolling(x, n, true);
    sum.iter()
        .zip(&sq)
        .map(|(s, s2)| {
            let m = s / n as f64 - r;
            (s2 / n as f64 - m * m).max(0.0).sqrt()
        })
        .collect()
}

/// Exponential average, alpha = 2/(n+1), seeded with the simple average of its first `n` values.
pub fn ema(x: &[f64], n: usize) -> S {
    smooth(x, n, 2.0 / (n as f64 + 1.0))
}

/// Wilder's smoothing (alpha = 1/n), as in RSI, ATR and ADX, seeded with the simple average.
pub fn wilder(x: &[f64], n: usize) -> S {
    smooth(x, n, 1.0 / n as f64)
}

fn smooth(x: &[f64], n: usize, a: f64) -> S {
    let mut out = nan(x.len());
    let (mut e, mut seed, mut cnt) = (f64::NAN, 0.0, 0usize);
    for i in 0..x.len() {
        if !x[i].is_finite() {
            if e.is_finite() {
                out[i] = e; // a gap in the input holds the last value
            }
            continue;
        }
        if cnt < n {
            seed += x[i];
            cnt += 1;
            if cnt == n {
                e = seed / n as f64;
                out[i] = e;
            }
            continue;
        }
        e = a * x[i] + (1.0 - a) * e;
        out[i] = e;
    }
    out
}

/// Rolling max (or min) over the last `n` values; NaN while the window holds a NaN.
pub fn extreme(x: &[f64], n: usize, max: bool) -> S {
    let mut out = nan(x.len());
    let mut dq: VecDeque<usize> = VecDeque::new();
    let mut last_nan: Option<usize> = None;
    for i in 0..x.len() {
        if !x[i].is_finite() {
            last_nan = Some(i);
            continue;
        }
        while let Some(&j) = dq.back() {
            if (max && x[j] <= x[i]) || (!max && x[j] >= x[i]) {
                dq.pop_back();
            } else {
                break;
            }
        }
        dq.push_back(i);
        while dq.front().map(|j| j + n <= i).unwrap_or(false) {
            dq.pop_front();
        }
        if i + 1 >= n && last_nan.map(|k| k + n <= i).unwrap_or(true) {
            out[i] = x[*dq.front().unwrap()];
        }
    }
    out
}

/// Rolling quantile q (0..1, linear between neighbours) over the last `n` values.
pub fn quantile(x: &[f64], n: usize, q: f64) -> S {
    let mut out = nan(x.len());
    let mut w: Vec<f64> = Vec::with_capacity(n + 1);
    let mut last_nan: Option<usize> = None;
    for i in 0..x.len() {
        if x[i].is_finite() {
            let p = w.partition_point(|v| *v < x[i]);
            w.insert(p, x[i]);
        } else {
            last_nan = Some(i);
        }
        if i >= n && x[i - n].is_finite() {
            let p = w.partition_point(|v| *v < x[i - n]);
            w.remove(p);
        }
        if i + 1 >= n && last_nan.map(|k| k + n <= i).unwrap_or(true) && !w.is_empty() {
            let pos = q.clamp(0.0, 1.0) * (w.len() - 1) as f64;
            let (a, f) = (pos.floor() as usize, pos.fract());
            out[i] = if a + 1 < w.len() { w[a] + (w[a + 1] - w[a]) * f } else { w[a] };
        }
    }
    out
}

/// True range: the bar's high-low widened to the previous close (NaN on the first bar).
pub fn true_range(c: &[f64], h: &[f64], l: &[f64]) -> S {
    (0..c.len()).map(|i| if i == 0 { f64::NAN } else { (h[i] - l[i]).max((h[i] - c[i - 1]).abs()).max((l[i] - c[i - 1]).abs()) }).collect()
}

/// Average true range, Wilder-smoothed.
pub fn atr(c: &[f64], h: &[f64], l: &[f64], n: usize) -> S {
    wilder(&true_range(c, h, l), n)
}

/// RSI (Wilder). 50 while the price did not move at all in the window.
pub fn rsi(x: &[f64], n: usize) -> S {
    let d: S = (0..x.len()).map(|i| if i == 0 { f64::NAN } else { x[i] - x[i - 1] }).collect();
    let up = wilder(&d.iter().map(|v| if v.is_finite() { v.max(0.0) } else { f64::NAN }).collect::<S>(), n);
    let dn = wilder(&d.iter().map(|v| if v.is_finite() { (-v).max(0.0) } else { f64::NAN }).collect::<S>(), n);
    up.iter()
        .zip(&dn)
        .map(|(u, w)| {
            if !u.is_finite() || !w.is_finite() {
                f64::NAN
            } else if *w == 0.0 {
                if *u == 0.0 { 50.0 } else { 100.0 }
            } else {
                100.0 - 100.0 / (1.0 + u / w)
            }
        })
        .collect()
}

/// Stochastic oscillator of `x` within its own high/low (0..100): %K over `n`, %D = SMA of %K.
/// 50 while the window is flat.
pub fn stoch_of(x: &[f64], h: &[f64], l: &[f64], n: usize) -> S {
    let (hh, ll) = (extreme(h, n, true), extreme(l, n, false));
    (0..x.len())
        .map(|i| {
            if !hh[i].is_finite() || !ll[i].is_finite() || !x[i].is_finite() {
                f64::NAN
            } else if hh[i] - ll[i] <= 0.0 {
                50.0
            } else {
                100.0 * (x[i] - ll[i]) / (hh[i] - ll[i])
            }
        })
        .collect()
}

/// Commodity channel index on the typical price (h+l+c)/3; 0 while the window is flat.
pub fn cci(c: &[f64], h: &[f64], l: &[f64], n: usize) -> S {
    let tp: S = (0..c.len()).map(|i| (h[i] + l[i] + c[i]) / 3.0).collect();
    let m = sma(&tp, n);
    (0..tp.len())
        .map(|i| {
            if !m[i].is_finite() {
                return f64::NAN;
            }
            let md = tp[i + 1 - n..=i].iter().map(|v| (v - m[i]).abs()).sum::<f64>() / n as f64;
            if md <= 0.0 { 0.0 } else { (tp[i] - m[i]) / (0.015 * md) }
        })
        .collect()
}

/// ADX with +DI and -DI (Wilder): (plus_di, minus_di, adx). 0 while the price does not move.
pub fn adx(c: &[f64], h: &[f64], l: &[f64], n: usize) -> (S, S, S) {
    let len = c.len();
    let mut pdm = nan(len);
    let mut mdm = nan(len);
    for i in 1..len {
        let (up, dn) = (h[i] - h[i - 1], l[i - 1] - l[i]);
        pdm[i] = if up > dn && up > 0.0 { up } else { 0.0 };
        mdm[i] = if dn > up && dn > 0.0 { dn } else { 0.0 };
    }
    let (tr, p, m) = (wilder(&true_range(c, h, l), n), wilder(&pdm, n), wilder(&mdm, n));
    let di = |x: f64, t: f64| if !x.is_finite() || !t.is_finite() { f64::NAN } else if t <= 0.0 { 0.0 } else { 100.0 * x / t };
    let pdi: S = (0..len).map(|i| di(p[i], tr[i])).collect();
    let mdi: S = (0..len).map(|i| di(m[i], tr[i])).collect();
    let dx: S = (0..len)
        .map(|i| {
            let s = pdi[i] + mdi[i];
            if !s.is_finite() { f64::NAN } else if s <= 0.0 { 0.0 } else { 100.0 * (pdi[i] - mdi[i]).abs() / s }
        })
        .collect();
    (pdi, mdi, wilder(&dx, n))
}

/// MACD: (line = EMA fast - EMA slow, signal = EMA of the line, histogram = line - signal).
pub fn macd(x: &[f64], fast: usize, slow: usize, signal: usize) -> (S, S, S) {
    let (f, s) = (ema(x, fast), ema(x, slow));
    let line: S = f.iter().zip(&s).map(|(a, b)| a - b).collect();
    let sig = ema(&line, signal);
    let hist = line.iter().zip(&sig).map(|(a, b)| a - b).collect();
    (line, sig, hist)
}

/// TRIX on ticks: the change per bar of a triple EMA, in ticks (ticks are log prices, so this is
/// the rate of change; dividing by the previous value, as for prices, would be wrong).
pub fn trix(x: &[f64], n: usize) -> S {
    let e3 = ema(&ema(&ema(x, n), n), n);
    (0..e3.len()).map(|i| if i == 0 { f64::NAN } else { e3[i] - e3[i - 1] }).collect()
}

/// Parabolic SAR from the bar highs and lows.
pub fn psar(c: &[f64], h: &[f64], l: &[f64], af0: f64, step: f64, max_af: f64) -> S {
    let len = c.len();
    let mut out = nan(len);
    if len < 2 {
        return out;
    }
    let mut up = c[1] >= c[0];
    let mut sar = if up { l[0] } else { h[0] };
    let mut ep = if up { h[1] } else { l[1] };
    let mut af = af0;
    for i in 1..len {
        sar += af * (ep - sar);
        if up {
            sar = sar.min(l[i - 1]).min(if i >= 2 { l[i - 2] } else { l[i - 1] });
            if l[i] < sar {
                (up, sar, ep, af) = (false, ep, l[i], af0);
            } else if h[i] > ep {
                ep = h[i];
                af = (af + step).min(max_af);
            }
        } else {
            sar = sar.max(h[i - 1]).max(if i >= 2 { h[i - 2] } else { h[i - 1] });
            if h[i] > sar {
                (up, sar, ep, af) = (true, ep, h[i], af0);
            } else if l[i] < ep {
                ep = l[i];
                af = (af + step).min(max_af);
            }
        }
        out[i] = sar;
    }
    out
}

/// Ichimoku as seen at each bar: (tenkan, kijun, span_a, span_b). The spans are the cloud drawn at
/// this bar, i.e. computed `shift` bars earlier.
pub fn ichimoku(h: &[f64], l: &[f64], tenkan: usize, kijun: usize, span_b: usize, shift: usize) -> (S, S, S, S) {
    let mid = |n: usize| -> S { extreme(h, n, true).iter().zip(&extreme(l, n, false)).map(|(a, b)| (a + b) / 2.0).collect() };
    let (t, k, b) = (mid(tenkan), mid(kijun), mid(span_b));
    let a: S = t.iter().zip(&k).map(|(x, y)| (x + y) / 2.0).collect();
    (t, k, lag(&a, shift), lag(&b, shift))
}

/// Heikin-Ashi bars: (open, high, low, close). A bar's open is the previous close (5-minute bars
/// are built from swaps and have no open of their own).
pub fn heikin_ashi(c: &[f64], h: &[f64], l: &[f64]) -> (S, S, S, S) {
    let len = c.len();
    let (mut o, mut hh, mut ll, mut cc) = (nan(len), nan(len), nan(len), nan(len));
    for i in 0..len {
        let open = if i == 0 { c[0] } else { c[i - 1] };
        cc[i] = (open + h[i] + l[i] + c[i]) / 4.0;
        o[i] = if i == 0 { (open + c[0]) / 2.0 } else { (o[i - 1] + cc[i - 1]) / 2.0 };
        hh[i] = h[i].max(o[i]).max(cc[i]);
        ll[i] = l[i].min(o[i]).min(cc[i]);
    }
    (o, hh, ll, cc)
}

/// Gaussian channel: the centre is four chained SMAs (close to a Gaussian filter), the band is
/// k population deviations of the last `n` values around that centre. (upper, mid, lower)
pub fn gaussian(x: &[f64], n: usize, k: f64) -> (S, S, S) {
    let c = sma(&sma(&sma(&sma(x, n), n), n), n);
    let sd: S = (0..x.len())
        .map(|i| {
            if !c[i].is_finite() || i + 1 < n {
                return f64::NAN;
            }
            (x[i + 1 - n..=i].iter().map(|v| (v - c[i]) * (v - c[i])).sum::<f64>() / n as f64).sqrt()
        })
        .collect();
    let up = c.iter().zip(&sd).map(|(m, d)| m + k * d).collect();
    let lo = c.iter().zip(&sd).map(|(m, d)| m - k * d).collect();
    (up, c, lo)
}

/// (x - SMA) / standard deviation over `n`; 0 while the window is flat.
pub fn zscore(x: &[f64], n: usize) -> S {
    let (m, sd) = (sma(x, n), stdev(x, n));
    (0..x.len()).map(|i| if !m[i].is_finite() || !sd[i].is_finite() { f64::NAN } else if sd[i] <= 1e-9 { 0.0 } else { (x[i] - m[i]) / sd[i] }).collect()
}

/// A trend score from -100 (down) to +100 (up), every period counted in units of `u` bars:
/// EMA 20 vs EMA 50 (±25), MACD 12/26/9 histogram sign (±20) and +DI vs -DI (±20), weighted by
/// ADX 14 / 25 (at most 2), plus (RSI 14 - 50) / 50 × 15.
pub fn trend(c: &[f64], h: &[f64], l: &[f64], u: usize) -> S {
    let (f, s) = (ema(c, 20 * u), ema(c, 50 * u));
    let (_, _, hist) = macd(c, 12 * u, 26 * u, 9 * u);
    let (pdi, mdi, ax) = adx(c, h, l, 14 * u);
    let r = rsi(c, 14 * u);
    let sign = |v: f64| if v > 0.0 { 1.0 } else if v < 0.0 { -1.0 } else { 0.0 };
    (0..c.len())
        .map(|i| {
            let parts = [f[i], s[i], hist[i], pdi[i], mdi[i], ax[i], r[i]];
            if parts.iter().any(|v| !v.is_finite()) {
                return f64::NAN;
            }
            let base = 25.0 * sign(f[i] - s[i]) + 20.0 * sign(hist[i]) + 20.0 * sign(pdi[i] - mdi[i]);
            (base * (ax[i] / 25.0).min(2.0) + (r[i] - 50.0) / 50.0 * 15.0).clamp(-100.0, 100.0)
        })
        .collect()
}

/// Momentum: how far the typical price is from its previous `look`-bar average, as a z-score over
/// `window` bars (sample deviation), averaged over `avg` bars. Above 0.25 is upward momentum.
pub fn momentum(c: &[f64], h: &[f64], l: &[f64], look: usize, window: usize, avg: usize) -> S {
    let tp: S = (0..c.len()).map(|i| (h[i] + l[i] + c[i]) / 3.0).collect();
    let prev = lag(&sma(&tp, look), 1);
    let dev: S = tp.iter().zip(&prev).map(|(a, b)| a - b).collect();
    let m = sma(&dev, window);
    let sd = stdev(&dev, window);
    let n = window as f64;
    let z: S = (0..dev.len())
        .map(|i| {
            if !m[i].is_finite() || !sd[i].is_finite() || window < 2 {
                return f64::NAN;
            }
            let s = sd[i] * (n / (n - 1.0)).sqrt(); // sample deviation
            if s <= 1e-9 { 0.0 } else { (dev[i] - m[i]) / s }
        })
        .collect();
    sma(&z, avg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9 * (1.0 + a.abs().max(b.abs()))
    }

    #[test]
    fn rolling_matches_naive_and_does_not_drift() {
        // large tick values with small moves: where plain running sums of squares lose precision
        let x: S = (0..20_000).map(|i| -197_000.0 + ((i * 7919) % 101) as f64 * 0.37).collect();
        let n = 288;
        let (m, sd) = (sma(&x, n), stdev(&x, n));
        for i in [n - 1, 5000, 12_345, 19_999] {
            let w = &x[i + 1 - n..=i];
            let mean = w.iter().sum::<f64>() / n as f64;
            let var = w.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n as f64;
            assert!(close(m[i], mean), "sma at {i}");
            assert!((sd[i] - var.sqrt()).abs() < 1e-6, "stdev at {i}: {} vs {}", sd[i], var.sqrt());
        }
        assert!(m[n - 2].is_nan());
    }

    #[test]
    fn ema_and_wilder_seed_with_the_average() {
        let x = [1.0, 2.0, 3.0, 4.0, 5.0];
        let e = ema(&x, 3);
        assert!(e[1].is_nan());
        assert!(close(e[2], 2.0));
        assert!(close(e[3], 0.5 * 4.0 + 0.5 * 2.0));
        let w = wilder(&[f64::NAN, 3.0, 3.0, 6.0], 2);
        assert!(close(w[2], 3.0) && close(w[3], 4.5));
    }

    #[test]
    fn flat_prices_are_neutral_not_nan() {
        let x = vec![100.0; 400];
        assert_eq!(rsi(&x, 14)[399], 50.0);
        assert_eq!(stoch_of(&x, &x, &x, 14)[399], 50.0);
        assert_eq!(cci(&x, &x, &x, 20)[399], 0.0);
        let (p, m, a) = adx(&x, &x, &x, 14);
        assert_eq!((p[399], m[399], a[399]), (0.0, 0.0, 0.0));
        assert_eq!(zscore(&x, 20)[399], 0.0);
        assert_eq!(momentum(&x, &x, &x, 5, 100, 14)[399], 0.0);
        assert_eq!(trix(&x, 10)[399], 0.0);
        assert_eq!(atr(&x, &x, &x, 14)[399], 0.0);
    }

    #[test]
    fn nan_inputs_warm_up_instead_of_poisoning() {
        let mut x: S = (0..100).map(|i| i as f64).collect();
        for v in x.iter_mut().take(10) {
            *v = f64::NAN;
        }
        let r = rsi(&x, 5);
        assert!(r[14].is_nan() && r[16].is_finite() && r[99] == 100.0);
        assert!(sma(&x, 5)[14].is_finite() && sma(&x, 5)[13].is_nan());
        assert!(extreme(&x, 5, true)[14] == 14.0 && extreme(&x, 5, true)[13].is_nan());
    }

    #[test]
    fn known_values() {
        let x: S = (1..=10).map(|v| v as f64).collect();
        let q = quantile(&x, 5, 0.5);
        assert_eq!(q[4], 3.0);
        assert_eq!(q[9], 8.0);
        assert_eq!(quantile(&x, 4, 0.25)[9], 7.75);
        let (line, sig, hist) = macd(&x, 2, 4, 2);
        assert!(line[3].is_finite() && sig[4].is_finite() && close(hist[4], line[4] - sig[4]));
        // a steady rise: trend and RSI up, %K at the top
        let up: S = (0..2000).map(|i| i as f64).collect();
        assert_eq!(rsi(&up, 14)[1999], 100.0);
        assert!(trend(&up, &up, &up, 1)[1999] > 50.0);
        assert!(close(stoch_of(&up, &up, &up, 14)[1999], 100.0));
        let (pdi, mdi, _) = adx(&up, &up.iter().map(|v| v + 1.0).collect::<S>(), &up.iter().map(|v| v - 1.0).collect::<S>(), 14);
        assert!(pdi[1999] > mdi[1999]);
        // ichimoku spans are shifted forward: no value before `shift` + the span's own window
        let (_, _, sa, sb) = ichimoku(&up, &up, 9, 26, 52, 26);
        assert!(sa[26 + 25].is_finite() && sa[26 + 24].is_nan() && sb[26 + 51].is_finite() && sb[26 + 50].is_nan());
        // heikin-ashi close is the bar's average
        let (_, _, _, hc) = heikin_ashi(&[1.0, 3.0], &[1.0, 4.0], &[1.0, 2.0]);
        assert!(close(hc[1], (1.0 + 4.0 + 2.0 + 3.0) / 4.0));
        // psar sits below a rising price
        let s = psar(&up, &up, &up, 0.02, 0.02, 0.2);
        assert!(s[1999] < up[1999]);
    }
}
