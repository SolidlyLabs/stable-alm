//! The write gate (plan section 2): what every signer asks before a transaction, and the engine
//! before it plans a move.
//!
//! Every token-moving action of the app is signed by one thread, the worker's cycle (pool runs,
//! claims, capital moves, plan steps and emergency exits are all work it does in turn); the risk
//! controller, the coordinator and Telegram only queue work and wake it. That is what keeps the
//! capital book's before/after attribution correct: nothing else touches the wallet while a pool runs.
//!
//! Refused while experimental features are on and:
//! - a safety lock covers the pool or its chain, or the pool is in an emergency exit, and the
//!   transaction would add to a position (withdrawals, swaps out and claims pass);
//! - an engine cap is reached (caps.enabled): rebalances per day (the pool's own setting), capital
//!   turned over today, execution cost over 7 days;
//! - the risk controller's heartbeat is older than risk.heartbeat_s while risk.mode is not off:
//!   a stopped controller cannot protect, so nothing is signed until it runs again.

use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// adds liquidity: a mint or an increase
    Deposit,
    /// a burn, collect or decrease
    Withdraw,
    Swap,
    Approve,
    Claim,
    Other,
}

pub fn classify(what: &str) -> Kind {
    let w = what.to_lowercase();
    if w.starts_with("mint") || w.starts_with("increase") || w.starts_with("add") || w.contains("deposit") {
        Kind::Deposit
    } else if w.starts_with("burn") || w.starts_with("collect") || w.starts_with("decrease") || w.contains("withdraw") {
        Kind::Withdraw
    } else if w.contains("swap") || w.starts_with("sell") {
        Kind::Swap
    } else if w.starts_with("approve") {
        Kind::Approve
    } else if w.contains("claim") {
        Kind::Claim
    } else {
        Kind::Other
    }
}

fn heartbeat_path() -> std::path::PathBuf {
    super::dir().join("risk_heartbeat")
}

/// The risk controller says it is alive.
pub fn beat() {
    let _ = std::fs::write(heartbeat_path(), super::now().to_string());
}

/// Seconds since the controller's last heartbeat (None: never ran).
pub fn heartbeat_age() -> Option<i64> {
    let t: i64 = std::fs::read_to_string(heartbeat_path()).ok()?.trim().parse().ok()?;
    Some(super::now() - t)
}

/// Today's and the last 7 days' figures against the engine caps.
pub fn caps(cid: u64, pool: &str, capital_usd: f64, max_actions: i64, x: &Value) -> Value {
    let now = super::now();
    let day = now / 86_400;
    let (mut rebalances, mut turnover) = (0i64, 0.0f64);
    for e in crate::engine::log_tail(8 << 20) {
        if e["chain_id"].as_u64().unwrap_or(crate::chains::BASE) != cid || !e["pool"].as_str().map(|p| p.eq_ignore_ascii_case(pool)).unwrap_or(false) {
            continue;
        }
        if e["ts"].as_i64().unwrap_or(0) / 86_400 != day || e["live"] != true {
            continue;
        }
        match e["kind"].as_str() {
            Some("rebalance") => rebalances += 1,
            Some("plan") => turnover += e["turnover"].as_f64().unwrap_or(0.0),
            _ => {}
        }
    }
    let wallet = crate::settings::load_settings()["wallet"].as_str().unwrap_or("").to_lowercase();
    let led = crate::ledger::summary(&wallet, &[cid]);
    let cost_7d = led[cid.to_string()]["by_pool"][pool.to_lowercase()]["cost_7d_usd"].as_f64();
    let cost_pct = cost_7d.filter(|_| capital_usd > 0.0).map(|c| c / capital_usd * 100.0);
    let c = &x["caps"];
    let (tmax, cmax) = (c["turnover_pct_day"].as_f64().unwrap_or(100.0), c["cost_pct_7d"].as_f64().unwrap_or(0.5));
    let mut hit = vec![];
    if max_actions > 0 && rebalances >= max_actions {
        hit.push(format!("{rebalances} rebalances today (cap {max_actions})"));
    }
    if turnover * 100.0 >= tmax {
        hit.push(format!("{:.0}% of the capital turned over today (cap {tmax:.0}%)", turnover * 100.0));
    }
    if let Some(p) = cost_pct.filter(|p| *p >= cmax) {
        hit.push(format!("execution cost {p:.2}% of the capital over 7 days (cap {cmax}%)"));
    }
    json!({"rebalances_today": rebalances, "max_rebalances": max_actions, "turnover_pct_today": turnover * 100.0, "max_turnover_pct": tmax,
           "cost_7d_usd": cost_7d, "cost_pct_7d": cost_pct, "max_cost_pct_7d": cmax, "hit": hit})
}

/// Why the engine must not start a move or deposit in this pool now (empty: it may).
pub fn engine_problems(cid: u64, pool: &str, capital_usd: f64, max_actions: i64) -> Vec<String> {
    let x = super::settings();
    if !super::enabled_in(&x) {
        return vec![];
    }
    let mut out = vec![];
    if let Some(l) = super::locks::active(cid, Some(pool)) {
        out.push(format!("safety lock: {}", l["reason"].as_str().unwrap_or("")));
    }
    if super::locks::emergency().map(|e| e["pools"].as_array().map(|a| a.iter().any(|p| p.as_str().map(|p| p.eq_ignore_ascii_case(pool)).unwrap_or(false))).unwrap_or(false)).unwrap_or(false) {
        out.push("emergency exit in progress for this pool".into());
    }
    if x["caps"]["enabled"].as_bool().unwrap_or(false) {
        for h in caps(cid, pool, capital_usd, max_actions, &x)["hit"].as_array().into_iter().flatten() {
            out.push(format!("engine cap: {}", h.as_str().unwrap_or("")));
        }
    }
    if let Some(e) = heartbeat_problem(&x) {
        out.push(e);
    }
    out
}

fn heartbeat_problem(x: &Value) -> Option<String> {
    if x["risk"]["mode"].as_str().unwrap_or("off") == "off" {
        return None;
    }
    let max = x["risk"]["heartbeat_s"].as_f64().unwrap_or(300.0) as i64;
    match heartbeat_age() {
        Some(a) if a <= max => None,
        Some(a) => Some(format!("the risk controller has not run for {a} s (limit {max} s)")),
        None => Some("the risk controller has not run yet".into()),
    }
}

/// Asked right before a transaction is signed. Err: do not sign.
pub fn pre_sign(cid: u64, pool: Option<&str>, what: &str) -> Result<(), String> {
    let x = super::settings();
    if !super::enabled_in(&x) {
        return Ok(());
    }
    if let Some(e) = heartbeat_problem(&x) {
        return Err(format!("write gate: {e}"));
    }
    let kind = classify(what);
    if kind == Kind::Deposit {
        if let Some(l) = super::locks::active(cid, pool) {
            return Err(format!("write gate: safety lock ({})", l["reason"].as_str().unwrap_or("")));
        }
        if let (Some(p), Some(e)) = (pool, super::locks::emergency()) {
            if e["pools"].as_array().map(|a| a.iter().any(|q| q.as_str().map(|q| q.eq_ignore_ascii_case(p)).unwrap_or(false))).unwrap_or(false) {
                return Err("write gate: this pool is in an emergency exit".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(classify("mint -10:10"), Kind::Deposit);
        assert_eq!(classify("burn -10:10"), Kind::Withdraw);
        assert_eq!(classify("swap via odos"), Kind::Swap);
        assert_eq!(classify("approve reset"), Kind::Approve);
        assert_eq!(classify("claim"), Kind::Claim);
    }
}
