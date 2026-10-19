//! The scheduler (plan section 11) and the plans' invalidation checks.
//!
//! | Review | When | Effort |
//! |---|---|---|
//! | daily | once a day after `agent.daily_utc_hour` (UTC) | high |
//! | severe trigger | a risk action, a plan that ended badly, an invalidation; at most one per 15 min | high |
//! | trigger | anything else queued by `trigger`; at most one per hour | medium |
//! | routine | every `agent.routine_h` hours; may only hold or escalate | low |
//!
//! Triggers queued while a review runs, or inside the debounce, are coalesced into the next one.
//! A review is never started while another runs (they run on this one thread).
//!
//! Every minute the confirmed plans of the last 30 days are checked against their changes'
//! invalidation rules: a breach that lasts `persist_minutes` runs the rule's action once (by code,
//! without a model call) and queues a severe trigger.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// The metrics an invalidation rule may watch (all per pool).
pub const METRICS: &[&str] = &["zscore_72h", "price_usd", "pool_vs_cex_bp", "vol_24h_pct", "out_of_range_24h", "f_lvr_7d",
                               "net_return_7d", "net_return_vs_hold_7d", "capital_usd"];

static LOCK: Mutex<()> = Mutex::new(());
static RUN_NOW: Mutex<Option<String>> = Mutex::new(None);
const PENDING: &str = "triggers.json";
const STATE: &str = "sched.json";

/// Queue a trigger for the next review (risk actions, ended plans, invalidations, user requests).
pub fn trigger(kind: &str, severity: &str, detail: &str) {
    let _g = LOCK.lock().unwrap();
    let mut v = super::read_json(PENDING, json!([]));
    if let Some(a) = v.as_array_mut() {
        a.push(json!({"ts": super::now(), "kind": kind, "severity": severity, "detail": detail.chars().take(400).collect::<String>()}));
        let n = a.len();
        if n > 100 {
            a.drain(..n - 100);
        }
    }
    let _ = super::write_json(PENDING, &v);
}

fn take_pending() -> Vec<Value> {
    let _g = LOCK.lock().unwrap();
    let v = super::read_json(PENDING, json!([])).as_array().cloned().unwrap_or_default();
    let _ = super::write_json(PENDING, &json!([]));
    v
}

pub fn pending() -> Vec<Value> {
    super::read_json(PENDING, json!([])).as_array().cloned().unwrap_or_default()
}

/// Ask for a review now (the dashboard's "Run a review" button): `kind` daily, routine or manual.
pub fn run_now(kind: &str) {
    *RUN_NOW.lock().unwrap() = Some(kind.to_string());
}

/// Which review is due, if any: (kind, effort).
pub fn due(x: &Value, st: &Value, pending: &[Value], now: i64) -> Option<(&'static str, &'static str)> {
    let a = &x["agent"];
    let day = now.div_euclid(86_400);
    let hour = now.rem_euclid(86_400) / 3600;
    let at = a["daily_utc_hour"].as_f64().unwrap_or(16.0) as i64;
    if hour >= at && st["last_daily_day"].as_i64().unwrap_or(-1) < day {
        return Some(("daily", "high"));
    }
    let last_t = st["last_trigger_ts"].as_i64().unwrap_or(0);
    if pending.iter().any(|t| t["severity"] == "severe") && now - st["last_severe_ts"].as_i64().unwrap_or(0) >= 900 {
        return Some(("severe_trigger", "high"));
    }
    if !pending.is_empty() && now - last_t >= 3600 {
        return Some(("trigger", "medium"));
    }
    let every = (a["routine_h"].as_f64().unwrap_or(4.0) * 3600.0) as i64;
    if now - st["last_routine_ts"].as_i64().unwrap_or(0) >= every && now - st["last_review_ts"].as_i64().unwrap_or(0) >= every / 2 {
        return Some(("routine", "low"));
    }
    None
}

