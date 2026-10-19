//! Per-pool performance (plan section 6), from what the app already records:
//! - logs/snapshots.jsonl, one line per cycle: each pool's capital (`pool_capital`, address → USD),
//!   its positions' token amounts and USD value, and its tick;
//! - logs/executor.jsonl, kind "capital": money added to a pool or withdrawn from it.
//!
//! For a window (24 h, 7 d, 30 d):
//! - **pnl** = capital at the end − at the start − net money added: transfers are not profit;
//! - **return** = pnl ÷ (start capital + time-weighted money added) (modified Dietz);
//! - **vs holding** = the end value against holding the token amounts the pool had at the start (and
//!   those it was given later), all in the pool's token1, so the pair's own price (ETH in USD, say)
//!   does not show as a loss. This is what the risk controller's loss limit watches (section 4).
//!
//! The token split of idle tokens is not in the snapshot: a pool's start amounts are its positions'
//! amounts scaled up to its whole capital.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Mutex;

/// One pool at one snapshot.
#[derive(Clone, Debug, Default)]
pub struct Sample {
    pub ts: i64,
    pub capital_usd: f64,
    /// token amounts in positions (whole tokens) and their USD value
    pub a0: f64,
    pub a1: f64,
    pub pos_usd: f64,
    /// token1 per token0 (whole tokens), from the tick
    pub price: f64,
}

impl Sample {
    /// USD price of token1, from the positions' value: usd = a0·p·u1 + a1·u1.
    pub fn u1(&self) -> Option<f64> {
        let d = self.a0 * self.price + self.a1;
        (d > 0.0 && self.pos_usd > 0.0).then(|| self.pos_usd / d)
    }
}

#[derive(Clone, Debug)]
pub struct Flow {
    pub ts: i64,
    /// USD into the pool (negative: out)
    pub usd: f64,
}

struct Store {
    offset: u64,
    /// pool address → samples, oldest first
    samples: HashMap<String, Vec<Sample>>,
}

static STORE: Mutex<Option<Store>> = Mutex::new(None);
const KEEP_S: i64 = 45 * 86_400;

fn decimals(cid: u64, pool: &str) -> Option<(i32, i32)> {
    let (t0, t1) = crate::chain::pool_tokens(cid, pool).ok()?;
    Some((crate::prices::meta(cid, &t0).ok()?.decimals as i32, crate::prices::meta(cid, &t1).ok()?.decimals as i32))
}

