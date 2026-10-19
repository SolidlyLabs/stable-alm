//! Per-pool data for the snapshot and the dashboard (plan section 6), from local data first:
//! - **geometry**: tick, the positions' ranges, distance to each boundary (ticks and %), in range;
//! - **regime** (tier 1), from the pool's own swaps in the local event store: realised volatility
//!   over 1 h, 24 h and 7 d, ATR in ticks, z-score against the 72 h mean, trend (ADX and the slope
//!   of a 24 h EMA), and the share of the last 24 h the price spent outside the current ranges;
//! - **activity**: hourly volume and fees for 7 days (the data processor's hourly history, local copy);
//! - **F/LVR** (data.rs, cached) with the plan's labels;
//! - **data quality**: each block says when it was observed, its source, and whether it is complete
//!   or stale.

use serde_json::{json, Value};

use crate::ta;

const H: i64 = 3600;

fn quality(observed: i64, source: &str, complete: bool, stale_after_s: i64) -> Value {
    let age = super::now() - observed;
    json!({"observed_at": observed, "retrieved_at": super::now(), "source": source, "complete": complete, "stale": age > stale_after_s})
}

/// Hourly bars (close, high, low tick) from swaps, oldest first; a bar with no swap repeats the close.
pub fn hourly_bars(swaps: &[(i64, i64)], from: i64, to: i64) -> (Vec<i64>, Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut ts = vec![];
    let (mut c, mut h, mut l) = (vec![], vec![], vec![]);
    let mut i = 0;
    let mut last: Option<f64> = None;
    let mut t = from.div_euclid(H) * H;
    while t < to {
        let (mut hi, mut lo) = (f64::MIN, f64::MAX);
        let mut any = false;
        while i < swaps.len() && swaps[i].0 < t + H {
            if swaps[i].0 >= t {
                let k = swaps[i].1 as f64;
                hi = hi.max(k);
                lo = lo.min(k);
                last = Some(k);
                any = true;
            } else {
                last = Some(swaps[i].1 as f64);
            }
            i += 1;
        }
        if let Some(cl) = last {
            ts.push(t);
            c.push(cl);
            h.push(if any { hi } else { cl });
            l.push(if any { lo } else { cl });
        }
        t += H;
    }
    (ts, c, h, l)
}

/// Realised volatility of tick changes (in % of price) over the last `n` bars, per bar.
fn vol(c: &[f64], n: usize) -> Option<f64> {
    if c.len() < n + 1 || n < 2 {
        return None;
    }
    let r: Vec<f64> = c[c.len() - n - 1..].windows(2).map(|w| (w[1] - w[0]) * 1.0001f64.ln() * 100.0).collect();
    let m = r.iter().sum::<f64>() / r.len() as f64;
    Some((r.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (r.len() - 1) as f64).sqrt())
}

fn last(s: &[f64]) -> Option<f64> {
    s.last().copied().filter(|x| x.is_finite())
}

/// The regime block from the pool's swaps (time, tick).
pub fn regime(swaps: &[(i64, i64)], ranges: &[(i64, i64)], now: i64) -> Value {
    let (_, c, h, l) = hourly_bars(swaps, now - 8 * 86_400, now);
    if c.len() < 30 {
        return json!({"complete": false, "reason": "less than 30 hours of swaps stored"});
    }
    // 5-minute closes over the last hour and day for the short volatility and time out of range
    let (_, c5, _, _) = {
        let mut ts = vec![];
        let mut cl = vec![];
        let mut i = 0;
        let mut lastk: Option<i64> = None;
        let mut t = (now - 86_400).div_euclid(300) * 300;
        while t < now {
            while i < swaps.len() && swaps[i].0 < t + 300 {
                lastk = Some(swaps[i].1);
                i += 1;
            }
            if let Some(k) = lastk {
                ts.push(t);
                cl.push(k as f64);
            }
            t += 300;
        }
        (ts, cl, (), ())
    };
    let atr = last(&ta::atr(&c, &h, &l, 14));
    let z = last(&ta::zscore(&c, 72));
    let (adx, _, _) = ta::adx(&c, &h, &l, 14);
    let ema = ta::ema(&c, 24);
    let slope = match (last(&ema), ema.len().checked_sub(7).map(|i| ema[i]).filter(|x| x.is_finite())) {
        (Some(a), Some(b)) => Some((a - b) / 6.0),
        _ => None,
    };
    let out_share = if ranges.is_empty() || c5.is_empty() {
        None
    } else {
        Some(c5.iter().filter(|k| !ranges.iter().any(|(a, b)| **k >= *a as f64 && **k < *b as f64)).count() as f64 / c5.len() as f64)
    };
    json!({
        "complete": true,
        "vol_1h_pct": vol(&c5, 12.min(c5.len().saturating_sub(1))),
        "vol_24h_pct": vol(&c, 24),
        "vol_7d_pct": vol(&c, (7 * 24).min(c.len() - 1)),
        "vol_unit": "stdev of per-bar price change, % (5-minute bars for 1 h, hourly for 24 h and 7 d)",
        "atr_ticks": atr,
        "zscore_72h": z,
        "adx_14h": last(&adx),
        "ema24_slope_ticks_per_h": slope,
        "out_of_range_24h": out_share,
        "last_tick": c.last(),
    })
}

