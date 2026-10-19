//! Baselines and the scored report (plan section 12).
//!
//! The forward test starts with `freeze`: each pool's strategy and capital are stored with their
//! hash (data/agent/shadow_freeze.json, and the journal). From then on the report compares what the
//! agent did with what would have happened without it, per pool, over the same period:
//!
//! | Book | What |
//! |---|---|
//! | actual | the pool's real result, cash-flow adjusted (perf) |
//! | static | the frozen strategy, backtested over the period (no agent) |
//! | ladder | the app's ladder, backtested over the period |
//! | hold | holding the pool's tokens |
//!
//! The agent's net edge is actual minus static, less the API spend over the period. The report is
//! rebuilt once a day (two backtests per pool, at most 2 at a time). Its verdict follows the
//! extension rules: under 30 days "too early"; then "keep" when the net edge is positive on at
//! least two thirds of the pools and in total, "extend" when it is positive in total only, else
//! "stop".

use serde_json::{json, Value};

const FREEZE: &str = "shadow_freeze.json";
const REPORT: &str = "shadow_report.json";
pub const MIN_DAYS: f64 = 30.0;

pub fn freeze() -> Result<Value, String> {
    let pools: Vec<Value> = crate::settings::load_pools().into_iter().filter(|p| p["enabled"] == true).map(|p| {
        let addr = p["pool"].as_str().unwrap_or("").to_lowercase();
        json!({"chain_id": crate::worker::pool_chain(&p), "pool": addr, "name": p["name"], "strategy": p["strategy"],
               "strategy_hash": super::plan::strategy_hash(&p["strategy"]), "capital_usd": super::perf::samples(&addr).last().map(|s| s.capital_usd)})
    }).collect();
    if pools.is_empty() {
        return Err("no pool is on".into());
    }
    let f = json!({"ts": super::now(), "pools": pools});
    let h = super::sha256_hex(&f.to_string());
    let f = json!({"ts": f["ts"], "pools": f["pools"], "hash": h});
    super::write_json(FREEZE, &f)?;
    let _ = super::write_json(REPORT, &json!(null));
    super::journal::record("forward_test_frozen", json!({"hash": h, "pools": f["pools"].as_array().map(|a| a.len())}));
    Ok(f)
}

pub fn frozen() -> Value {
    super::read_json(FREEZE, json!(null))
}

/// The verdict from per-pool net edges (as shares of capital) and the period's length.
pub fn verdict(days: f64, edges: &[f64]) -> &'static str {
    if days < MIN_DAYS {
        return "too early";
    }
    if edges.is_empty() {
        return "no data";
    }
    let total: f64 = edges.iter().sum();
    let pos = edges.iter().filter(|e| **e > 0.0).count() as f64;
    if total > 0.0 && pos >= (edges.len() as f64 * 2.0 / 3.0).ceil() {
        "keep"
    } else if total > 0.0 {
        "extend"
    } else {
        "stop"
    }
}

fn backtest(req: Value) -> Result<Value, String> {
    let wallet = crate::settings::load_settings()["wallet"].as_str().unwrap_or("").to_string();
    let mut tries = 0;
    let id = loop {
        match crate::backtest::start(req.clone(), wallet.clone()) {
            Ok(id) => break id,
            Err(e) if e.contains("already running") && tries < 90 => {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_secs(10));
            }
            Err(e) => return Err(e),
        }
    };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(1200);
    loop {
        let j = crate::backtest::job(id).ok_or("the backtest disappeared")?;
        match j["state"].as_str() {
            Some("done") => return Ok(j["result"].clone()),
            Some("error") => return Err(j["error"].as_str().unwrap_or("backtest failed").into()),
            _ if std::time::Instant::now() > until => return Err("the backtest took too long".into()),
            _ => std::thread::sleep(std::time::Duration::from_secs(3)),
        }
    }
}

/// Rebuild the report (blocking: a few backtests).
pub fn build() -> Value {
    let f = frozen();
    if !f.is_object() {
        return json!({"frozen": false, "note": "the forward test has not started (freeze it on the Experimental tab)"});
    }
    let now = super::now();
    let from = f["ts"].as_i64().unwrap_or(now);
    let days = (now - from) as f64 / 86_400.0;
    let x = super::settings();
    let mut rows = vec![];
    let mut edges = vec![];
    for p in f["pools"].as_array().into_iter().flatten() {
        let addr = p["pool"].as_str().unwrap_or("");
        let samples = super::perf::samples(addr);
        let flows = super::perf::flows(from - 3600).remove(addr).unwrap_or_default();
        let actual = super::perf::window(&samples, &flows, from, now);
        let cap = p["capital_usd"].as_f64().unwrap_or(1000.0).max(10.0);
        let bt_days = days.ceil().clamp(1.0, 730.0) as i64;
        let bt = backtest(json!({"chain_id": p["chain_id"], "pool": addr, "days": bt_days, "capital_usd": cap, "strategy": p["strategy"],
                                 "compare": [{"id": "ladder", "name": "Ladder", "strategy": {"type": "ladder"}}], "hold": true}));
        let share = |v: &Value, base: &Value| v["pnl"].as_f64().zip(base["start_value"].as_f64()).filter(|(_, c)| *c > 0.0).map(|(p, c)| p / c);
        let (st, lad, hold) = match &bt {
            Ok(r) => (share(&r["runs"]["main"], &r["runs"]["main"]), r["runs"]["compare"].as_array().and_then(|a| a.first()).and_then(|v| share(v, &r["runs"]["main"])), share(&r["runs"]["hold"], &r["runs"]["main"])),
            Err(_) => (None, None, None),
        };
        let act = actual["return"].as_f64();
        let edge = act.zip(st).map(|(a, s)| a - s);
        if let Some(e) = edge {
            edges.push(e);
        }
        rows.push(json!({"pool": addr, "name": p["name"], "actual": act, "static": st, "ladder": lad, "hold": hold, "edge_vs_static": edge,
                         "actual_detail": actual, "backtest_error": bt.err()}));
    }
    let api = super::budget::spent_since(&x, from);
    let cap_total: f64 = f["pools"].as_array().into_iter().flatten().filter_map(|p| p["capital_usd"].as_f64()).sum();
    let api_share = if cap_total > 0.0 { api / cap_total } else { 0.0 };
    // the API spend is shared out over the pools by capital
    let net: Vec<f64> = edges.iter().map(|e| e - api_share).collect();
    let r = json!({"frozen": true, "freeze_hash": f["hash"], "from": from, "to": now, "days": days, "pools": rows,
                   "api_usd": api, "api_share": api_share, "net_edges": net, "verdict": verdict(days, &net), "built": now});
    let _ = super::write_json(REPORT, &r);
    r
}

pub fn report() -> Value {
    super::read_json(REPORT, json!(null))
}

/// Rebuild the report once a day while the forward test runs.
pub fn start() {
    std::thread::Builder::new().name("agent-shadow".into()).spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(1800));
        if !super::enabled() || !frozen().is_object() {
            continue;
        }
        let last = report()["built"].as_i64().unwrap_or(0);
        if super::now() - last >= 86_400 {
            let _ = std::panic::catch_unwind(build);
        }
    }).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts() {
        assert_eq!(verdict(10.0, &[0.1]), "too early");
        assert_eq!(verdict(40.0, &[0.01, 0.02, -0.001]), "keep");
        assert_eq!(verdict(40.0, &[0.05, -0.01, -0.01]), "extend");
        assert_eq!(verdict(40.0, &[-0.01, 0.001]), "stop");
    }
}
