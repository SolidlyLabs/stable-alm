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
//! | rules | the rules manager: the frozen strategy re-tuned every week by the auto optimizer on the 4 weeks before, walk-forward over the period (deterministic research, no agent) |
//! | ladder | the app's ladder, backtested over the period |
//! | hold | holding the pool's tokens |
//!
//! The agent's net edge is actual minus static, less the API spend over the period; its edge over
//! the rules manager (`edge_vs_rules`) is what the agent's research adds over the optimizer's own,
//! and is the comparison the release should rest on (plan section 17, 9a). The report is rebuilt
//! once a day (two backtests and one walk-forward per pool, 2 workers). Its verdict follows the
//! extension rules: under 30 days "too early"; then "keep" when the net edge is positive on at
//! least two thirds of the pools and in total, "extend" when it is positive in total only, else
//! "stop"; `verdict_vs_rules` applies the same rule to the edge over the rules manager.

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

/// The rules manager's return over [from, now) as a share of capital: the frozen strategy re-tuned
/// every 7 days on the 4 weeks before, by the same walk-forward the Strategy lab and the agent's
/// optimize tool use. Weekly periods are laid so the tested ones start at the freeze.
fn rules_book(p: &Value, from: i64, now: i64, cap: f64) -> Result<(f64, Value), String> {
    let weeks = ((now - from) as f64 / (7.0 * 86_400.0)).floor() as i64;
    if weeks < 1 {
        return Err("needs a full week of the forward test".into());
    }
    let until = from + weeks * 7 * 86_400;
    let spec = json!({"auto": true, "mode": "walk", "holdout": true, "objective": "return", "periods": weeks + 4, "train": 4, "runs": 30,
                      "workers": 2, "current_in_sample": false});
    let pool = p["pool"].as_str().unwrap_or("");
    let cid = p["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
    let r = backtest(json!({"chain_id": cid, "pool": pool, "days": (weeks + 4) * 7, "until": until, "capital_usd": cap, "strategy": p["strategy"],
                            "costs": super::sgate::costs(cid, pool, 1.0), "optimize": spec}))?;
    let w = &r["optimize"]["walk"];
    let pd = w["period_days"].as_f64().unwrap_or(7.0);
    let row = |name: &str| w["rows"].as_array().into_iter().flatten().find(|x| x["name"] == name).map(|x| {
        x["values"].as_array().into_iter().flatten().filter_map(|v| v.as_f64()).map(|v| v / 100.0 * pd / 365.0).sum::<f64>()
    });
    let rules = row("re-tuned every period").ok_or("the walk-forward tested no period")?;
    Ok((rules, json!({"weeks": weeks, "until": until, "rules": rules, "frozen_same_weeks": row("current settings"), "steps": w["steps"], "verdict": r["optimize"]["suggestion"]["why"]})))
}

/// Rebuild the report (blocking: a few backtests and a walk-forward per pool).
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
    let mut rule_edges = vec![];
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
        let rb = rules_book(p, from, now, cap);
        let rules = rb.as_ref().ok().map(|r| r.0);
        let edge_rules = act.zip(rules).map(|(a, r)| a - r);
        if let Some(e) = edge_rules {
            rule_edges.push(e);
        }
        rows.push(json!({"pool": addr, "name": p["name"], "actual": act, "static": st, "rules": rules, "ladder": lad, "hold": hold, "edge_vs_static": edge,
                         "edge_vs_rules": edge_rules, "rules_detail": rb.as_ref().map(|r| r.1.clone()).unwrap_or_else(|e| json!({"error": e})),
                         "actual_detail": actual, "backtest_error": bt.err()}));
    }
    let api = super::budget::spent_since(&x, from);
    let cap_total: f64 = f["pools"].as_array().into_iter().flatten().filter_map(|p| p["capital_usd"].as_f64()).sum();
    let api_share = if cap_total > 0.0 { api / cap_total } else { 0.0 };
    // the API spend is shared out over the pools by capital
    let net: Vec<f64> = edges.iter().map(|e| e - api_share).collect();
    let net_rules: Vec<f64> = rule_edges.iter().map(|e| e - api_share).collect();
    let r = json!({"frozen": true, "freeze_hash": f["hash"], "from": from, "to": now, "days": days, "pools": rows,
                   "api_usd": api, "api_share": api_share, "net_edges": net, "verdict": verdict(days, &net),
                   "net_edges_vs_rules": net_rules, "verdict_vs_rules": verdict(days, &net_rules), "built": now});
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