/// Hourly volume and fees for the last 7 days, from the processor's history (local copy).
pub fn activity(cid: u64, pool: &str, now: i64) -> Value {
    let p = crate::data::pool_dir(cid, pool).join("apr_hourly.json");
    let rows: Vec<Value> = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let mut hours: Vec<(i64, f64, f64)> = rows.iter()
        .filter_map(|r| Some((super::num(&r["timestamp"])? as i64, super::num(&r["volumeUSD"]).unwrap_or(0.0), super::num(&r["feesUSD"]).unwrap_or(0.0))))
        .filter(|r| r.0 >= now - 7 * 86_400)
        .collect();
    hours.sort_by_key(|r| r.0);
    let newest = hours.last().map(|r| r.0).unwrap_or(0);
    let vol: f64 = hours.iter().map(|r| r.1).sum();
    let fees: f64 = hours.iter().map(|r| r.2).sum();
    let day = |k: i64| hours.iter().filter(|r| r.0 >= now - (k + 1) * 86_400 && r.0 < now - k * 86_400).map(|r| (r.1, r.2)).fold((0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1));
    let daily: Vec<Value> = (0..7).rev().map(|k| { let (v, f) = day(k); json!({"days_ago": k, "volume_usd": v, "fees_usd": f}) }).collect();
    json!({"volume_7d_usd": vol, "fees_7d_usd": fees, "hours": hours.len(), "daily": daily,
           "quality": quality(newest, "data processor (local copy)", hours.len() >= 160, 3 * H)})
}

/// F/LVR with the plan's labels: unknown below 80% coverage; no activity; no measurable LVR; ratio.
pub fn flvr_label(v: &Value) -> Value {
    // data.rs: {ratio, f, lvr, swaps, hours, since}; coverage = hours with a price over the 7 days
    let cov = v["coverage"].as_f64().or_else(|| v["hours"].as_f64().map(|h| h / 168.0));
    let (fees, lvr) = (v["fees"].as_f64().or_else(|| v["f"].as_f64()), v["lvr"].as_f64());
    let label = match (cov, fees, lvr) {
        (Some(c), _, _) if c < 0.8 => "unknown".to_string(),
        (_, Some(f), Some(l)) if f <= 0.0 && l <= 0.0 => "no activity".into(),
        (_, Some(f), Some(l)) if f > 0.0 && l <= 0.0 => "no measurable LVR".into(),
        (_, Some(f), Some(l)) => {
            let r = f / l;
            if r > 50.0 { "> 50".into() } else { format!("{r:.2}") }
        }
        _ => v["ratio"].as_f64().map(|r| format!("{r:.2}")).unwrap_or_else(|| "unknown".into()),
    };
    json!({"label": label, "raw": v})
}