/// Parse one snapshot line into per-pool samples.
pub fn parse_line(v: &Value, names: &HashMap<String, String>, dec: &mut dyn FnMut(u64, &str) -> Option<(i32, i32)>) -> Vec<(String, Sample)> {
    let ts = v["ts"].as_i64().unwrap_or(0);
    let mut out = vec![];
    let caps = v["pool_capital"].as_object().cloned().unwrap_or_default();
    for (name, info) in v["pools"].as_object().into_iter().flatten() {
        let Some(addr) = names.get(name) else { continue };
        let Some(cap) = caps.get(addr).and_then(|c| c.as_f64()) else { continue };
        let cid = info["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
        let (mut a0, mut a1, mut usd) = (0.0, 0.0, 0.0);
        for p in info["positions"].as_array().into_iter().flatten() {
            a0 += p["amount0"].as_f64().unwrap_or(0.0);
            a1 += p["amount1"].as_f64().unwrap_or(0.0);
            usd += p["usd"].as_f64().unwrap_or(0.0);
        }
        let price = match (info["tick"].as_i64(), dec(cid, addr)) {
            (Some(t), Some((d0, d1))) => 1.0001f64.powf(t as f64) * 10f64.powi(d0 - d1),
            _ => 0.0,
        };
        out.push((addr.clone(), Sample { ts, capital_usd: cap, a0, a1, pos_usd: usd, price }));
    }
    out
}

/// Read what was appended to the snapshots since the last call.
fn refresh() {
    let path = crate::paths::logs().join("snapshots.jsonl");
    let mut g = STORE.lock().unwrap();
    let st = g.get_or_insert_with(|| Store { offset: 0, samples: HashMap::new() });
    let Ok(mut f) = std::fs::File::open(&path) else { return };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < st.offset {
        st.offset = 0;
        st.samples.clear();
    }
    // on the first read, only the last 45 days matter: skip ahead roughly
    if st.offset == 0 && len > 400 << 20 {
        st.offset = len - (400 << 20);
    }
    let _ = f.seek(SeekFrom::Start(st.offset));
    let mut text = String::new();
    if f.read_to_string(&mut text).is_err() {
        return;
    }
    let complete = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let cutoff = super::now() - KEEP_S;
    let names: HashMap<String, String> = crate::settings::load_pools().iter()
        .filter_map(|p| Some((p["name"].as_str()?.to_string(), p["pool"].as_str()?.to_lowercase()))).collect();
    let mut known: HashMap<String, Option<(i32, i32)>> = HashMap::new();
    let mut dec = |cid: u64, a: &str| *known.entry(a.to_string()).or_insert_with(|| decimals(cid, a));
    for line in text[..complete].lines() {
        // cheap check before parsing: only lines that carry pool capital are used
        if !line.contains("\"pool_capital\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["ts"].as_i64().unwrap_or(0) < cutoff {
            continue;
        }
        for (addr, s) in parse_line(&v, &names, &mut dec) {
            st.samples.entry(addr).or_default().push(s);
        }
    }
    st.offset += complete as u64;
    for v in st.samples.values_mut() {
        let first = v.partition_point(|s| s.ts < cutoff);
        v.drain(..first);
    }
}

/// Money added to (positive) or taken from (negative) each pool, from the executor log.
pub fn flows(since: i64) -> HashMap<String, Vec<Flow>> {
    let mut out: HashMap<String, Vec<Flow>> = HashMap::new();
    for e in crate::engine::log_tail(32 << 20) {
        if e["kind"] != "capital" {
            continue;
        }
        let ts = e["ts"].as_i64().unwrap_or(0);
        if ts < since {
            continue;
        }
        let usd = e["usd"].as_f64().unwrap_or(0.0);
        let pool = e["pool"].as_str().unwrap_or("").to_lowercase();
        match e["what"].as_str().unwrap_or("") {
            "add" => out.entry(pool).or_default().push(Flow { ts, usd }),
            "withdraw" => {
                out.entry(pool).or_default().push(Flow { ts, usd: -usd });
                if let Some(to) = e["to"].as_str() {
                    out.entry(to.to_lowercase()).or_default().push(Flow { ts, usd });
                }
            }
            _ => {}
        }
    }
    out
}

/// A window's figures from samples (oldest first) and flows.
pub fn window(samples: &[Sample], flows: &[Flow], from: i64, to: i64) -> Value {
    let inside: Vec<&Sample> = samples.iter().filter(|s| s.ts >= from && s.ts <= to).collect();
    let (Some(a), Some(b)) = (inside.first(), inside.last()) else { return json!({"complete": false, "reason": "no samples in the window"}) };
    if b.ts <= a.ts {
        return json!({"complete": false, "reason": "one sample only"});
    }
    let fl: Vec<&Flow> = flows.iter().filter(|f| f.ts > a.ts && f.ts <= b.ts).collect();
    let net: f64 = fl.iter().map(|f| f.usd).sum();
    let span = (b.ts - a.ts) as f64;
    let weighted: f64 = fl.iter().map(|f| f.usd * (b.ts - f.ts) as f64 / span).sum();
    let pnl = b.capital_usd - a.capital_usd - net;
    let base = a.capital_usd + weighted;
    let ret = if base > 0.0 { Some(pnl / base) } else { None };
    // against holding, in token1: the start mix (scaled to the capital), plus what was added, held
    let vs = (|| {
        let (u1a, u1b) = (a.u1()?, b.u1()?);
        if a.price <= 0.0 || b.price <= 0.0 {
            return None;
        }
        let k = a.capital_usd / a.pos_usd.max(1e-9);
        let (h0, h1) = (a.a0 * k, a.a1 * k);
        let mut hold_t1 = h0 * b.price + h1;
        // money added is held at the mix of the moment it came in (the start mix), in token1 then
        for f in &fl {
            let s = inside.iter().rev().find(|s| s.ts <= f.ts).copied().unwrap_or(a);
            let u1 = s.u1().unwrap_or(u1a);
            let mix_val_t1 = h0 * s.price + h1;
            if mix_val_t1 > 0.0 {
                let units = (f.usd / u1) / mix_val_t1;
                hold_t1 += units * (h0 * b.price + h1);
            }
        }
        let end_t1 = b.capital_usd / u1b;
        let excess_t1 = end_t1 - hold_t1;
        let base_t1 = base / u1a;
        Some((excess_t1 * u1b, if base_t1 > 0.0 { excess_t1 / base_t1 } else { 0.0 }))
    })();
    json!({
        "complete": true, "from": a.ts, "to": b.ts, "start_usd": a.capital_usd, "end_usd": b.capital_usd, "net_flows_usd": net,
        "pnl_usd": pnl, "return": ret, "vs_hold_usd": vs.map(|x| x.0), "vs_hold": vs.map(|x| x.1), "samples": inside.len(),
    })
}

/// Every configured pool's figures over 24 h, 7 d and 30 d.
pub fn all() -> Value {
    refresh();
    let now = super::now();
    let fl = flows(now - 31 * 86_400);
    let g = STORE.lock().unwrap();
    let Some(st) = g.as_ref() else { return json!({}) };
    let mut out = serde_json::Map::new();
    for p in crate::settings::load_pools() {
        let addr = p["pool"].as_str().unwrap_or("").to_lowercase();
        let empty = vec![];
        let s = st.samples.get(&addr).unwrap_or(&empty);
        let f = fl.get(&addr).cloned().unwrap_or_default();
        out.insert(addr.clone(), json!({
            "name": p["name"],
            "h24": window(s, &f, now - 86_400, now),
            "d7": window(s, &f, now - 7 * 86_400, now),
            "d30": window(s, &f, now - 30 * 86_400, now),
            "capital_usd": s.last().map(|x| x.capital_usd),
        }));
    }
    Value::Object(out)
}

/// One pool's figures for a window ending now.
pub fn pool(addr: &str, seconds: i64) -> Value {
    refresh();
    let now = super::now();
    let addr = addr.to_lowercase();
    let fl = flows(now - seconds - 3600).remove(&addr).unwrap_or_default();
    let g = STORE.lock().unwrap();
    let s = g.as_ref().and_then(|st| st.samples.get(&addr).cloned()).unwrap_or_default();
    window(&s, &fl, now - seconds, now)
}

/// The pool's samples (for the shadow books and the snapshot).
pub fn samples(addr: &str) -> Vec<Sample> {
    refresh();
    STORE.lock().unwrap().as_ref().and_then(|st| st.samples.get(&addr.to_lowercase()).cloned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(ts: i64, cap: f64, a0: f64, a1: f64, price: f64, u1: f64) -> Sample {
        Sample { ts, capital_usd: cap, a0, a1, pos_usd: (a0 * price + a1) * u1, price }
    }

    #[test]
    fn money_added_is_not_profit() {
        // 1000 → 1600 with 500 added halfway: 100 earned
        let v = vec![s(0, 1000.0, 500.0, 500.0, 1.0, 1.0), s(100, 1600.0, 800.0, 800.0, 1.0, 1.0)];
        let w = window(&v, &[Flow { ts: 50, usd: 500.0 }], 0, 100);
        assert!((w["pnl_usd"].as_f64().unwrap() - 100.0).abs() < 1e-9);
        // modified Dietz: 100 / (1000 + 500 × 0.5)
        assert!((w["return"].as_f64().unwrap() - 0.08).abs() < 1e-9);
    }

    #[test]
    fn the_pairs_own_price_is_not_a_loss_against_holding() {
        // ETH pair: both tokens fall 10% in USD, the pool ratio is unchanged, nothing earned
        let a = s(0, 2000.0, 0.5, 0.5, 1.0, 2000.0);
        let b = s(100, 1800.0, 0.5, 0.5, 1.0, 1800.0);
        let w = window(&[a, b], &[], 0, 100);
        assert!((w["pnl_usd"].as_f64().unwrap() + 200.0).abs() < 1e-6);
        assert!(w["vs_hold_usd"].as_f64().unwrap().abs() < 1e-6, "{w}");
    }

    #[test]
    fn a_strategy_loss_shows_against_holding() {
        // the ratio moved 10% and the position sold the riser: worse than holding the start mix
        let a = s(0, 1000.0, 500.0, 500.0, 1.0, 1.0);
        let b = s(100, 1000.0, 0.0, 1000.0, 1.1, 1.0);
        let w = window(&[a, b], &[], 0, 100);
        // holding: 500 × 1.1 + 500 = 1050 → 50 behind
        assert!((w["vs_hold_usd"].as_f64().unwrap() + 50.0).abs() < 1e-6, "{w}");
    }
}
