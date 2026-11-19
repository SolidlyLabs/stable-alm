//! Perp portfolio strategies: several coins' perps, long or short, sized daily by a trend rule.
//! The first one, "Trend perps v3", is built from a public Hyperliquid BTC vault's rule (research notes,
//! kept out of the repo):
//!
//! per coin: forecast = mean over speeds (fast, slow) of  cap·tanh((s/σ)/cap) · v^vol_exp / σ²^(sig_exp/2),
//! each speed scaled to median |x| = 1 over the calibration span, s = ln(EMA_fast/EMA_slow) of hourly
//! closes, σ² = 8760·EWMA_vol_span(r²), v = 24 h quote volume / its 30-day mean; times exp(Σ β·z(tilt))
//! for the tilts (Coinbase premium, funding, Bybit long/short crowding), z-scored on the calibration span.
//! Each sleeve trades once a day at the close of the hour ending `trade_hour_utc`, and is scaled so its
//! calibration-span vol is `sleeve_vol`. The portfolio sums the sleeves by weight, scales by
//! target_vol / trailing `overlay_days` realised vol (capped at `lev_cap`), then by one factor so the
//! calibration-span vol is `target_vol`.
//!
//! The engine follows the research code's (pandas) conventions so that the two agree day by day.
use crate::marketdata::{self, HOUR};
use serde_json::{json, Map, Value};

const DAY: i64 = 86400;

#[derive(Clone, Debug)]
pub struct Tilt {
    pub kind: String, // coinbase_premium | funding | crowding
    pub coin: String, // for coinbase_premium: whose Coinbase price (BTC)
    pub beta: f64,
}

#[derive(Clone, Debug)]
pub struct Signal {
    pub speeds: Vec<(f64, f64)>,
    pub cap: f64,
    pub vol_exp: f64,
    pub sig_exp: f64,
    pub vol_span: f64,
    pub vwin: usize,
    pub vavg_days: usize,
    /// applied in order; each group is one exp(Σ β z) step, z signed by the forecast so far
    pub tilts: Vec<Vec<Tilt>>,
}

#[derive(Clone, Debug)]
pub struct Sleeve {
    pub coin: String,
    pub weight: f64,
    pub signal: Signal,
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub name: String,
    pub sleeves: Vec<Sleeve>,
    pub target_vol: f64,
    pub sleeve_vol: f64,
    pub overlay_days: usize,
    pub lev_cap: f64,
    pub trade_hour_utc: i64,
    pub cal_from: i64,
    pub cal_to: i64,
    pub taker_bps: f64,
    pub data_from: i64,
}

pub const COINS: [&str; 8] = ["BTC", "ETH", "SOL", "BNB", "XRP", "DOGE", "ADA", "LINK"];

/// The Trend perps v3 design as a strategy JSON (BTC 50% of risk, 7 alts 50%).
pub fn trend_perps_v3() -> Value {
    let speeds = json!([[24, 480], [24, 960], [48, 480], [48, 960]]);
    let mut sleeves = vec![json!({"coin": "BTC", "weight": 0.5, "signal": {"speeds": speeds, "tilts": [
        [{"kind": "coinbase_premium", "coin": "BTC", "beta": 0.8}, {"kind": "funding", "beta": -0.1}],
        [{"kind": "crowding", "beta": -0.5}]]}})];
    for c in &COINS[1..] {
        sleeves.push(json!({"coin": c, "weight": 0.5 / 7.0, "signal": {"speeds": speeds, "tilts": [[{"kind": "coinbase_premium", "coin": "BTC", "beta": 0.3}]]}}));
    }
    json!({"type": "perp_portfolio", "name": "Trend perps v3", "sleeves": sleeves, "target_vol": 0.3135, "sleeve_vol": 0.20,
           "overlay_days": 60, "lev_cap": 3.0, "trade_hour_utc": 1, "calibrate": {"from": 1_583_020_800i64, "to": 1_704_067_200i64},
           "costs": {"taker_bps": 4.5}})
}

fn f(v: &Value, d: f64) -> f64 {
    v.as_f64().unwrap_or(d)
}