/// One pass: the invalidation checks, then a review when one is due.
pub fn pass(worker: &crate::worker::Worker) {
    let x = super::settings();
    if !super::enabled_in(&x) {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check_invalidations(worker, &x)));
    if x["agent"]["mode"].as_str().unwrap_or("off") == "off" {
        *RUN_NOW.lock().unwrap() = None;
        return;
    }
    let now = super::now();
    let mut st = super::read_json(STATE, json!({}));
    let manual = RUN_NOW.lock().unwrap().take();
    let pend = pending();
    let (kind, effort) = match manual.as_deref() {
        Some("daily") => ("daily", "high"),
        Some("routine") => ("routine", "low"),
        Some(_) => ("manual", "medium"),
        None => match due(&x, &st, &pend, now) {
            Some(d) => d,
            None => return,
        },
    };
    let triggers = if kind == "routine" { vec![] } else { take_pending() };
    st["last_review_ts"] = json!(now);
    match kind {
        "daily" => st["last_daily_day"] = json!(now.div_euclid(86_400)),
        "routine" => st["last_routine_ts"] = json!(now),
        "severe_trigger" => {
            st["last_severe_ts"] = json!(now);
            st["last_trigger_ts"] = json!(now);
        }
        "trigger" => st["last_trigger_ts"] = json!(now),
        _ => {}
    }
    st["last_kind"] = json!(kind);
    let _ = super::write_json(STATE, &st);
    let out = super::review::run(worker, kind, effort, &triggers);
    let mut st = super::read_json(STATE, json!({}));
    st["last_outcome"] = json!({"ts": super::now(), "kind": kind, "outcome": out["outcome"], "review_id": out["review_id"]});
    let _ = super::write_json(STATE, &st);
}

pub fn status() -> Value {
    let x = super::settings();
    let st = super::read_json(STATE, json!({}));
    let pend = pending();
    json!({"state": st, "pending": pend, "due_now": due(&x, &st, &pend, super::now()).map(|d| d.0)})
}

// -- invalidation -------------------------------------------------------------------------------------

/// A metric's value for a pool now.
pub fn metric(worker: &crate::worker::Worker, cid: u64, pool: &str, name: &str) -> Option<f64> {
    let cfg = crate::settings::load_pools().into_iter().find(|p| p["pool"].as_str().map(|a| a.eq_ignore_ascii_case(pool)).unwrap_or(false));
    match name {
        "zscore_72h" | "vol_24h_pct" | "out_of_range_24h" => {
            let cfg = cfg?;
            let acct = worker.status.lock().unwrap()["account"]["pools"][cfg["name"].as_str().unwrap_or("")].clone();
            super::features::pool(cid, &cfg, &acct)["regime"][name].as_f64()
        }
        "price_usd" => {
            let (t0, _) = crate::chain::pool_tokens(cid, pool).ok()?;
            crate::prices::usd(cid, &t0).ok()
        }
        "pool_vs_cex_bp" => {
            let (p, s0, s1) = super::risk::pool_price(cid, pool)?;
            super::refdata::pool_vs_cex(p, &s0, &s1)["diff_bp"].as_f64()
        }
        "f_lvr_7d" => crate::data::cached_flvr(cid, &pool.to_lowercase()).and_then(|v| v["ratio"].as_f64()),
        "net_return_7d" => super::perf::pool(pool, 7 * 86_400)["return"].as_f64().map(|r| r * 100.0),
        "net_return_vs_hold_7d" => super::perf::pool(pool, 7 * 86_400)["vs_hold"].as_f64().map(|r| r * 100.0),
        "capital_usd" => super::exec::capital_of(worker, pool),
        _ => None,
    }
}

pub fn breached(value: f64, op: &str, threshold: f64) -> bool {
    match op {
        "<" => value < threshold,
        ">" => value > threshold,
        _ => false,
    }
}

