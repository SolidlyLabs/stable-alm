//! The risk controller (plan section 4): code only, outside every strategy, on a wall-clock timer
//! every minute while experimental features are on and risk.mode is not "off".
//!
//! | Condition | monitor | act |
//! |---|---|---|
//! | no fresh exchange price for a pool's token (risk.stale_min) | lock, alert | lock, alert |
//! | the pool's price and the exchanges' disagree beyond the class threshold | lock, alert | lock, alert |
//! | a source disagrees with the other exchange (the one further from the pool is suspect for 1 h) | alert | alert |
//! | a stablecoin off its peg (risk.peg_pct) | lock, alert | emergency exit to the first healthy safe asset, else keep the tokens |
//! | the pool behind holding its own tokens (risk.loss_24h_pct, loss_7d_pct) | lock, alert | exit to Unallocated (switched off) |
//! | gas below the chain's minimum | lock the chain, alert | lock the chain, alert |
//! | an emergency exit's transaction stuck (risk.stuck_tx_s) | alert | replace it at the same nonce |
//!
//! The exchanges are the off-chain class; the pool (and Liquidity Lens, once real) the on-chain one.
//! A disagreement between the classes always locks: on-chain agreement never outvotes an exchange.
//! A lock lifts when its condition has cleared and the user confirms (or the user's timer runs out).
//! At most one emergency exit per pool per hour. Every action is written to the journal.
//!
//! Exits are not done here: they are queued (a withdrawal request with the emergency's cost bound, or
//! the pool switched off) and the worker is woken, so its one signing thread does them next.

use serde_json::{json, Value};
use std::sync::Arc;

use super::{locks, refdata};

pub fn pool_price(cid: u64, pool: &str) -> Option<(f64, String, String)> {
    let (t0, t1) = crate::chain::pool_tokens(cid, pool).ok()?;
    let (m0, m1) = (crate::prices::meta(cid, &t0).ok()?, crate::prices::meta(cid, &t1).ok()?);
    let (_, tick, _) = crate::chain::slot0(cid, pool).ok()?;
    Some((1.0001f64.powf(tick as f64) * 10f64.powi(m0.decimals as i32 - m1.decimals as i32), m0.symbol, m1.symbol))
}

/// Exchange sources marked suspect (they disagree with the other exchange; the one further from the
/// pool), each for an hour: data/agent/suspects.json {"<source>:<asset>": until}.
pub fn suspects() -> Value {
    let now = super::now();
    let mut v = super::read_json("suspects.json", json!({}));
    if let Some(o) = v.as_object_mut() {
        o.retain(|_, t| t.as_i64().unwrap_or(0) > now);
    }
    v
}

fn mark_suspect(source: &str, asset: &str) -> bool {
    let mut v = suspects();
    let k = format!("{source}:{asset}");
    let new = v.get(&k).is_none();
    v[k] = json!(super::now() + 3600);
    let _ = super::write_json("suspects.json", &v);
    new
}

/// One condition's outcome for the journal and the dashboard.
fn found(out: &mut Vec<Value>, cid: u64, pool: Option<&str>, condition: &str, severity: &str, detail: String) {
    out.push(json!({"chain_id": cid, "pool": pool, "condition": condition, "severity": severity, "detail": detail}));
}

/// The last emergency exit of a pool (unix seconds), for the once-an-hour limit.
fn last_exit(pool: &str) -> i64 {
    super::read_json("risk_state.json", json!({}))["last_exit"][pool].as_i64().unwrap_or(0)
}

fn note_exit(pool: &str) {
    let mut v = super::read_json("risk_state.json", json!({}));
    v["last_exit"][pool] = json!(super::now());
    let _ = super::write_json("risk_state.json", &v);
}