pub fn parse(s: &Map<String, Value>) -> Result<Spec, String> {
    if s.get("type").and_then(|v| v.as_str()) != Some("perp_portfolio") {
        return Err("not a perp portfolio".into());
    }
    let sl = s.get("sleeves").and_then(|v| v.as_array()).ok_or("sleeves: a list of {coin, weight, signal}")?;
    if sl.is_empty() || sl.len() > 20 {
        return Err("sleeves: 1 to 20".into());
    }
    let mut sleeves = vec![];
    for x in sl {
        let coin = x["coin"].as_str().ok_or("sleeve coin")?.to_uppercase();
        if !(2..=10).contains(&coin.len()) || !coin.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(format!("coin {coin:?}"));
        }
        let weight = f(&x["weight"], 1.0 / sl.len() as f64);
        if !(0.0..=1.0).contains(&weight) {
            return Err(format!("{coin}: weight 0 to 1"));
        }
        let g = &x["signal"];
        let speeds: Vec<(f64, f64)> = g["speeds"].as_array().map(|a| a.iter().filter_map(|p| Some((p[0].as_f64()?, p[1].as_f64()?))).collect())
            .unwrap_or_else(|| vec![(24.0, 480.0), (24.0, 960.0), (48.0, 480.0), (48.0, 960.0)]);
        if speeds.is_empty() || speeds.iter().any(|(a, b)| !(*a >= 2.0 && *b > *a && *b <= 8760.0)) {
            return Err(format!("{coin}: speeds are [fast, slow] hours, 2 <= fast < slow <= 8760"));
        }
        let mut tilts = vec![];
        for grp in g["tilts"].as_array().cloned().unwrap_or_default() {
            let mut out = vec![];
            for t in grp.as_array().cloned().unwrap_or_default() {
                let kind = t["kind"].as_str().unwrap_or("").to_string();
                if !["coinbase_premium", "funding", "crowding"].contains(&kind.as_str()) {
                    return Err(format!("{coin}: tilt kind {kind:?}"));
                }
                let beta = f(&t["beta"], 0.0);
                if beta.abs() > 5.0 {
                    return Err(format!("{coin}: tilt beta -5 to 5"));
                }
                out.push(Tilt { kind, coin: t["coin"].as_str().unwrap_or("BTC").to_uppercase(), beta });
            }
            tilts.push(out);
        }
        let signal = Signal {
            speeds,
            cap: f(&g["cap"], 0.072),
            vol_exp: f(&g["vol_exp"], 1.11),
            sig_exp: f(&g["sig_exp"], 2.5),
            vol_span: f(&g["vol_span"], 336.0),
            vwin: g["vwin"].as_u64().unwrap_or(24) as usize,
            vavg_days: g["vavg_days"].as_u64().unwrap_or(30) as usize,
            tilts,
        };
        if !(signal.cap > 0.0 && signal.vol_span >= 2.0 && (1..=720).contains(&signal.vwin) && (1..=365).contains(&signal.vavg_days)) {
            return Err(format!("{coin}: signal settings out of range"));
        }
        sleeves.push(Sleeve { coin, weight, signal });
    }
    let cal = s.get("calibrate").cloned().unwrap_or(json!({}));
    let spec = Spec {
        name: s.get("name").and_then(|v| v.as_str()).unwrap_or("Perp portfolio").to_string(),
        sleeves,
        target_vol: s.get("target_vol").map(|v| f(v, 0.3135)).unwrap_or(0.3135),
        sleeve_vol: s.get("sleeve_vol").map(|v| f(v, 0.2)).unwrap_or(0.2),
        overlay_days: s.get("overlay_days").and_then(|v| v.as_u64()).unwrap_or(60) as usize,
        lev_cap: s.get("lev_cap").map(|v| f(v, 3.0)).unwrap_or(3.0),
        trade_hour_utc: s.get("trade_hour_utc").and_then(|v| v.as_i64()).unwrap_or(1),
        cal_from: cal["from"].as_i64().unwrap_or(1_583_020_800),
        cal_to: cal["to"].as_i64().unwrap_or(1_704_067_200),
        taker_bps: s.get("costs").map(|c| f(&c["taker_bps"], 4.5)).unwrap_or(4.5),
        data_from: s.get("data_from").and_then(|v| v.as_i64()).unwrap_or(1_567_296_000), // 2019-09-01
    };
    if !(0.01..=2.0).contains(&spec.target_vol) || !(0.01..=2.0).contains(&spec.sleeve_vol) || !(0.5..=10.0).contains(&spec.lev_cap)
        || !(5..=365).contains(&spec.overlay_days) || !(0..24).contains(&spec.trade_hour_utc) || spec.cal_to <= spec.cal_from + 90 * DAY {
        return Err("portfolio settings out of range".into());
    }
    Ok(spec)
}

// ---------- series helpers (pandas semantics) ----------

fn ewm(x: &[f64], span: f64) -> Vec<f64> {
    let a = 2.0 / (span + 1.0);
    let mut out = vec![f64::NAN; x.len()];
    let mut y = f64::NAN;
    for (i, &v) in x.iter().enumerate() {
        if y.is_nan() {
            if !v.is_nan() {
                y = v;
            }
        } else if !v.is_nan() {
            y += a * (v - y);
        }
        out[i] = y;
    }
    out
}

/// Rolling sum over `w` values, NaN unless all `w` are present.
fn rolling_sum_full(x: &[f64], w: usize) -> Vec<f64> {
    rolling_mean(x, w, w).iter().map(|m| m * w as f64).collect()
}

/// Rolling mean over the last `w` rows of the non-NaN values, when at least `minp` are present.
fn rolling_mean(x: &[f64], w: usize, minp: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    let (mut s, mut n) = (0.0, 0usize);
    for i in 0..x.len() {
        if !x[i].is_nan() {
            s += x[i];
            n += 1;
        }
        if i >= w && !x[i - w].is_nan() {
            s -= x[i - w];
            n -= 1;
        }
        if n >= minp && n > 0 {
            out[i] = s / n as f64;
        }
    }
    out
}