fn check_invalidations(worker: &crate::worker::Worker, x: &Value) {
    let now = super::now();
    for p in super::plan::all().into_iter().filter(|p| p["state"] == "confirmed" && now - p["created"].as_i64().unwrap_or(0) < 30 * 86_400) {
        let id = p["plan_id"].as_str().unwrap_or("").to_string();
        for (i, c) in p["changes"].as_array().into_iter().flatten().enumerate() {
            let inv = &c["invalidation"];
            if p["invalidated"][i.to_string()].is_object() || c["type"] == "exit_pool" {
                continue;
            }
            let pool = c["pool"].as_str().unwrap_or("");
            let cid = c["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
            let (Some(m), Some(op), Some(th)) = (inv["metric"].as_str(), inv["op"].as_str(), inv["threshold"].as_f64()) else { continue };
            let Some(v) = metric(worker, cid, pool, m) else { continue };
            let key = i.to_string();
            if !breached(v, op, th) {
                if p["breach_since"][&key].is_i64() {
                    let _ = super::plan::update(&id, |pp| { pp["breach_since"][&key] = Value::Null; });
                }
                continue;
            }
            let since = match p["breach_since"][&key].as_i64() {
                Some(s) => s,
                None => {
                    let _ = super::plan::update(&id, |pp| pp["breach_since"][&key] = json!(now));
                    now
                }
            };
            let persist = inv["persist_minutes"].as_f64().unwrap_or(0.0).max(0.0) * 60.0;
            if ((now - since) as f64) < persist {
                continue;
            }
            let action = inv["action"].as_str().unwrap_or("alert");
            let detail = format!("plan {id} change {}: {m} {v:.4} {op} {th}", i + 1);
            let done = invalidate(worker, x, cid, pool, c, action, &detail);
            let _ = super::plan::update(&id, |pp| pp["invalidated"][&key] = json!({"ts": now, "value": v, "action": action, "result": done}));
            super::journal::record("invalidation", json!({"plan_id": id, "change": i + 1, "metric": m, "value": v, "op": op, "threshold": th, "action": action, "result": done}));
            super::notify::alert("severe", "Plan invalidated", &format!("{detail} → {action}: {done}"));
            trigger("invalidation", "severe", &detail);
        }
    }
}

fn invalidate(worker: &crate::worker::Worker, x: &Value, cid: u64, pool: &str, c: &Value, action: &str, why: &str) -> String {
    let name = crate::settings::load_pools().into_iter().find(|p| p["pool"].as_str().map(|a| a.eq_ignore_ascii_case(pool)).unwrap_or(false))
        .and_then(|p| p["name"].as_str().map(String::from)).unwrap_or_else(|| pool.to_string());
    match action {
        "exit_to_safe" => {
            let safe = super::exposure::safe_assets(cid).into_iter().next();
            super::risk::emergency_exit(cid, pool, &name, safe.as_deref(), why, x, worker);
            "exit to the safe asset queued".into()
        }
        "withdraw_to_unallocated" => {
            let cap = super::exec::capital_of(worker, pool).unwrap_or(0.0);
            if cap > 1.0 {
                let id = crate::capital::push(json!({"kind": "withdraw", "chain_id": cid, "pools": [pool], "usd": cap * 1.02, "to": null, "reason": why}));
                worker.request(false);
                format!("withdrawal queued ({id})")
            } else {
                "nothing to withdraw".into()
            }
        }
        "revert_strategy" => match c["previous_strategy"].is_object() {
            true => match super::exec::apply_strategy(pool, &c["previous_strategy"]) {
                Ok(()) => {
                    let live = crate::settings::load_settings()["execute"].as_bool().unwrap_or(false);
                    super::exec::reset_strategy_state(cid, pool, live);
                    worker.request(false);
                    "previous strategy restored".into()
                }
                Err(e) => format!("could not restore the previous strategy: {e}"),
            },
            false => "no previous strategy to restore".into(),
        },
        _ => "alert only".into(),
    }
}

pub fn start(worker: Arc<crate::worker::Worker>) {
    std::thread::Builder::new().name("agent-sched".into()).spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(90));
        loop {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pass(&worker))).is_err() {
                crate::error!("agent", "the scheduler pass panicked");
            }
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_due() {
        let x = super::super::defaults();
        let day = 20_000 * 86_400;
        // before the daily hour, a routine review is due on a fresh start
        assert_eq!(due(&x, &json!({}), &[], day + 3600), Some(("routine", "low")));
        // after it, the daily one
        assert_eq!(due(&x, &json!({}), &[], day + 17 * 3600), Some(("daily", "high")));
        let st = json!({"last_daily_day": 20_000, "last_routine_ts": day + 16 * 3600, "last_review_ts": day + 16 * 3600, "last_trigger_ts": day + 16 * 3600});
        assert_eq!(due(&x, &st, &[], day + 17 * 3600), None);
        // a normal trigger waits for the hour; a severe one only 15 minutes
        let n = [json!({"severity": "normal"})];
        assert_eq!(due(&x, &st, &n, day + 16 * 3600 + 1200), None);
        assert_eq!(due(&x, &st, &n, day + 17 * 3600 + 1), Some(("trigger", "medium")));
        let s = [json!({"severity": "severe"})];
        assert_eq!(due(&x, &st, &s, day + 16 * 3600 + 1200), Some(("severe_trigger", "high")));
        assert!(breached(-3.0, "<", -2.5) && !breached(-2.0, "<", -2.5) && breached(5.0, ">", 1.0));
    }
}
