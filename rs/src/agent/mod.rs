//! The agent plan (docs/AGENT_PLAN.md), behind Settings → Experimental features (off by default).
//!
//! | Module | Job (plan section) |
//! |---|---|
//! | `perf` | per-pool performance, cash-flow adjusted, and against holding (6) |
//! | `refdata` | reference prices from exchanges, token health, gas, funding and open interest (6) |
//! | `features` | per-pool snapshot data: geometry, regime, activity, swap costs, data quality (6) |
//! | `history_sync` | the 45-day local store for managed and candidate pools (6) |
//! | `exposure` | token classes, exposure and the limits on a projected state (5) |
//! | `locks` | safety locks and the emergency state (4) |
//! | `gate` | the write gate every signer asks before a transaction (2) |
//! | `risk` | the risk controller on a wall-clock timer (4) |
//! | `journal` | the decision record, hash-chained (3) |
//! | `plan` | the plan object and its lifecycle (3) |
//! | `exec` | the execution coordinator (3) |
//! | `sgate` | the strategy gate: long window plus the last 14 days (9) |
//! | `policy` | the policy gate a plan must pass (3, 5) |
//! | `snapshot` | the per-review snapshot (7) |
//! | `tools` | the agent's bounded tools (8) |
//! | `claude` | the Messages API adapter (13, 15) |
//! | `budget` | API spend allowance and reservations (13) |
//! | `review` | one review: snapshot → model → tools → outcome (10) |
//! | `sched` | the scheduler and triggers (11) |
//! | `notify` | desktop and Telegram alerts, Telegram approvals (14) |
//! | `shadow` | baselines, shadow books and the scored report (12) |
//!
//! Nothing here runs unless `experimental.enabled` is on, and each part has its own switch on top
//! (risk.mode, caps.enabled, agent.mode, telegram.enabled). With all of them off the app behaves
//! exactly as without this module.

use serde_json::{json, Value};
use std::path::PathBuf;

pub mod budget;
pub mod claude;
pub mod exec;
pub mod exposure;
pub mod features;
pub mod gate;
pub mod history_sync;
pub mod journal;
pub mod locks;
pub mod notify;
pub mod perf;
pub mod plan;
pub mod policy;
pub mod refdata;
pub mod review;
pub mod risk;
pub mod sched;
pub mod sgate;
pub mod shadow;
pub mod snapshot;
pub mod tools;

/// The settings under `experimental`, with their defaults.
pub fn defaults() -> Value {
    json!({
        "enabled": false,
        // llens.rs: "placeholder" (generated data) or "off"
        "llens": "placeholder",
        // risk.rs: "off", "monitor" (alerts and locks only) or "act" (also emergency exits)
        "risk": {"mode": "off", "stale_min": 10.0, "disagree_bp_stable": 30.0, "disagree_bp_eth": 100.0, "disagree_bp_other": 200.0,
                 "peg_pct": 1.0, "loss_24h_pct": 3.0, "loss_7d_pct": 6.0, "heartbeat_s": 300.0, "lock_hours": 24.0, "stuck_tx_s": 120.0,
                 "emergency_gwei": 2.0},
        // engine caps for every strategy (the rebalance count is the pool's own max_actions_per_day)
        "caps": {"enabled": false, "turnover_pct_day": 100.0, "cost_pct_7d": 0.5},
        // exposure and action limits (plan section 5)
        "limits": {"eth_pct": 50.0, "btc_pct": 25.0, "other_pct": 10.0, "issuer_pct": 75.0, "lst_pct": 40.0, "pool_pct": 75.0,
                   "chain_pct": 100.0, "exit_capacity_pct": 90.0, "move_pct_day": 20.0, "new_pool_pct": 5.0, "new_pool_days": 14.0,
                   "actions_per_pool_day": 3.0, "no_reversal_h": 12.0},
        // execution settings (plan section 3): bounds on what a plan step may cost
        "execution": {"max_cost_bp": 15.0, "max_emergency_bp": 100.0, "quote_age_s": 60.0, "plan_age_h": 6.0, "price_move_pct": 1.0},
        // the agent (plan sections 10-13): "off", "advise" (plans wait for approval) or "auto"
        "agent": {"mode": "off", "model": "claude-opus-5-5", "budget_usd_month": 0.0, "reserve_pct": 25.0, "daily_utc_hour": 16.0,
                  "routine_h": 4.0, "web_search": true, "searches_per_review": 5.0, "web_search_tool": "web_search_20260209",
                  "approved_tokens": [], "results_read": false, "max_tool_calls": 40.0, "review_minutes": 10.0, "owner": false},
        // telegram (plan section 14): the linked private chat and user
        "telegram": {"enabled": false, "chat_id": null, "user_id": null, "user_name": null},
    })
}