/// Rolling sample std (ddof 1) over `w` values, NaN unless all `w` are present.
fn rolling_std_full(x: &[f64], w: usize) -> Vec<f64> {
    let mut out = vec![f64::NAN; x.len()];
    for i in (w.saturating_sub(1))..x.len() {
        let win = &x[i + 1 - w..=i];
        if win.iter().any(|v| v.is_nan()) {
            continue;
        }
        let m = win.iter().sum::<f64>() / w as f64;
        out[i] = (win.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (w as f64 - 1.0)).sqrt();
    }
    out
}

fn mean_std(x: &[f64]) -> (f64, f64) {
    let v: Vec<f64> = x.iter().copied().filter(|v| !v.is_nan()).collect();
    if v.len() < 2 {
        return (f64::NAN, f64::NAN);
    }
    let m = v.iter().sum::<f64>() / v.len() as f64;
    (m, (v.iter().map(|a| (a - m).powi(2)).sum::<f64>() / (v.len() as f64 - 1.0)).sqrt())
}

fn median(mut v: Vec<f64>) -> f64 {
    v.retain(|x| !x.is_nan());
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

fn sign(x: f64) -> f64 {
    if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else if x == 0.0 { 0.0 } else { f64::NAN }
}

// ---------- data ----------

/// One coin's hourly grid from its first stored hour: close (carried forward), quote volume (0 when
/// missing), funding rate at event hours (0 otherwise).
pub struct Coin {
    pub t0: i64,
    pub close: Vec<f64>,
    pub qv: Vec<f64>,
    pub fund: Vec<f64>,
}

impl Coin {
    pub fn t(&self, i: usize) -> i64 {
        self.t0 + i as i64 * HOUR
    }
    pub fn idx(&self, t: i64) -> Option<usize> {
        let i = (t - self.t0).div_euclid(HOUR);
        (i >= 0 && (i as usize) < self.close.len()).then_some(i as usize)
    }
    pub fn len(&self) -> usize {
        self.close.len()
    }
}

pub fn load_coin(coin: &str, from: i64, to: i64) -> Result<Coin, String> {
    let k = marketdata::get("perp", coin, from, to)?;
    let t0 = k[0].t;
    let n = ((k.last().unwrap().t - t0) / HOUR + 1) as usize;
    let mut close = vec![f64::NAN; n];
    let mut qv = vec![0.0; n];
    for r in &k {
        let i = ((r.t - t0) / HOUR) as usize;
        close[i] = r.v[3];
        qv[i] = if r.v[5].is_finite() { r.v[5] } else { 0.0 };
    }
    for i in 1..n {
        if close[i].is_nan() {
            close[i] = close[i - 1];
        }
    }
    let mut fund = vec![0.0; n];
    if let Ok(fr) = marketdata::get("funding", coin, from, to) {
        for r in fr {
            let i = (r.t - t0).div_euclid(HOUR);
            if i >= 0 && (i as usize) < n && r.v[0].is_finite() {
                fund[i as usize] = r.v[0];
            }
        }
    }
    Ok(Coin { t0, close, qv, fund })
}

/// An hourly series looked up on another coin's grid (NaN where absent).
fn on_grid(rows: &[marketdata::Row], col: usize, c: &Coin) -> Vec<f64> {
    let mut out = vec![f64::NAN; c.len()];
    for r in rows {
        if let Some(i) = c.idx(r.t) {
            out[i] = r.v[col];
        }
    }
    out
}

pub struct Data {
    pub coins: std::collections::HashMap<String, Coin>,
    /// Coinbase close by coin, on that coin's... grid of the price-reference coin (see tilts)
    pub coinbase: std::collections::HashMap<String, Vec<marketdata::Row>>,
    pub lsr: std::collections::HashMap<String, Vec<marketdata::Row>>,
}

pub fn load(spec: &Spec, to: i64) -> Result<Data, String> {
    let mut d = Data { coins: Default::default(), coinbase: Default::default(), lsr: Default::default() };
    for s in &spec.sleeves {
        d.coins.insert(s.coin.clone(), load_coin(&s.coin, spec.data_from, to)?);
        for g in &s.signal.tilts {
            for t in g {
                match t.kind.as_str() {
                    "coinbase_premium" => {
                        if !d.coinbase.contains_key(&t.coin) {
                            d.coinbase.insert(t.coin.clone(), marketdata::get("coinbase", &t.coin, spec.data_from, to)?);
                        }
                        if !d.coins.contains_key(&t.coin) {
                            d.coins.insert(t.coin.clone(), load_coin(&t.coin, spec.data_from, to)?);
                        }
                    }
                    "crowding" => {
                        if !d.lsr.contains_key(&s.coin) {
                            d.lsr.insert(s.coin.clone(), marketdata::get("lsr", &s.coin, spec.data_from, to)?);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(d)
}

// ---------- signal ----------

fn raw(c: &Coin, g: &Signal, fast: f64, slow: f64) -> Vec<f64> {
    let ef = ewm(&c.close, fast);
    let es = ewm(&c.close, slow);
    let mut r2 = vec![f64::NAN; c.len()];
    for i in 1..c.len() {
        r2[i] = (c.close[i] / c.close[i - 1]).ln().powi(2);
    }
    let s2: Vec<f64> = ewm(&r2, g.vol_span).iter().map(|v| 8760.0 * v).collect();
    let v24 = rolling_sum_full(&c.qv, g.vwin);
    let vm = rolling_mean(&v24, g.vavg_days * 24, g.vavg_days * 24);
    (0..c.len())
        .map(|i| {
            let s = (ef[i] / es[i]).ln();
            let x = s / s2[i].sqrt();
            let v = (v24[i] / vm[i]).max(1e-3);
            let v = if (v24[i] / vm[i]).is_nan() { f64::NAN } else { v };
            g.cap * (x / g.cap).tanh() * v.powf(g.vol_exp) / s2[i].powf(g.sig_exp / 2.0)
        })
        .collect()
}

fn z_on(x: &[f64], c: &Coin, a: i64, b: i64) -> Vec<f64> {
    let win: Vec<f64> = (0..c.len()).filter(|i| c.t(*i) >= a && c.t(*i) < b).map(|i| x[i]).collect();
    let (m, sd) = mean_std(&win);
    x.iter().map(|v| { let z = ((v - m) / sd).clamp(-3.0, 3.0); if z.is_nan() { 0.0 } else { z } }).collect()
}

/// The coin's tilted forecast on its own hourly grid (0 where not yet defined).
pub fn forecast(spec: &Spec, d: &Data, sl: &Sleeve) -> Vec<f64> {
    forecast_norm(spec, d, sl, None)
}

/// A speed's scale at each hour: the median |x| of its values at the trade hour's close over the
/// last `days` days (at least 60 of them; NaN before), known at that hour.
fn trailing_median(x: &[f64], c: &Coin, trade_hour: i64, days: usize) -> Vec<f64> {
    let at: Vec<usize> = (0..c.len()).filter(|i| (c.t(*i) + HOUR).rem_euclid(DAY) == trade_hour * HOUR).collect();
    let mut out = vec![f64::NAN; c.len()];
    let mut k = 0usize;
    let mut cur = f64::NAN;
    for i in 0..c.len() {
        while k < at.len() && at[k] <= i {
            let lo = (k + 1).saturating_sub(days);
            let w: Vec<f64> = at[lo..=k].iter().map(|j| x[*j].abs()).filter(|v| v.is_finite()).collect();
            cur = if w.len() >= 60 { median(w) } else { f64::NAN };
            k += 1;
        }
        out[i] = cur;
    }
    out
}

/// `forecast` with each speed scaled by its median |x| over the calibration span (None, the
/// portfolio's way) or over a trailing window of days (the node's way: any coin, however new).
pub fn forecast_norm(spec: &Spec, d: &Data, sl: &Sleeve, trailing_days: Option<usize>) -> Vec<f64> {
    let c = &d.coins[&sl.coin];
    let g = &sl.signal;
    let mut ens = vec![0.0; c.len()];
    for &(fa, sw) in &g.speeds {
        let x = raw(c, g, fa, sw);
        match trailing_days {
            None => {
                let med = median((0..c.len()).filter(|i| c.t(*i) < spec.cal_to).map(|i| x[i].abs()).collect());
                for i in 0..c.len() {
                    ens[i] += x[i] / med;
                }
            }
            Some(days) => {
                let med = trailing_median(&x, c, spec.trade_hour_utc, days);
                for i in 0..c.len() {
                    ens[i] += x[i] / med[i];
                }
            }
        }
    }
    let n = g.speeds.len() as f64;
    let mut base: Vec<f64> = ens.iter().map(|v| if v.is_nan() { 0.0 } else { v / n }).collect();
    for grp in &g.tilts {
        let sg: Vec<f64> = base.iter().map(|v| sign(*v)).collect();
        let mut t = vec![0.0; c.len()];
        for tl in grp {
            let feat: Vec<f64> = match tl.kind.as_str() {
                "coinbase_premium" => {
                    // (Coinbase close / Binance perp close − 1), 24 h mean (12 h minimum), of the reference coin
                    let rc = &d.coins[&tl.coin];
                    let cb = on_grid(&d.coinbase[&tl.coin], 3, rc);
                    let prem: Vec<f64> = (0..rc.len()).map(|i| cb[i] / rc.close[i] - 1.0).collect();
                    let pm = rolling_mean(&prem, 24, 12);
                    (0..c.len()).map(|i| rc.idx(c.t(i)).map(|j| pm[j]).unwrap_or(f64::NAN)).collect()
                }
                "funding" => {
                    let mut ff: Vec<f64> = c.fund.iter().map(|v| if *v == 0.0 { f64::NAN } else { *v }).collect();
                    for i in 1..ff.len() {
                        if ff[i].is_nan() {
                            ff[i] = ff[i - 1];
                        }
                    }
                    rolling_mean(&ff, 72, 8)
                }
                _ => {
                    // crowding: long/short ratio minus its 30-day mean (720 h, all present)
                    let mut l = on_grid(&d.lsr[&sl.coin], 0, c);
                    for i in 1..l.len() {
                        if l[i].is_nan() {
                            l[i] = l[i - 1];
                        }
                    }
                    let m = rolling_mean(&l, 720, 720);
                    (0..c.len()).map(|i| l[i] - m[i]).collect()
                }
            };
            let signed: Vec<f64> = (0..c.len()).map(|i| sg[i] * feat[i]).collect();
            let z = z_on(&signed, c, spec.cal_from, spec.cal_to);
            for i in 0..c.len() {
                t[i] += tl.beta * z[i];
            }
        }
        for i in 0..c.len() {
            base[i] *= t[i].exp();
        }
    }
    base
}

// ---------- the node strategies' trend_forecast block ----------

/// The settings of a `trend_forecast` block (graph.rs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeCfg {
    /// the coin's tilts from the v3 design (BTC: Coinbase premium, funding, crowding; others: BTC's Coinbase premium)
    pub tilts: bool,
    /// the forecast is clipped to ±clip (a new coin's first values can be far beyond its median)
    pub clip: f64,
    /// hold the value from the trade hour's close for a day (the portfolio's cadence), else hourly
    pub daily: bool,
    pub trade_hour: i64,
    /// days of the trailing window that scales each speed
    pub norm_days: usize,
}

type NodeKey = (String, bool, u64, bool, i64, usize, i64);
static NODE_CACHE: std::sync::Mutex<Option<std::collections::HashMap<NodeKey, (i64, std::sync::Arc<Vec<f64>>)>>> = std::sync::Mutex::new(None);

/// The block's hourly forecast for `coin` through hour `to` on the coin's grid: (first hour, values),
/// each value known at the close of its hour. Kept in memory per hour, so a Lab session's backtests
/// and the worker's cycles compute it once an hour.
pub fn node_hourly(coin: &str, cfg: &NodeCfg, to: i64) -> Result<(i64, std::sync::Arc<Vec<f64>>), String> {
    let coin = coin.to_uppercase();
    let to_h = to.div_euclid(HOUR) * HOUR;
    let key: NodeKey = (coin.clone(), cfg.tilts, cfg.clip.to_bits(), cfg.daily, cfg.trade_hour, cfg.norm_days, to_h);
    if let Some(hit) = NODE_CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&key)) {
        return Ok(hit.clone());
    }
    let tilts = if !cfg.tilts {
        json!([])
    } else if coin == "BTC" {
        json!([[{"kind": "coinbase_premium", "coin": "BTC", "beta": 0.8}, {"kind": "funding", "beta": -0.1}], [{"kind": "crowding", "beta": -0.5}]])
    } else {
        json!([[{"kind": "coinbase_premium", "coin": "BTC", "beta": 0.3}]])
    };
    let st = json!({"type": "perp_portfolio", "trade_hour_utc": cfg.trade_hour,
                    "sleeves": [{"coin": coin, "weight": 1.0, "signal": {"tilts": tilts}}]});
    let spec = parse(st.as_object().unwrap())?;
    let d = load(&spec, to_h + HOUR)?;
    let sl = &spec.sleeves[0];
    let f = forecast_norm(&spec, &d, sl, Some(cfg.norm_days));
    let c = &d.coins[&sl.coin];
    let mut out: Vec<f64> = f.iter().map(|v| if v.is_finite() && *v != 0.0 { v.clamp(-cfg.clip, cfg.clip) } else { f64::NAN }).collect();
    if cfg.daily {
        let mut held = f64::NAN;
        for i in 0..out.len() {
            if (c.t(i) + HOUR).rem_euclid(DAY) == cfg.trade_hour * HOUR {
                held = out[i];
            }
            out[i] = held;
        }
    }
    let v = (c.t0, std::sync::Arc::new(out));
    let mut g = NODE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(Default::default);
    m.retain(|k, _| k.6 >= to_h - 2 * HOUR); // only the latest hours' values are kept
    m.insert(key, v.clone());
    Ok(v)
}

/// The block's value at each bar end: the forecast of the last hour closed by then (NaN before
/// the forecast starts, or with no data).
pub fn node_series(coin: &str, cfg: &NodeCfg, ends: &[i64]) -> Result<Vec<f64>, String> {
    let Some(last) = ends.last() else { return Ok(vec![]) };
    let (t0, f) = node_hourly(coin, cfg, *last)?;
    Ok(ends
        .iter()
        .map(|e| {
            // the hour that opened at or before e − 1 h has closed by e
            let h = (e - HOUR).div_euclid(HOUR) * HOUR;
            let i = (h - t0).div_euclid(HOUR);
            if i >= 0 && (i as usize) < f.len() { f[i as usize] } else { f64::NAN }
        })
        .collect())
}

// ---------- simulation ----------

pub struct SleeveRun {
    /// hourly net return
    pub ret: Vec<f64>,
    pub fees: f64,
    pub funding: f64,
    pub trades: usize,
}

/// One sleeve, hourly: leverage held drifts with price; at the close of the bar ending
/// `trade_hour_utc` it moves to `k·target`, paying taker fees on the change.
pub fn sim(c: &Coin, target: &[f64], k: f64, trade_hour: i64, fee: f64) -> SleeveRun {
    let n = c.len();
    let mut ret = vec![0.0; n];
    let (mut lev, mut fees, mut funding, mut trades) = (0.0f64, 0.0, 0.0, 0usize);
    for i in 0..n {
        let r = if i == 0 { 0.0 } else { c.close[i] / c.close[i - 1] - 1.0 };
        let fp = lev * c.fund[i];
        let mut pnl = lev * r - fp;
        funding -= fp;
        lev = if 1.0 + pnl != 0.0 { lev * (1.0 + r) / (1.0 + pnl) } else { 0.0 };
        if (c.t(i) + HOUR).rem_euclid(DAY) / HOUR == trade_hour && !target[i].is_nan() {
            let new = k * target[i];
            let cost = fee * (new - lev).abs();
            if (new - lev).abs() > 0.0 {
                trades += 1;
            }
            pnl -= cost;
            fees += cost;
            lev = new;
        }
        ret[i] = pnl;
    }
    SleeveRun { ret, fees, funding, trades }
}

/// Daily closes of compounded hourly returns: (day, value at its last hour).
fn daily_eq(c: &Coin, ret: &[f64], a: i64, b: i64) -> Vec<(i64, f64)> {
    let mut out: Vec<(i64, f64)> = vec![];
    let mut eq = 1.0;
    for i in 0..c.len() {
        let t = c.t(i);
        if t < a || t >= b {
            continue;
        }
        eq *= 1.0 + ret[i];
        let day = t.div_euclid(DAY);
        match out.last_mut() {
            Some(l) if l.0 == day => l.1 = eq,
            _ => out.push((day, eq)),
        }
    }
    out
}

fn daily_returns(eq: &[(i64, f64)]) -> Vec<(i64, f64)> {
    eq.windows(2).map(|w| (w[1].0, w[1].1 / w[0].1 - 1.0)).collect()
}

fn ann_vol(r: &[f64]) -> f64 {
    mean_std(r).1 * 365f64.sqrt()
}

/// k so that the sleeve's daily vol over [a, b) is `vol` (fees make it slightly nonlinear: 4 passes).
pub fn scale(c: &Coin, target: &[f64], vol: f64, a: i64, b: i64, trade_hour: i64, fee: f64) -> f64 {
    let mut k = 1.0;
    for _ in 0..4 {
        let r = sim(c, target, k, trade_hour, fee);
        let d: Vec<f64> = daily_returns(&daily_eq(c, &r.ret, a, b)).iter().map(|x| x.1).collect();
        let v = ann_vol(&d);
        if !(v > 0.0) {
            break;
        }
        k *= vol / v;
    }
    k
}

pub struct Run {
    pub days: Vec<i64>,
    /// portfolio daily net return (after overlay and final scale)
    pub ret: Vec<f64>,
    /// per sleeve: coin, k, daily return (0 before its data), fees, funding, trades
    pub sleeves: Vec<(String, f64, Vec<f64>, f64, f64, usize)>,
    pub overlay: Vec<f64>,
    pub scale: f64,
    /// per sleeve: latest hourly forecast and its hour
    pub latest: Vec<(String, f64, i64)>,
}

pub fn run(spec: &Spec, d: &Data) -> Result<Run, String> {
    let fee = spec.taker_bps / 1e4;
    let mut sl_out = vec![];
    let mut latest = vec![];
    let (mut d0, mut d1) = (i64::MAX, i64::MIN);
    for s in &spec.sleeves {
        let c = &d.coins[&s.coin];
        let fc = forecast(spec, d, s);
        let a = spec.cal_from.max(c.t0 + 60 * DAY);
        let k = scale(c, &fc, spec.sleeve_vol, a, spec.cal_to, spec.trade_hour_utc, fee);
        let r = sim(c, &fc, k, spec.trade_hour_utc, fee);
        // as the research code: the first day's return is 0, later days compound from the day before
        let eq = daily_eq(c, &r.ret, i64::MIN, i64::MAX);
        let mut dr = vec![(eq[0].0, 0.0)];
        dr.extend(daily_returns(&eq));
        d0 = d0.min(dr.first().map(|x| x.0).unwrap_or(i64::MAX));
        d1 = d1.max(dr.last().map(|x| x.0).unwrap_or(i64::MIN));
        let last = (0..c.len()).rev().find(|i| (c.t(*i) + HOUR).rem_euclid(DAY) / HOUR == spec.trade_hour_utc).unwrap_or(c.len() - 1);
        latest.push((s.coin.clone(), k * fc[last], c.t(last) + HOUR));
        sl_out.push((s.coin.clone(), k, dr, r.fees, r.funding, r.trades));
    }
    if d0 > d1 {
        return Err("no data".into());
    }
    let days: Vec<i64> = (d0..=d1).collect();
    let n = days.len();
    let mut sleeves = vec![];
    let mut port = vec![0.0; n];
    for (s, (coin, k, dr, fees, fu, tr)) in spec.sleeves.iter().zip(sl_out) {
        let mut v = vec![0.0; n];
        for (day, x) in dr {
            v[(day - d0) as usize] = x;
        }
        for i in 0..n {
            port[i] += s.weight * v[i];
        }
        sleeves.push((coin, k, v, fees, fu, tr));
    }
    // overlay: target / trailing realised vol, known at the day's start
    let sd = rolling_std_full(&port, spec.overlay_days);
    let mut overlay = vec![1.0; n];
    for i in 1..n {
        let rv = sd[i - 1] * 365f64.sqrt();
        overlay[i] = if rv.is_nan() { 1.0 } else { (spec.target_vol / rv).clamp(0.0, spec.lev_cap) };
    }
    let r2: Vec<f64> = (0..n).map(|i| port[i] * overlay[i]).collect();
    let cal: Vec<f64> = (0..n).filter(|i| days[*i] * DAY >= spec.cal_from && days[*i] * DAY < spec.cal_to).map(|i| r2[i]).collect();
    let kp = spec.target_vol / ann_vol(&cal);
    let ret = r2.iter().map(|x| x * kp).collect();
    Ok(Run { days, ret, sleeves, overlay: overlay.iter().map(|m| m * kp).collect(), scale: kp, latest })
}

/// Target leverage per coin now: portfolio scale × today's overlay × weight × sleeve forecast.
pub fn targets(spec: &Spec, r: &Run) -> Vec<(String, f64, i64)> {
    let m = *r.overlay.last().unwrap_or(&1.0);
    spec.sleeves.iter().zip(&r.latest).map(|(s, (c, l, t))| (c.clone(), m * s.weight * l, *t)).collect()
}

fn stats(r: &[f64]) -> Value {
    if r.len() < 2 {
        return json!({});
    }
    let mut eq = 1.0;
    let mut peak = 1.0f64;
    let mut dd = 0.0f64;
    for x in r {
        eq *= 1.0 + x;
        peak = peak.max(eq);
        dd = dd.min(eq / peak - 1.0);
    }
    let (m, s) = mean_std(r);
    let yrs = r.len() as f64 / 365.0;
    json!({"return": eq - 1.0, "cagr": eq.powf(1.0 / yrs) - 1.0, "vol": s * 365f64.sqrt(), "sharpe": m / s * 365f64.sqrt(), "max_drawdown": dd})
}

/// A backtest over [from, to) in the Lab's result shape (runs.main with series, events and stats).
pub fn backtest(strategy: &Map<String, Value>, from: i64, to: i64, capital: f64) -> Result<Value, String> {
    let spec = parse(strategy)?;
    let d = load(&spec, to)?;
    let r = run(&spec, &d)?;
    let sel: Vec<usize> = (0..r.days.len()).filter(|i| r.days[*i] * DAY >= from && r.days[*i] * DAY < to).collect();
    if sel.len() < 2 {
        return Err("the period has no data".into());
    }
    let pr: Vec<f64> = sel.iter().map(|i| r.ret[*i]).collect();
    let mut v = capital;
    let mut series = vec![];
    for (j, i) in sel.iter().enumerate() {
        v *= 1.0 + pr[j];
        series.push(json!([(r.days[*i] + 1) * DAY, v, capital]));
    }
    let sleeves: Vec<Value> = spec.sleeves.iter().zip(&r.sleeves).map(|(s, (coin, k, dr, fees, fu, tr))| {
        let x: Vec<f64> = sel.iter().map(|i| dr[*i]).collect();
        json!({"coin": coin, "weight": s.weight, "k": k, "stats": stats(&x), "fees_all": fees, "funding_all": fu, "trades_all": tr})
    }).collect();
    let tg: Vec<Value> = targets(&spec, &r).into_iter().map(|(c, l, t)| json!({"coin": c, "leverage": l, "at": t})).collect();
    let st = stats(&pr);
    let pnl = v - capital;
    let cal_sel: Vec<f64> = (0..r.days.len()).filter(|i| r.days[*i] * DAY >= spec.cal_from && r.days[*i] * DAY < spec.cal_to).map(|i| r.ret[i]).collect();
    Ok(json!({
        "kind": "perp_portfolio", "from": from, "to": to, "span_days": sel.len(), "capital": capital,
        "runs": {"main": {"label": spec.name, "start_value": capital, "value": v, "pnl": pnl,
            "apr": st["cagr"], "max_drawdown": st["max_drawdown"], "series": series, "events": [], "stats": st}},
        "sleeves": sleeves, "targets": tg, "overlay_now": r.overlay.last(), "scale": r.scale,
        "calibration": {"from": spec.cal_from, "to": spec.cal_to, "stats": stats(&cal_sel)},
        "notes": ["Daily rebalance at the close of the hour ending trade_hour_utc; taker fees; Binance funding; settings calibrated on the calibration span."],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers_follow_pandas() {
        let x = [1.0, 2.0, 3.0, f64::NAN, 5.0];
        let e = ewm(&x, 3.0);
        assert_eq!(e[0], 1.0);
        assert!((e[1] - 1.5).abs() < 1e-12 && (e[2] - 2.25).abs() < 1e-12 && (e[3] - 2.25).abs() < 1e-12);
        let m = rolling_mean(&x, 2, 1);
        assert_eq!(m[3], 3.0);
        assert!(rolling_sum_full(&x, 2)[3].is_nan());
        assert_eq!(rolling_sum_full(&x, 2)[2], 5.0);
        let s = rolling_std_full(&[1.0, 2.0, 4.0], 3);
        assert!((s[2] - (7.0f64 / 3.0).sqrt()).abs() < 1e-12);
        assert_eq!(median(vec![3.0, f64::NAN, 1.0, 2.0, 4.0]), 2.5);
    }

    #[test]
    fn the_design_parses() {
        let v = trend_perps_v3();
        let s = parse(v.as_object().unwrap()).unwrap();
        assert_eq!(s.sleeves.len(), 8);
        assert_eq!(s.sleeves[0].signal.tilts.len(), 2);
        assert!((s.sleeves.iter().map(|x| x.weight).sum::<f64>() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_sleeve_trades_once_a_day_and_pays_fees() {
        let n = 24 * 5;
        let c = Coin { t0: 0, close: (0..n).map(|i| 100.0 + i as f64 * 0.1).collect(), qv: vec![1.0; n], fund: vec![0.0; n] };
        // at 2x the leverage drifts with price, so every day's close trades back to 2x
        let tg = vec![2.0; n];
        let r = sim(&c, &tg, 1.0, 1, 4.5e-4);
        assert_eq!(r.trades, 5);
        assert!(r.fees > 2.0 * 4.5e-4 && r.fees < 2.0 * 4.5e-4 * 1.1);
        // long a rising coin makes money
        assert!(r.ret.iter().sum::<f64>() > 0.0);
    }

    /// Parity with the research code: PARITY_DIR holds portfolio_daily.csv from
    /// the research export_parity.py script; ALM_HOME must point at a data copy (not the live folder).
    #[test]
    #[ignore]
    fn parity_with_research() {
        let home = std::env::var("ALM_HOME").expect("ALM_HOME");
        assert!(!home.to_lowercase().contains("appdata\\local\\stablealm"), "use a copy, not the live folder");
        let dir = std::env::var("PARITY_DIR").expect("PARITY_DIR");
        let spec = parse(trend_perps_v3().as_object().unwrap()).unwrap();
        let to = std::env::var("PARITY_TO").ok().and_then(|s| s.parse().ok()).unwrap_or(1_795_000_000i64);
        let d = load(&spec, to).unwrap();
        let r = run(&spec, &d).unwrap();
        let body = std::fs::read_to_string(format!("{dir}/portfolio_daily.csv")).unwrap();
        let mut theirs = std::collections::HashMap::new();
        for l in body.lines().skip(1) {
            let mut p = l.split(',');
            let day = p.next().unwrap();
            let v: f64 = p.next().unwrap_or("nan").parse().unwrap_or(f64::NAN);
            // "2020-03-01 00:00:00+00:00"
            let ymd: Vec<i64> = day[..10].split('-').map(|x| x.parse().unwrap()).collect();
            let days = days_from_civil(ymd[0], ymd[1], ymd[2]);
            theirs.insert(days, v);
        }
        let (mut a, mut b) = (vec![], vec![]);
        for (i, day) in r.days.iter().enumerate() {
            if let Some(t) = theirs.get(day) {
                if t.is_finite() && *day * DAY >= 1_583_020_800 {
                    a.push(r.ret[i]);
                    b.push(*t);
                }
            }
        }
        let n = a.len() as f64;
        let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
        let cov: f64 = a.iter().zip(&b).map(|(x, y)| (x - ma) * (y - mb)).sum();
        let corr = cov / (a.iter().map(|x| (x - ma).powi(2)).sum::<f64>().sqrt() * b.iter().map(|y| (y - mb).powi(2)).sum::<f64>().sqrt());
        let maxdiff = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
        let tot = |v: &[f64]| v.iter().fold(1.0, |e, x| e * (1.0 + x)) - 1.0;
        println!("days {} corr {corr:.6} max |diff| {maxdiff:.6} total rust {:.4} research {:.4} scale {:.4}", a.len(), tot(&a), tot(&b), r.scale);
        for (c, k, ..) in &r.sleeves {
            println!("  {c} k {k:.5}");
        }
        assert!(corr > 0.999, "corr {corr}");
    }

    fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }
}
