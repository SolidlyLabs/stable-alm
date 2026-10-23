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

/// Parkinson range volatility per bar, in ticks: sqrt(mean((high-low)^2) / (4 ln 2)).
/// Tick differences already are log-price differences (up to the constant ln(1.0001)), so the
/// output is a log-price standard deviation in ticks, not a percentage or an annualized value.
/// Requires `n` finite, ordered high/low pairs. It does not measure gaps between bars.
pub fn parkinson(h: &[f64], l: &[f64], n: usize) -> S {
    if n == 0 || n > h.len() || h.len() != l.len() {
        return nan(h.len());
    }
    let ranges: S = h.iter().zip(l).map(|(&hi, &lo)| {
        if hi.is_finite() && lo.is_finite() && hi >= lo {
            let range = hi - lo;
            let square = range * range;
            if square.is_finite() { square } else { f64::NAN }
        } else {
            f64::NAN
        }
    }).collect();
    sma(&ranges, n).into_iter().map(|v| {
        if v.is_finite() { (v.max(0.0) / (4.0 * std::f64::consts::LN_2)).sqrt() } else { f64::NAN }
    }).collect()
}

/// Kaufman's efficiency ratio: absolute net move divided by the sum of absolute moves over
/// `n` changes (`n+1` consecutive finite values). 0 for a flat window, 1 for a monotone move.
pub fn efficiency_ratio(x: &[f64], n: usize) -> S {
    let mut out = nan(x.len());
    if n == 0 || n >= x.len() {
        return out;
    }
    let changes: S = (0..x.len()).map(|i| {
        if i > 0 && x[i].is_finite() && x[i - 1].is_finite() {
            (x[i] - x[i - 1]).abs()
        } else {
            f64::NAN
        }
    }).collect();
    let path = rolling(&changes, n, false).0;
    for i in n..x.len() {
        if path[i].is_finite() && x[i].is_finite() && x[i - n].is_finite() {
            out[i] = if path[i] <= 0.0 { 0.0 } else { ((x[i] - x[i - n]).abs() / path[i]).clamp(0.0, 1.0) };
        }
    }
    out
}

/// Kaufman's adaptive moving average. The smoothing constant is
/// (ER(n) * (2/(fast+1) - 2/(slow+1)) + 2/(slow+1))^2.
/// Seeds from the mean of `n+1` consecutive values; a nonfinite input resets the state and
/// requires a fresh seed. `fast` and `slow` are smoothing periods, not extra warm-up windows.
pub fn kama(x: &[f64], n: usize, fast: usize, slow: usize) -> S {
    let mut out = nan(x.len());
    if n == 0 || n >= x.len() || fast == 0 || slow == 0 || fast > slow {
        return out;
    }
    let er = efficiency_ratio(x, n);
    let seeds = sma(x, n + 1);
    let (a_fast, a_slow) = (2.0 / (fast as f64 + 1.0), 2.0 / (slow as f64 + 1.0));
    let mut value = f64::NAN;
    for i in 0..x.len() {
        if !er[i].is_finite() || !seeds[i].is_finite() {
            value = f64::NAN;
            continue;
        }
        if value.is_finite() {
            let a = (er[i] * (a_fast - a_slow) + a_slow).powi(2);
            value += a * (x[i] - value);
        } else {
            value = seeds[i];
        }
        out[i] = value;
    }
    out
}