/// Everything the snapshot shows for one configured pool.
pub fn pool(cid: u64, cfg: &Value, acct_pool: &Value) -> Value {
    let now = super::now();
    let addr = cfg["pool"].as_str().unwrap_or("").to_lowercase();
    let tick = acct_pool["tick"].as_i64();
    let ranges: Vec<(i64, i64)> = acct_pool["positions"].as_array().into_iter().flatten()
        .filter_map(|p| Some((p["tl"].as_i64()?, p["tu"].as_i64()?))).collect();
    let spacing = crate::chain::tick_spacing(cid, &addr).ok();
    let geometry = match tick {
        Some(t) => {
            let rows: Vec<Value> = acct_pool["positions"].as_array().into_iter().flatten().map(|p| {
                let (tl, tu) = (p["tl"].as_i64().unwrap_or(0), p["tu"].as_i64().unwrap_or(0));
                json!({"tl": tl, "tu": tu, "usd": p["usd"], "in_range": p["in_range"], "liquidity": p["liquidity"],
                       "to_lower_ticks": t - tl, "to_upper_ticks": tu - t,
                       "to_lower_pct": (1.0001f64.powf((t - tl) as f64) - 1.0) * 100.0, "to_upper_pct": (1.0001f64.powf((tu - t) as f64) - 1.0) * 100.0})
            }).collect();
            json!({"tick": t, "tick_spacing": spacing, "positions": rows, "in_range": acct_pool["in_range"], "deployed_usd": acct_pool["deployed_usd"]})
        }
        None => json!({"complete": false, "reason": "no position data this cycle"}),
    };
    let swaps: Vec<(i64, i64)> = crate::history::read(cid, &addr, now - 8 * 86_400, now + H).into_iter()
        .filter(|e| e.kind == "swap").map(|e| (e.ts, e.tick)).collect();
    let newest = swaps.last().map(|s| s.0).unwrap_or(0);
    let mut reg = regime(&swaps, &ranges, now);
    reg["quality"] = quality(newest, "local event store", reg["complete"] == true, 2 * H);
    let flvr = crate::data::cached_flvr(cid, &addr).map(|v| flvr_label(&v)).unwrap_or(json!({"label": "unknown"}));
    json!({
        "chain_id": cid, "pool": addr, "name": cfg["name"], "enabled": cfg["enabled"], "strategy": cfg["strategy"],
        "geometry": geometry, "regime": reg, "activity": activity(cid, &addr, now), "flvr": flvr,
        "performance": super::perf::pool(&addr, 7 * 86_400),
        "performance_24h": super::perf::pool(&addr, 86_400),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_and_regime_from_swaps() {
        let now = 1_000 * H;
        // a sine over 8 days, one swap every 10 minutes
        let swaps: Vec<(i64, i64)> = (0..(8 * 24 * 6)).map(|k| (now - 8 * 86_400 + k * 600, ((k as f64 / 40.0).sin() * 50.0) as i64)).collect();
        let (ts, c, h, l) = hourly_bars(&swaps, now - 8 * 86_400, now);
        assert_eq!(ts.len(), 8 * 24);
        assert!(h.iter().zip(&l).all(|(a, b)| a >= b));
        assert_eq!(c.len(), h.len());
        let r = regime(&swaps, &[(-10, 10)], now);
        assert_eq!(r["complete"], json!(true));
        assert!(r["atr_ticks"].as_f64().unwrap() > 0.0);
        let out = r["out_of_range_24h"].as_f64().unwrap();
        assert!(out > 0.3 && out < 1.0, "{out}");
    }

    #[test]
    fn flvr_labels() {
        assert_eq!(flvr_label(&json!({"coverage": 0.5, "fees": 1.0, "lvr": 1.0}))["label"], "unknown");
        assert_eq!(flvr_label(&json!({"coverage": 0.9, "fees": 0.0, "lvr": 0.0}))["label"], "no activity");
        assert_eq!(flvr_label(&json!({"coverage": 0.9, "fees": 2.0, "lvr": 0.0}))["label"], "no measurable LVR");
        assert_eq!(flvr_label(&json!({"coverage": 0.9, "fees": 200.0, "lvr": 1.0}))["label"], "> 50");
        assert_eq!(flvr_label(&json!({"coverage": 0.9, "fees": 3.0, "lvr": 1.5}))["label"], "2.00");
        assert_eq!(flvr_label(&json!({"f": 3.0, "lvr": 1.5, "hours": 160}))["label"], "2.00");
        assert_eq!(flvr_label(&json!({"f": 3.0, "lvr": 1.5, "hours": 20}))["label"], "unknown");
    }
}
