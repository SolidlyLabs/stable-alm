//! The strategy gate (plan section 9): a strategy a plan puts on a pool is backtested first, on the
//! local event store, by the same backtester the Lab uses (at most 2 at a time, its own limit).
//!
//! | Run | Window | Costs |
//! |---|---|---|
//! | long | the last 45 days | the pool's own (swap, gas, spot check) |
//! | recent | the last 14 days | the same |
//! | thin | the last 45 days | swap costs doubled |
//! | overfit check | the last 45 days | walk-forward and PBO over the strategy's neighbours (advisory) |
//!
//! Each run compares the new strategy with the pool's current one and with holding. The gate passes
//! when the new strategy, net of costs, is no worse than the current one over the long window and
//! not more than 0.25% of capital worse over the recent one, and still beats holding with thin costs
//! (or, on a pool without a strategy, makes money in both long runs). A "likely overfit" verdict, or
//! a strategy found by searching (the plan says so), only asks for approval.

use serde_json::{json, Value};

const LONG_DAYS: i64 = 45;
const RECENT_DAYS: i64 = 14;
const RECENT_TOLERANCE: f64 = 0.0025;

fn wait(id: u64, minutes: u64) -> Result<Value, String> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(minutes * 60);
    loop {
        match crate::backtest::job(id) {
            Some(j) if j["state"] == "done" => return Ok(j["result"].clone()),
            Some(j) if j["state"] == "error" => return Err(j["error"].as_str().unwrap_or("backtest failed").to_string()),
            None => return Err("the backtest disappeared".into()),
            _ => {}
        }
        if std::time::Instant::now() > until {
            return Err("the backtest took too long".into());
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

/// Start a backtest, waiting for a free slot (the backtester runs 2 at a time).
fn run(req: Value) -> Result<Value, String> {
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
    wait(id, 20)
}

fn costs(cid: u64, pool: &str, swap_mult: f64) -> Value {
    let cfg = crate::settings::load_pools().into_iter().find(|p| p["pool"].as_str().map(|a| a.eq_ignore_ascii_case(pool)).unwrap_or(false));
    let spot = cfg.as_ref().and_then(|c| c["max_spot_dev_ticks"].as_f64()).unwrap_or(10.0);
    let gas = super::refdata::gas(cid, crate::settings::load_settings()["wallet"].as_str().unwrap_or(""))["rebalance_usd"].as_f64().unwrap_or(0.02);
    json!({"swap_bps": 0.3 * swap_mult, "stable_swap_bps": 1.0 * swap_mult, "gas_usd": gas, "max_spot_dev_ticks": spot})
}

/// (new, current, hold) net results as a share of capital.
fn shares(r: &Value) -> (Option<f64>, Option<f64>, Option<f64>) {
    let cap = r["runs"]["main"]["start_value"].as_f64().filter(|c| *c > 0.0);
    let f = |v: &Value| cap.and_then(|c| v["pnl"].as_f64().map(|p| p / c));
    (f(&r["runs"]["main"]), r["runs"]["compare"].as_array().and_then(|a| a.first()).and_then(f), f(&r["runs"]["hold"]))
}

/// The verdict from the three runs' shares (the rule above).
pub fn decide(long: (Option<f64>, Option<f64>, Option<f64>), recent: (Option<f64>, Option<f64>, Option<f64>),
              thin: (Option<f64>, Option<f64>, Option<f64>), has_current: bool) -> (bool, String) {
    let (Some(ln), Some(rn), Some(tn)) = (long.0, recent.0, thin.0) else { return (false, "a backtest gave no result".into()) };
    if has_current {
        let (Some(lc), Some(rc)) = (long.1, recent.1) else { return (false, "the current strategy gave no result".into()) };
        if ln + 1e-9 < lc {
            return (false, format!("over {LONG_DAYS} days it makes {:.2}% against the current strategy's {:.2}%", ln * 100.0, lc * 100.0));
        }
        if rn + RECENT_TOLERANCE < rc {
            return (false, format!("over the last {RECENT_DAYS} days it makes {:.2}% against the current strategy's {:.2}%", rn * 100.0, rc * 100.0));
        }
        if let Some(th) = thin.2 {
            if tn < th {
                return (false, format!("with swap costs doubled it makes {:.2}%, less than holding ({:.2}%)", tn * 100.0, th * 100.0));
            }
        }
    } else if ln <= 0.0 || tn <= 0.0 {
        return (false, format!("it loses money over {LONG_DAYS} days ({:.2}%, thin costs {:.2}%)", ln * 100.0, tn * 100.0));
    }
    (true, format!("{LONG_DAYS} d {:.2}%, {RECENT_DAYS} d {:.2}%, thin {:.2}%", ln * 100.0, rn * 100.0, tn * 100.0))
}

/// Run the gate for a strategy on a pool (blocking: minutes).
pub fn check(cid: u64, pool: &str, strategy: &Value, current: Option<Value>, capital_usd: f64) -> Result<Value, String> {
    let current = current.filter(|c| c.is_object() && !c.as_object().unwrap().is_empty());
    let compare = match &current {
        Some(c) => json!([{"id": "current", "name": "Current", "strategy": c}]),
        None => json!(false),
    };
    let base = |days: i64, mult: f64| json!({"chain_id": cid, "pool": pool, "days": days, "capital_usd": capital_usd, "strategy": strategy,
                                              "compare": compare, "hold": true, "costs": costs(cid, pool, mult)});
    let long = run(base(LONG_DAYS, 1.0))?;
    let recent = run(base(RECENT_DAYS, 1.0))?;
    let thin = run(base(LONG_DAYS, 2.0))?;
    let (pass, why) = decide(shares(&long), shares(&recent), shares(&thin), current.is_some());
    let mut advisory = vec![];
    let mut wf = Value::Null;
    if pass {
        let mut r = base(LONG_DAYS, 1.0);
        r["walkforward"] = json!(true);
        match run(r) {
            Ok(v) => {
                wf = v["walkforward"].clone();
                if wf["verdict"] == "likely overfit" {
                    advisory.push(format!("overfit check: likely overfit (score {})", wf["score"]));
                }
            }
            Err(e) => advisory.push(format!("overfit check did not run: {e}")),
        }
    }
    let brief = |r: &Value| json!({"new": shares(r).0, "current": shares(r).1, "hold": shares(r).2, "actions": r["runs"]["main"]["actions"],
                                   "costs": r["runs"]["main"]["costs"], "spot_blocked_bars": r["runs"]["main"]["spot_blocked_bars"], "span_days": r["span_days"]});
    let out = json!({"pass": pass, "why": why, "advisory": advisory, "long": brief(&long), "recent": brief(&recent), "thin": brief(&thin),
                     "walkforward": {"verdict": wf["verdict"], "score": wf["score"], "pbo": wf["pbo"], "wfe": wf["wfe"]}});
    super::journal::record("strategy_gate", json!({"chain_id": cid, "pool": pool, "strategy_hash": super::plan::strategy_hash(strategy), "result": out}));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rule() {
        let s = |n: f64, c: f64, h: f64| (Some(n), Some(c), Some(h));
        assert!(decide(s(0.02, 0.015, 0.0), s(0.005, 0.006, 0.0), s(0.01, 0.0, 0.0), true).0);
        assert!(!decide(s(0.01, 0.015, 0.0), s(0.005, 0.006, 0.0), s(0.01, 0.0, 0.0), true).0);
        assert!(!decide(s(0.02, 0.015, 0.0), s(0.0, 0.006, 0.0), s(0.01, 0.0, 0.0), true).0);
        assert!(!decide(s(0.02, 0.015, 0.0), s(0.005, 0.006, 0.0), s(-0.01, 0.0, 0.0), true).0);
        assert!(decide((Some(0.01), None, Some(0.0)), (Some(0.0), None, None), (Some(0.005), None, None), false).0);
        assert!(!decide((Some(-0.01), None, Some(0.0)), (Some(0.0), None, None), (Some(0.005), None, None), false).0);
    }

    /// The whole gate on a copy of real data: ALM_HOME must point at a copy of the app's folder
    /// (never the live one). cargo test -- --ignored sgate_on_a_copy --nocapture
    #[test]
    #[ignore]
    fn sgate_on_a_copy() {
        let home = std::env::var("ALM_HOME").expect("set ALM_HOME to a copy of the app folder");
        assert!(!home.to_lowercase().replace('\\', "/").trim_end_matches('/').ends_with("appdata/local/stablealm"), "not on the live folder");
        let pool = "0x9ab7730b09ebd9ff2df70f06339bd289a1680a46";
        let new = json!({"type": "ladder", "lookback": 86400, "span": 3, "every": 72, "turnover": 0.4, "guard": 15});
        let cur = json!({"type": "ladder", "lookback": 86400, "span": 5, "every": 72, "turnover": 0.4, "guard": 15});
        let r = check(crate::chains::BASE, pool, &new, Some(cur), 10_000.0).expect("the gate ran");
        println!("{}", serde_json::to_string_pretty(&r).unwrap());
        for w in ["long", "recent", "thin"] {
            assert!(r[w]["new"].is_number() && r[w]["current"].is_number() && r[w]["hold"].is_number(), "{w}");
        }
        assert_eq!(r["pass"], json!(true), "the current span-3 ladder beats span 5 on this data");
        assert!(r["walkforward"]["verdict"].is_string(), "the overfit check ran");
    }
}