/// Rolling least-squares regression on bar positions 0..n-1: (slope per bar, R squared).
/// Uses centered rolling moments, with an exact resync every max(n, 4096) bars, so total work
/// is O(number of bars), including large windows. Both outputs are 0 for a flat window.
/// Requires n >= 2 consecutive finite values and resets after a nonfinite input.
pub fn linreg(x: &[f64], n: usize) -> (S, S) {
    let (mut slope, mut r2) = (nan(x.len()), nan(x.len()));
    if n < 2 || n > x.len() {
        return (slope, r2);
    }
    let nf = n as f64;
    let centre = (nf - 1.0) / 2.0;
    let xx = nf * (nf * nf - 1.0) / 12.0;
    let resync = n.max(4096);
    // Keep the mean relative to a nearby tick reference. Updating an absolute mean at large
    // negative tick levels loses low bits each bar; the covariance recurrence would accumulate
    // those tiny errors into slope drift, especially for short windows.
    let (mut count, mut reference, mut mean, mut yy, mut xy) = (0usize, 0.0, 0.0, 0.0, 0.0);
    let mut last_change: Option<usize> = None;
    for i in 0..x.len() {
        if !x[i].is_finite() {
            (count, reference, mean, yy, xy) = (0, 0.0, 0.0, 0.0, 0.0);
            last_change = None;
            continue;
        }
        if count == 0 {
            reference = x[i];
            mean = 0.0;
        } else if x[i] != x[i - 1] {
            last_change = Some(i);
        }
        let value = x[i] - reference;
        if count < n {
            count += 1;
            let k = count as f64;
            let d = value - mean;
            mean += d / k;
            yy += d * (value - mean);
            xy += k / 2.0 * (value - mean);
        } else {
            let old = x[i - n] - reference;
            let old_mean = mean;
            let d = value - old;
            mean += d / nf;
            yy += d * ((value - mean) + (old - old_mean));
            xy += centre * d + nf * (old - old_mean);
        }
        if count < n {
            continue;
        }
        if last_change.map(|j| j <= i + 1 - n).unwrap_or(true) {
            // An exactly flat window should stay exactly neutral, even after a moving window.
            (reference, mean, yy, xy) = (x[i], 0.0, 0.0, 0.0);
            (slope[i], r2[i]) = (0.0, 0.0);
            continue;
        }
        if i % resync == resync - 1 || yy <= 0.0 || !yy.is_finite() || !xy.is_finite() {
            let w = &x[i + 1 - n..=i];
            reference = w[0];
            mean = w.iter().map(|v| v - reference).sum::<f64>() / nf;
            yy = w.iter().map(|v| (v - reference - mean).powi(2)).sum();
            xy = w.iter().enumerate().map(|(j, v)| (j as f64 - centre) * (v - reference - mean)).sum();
        }
        if yy.is_finite() && yy > 0.0 && xy.is_finite() {
            slope[i] = xy / xx;
            r2[i] = (xy * xy / (xx * yy)).clamp(0.0, 1.0);
        }
    }
    (slope, r2)
}