/// Queue an emergency exit of a pool: its whole capital sold to `safe` within the emergency bound,
/// or (no healthy safe asset) the pool switched off, so its positions are withdrawn and the tokens
/// kept as they are.
pub fn emergency_exit(cid: u64, pool: &str, name: &str, safe: Option<&str>, reason: &str, x: &Value, worker: &crate::worker::Worker) {
    if super::now() - last_exit(pool) < 3600 {
        return;
    }
    note_exit(pool);
    locks::start_emergency(cid, &[pool.to_string()], reason);
    let cap = super::perf::pool(pool, 3600)["end_usd"].as_f64()
        .or_else(|| super::perf::samples(pool).last().map(|s| s.capital_usd)).unwrap_or(0.0);
    match safe {
        Some(t) if cap > 1.0 => {
            let id = crate::capital::push(json!({"kind": "withdraw", "chain_id": cid, "pools": [pool], "usd": cap * 1.02, "to": null, "safe": t,
                                                  "max_cost_bp": x["execution"]["max_emergency_bp"].as_f64().unwrap_or(100.0), "emergency": true}));
            super::journal::record("emergency_exit", json!({"chain_id": cid, "pool": pool, "name": name, "to": t, "usd": cap, "request": id, "reason": reason}));
        }
        _ => {
            switch_off(pool, reason);
            super::journal::record("emergency_exit", json!({"chain_id": cid, "pool": pool, "name": name, "to": "kept as tokens (pool switched off)", "reason": reason}));
        }
    }
    super::notify::alert("severe", &format!("Emergency exit: {name}"), reason);
    super::sched::trigger("risk_action", "severe", &format!("{name}: {reason}"));
    worker.request(false);
}

/// Switch a pool off in pools.json (its positions are withdrawn on the next cycle).
pub fn switch_off(pool: &str, reason: &str) {
    let mut pools = crate::settings::load_pools();
    let mut changed = false;
    for p in pools.iter_mut() {
        if p["pool"].as_str().map(|a| a.eq_ignore_ascii_case(pool)).unwrap_or(false) && p["enabled"] == true {
            p["enabled"] = json!(false);
            changed = true;
        }
    }
    if changed {
        match crate::settings::save_pools(&pools) {
            Ok(()) => crate::warn!("risk", "pool {pool} switched off: {reason}"),
            Err(e) => crate::error!("risk", "pool {pool} could not be switched off: {e}"),
        }
    }
}