/// Reject experimental settings out of range.
pub fn validate(x: &Value) -> Result<(), String> {
    if x.is_null() {
        return Ok(());
    }
    let en = |path: &str, v: &Value, ok: &[&str]| -> Result<(), String> {
        match v.as_str() {
            None if v.is_null() => Ok(()),
            Some(s) if ok.contains(&s) => Ok(()),
            _ => Err(format!("experimental.{path} is one of {}", ok.join(", "))),
        }
    };
    en("llens", &x["llens"], &["placeholder", "off"])?;
    en("risk.mode", &x["risk"]["mode"], &["off", "monitor", "act"])?;
    en("agent.mode", &x["agent"]["mode"], &["off", "advise", "auto"])?;
    let ranges: &[(&str, &str, f64, f64)] = &[
        ("risk", "stale_min", 1.0, 1440.0), ("risk", "disagree_bp_stable", 1.0, 5000.0), ("risk", "disagree_bp_eth", 1.0, 5000.0),
        ("risk", "disagree_bp_other", 1.0, 5000.0), ("risk", "peg_pct", 0.05, 50.0), ("risk", "loss_24h_pct", 0.1, 100.0),
        ("risk", "loss_7d_pct", 0.1, 100.0), ("risk", "heartbeat_s", 60.0, 3600.0), ("risk", "lock_hours", 0.0, 720.0), ("risk", "stuck_tx_s", 30.0, 3600.0), ("risk", "emergency_gwei", 0.001, 1000.0),
        ("caps", "turnover_pct_day", 1.0, 10000.0), ("caps", "cost_pct_7d", 0.01, 100.0),
        ("limits", "eth_pct", 0.0, 100.0), ("limits", "btc_pct", 0.0, 100.0), ("limits", "other_pct", 0.0, 100.0), ("limits", "issuer_pct", 0.0, 100.0),
        ("limits", "lst_pct", 0.0, 100.0), ("limits", "pool_pct", 0.0, 100.0), ("limits", "chain_pct", 0.0, 100.0), ("limits", "exit_capacity_pct", 0.0, 100.0),
        ("limits", "move_pct_day", 0.0, 100.0), ("limits", "new_pool_pct", 0.0, 100.0), ("limits", "new_pool_days", 0.0, 365.0),
        ("limits", "actions_per_pool_day", 0.0, 100.0), ("limits", "no_reversal_h", 0.0, 720.0),
        ("execution", "max_cost_bp", 0.0, 1000.0), ("execution", "max_emergency_bp", 0.0, 2000.0), ("execution", "quote_age_s", 5.0, 3600.0),
        ("execution", "plan_age_h", 0.1, 168.0), ("execution", "price_move_pct", 0.01, 50.0),
        ("agent", "budget_usd_month", 0.0, 100000.0), ("agent", "reserve_pct", 0.0, 90.0), ("agent", "daily_utc_hour", 0.0, 23.0),
        ("agent", "routine_h", 1.0, 48.0), ("agent", "searches_per_review", 0.0, 20.0), ("agent", "max_tool_calls", 1.0, 200.0),
        ("agent", "review_minutes", 1.0, 60.0),
    ];
    for (g, k, lo, hi) in ranges {
        let v = &x[*g][*k];
        if v.is_null() {
            continue;
        }
        let Some(n) = v.as_f64().filter(|n| n.is_finite()) else { return Err(format!("experimental.{g}.{k} must be a number")) };
        if n < *lo || n > *hi {
            return Err(format!("experimental.{g}.{k} must be between {lo} and {hi} (got {n})"));
        }
    }
    if let Some(m) = x["agent"]["model"].as_str() {
        if m.is_empty() || m.len() > 80 || !m.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_') {
            return Err("experimental.agent.model is a model id like claude-opus-5-5".into());
        }
    }
    if let Some(t) = x["agent"]["approved_tokens"].as_array() {
        if t.len() > 200 || !t.iter().all(|e| e["chain_id"].as_u64().is_some() && e["address"].as_str().map(is_addr).unwrap_or(false)) {
            return Err("experimental.agent.approved_tokens: [{chain_id, address}], at most 200".into());
        }
    }
    Ok(())
}