/// Distance below the rolling high, in the input's tick units (positive drawdown).
/// Includes the current bar and requires `n` consecutive finite values. For ticks, the
/// corresponding price drawdown fraction is 1 - exp(-drawdown * ln(1.0001)).
pub fn drawdown(x: &[f64], n: usize) -> S {
    if n == 0 || n > x.len() {
        return nan(x.len());
    }
    extreme(x, n, true).iter().zip(x).map(|(&high, &value)| {
        if high.is_finite() && value.is_finite() { (high - value).max(0.0) } else { f64::NAN }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9 * (1.0 + a.abs().max(b.abs()))
    }

    fn assert_causal(f: impl Fn(&[f64]) -> S) {
        let mut x: S = (0..100).map(|i| -197_000.0 + ((i * 7919) % 101) as f64 * 0.37).collect();
        x[0] = f64::NAN;
        x[19] = f64::NAN;
        x[53] = f64::INFINITY;
        let whole = f(&x);
        for end in [0, 1, 3, 5, 19, 20, 24, 53, 54, 59, 75] {
            let prefix = f(&x[..end]);
            assert_eq!(prefix.len(), end);
            for i in 0..end {
                assert!(prefix[i].is_nan() && whole[i].is_nan() || prefix[i] == whole[i], "appending future bars changed value {i} at prefix {end}: {} vs {}", prefix[i], whole[i]);
            }
        }
    }

    #[test]
    fn parkinson_reference_warmup_and_invalid_ranges() {
        // Ranges 2 and 4: mean squared log-tick range is 10.
        let h = [-8.0, -6.0, -7.0, -5.0];
        let l = [-10.0, -10.0, -10.0, -10.0];
        let v = parkinson(&h, &l, 2);
        assert!(v[0].is_nan());
        assert!(close(v[1], (10.0 / (4.0 * std::f64::consts::LN_2)).sqrt()));
        assert!(close(v[2], (12.5 / (4.0 * std::f64::consts::LN_2)).sqrt()));
        assert_eq!(parkinson(&h, &h, 2)[3], 0.0);
        let g = parkinson(&[3.0, 1.0, 3.0, 3.0, f64::NAN, 3.0, 3.0], &[2.0; 7], 2);
        assert!(g[1].is_nan() && g[2].is_nan() && g[3].is_finite());
        assert!(g[4].is_nan() && g[5].is_nan() && g[6].is_finite());
        assert!(parkinson(&h, &l, 0).iter().all(|v| v.is_nan()));
        assert!(parkinson(&h, &l, usize::MAX).iter().all(|v| v.is_nan()));
        assert!(parkinson(&h, &l[..2], 2).iter().all(|v| v.is_nan()));
        assert!(parkinson(&[], &[], 1).is_empty());
    }

    #[test]
    fn parkinson_has_no_lookahead() {
        assert_causal(|x| {
            let h: S = x.iter().enumerate().map(|(i, v)| v + (i % 5) as f64).collect();
            parkinson(&h, x, 4)
        });
    }

    #[test]
    fn efficiency_ratio_reference_warmup_flat_and_gaps() {
        // 1 -> 3 -> 2 -> 4: net 3, path 2+1+2=5.
        let e = efficiency_ratio(&[1.0, 3.0, 2.0, 4.0, 5.0], 3);
        assert!(e[..3].iter().all(|v| v.is_nan()));
        assert!(close(e[3], 3.0 / 5.0));
        assert!(close(e[4], 2.0 / 4.0));
        assert_eq!(efficiency_ratio(&[-3.0, -2.0, -1.0, 0.0], 3)[3], 1.0);
        assert_eq!(efficiency_ratio(&[3.0, 2.0, 1.0, 0.0], 3)[3], 1.0);
        assert_eq!(efficiency_ratio(&[7.0; 4], 3)[3], 0.0);
        let g = efficiency_ratio(&[1.0, 2.0, 3.0, f64::NAN, 4.0, 5.0, 6.0], 2);
        assert_eq!(g[2], 1.0);
        assert!(g[3..6].iter().all(|v| v.is_nan()));
        assert_eq!(g[6], 1.0);
        for n in [0, 4, usize::MAX] {
            assert!(efficiency_ratio(&[1.0; 4], n).iter().all(|v| v.is_nan()));
        }
        assert!(efficiency_ratio(&[], 1).is_empty());
    }

    #[test]
    fn efficiency_ratio_has_no_lookahead() {
        assert_causal(|x| efficiency_ratio(x, 4));
    }

    #[test]
    fn kama_reference_seed_adaptation_and_gap_reset() {
        let k = kama(&[1.0, 2.0, 3.0, 4.0, 3.0, 3.0], 2, 2, 4);
        assert!(k[0].is_nan() && k[1].is_nan());
        assert_eq!(k[2], 2.0); // mean of the first n+1 values
        assert!(close(k[3], 2.0 + 4.0 / 9.0 * (4.0 - 2.0))); // ER=1, fast alpha squared
        assert!(close(k[4], k[3] + 4.0 / 25.0 * (3.0 - k[3]))); // ER=0, slow alpha squared
        assert!(close(k[5], k[4] + 4.0 / 9.0 * (3.0 - k[4]))); // ER=1 again
        assert_eq!(kama(&[-7.0; 10], 2, 2, 30)[9], -7.0);
        let g = kama(&[1.0, 2.0, 3.0, 4.0, f64::NAN, 10.0, 11.0, 12.0, 13.0], 2, 2, 4);
        assert!(g[4..7].iter().all(|v| v.is_nan()));
        assert_eq!(g[7], 11.0); // old state is discarded, not held across missing observations
        assert!(close(g[8], 11.0 + 4.0 / 9.0 * 2.0));
        for (n, fast, slow) in [(0, 2, 30), (10, 2, 30), (usize::MAX, 2, 30), (2, 0, 30), (2, 2, 0), (2, 30, 2)] {
            assert!(kama(&[1.0; 10], n, fast, slow).iter().all(|v| v.is_nan()));
        }
        assert!(kama(&[], 2, 2, 30).is_empty());
    }

    #[test]
    fn kama_has_no_lookahead() {
        assert_causal(|x| kama(x, 4, 2, 30));
    }

    #[test]
    fn linreg_reference_warmup_flat_and_gap_reset() {
        let (s, r) = linreg(&[1.0, 2.0, 4.0, 6.0], 3);
        assert!(s[..2].iter().chain(&r[..2]).all(|v| v.is_nan()));
        assert!(close(s[2], 1.5));
        assert!(close(r[2], 27.0 / 28.0)); // cov=3, Sxx=2, Syy=14/3
        assert!(close(s[3], 2.0) && close(r[3], 1.0));
        let (s, r) = linreg(&[-1.0, -3.0, -5.0, -5.0, -5.0, f64::INFINITY, 4.0, 5.0, 6.0], 3);
        assert_eq!((s[2], r[2]), (-2.0, 1.0));
        assert_eq!((s[4], r[4]), (0.0, 0.0));
        assert!(s[5..8].iter().chain(&r[5..8]).all(|v| v.is_nan()));
        assert_eq!((s[8], r[8]), (1.0, 1.0));
        for n in [0, 1, 5, usize::MAX] {
            let (s, r) = linreg(&[1.0; 4], n);
            assert!(s.iter().chain(&r).all(|v| v.is_nan()));
        }
        assert_eq!(linreg(&[], 2), (vec![], vec![]));
    }

    #[test]
    fn linreg_rolling_moments_match_direct_reference() {
        let x: S = (0..20_000).map(|i| -197_000.0 + ((i * 7919) % 101) as f64 * 0.37 + i as f64 * 0.01).collect();
        for n in [2, 17, 288, 5000] {
            let (s, r) = linreg(&x, n);
            for i in [n - 1, 5000, 12_345, 19_999] {
                let w = &x[i + 1 - n..=i];
                let mean = w.iter().sum::<f64>() / n as f64;
                let centre = (n - 1) as f64 / 2.0;
                let xy: f64 = w.iter().enumerate().map(|(j, v)| (j as f64 - centre) * (v - mean)).sum();
                let xx: f64 = (0..n).map(|j| (j as f64 - centre).powi(2)).sum();
                let yy: f64 = w.iter().map(|v| (v - mean).powi(2)).sum();
                assert!((s[i] - xy / xx).abs() < 1e-6, "slope n={n} i={i}: {} vs {}", s[i], xy / xx);
                assert!((r[i] - xy * xy / (xx * yy)).abs() < 1e-6, "r2 n={n} i={i}: {} vs {}", r[i], xy * xy / (xx * yy));
            }
        }
    }

    #[test]
    fn linreg_has_no_lookahead() {
        assert_causal(|x| linreg(x, 4).0);
        assert_causal(|x| linreg(x, 4).1);
    }

    #[test]
    fn drawdown_reference_warmup_expiry_and_gaps() {
        let d = drawdown(&[-10.0, -6.0, -9.0, -11.0, -12.0, -5.0], 3);
        assert!(d[..2].iter().all(|v| v.is_nan()));
        assert_eq!(&d[2..], &[3.0, 5.0, 3.0, 0.0]);
        assert_eq!(drawdown(&[7.0; 4], 3)[3], 0.0);
        assert_eq!(drawdown(&[-7.0; 4], 1), vec![0.0; 4]);
        let g = drawdown(&[3.0, 2.0, f64::NAN, 4.0, 2.0], 2);
        assert_eq!(g[1], 1.0);
        assert!(g[2].is_nan() && g[3].is_nan());
        assert_eq!(g[4], 2.0);
        for n in [0, 5, usize::MAX] {
            assert!(drawdown(&[1.0; 4], n).iter().all(|v| v.is_nan()));
        }
        assert!(drawdown(&[], 1).is_empty());
    }

    #[test]
    fn drawdown_has_no_lookahead() {
        assert_causal(|x| drawdown(x, 4));
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