/// One evaluation of every condition. Returns what it found.
pub fn evaluate(worker: &crate::worker::Worker) -> Vec<Value> {
    let x = super::settings();
    let mode = x["risk"]["mode"].as_str().unwrap_or("off").to_string();
    let act = mode == "act";
    let r = &x["risk"];
    let wallet = crate::settings::load_settings()["wallet"].as_str().unwrap_or("").to_lowercase();
    let lock_hours = r["lock_hours"].as_f64().unwrap_or(24.0);
    let stale_s = (r["stale_min"].as_f64().unwrap_or(10.0) * 60.0) as i64;
    let mut out = vec![];
    let mut chains: Vec<u64> = vec![];
    for p in crate::settings::load_pools() {
        let cid = crate::worker::pool_chain(&p);
        let addr = p["pool"].as_str().unwrap_or("").to_lowercase();
        let name = p["name"].as_str().unwrap_or(&addr).to_string();
        if !chains.contains(&cid) {
            chains.push(cid);
        }
        let Some((price, s0, s1)) = pool_price(cid, &addr) else {
            found(&mut out, cid, Some(&addr), "pool_unreadable", "normal", format!("{name}: the pool's price could not be read"));
            continue;
        };
        let held = p["enabled"] == true || super::perf::samples(&addr).last().map(|s| s.capital_usd > 1.0).unwrap_or(false);
        // 1. stale reference data, and 2. disagreement
        let m0 = refdata::mids(&s0);
        let m1 = refdata::mids(&s1);
        let fresh = |m: &Value| m["sources"].as_array().map(|a| a.iter().any(|s| s["stale"] == false && s["age_s"].as_i64().map(|t| t <= stale_s).unwrap_or(false))).unwrap_or(false);
        if !fresh(&m0) || !fresh(&m1) {
            let why = format!("{name}: no fresh exchange price for {} within {} min", if !fresh(&m0) { &s0 } else { &s1 }, stale_s / 60);
            found(&mut out, cid, Some(&addr), "stale", "normal", why.clone());
            if held && locks::set(cid, Some(&addr), "stale", &why) {
                super::notify::alert("normal", "Reference prices stale", &why);
            }
        } else {
            locks::cleared(cid, Some(&addr), "stale", lock_hours);
            let d = refdata::pool_vs_cex(price, &s0, &s1);
            let lim = refdata::disagree_bp(&x, &s0, &s1);
            match d["diff_bp"].as_f64() {
                Some(bp) if bp.abs() > lim => {
                    let why = format!("{name}: the pool's price is {bp:.0} bp from the exchanges' (limit {lim:.0} bp)");
                    found(&mut out, cid, Some(&addr), "disagree", "normal", why.clone());
                    if held && locks::set(cid, Some(&addr), "disagree", &why) {
                        super::notify::alert("normal", "Price disagreement", &why);
                        super::sched::trigger("disagree", "normal", &why);
                    }
                }
                Some(_) => locks::cleared(cid, Some(&addr), "disagree", lock_hours),
                None => {}
            }
            // a source far from the other exchange: the one further from the pool is suspect
            for (sym, m) in [(&s0, &m0), (&s1, &m1)] {
                let srcs: Vec<(String, f64)> = m["sources"].as_array().into_iter().flatten()
                    .filter_map(|s| Some((s["source"].as_str()?.to_string(), s["mid"].as_f64()?))).collect();
                if srcs.len() == 2 {
                    let gap = (srcs[0].1 / srcs[1].1 - 1.0).abs() * 1e4;
                    let lim1 = refdata::disagree_bp(&x, sym, sym);
                    if gap > lim1 {
                        // the pool's own view of this token, through the other token's exchange price
                        let other_mid = if sym == &s0 { m1["median"].as_f64().map(|u1| price * u1) } else { m0["median"].as_f64().map(|u0| u0 / price) };
                        if let Some(pv) = other_mid {
                            let worst = if (srcs[0].1 - pv).abs() > (srcs[1].1 - pv).abs() { &srcs[0].0 } else { &srcs[1].0 };
                            if mark_suspect(worst, m["asset"].as_str().unwrap_or("")) {
                                let why = format!("{} {sym}: {gap:.0} bp from the other exchange; the one further from the pool is suspect for an hour", worst);
                                super::notify::alert("normal", "Exchange price suspect", &why);
                                found(&mut out, cid, Some(&addr), "suspect_source", "normal", why);
                            }
                        }
                    }
                }
            }
        }
        // 3. token health
        for sym in [&s0, &s1] {
            let h = refdata::health(sym, &x);
            if h["healthy"] == false {
                let why = format!("{name}: {sym} is {:.2}% off its peg (limit {}%)", h["peg_dev_pct"].as_f64().unwrap_or(0.0), r["peg_pct"]);
                found(&mut out, cid, Some(&addr), "token_health", "severe", why.clone());
                let new = locks::set(cid, Some(&addr), "token_health", &why);
                if act && held {
                    let safe = super::exposure::safe_assets(cid).into_iter().find(|t| {
                        let s = crate::prices::meta(cid, t).map(|m| m.symbol).unwrap_or_default();
                        !s.eq_ignore_ascii_case(sym) && refdata::health(&s, &x)["healthy"] == true
                    });
                    emergency_exit(cid, &addr, &name, safe.as_deref(), &why, &x, worker);
                } else if new {
                    super::notify::alert("severe", "Token off its peg", &why);
                }
            } else if h["healthy"] == true {
                locks::cleared(cid, Some(&addr), "token_health", lock_hours);
            }
        }
        // 4. loss against holding
        if held {
            for (secs, key, label) in [(86_400i64, "loss_24h_pct", "24 h"), (7 * 86_400, "loss_7d_pct", "7 days")] {
                let w = super::perf::pool(&addr, secs);
                let lim = r[key].as_f64().unwrap_or(3.0) / 100.0;
                // only a window the samples really cover (a new pool's first hours do not count as 7 days)
                let covered = w["from"].as_i64().zip(w["to"].as_i64()).map(|(a, b)| (b - a) as f64 >= secs as f64 * 0.8).unwrap_or(false);
                if let Some(v) = w["vs_hold"].as_f64().filter(|_| covered) {
                    let cond = format!("loss_{key}");
                    if v < -lim {
                        let why = format!("{name}: {:.2}% behind holding its own tokens over {label} (limit {:.1}%)", v * 100.0, lim * 100.0);
                        found(&mut out, cid, Some(&addr), &cond, "severe", why.clone());
                        let new = locks::set(cid, Some(&addr), &cond, &why);
                        if act {
                            if p["enabled"] == true && super::now() - last_exit(&addr) >= 3600 {
                                note_exit(&addr);
                                switch_off(&addr, &why);
                                super::journal::record("loss_exit", json!({"chain_id": cid, "pool": addr, "name": name, "reason": why}));
                                super::notify::alert("severe", &format!("Loss limit: {name} switched off"), &why);
                                super::sched::trigger("risk_action", "severe", &why);
                                worker.request(false);
                            }
                        } else if new {
                            super::notify::alert("severe", "Loss limit reached", &why);
                        }
                    } else {
                        locks::cleared(cid, Some(&addr), &cond, lock_hours);
                    }
                }
            }
        }
    }
    // 5. gas, and 6. stuck transactions of an emergency
    for cid in chains {
        if wallet.is_empty() {
            break;
        }
        let g = refdata::gas(cid, &wallet);
        if g["low"] == true {
            let why = format!("chain {cid}: {:.5} {} for gas, below the minimum {}", g["native_balance"].as_f64().unwrap_or(0.0), g["symbol"].as_str().unwrap_or(""), g["min_native"]);
            found(&mut out, cid, None, "gas", "normal", why.clone());
            if locks::set(cid, None, "gas", &why) {
                super::notify::alert("normal", "Gas low", &why);
            }
        } else if g["low"] == false {
            locks::cleared(cid, None, "gas", lock_hours);
        }
        if let (Some(e), Some(age)) = (locks::emergency(), crate::chain::pending_age(cid, &wallet)) {
            let stuck = r["stuck_tx_s"].as_f64().unwrap_or(120.0) as i64;
            if e["chain_id"].as_u64() == Some(cid) && age >= stuck {
                let why = format!("chain {cid}: a transaction has been pending {age} s during an emergency");
                found(&mut out, cid, None, "stuck_tx", "severe", why.clone());
                if act && crate::settings::load_settings()["execute"] == true {
                    match crate::signer::load_account() {
                        Ok(acct) => match crate::chain::replace_stuck(cid, &acct, r["emergency_gwei"].as_f64().unwrap_or(2.0)) {
                            Ok(Some(h)) => { super::journal::record("replace_stuck", json!({"chain_id": cid, "hash": h})); }
                            Ok(None) => {}
                            Err(err) => super::notify::alert("severe", "Stuck transaction not replaced", &err),
                        },
                        Err(err) => super::notify::alert("severe", "Stuck transaction not replaced", &err),
                    }
                } else {
                    super::notify::alert("severe", "Transaction stuck during an emergency", &why);
                }
            }
        }
    }
    // the emergency ends once its pools hold no positions
    if let Some(e) = locks::emergency() {
        let acct = worker.status.lock().unwrap()["account"].clone();
        let names: Vec<(String, String)> = crate::settings::load_pools().iter()
            .filter_map(|p| Some((p["pool"].as_str()?.to_lowercase(), p["name"].as_str()?.to_string()))).collect();
        let open = e["pools"].as_array().into_iter().flatten().filter_map(|p| p.as_str()).any(|p| {
            names.iter().find(|n| n.0 == p).map(|n| acct["pools"][&n.1]["positions"].as_array().map(|a| !a.is_empty()).unwrap_or(false)).unwrap_or(false)
        });
        let pending = crate::capital::requests().iter().any(|q| q["emergency"] == true);
        if !open && !pending {
            locks::end_emergency();
            super::journal::record("emergency_end", e);
        }
    }
    out
}

/// The controller's loop: a heartbeat and an evaluation every minute while it is on.
pub fn start(worker: Arc<crate::worker::Worker>) {
    std::thread::Builder::new().name("risk".into()).spawn(move || loop {
        let x = super::settings();
        if super::enabled_in(&x) && x["risk"]["mode"].as_str().unwrap_or("off") != "off" {
            let t0 = std::time::Instant::now();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| evaluate(&worker)));
            match r {
                Ok(found) => {
                    gate_beat();
                    let _ = super::write_json("risk_last.json", &json!({"ts": super::now(), "found": found, "ms": t0.elapsed().as_millis() as u64,
                                                                         "mode": x["risk"]["mode"]}));
                }
                Err(_) => crate::error!("risk", "risk controller evaluation panicked; the write gate stops signing once its heartbeat is old"),
            }
            locks::tidy();
        }
        std::thread::sleep(std::time::Duration::from_secs(60));
    }).ok();
}

fn gate_beat() {
    super::gate::beat();
}

pub fn last() -> Value {
    super::read_json("risk_last.json", json!(null))
}