pub fn is_addr(a: &str) -> bool {
    a.len() == 42 && a.starts_with("0x") && a[2..].chars().all(|c| c.is_ascii_hexdigit())
}

/// The experimental settings as they are now (defaults filled in).
pub fn settings() -> Value {
    crate::settings::load_settings()["experimental"].clone()
}

pub fn enabled_in(x: &Value) -> bool {
    x["enabled"].as_bool().unwrap_or(false)
}

pub fn enabled() -> bool {
    enabled_in(&settings())
}

/// Where the agent's own files live: data/agent/.
pub fn dir() -> PathBuf {
    let d = crate::paths::data().join("agent");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn now() -> i64 {
    crate::engine::now() as i64
}

/// A number from JSON, accepting numeric strings.
pub fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).filter(|x: &f64| x.is_finite())
}

/// A JSON file of this module's, or `default` when missing or unreadable.
pub fn read_json(name: &str, default: Value) -> Value {
    std::fs::read_to_string(dir().join(name)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(default)
}

pub fn write_json(name: &str, v: &Value) -> Result<(), String> {
    crate::settings::write_atomic(&dir().join(name), v).map_err(|e| e.to_string())
}

/// Append one line to a JSONL file of this module's.
pub fn append(name: &str, v: &Value) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir().join(name)) {
        let _ = writeln!(f, "{v}");
    }
}

/// The last `max` lines of a JSONL file of this module's.
pub fn tail(name: &str, max: usize) -> Vec<Value> {
    let s = std::fs::read_to_string(dir().join(name)).unwrap_or_default();
    let rows: Vec<Value> = s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    rows[rows.len().saturating_sub(max)..].to_vec()
}

pub fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Start every background part that its settings turn on (called once at startup); each checks its
/// switch again on every turn, so turning one off later stops it doing anything.
pub fn start(worker: std::sync::Arc<crate::worker::Worker>) {
    risk::start(worker.clone());
    sched::start(worker.clone());
    notify::start(worker.clone());
    history_sync::start();
    refdata::start();
    shadow::start();
    exec::start(worker);
}

/// After the settings change: a change of mode, limits or execution bounds voids the plans waiting
/// for approval (they were checked against the old ones); turning the agent off cancels them.
pub fn settings_changed(old: &Value, new: &Value) {
    let off = !enabled_in(new) || new["agent"]["mode"] == "off";
    if off && (enabled_in(old) && old["agent"]["mode"] != "off") {
        plan::void_pending("the agent was turned off");
    } else if old["agent"]["mode"] != new["agent"]["mode"] || old["limits"] != new["limits"] || old["execution"] != new["execution"] {
        plan::void_pending("the limits or the agent mode changed");
    }
    if old != new {
        journal::record("settings", json!({"before": old, "after": new}));
    }
}

/// GET /api/agent[/…]: the Experimental tab's agent data.
pub fn api_get(rest: &str, worker: &std::sync::Arc<crate::worker::Worker>) -> Result<Value, String> {
    let x = settings();
    match rest {
        "" | "/" => {
            let plans: Vec<Value> = plan::all().into_iter().rev().take(30).collect();
            Ok(json!({
                "settings": x, "now": now(),
                "risk": risk::last(), "locks": locks::all(), "emergency": locks::emergency(), "heartbeat_age_s": gate::heartbeat_age(),
                "suspects": risk::suspects(), "performance": perf::all(), "plans": plans, "open_plans": plan::all().iter().filter(|p| !plan::terminal(p["state"].as_str().unwrap_or(""))).count(), "reviews": review::recent(20),
                "review_running": review::running(), "sched": sched::status(), "budget": budget::status(&x),
                "alerts": notify::recent(30), "journal": journal::recent(40, &[]), "history_sync": history_sync::status(),
                "forward_test": shadow::frozen(), "report": shadow::report(),
                "has_api_key": claude::has_key(), "has_bot_token": notify::has_token(),
                "telegram": {"linked": x["telegram"]["chat_id"].is_i64(), "user": x["telegram"]["user_name"], "enabled": x["telegram"]["enabled"]},
                "capital": worker.status.lock().unwrap()["capital"].clone(),
            }))
        }
        "/exposure" => {
            let st = worker.status.lock().unwrap().clone();
            let h = exposure::holdings(&st["account"], &st["capital"]);
            let safe: Vec<String> = crate::settings::load_pools().iter().map(crate::worker::pool_chain)
                .filter_map(|c| exposure::safe_assets(c).into_iter().next()).collect();
            let s = exposure::summary(&h, &safe);
            Ok(json!({"summary": s, "breaches": exposure::breaches(&s, &x["limits"])}))
        }
        "/features" => {
            let st = worker.status.lock().unwrap().clone();
            Ok(json!(crate::settings::load_pools().iter().map(|p| {
                let acct = st["account"]["pools"][p["name"].as_str().unwrap_or("")].clone();
                features::pool(crate::worker::pool_chain(p), p, &acct)
            }).collect::<Vec<_>>()))
        }
        "/funding" => Ok(refdata::funding()),
        "/journal/verify" => Ok(match journal::verify() {
            Ok(n) => json!({"ok": true, "records": n}),
            Err(e) => json!({"ok": false, "error": e}),
        }),
        r if r.starts_with("/review/") => review::get(&r["/review/".len()..]).ok_or_else(|| "no such review".to_string()),
        _ => Err("unknown agent path".into()),
    }
}

/// POST /api/agent/<action>.
pub fn api_post(action: &str, body: &Value, worker: &std::sync::Arc<crate::worker::Worker>) -> Result<Value, String> {
    let s = |k: &str| body[k].as_str().unwrap_or("").to_string();
    match action {
        "key" => {
            claude::set_key(&s("key"))?;
            journal::record("api_key", json!({"set": !s("key").trim().is_empty()}));
            Ok(json!({"ok": true, "has_api_key": claude::has_key()}))
        }
        "key_test" => claude::test_key(),
        "telegram_token" => Ok(json!({"ok": true, "bot": notify::set_token(&s("token"))?})),
        "telegram_link" => Ok(json!({"ok": true, "code": notify::link_code()?, "valid_s": 600})),
        "telegram_unlink" => {
            notify::unlink("from the dashboard");
            Ok(json!({"ok": true}))
        }
        "decide" => {
            let p = plan::decide(&s("plan_id"), &s("plan_hash"), body["approve"].as_bool().unwrap_or(false), "dashboard")?;
            worker.request(false);
            Ok(json!({"ok": true, "plan": p}))
        }
        "cancel" => Ok(json!({"ok": true, "plan": plan::transition(&s("plan_id"), "cancelled", "cancelled from the dashboard", json!({}))?})),
        "lock_confirm" => {
            locks::confirm(&s("id"))?;
            worker.request(false);
            Ok(json!({"ok": true}))
        }
        "review" => {
            if settings()["agent"]["mode"] == "off" {
                return Err("the agent is off (Experimental → Agent mode)".into());
            }
            if review::running() {
                return Err("a review is running".into());
            }
            sched::run_now(match s("kind").as_str() { "daily" => "daily", "routine" => "routine", _ => "manual" });
            Ok(json!({"ok": true, "note": "the review starts within a minute"}))
        }
        "history_sync" => {
            std::thread::spawn(|| { let _ = history_sync::pass(true); });
            Ok(json!({"ok": true}))
        }
        "freeze" => Ok(json!({"ok": true, "freeze": shadow::freeze()?})),
        "report" => {
            std::thread::spawn(|| { let _ = std::panic::catch_unwind(shadow::build); });
            Ok(json!({"ok": true, "note": "the report is rebuilt in the background (a few backtests)"}))
        }
        _ => Err(format!("unknown agent action {action}")),
    }
}
