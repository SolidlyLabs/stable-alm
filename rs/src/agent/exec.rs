//! The execution coordinator (plan section 3). Every 30 s it takes the oldest queued plan (one at
//! a time), re-validates it, and runs its steps in order: exits and capital decreases first, then
//! strategy changes, then deposits. A step is queued work for the worker's signing thread:
//!
//! | Step | What it queues | Done when |
//! |---|---|---|
//! | exit_pool | the pool switched off | it holds no positions and no capital request is left |
//! | capital down | a withdrawal to the safe asset, with the plan's cost bound | the request is gone |
//! | strategy | the new strategy written, the pool's strategy state reset, a re-solve this cycle | at once |
//! | capital up | an Add from Unallocated; the pool deploys within the plan's cost bound | the request is gone |
//! | add_pool | the pool added (enabled, its strategy), then an Add of its start capital | the request is gone |
//!
//! Steps are stored with the plan (keyed by plan and step number) before anything is queued, so a
//! restart resumes from the last confirmed step after re-validating, and a step is never queued twice.
//! A step not done after 2 hours is queued once more, twice at most; then the plan is
//! `partially_completed`: what was released stays in Unallocated, and the user is alerted. There is
//! no automatic undo, and no model call to improvise.
//!
//! While a step's pool has an active plan, the worker runs it with the plan's effective cost bound
//! (`bounded`), the stricter of the user's execution settings and the plan's own.

use serde_json::{json, Value};
use std::sync::Arc;

use super::plan;

const STEP_TIMEOUT_S: i64 = 2 * 3600;
const MAX_ATTEMPTS: i64 = 3;

/// The effective bounds of a plan: the stricter of the user's settings and the plan's request.
pub fn effective_bounds(x: &Value, requested: &Value) -> Value {
    let e = &x["execution"];
    let pick = |k: &str, d: f64| {
        let s = e[k].as_f64().unwrap_or(d);
        requested[k].as_f64().filter(|r| r.is_finite() && *r >= 0.0).map(|r| r.min(s)).unwrap_or(s)
    };
    json!({"max_cost_bp": pick("max_cost_bp", 15.0), "quote_age_s": pick("quote_age_s", 60.0), "plan_age_h": pick("plan_age_h", 6.0),
           "price_move_pct": pick("price_move_pct", 1.0)})
}

/// The pools with an executing plan step and their cost bound (data/agent/bounds.json).
fn set_bound(pool: &str, bp: Option<f64>) {
    let mut v = super::read_json("bounds.json", json!({}));
    match bp {
        Some(b) => v[pool.to_lowercase()] = json!(b),
        None => {
            if let Some(o) = v.as_object_mut() {
                o.remove(&pool.to_lowercase());
            }
        }
    }
    let _ = super::write_json("bounds.json", &v);
}

/// The pool's settings with a plan's cost bound applied, while one is executing on it.
pub fn bounded(cfg: &crate::settings::PoolConfig) -> crate::settings::PoolConfig {
    let mut c = cfg.clone();
    if super::enabled() {
        if let Some(b) = super::read_json("bounds.json", json!({}))[cfg.pool.to_lowercase()].as_f64() {
            c.max_swap_cost_bps = c.max_swap_cost_bps.min(b);
            c.slippage_bps = c.slippage_bps.min(b);
            c.plan_bound_bps = Some(b);
        }
    }
    c
}

fn pool_cfg(addr: &str) -> Option<Value> {
    crate::settings::load_pools().into_iter().find(|p| p["pool"].as_str().map(|a| a.eq_ignore_ascii_case(addr)).unwrap_or(false))
}

/// A pool's capital now (USD), from the worker's last published capital.
pub fn capital_of(worker: &crate::worker::Worker, addr: &str) -> Option<f64> {
    let st = worker.status.lock().unwrap();
    for ch in st["capital"].as_object()?.values() {
        for p in ch["pools"].as_object().into_iter().flat_map(|m| m.values()) {
            if p["pool"].as_str().map(|a| a.eq_ignore_ascii_case(addr)).unwrap_or(false) {
                return p["capital_usd"].as_f64();
            }
        }
    }
    None
}

fn has_positions(worker: &crate::worker::Worker, addr: &str) -> bool {
    let Some(cfg) = pool_cfg(addr) else { return false };
    let name = cfg["name"].as_str().unwrap_or("").to_string();
    worker.status.lock().unwrap()["account"]["pools"][&name]["positions"].as_array().map(|a| !a.is_empty()).unwrap_or(false)
}

/// The steps of a plan, in order.
pub fn steps_of(p: &Value, worker: &crate::worker::Worker) -> Vec<Value> {
    let mut exits = vec![];
    let mut downs = vec![];
    let mut strategies = vec![];
    let mut ups = vec![];
    for (i, c) in p["changes"].as_array().into_iter().flatten().enumerate() {
        let pool = c["pool"].as_str().unwrap_or("").to_lowercase();
        let chain = c["chain_id"].as_u64().or_else(|| pool_cfg(&pool).map(|x| crate::worker::pool_chain(&x))).unwrap_or(crate::chains::BASE);
        match c["type"].as_str().unwrap_or("") {
            "exit_pool" => exits.push(json!({"kind": "exit_pool", "change": i, "pool": pool, "chain_id": chain})),
            "capital" => {
                let cur = capital_of(worker, &pool).unwrap_or(0.0);
                let target = c["target_usd"].as_f64().unwrap_or(cur);
                if target + 1.0 < cur {
                    downs.push(json!({"kind": "withdraw", "change": i, "pool": pool, "chain_id": chain, "usd": cur - target}));
                } else if target > cur + 1.0 {
                    ups.push(json!({"kind": "add", "change": i, "pool": pool, "chain_id": chain, "usd": target - cur}));
                }
            }
            "strategy" => strategies.push(json!({"kind": "strategy", "change": i, "pool": pool, "chain_id": chain, "strategy": c["strategy"]})),
            "add_pool" => {
                ups.push(json!({"kind": "add_pool", "change": i, "pool": pool, "chain_id": chain, "strategy": c["strategy"], "name": c["name"]}));
                ups.push(json!({"kind": "add", "change": i, "pool": pool, "chain_id": chain, "usd": c["start_usd"]}));
            }
            _ => {}
        }
    }
    let mut out: Vec<Value> = exits.into_iter().chain(downs).chain(strategies).chain(ups).collect();
    for (n, s) in out.iter_mut().enumerate() {
        s["step_no"] = json!(n + 1);
        s["state"] = json!("pending");
        s["attempts"] = json!(0);
    }
    out
}

/// Reset a pool's strategy state (plan section 3): the new strategy starts "not exited" with no
/// timers and decides on this cycle. The old state goes to the journal.
pub fn reset_strategy_state(cid: u64, pool: &str, live: bool) {
    let old = crate::custom::load_state(cid, pool, live);
    let mut st = crate::custom::State::default();
    st.force_solve = true;
    crate::custom::save_state(cid, pool, live, &st);
    super::journal::record("strategy_state_reset", json!({"chain_id": cid, "pool": pool, "live": live, "old_state": serde_json::to_value(&old).unwrap_or(Value::Null)}));
}

/// Write a pool's strategy (and switch it on), keeping everything else.
pub fn apply_strategy(pool: &str, strategy: &Value) -> Result<(), String> {
    crate::settings::update_pools(|pools| {
        let p = pools.iter_mut().find(|p| p["pool"].as_str().map(|a| a.eq_ignore_ascii_case(pool)).unwrap_or(false)).ok_or("pool not configured")?;
        p["strategy"] = crate::ladder::normalized(strategy);
        crate::settings::validate_pool(p)
    })
}

fn add_pool(cid: u64, pool: &str, strategy: &Value, name: Option<&str>) -> Result<(), String> {
    if pool_cfg(pool).is_some() {
        return apply_strategy(pool, strategy);
    }
    let (t0, t1) = crate::chain::pool_tokens(cid, pool)?;
    let (s0, s1) = (crate::prices::meta(cid, &t0)?.symbol, crate::prices::meta(cid, &t1)?.symbol);
    let mut e = crate::settings::pool_defaults();
    e["pool"] = json!(pool);
    e["chain_id"] = json!(cid);
    e["name"] = json!(name.map(String::from).unwrap_or_else(|| format!("{}-{}", s0.to_lowercase(), s1.to_lowercase())));
    e["strategy"] = crate::ladder::normalized(strategy);
    e["enabled"] = json!(true);
    crate::settings::validate_pool(&e)?;
    crate::settings::update_pools(|pools| {
        pools.push(e);
        Ok(())
    })
}

/// Queue a step's work (once); returns the capital request id it queued, if any.
fn start_step(p: &Value, s: &Value, bounds: &Value, live: bool) -> Result<Option<String>, String> {
    let pool = s["pool"].as_str().unwrap_or("");
    let cid = s["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
    let id = p["plan_id"].as_str().unwrap_or("");
    let bp = bounds["max_cost_bp"].as_f64().unwrap_or(15.0);
    match s["kind"].as_str().unwrap_or("") {
        "exit_pool" => {
            super::risk::switch_off(pool, &format!("plan {id}"));
            Ok(None)
        }
        "withdraw" => {
            let safe = super::exposure::safe_assets(cid).into_iter().next();
            set_bound(pool, Some(bp));
            Ok(Some(crate::capital::push(json!({"kind": "withdraw", "chain_id": cid, "pools": [pool], "usd": s["usd"], "to": null,
                                                "safe": safe, "max_cost_bp": bp, "plan": id}))))
        }
        "strategy" => {
            // the move the new strategy makes is the plan's: under its cost bound
            set_bound(pool, Some(bp));
            apply_strategy(pool, &s["strategy"])?;
            reset_strategy_state(cid, pool, live);
            Ok(None)
        }
        "add_pool" => {
            set_bound(pool, Some(bp));
            add_pool(cid, pool, &s["strategy"], s["name"].as_str())?;
            reset_strategy_state(cid, pool, live);
            Ok(None)
        }
        "add" => {
            set_bound(pool, Some(bp));
            Ok(Some(crate::capital::push(json!({"kind": "add", "chain_id": cid, "pool": pool, "usd": s["usd"], "plan": id}))))
        }
        k => Err(format!("unknown step {k}")),
    }
}

/// Whether the capital request `rid` (or a part it was split into) is still queued.
fn request_pending(rid: &str) -> bool {
    crate::capital::requests().iter().any(|r| r["id"].as_str().map(|x| x == rid || x.starts_with(&format!("{rid}-"))).unwrap_or(false))
}

/// What of the capital request `rid` is still to do (USD): its parts still queued (a withdrawal's
/// per-pool releases keep only their remainder), or None when none is.
fn outstanding(rid: &str) -> Option<f64> {
    let parts: Vec<f64> = crate::capital::requests().iter()
        .filter(|r| r["id"].as_str().map(|x| x == rid || x.starts_with(&format!("{rid}-"))).unwrap_or(false))
        .map(|r| r["usd"].as_f64().unwrap_or(0.0)).collect();
    if parts.is_empty() { None } else { Some(parts.iter().sum()) }
}

/// When the worker last finished a cycle (unix seconds).
fn last_cycle(worker: &crate::worker::Worker) -> i64 {
    worker.status.lock().unwrap()["last_cycle"].as_i64().unwrap_or(0)
}

/// When the last finished cycle began (unix seconds).
fn last_cycle_started(worker: &crate::worker::Worker) -> i64 {
    worker.status.lock().unwrap()["last_cycle_started"].as_i64().unwrap_or(0)
}

/// Is the step's work done? A withdraw or add step always queues a request: one without its request
/// (a restart between saving the step and queuing it, a re-queue that failed) is not done, and times
/// out into a retry. An add's request is gone once its tokens are assigned, before the pool deploys
/// them with the plan's cost bound: it is done only when a cycle has finished after that
/// (`gone_at`, set by the coordinator), or the bound would be cleared before the deploy.
/// Whether the pool has acted on the strategy a step applied (`submitted`) under the plan's bound:
/// a cycle that began after it ran the pool, and either its move cleared the forced re-solve or the
/// strategy chose not to move. A run that was blocked, failed or skipped is not it.
fn acted_on_strategy(s: &Value, worker: &crate::worker::Worker, live: bool) -> bool {
    let sub = s["submitted"].as_i64().unwrap_or(i64::MAX);
    if last_cycle_started(worker) <= sub {
        return false;
    }
    let (cid, pool) = (s["chain_id"].as_u64().unwrap_or(crate::chains::BASE), s["pool"].as_str().unwrap_or(""));
    if !crate::custom::load_state(cid, pool, live).force_solve {
        return true;
    }
    let Some(name) = pool_cfg(pool).and_then(|p| p["name"].as_str().map(String::from)) else { return false };
    let st = worker.status.lock().unwrap()["pools"][&name].clone();
    ran_clean_since(worker, s, sub) && ["keep", "wait", "exited"].contains(&st["mode"].as_str().unwrap_or(""))
}

/// Whether the step's pool has run since `since` without its move being blocked or failing (its
/// deploy happened, or its strategy chose not to make one).
fn ran_clean_since(worker: &crate::worker::Worker, s: &Value, since: i64) -> bool {
    let Some(name) = pool_cfg(s["pool"].as_str().unwrap_or("")).and_then(|p| p["name"].as_str().map(String::from)) else { return false };
    let st = worker.status.lock().unwrap()["pools"][&name].clone();
    st["ran_at"].as_i64().map(|t| t > since).unwrap_or(false) && st["blocked"] != true && !["error", "hold", "disabled"].contains(&st["mode"].as_str().unwrap_or(""))
}

fn step_done(s: &Value, worker: &crate::worker::Worker, live: bool) -> bool {
    let req = s["request"].as_str();
    match s["kind"].as_str().unwrap_or("") {
        "exit_pool" => !has_positions(worker, s["pool"].as_str().unwrap_or("")),
        // the worker acts on the new strategy in its next cycle: done once a cycle that began after
        // it was applied has finished (the plan's bound holds until then)
        "strategy" | "add_pool" => acted_on_strategy(s, worker, live),
        "withdraw" => req.map(|r| !request_pending(r)).unwrap_or(false),
        "add" => match req {
            Some(r) if !request_pending(r) => s["gone_at"].as_i64().map(|g| last_cycle(worker) > g && ran_clean_since(worker, s, g)).unwrap_or(false),
            _ => false,
        },
        _ => req.map(|r| !request_pending(r)).unwrap_or(true),
    }
}

/// One pass of the coordinator.
pub fn pass(worker: &crate::worker::Worker) {
    let x = super::settings();
    if !super::enabled_in(&x) {
        return;
    }
    let live = crate::settings::load_settings()["execute"].as_bool().unwrap_or(false);
    let all = plan::all();
    // pre-emption: an emergency on a pool a plan touches supersedes it
    if let Some(e) = super::locks::emergency() {
        let hit: Vec<String> = e["pools"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)).collect();
        for p in all.iter().filter(|p| p["state"] == "queued" || p["state"] == "executing") {
            let touches = p["changes"].as_array().into_iter().flatten().any(|c| hit.iter().any(|h| c["pool"].as_str().map(|a| a.eq_ignore_ascii_case(h)).unwrap_or(false)));
            if touches {
                let id = p["plan_id"].as_str().unwrap_or("");
                // its steps not done yet are dropped (a queued request not run is removed); what was released stays Unallocated
                for s in p["steps"].as_array().into_iter().flatten() {
                    if s["state"] == "submitted" {
                        if let Some(r) = s["request"].as_str() {
                            remove_request(r);
                        }
                    }
                }
                let _ = plan::transition(id, "superseded", "an emergency exit took over its pools", json!({}));
                clear_bounds(p);
                super::notify::alert("severe", "Plan superseded", &format!("plan {id}: an emergency exit took over"));
                super::sched::trigger("plan_ended", "severe", &format!("plan {id} superseded"));
            }
        }
    }
    let all = plan::all();
    // expire plans waiting too long
    for p in all.iter().filter(|p| p["state"] == "awaiting_approval" || p["state"] == "validated") {
        let age_h = (super::now() - p["snapshot_ts"].as_i64().or(p["created"].as_i64()).unwrap_or(0)) as f64 / 3600.0;
        let max = effective_bounds(&x, &p["requested_bounds"])["plan_age_h"].as_f64().unwrap_or(6.0);
        if age_h > max {
            let _ = plan::transition(p["plan_id"].as_str().unwrap_or(""), "expired", &format!("older than {max} h before it started"), json!({}));
        }
    }
    // the plan being executed, else the oldest queued one
    let cur = all.iter().find(|p| p["state"] == "executing").cloned()
        .or_else(|| all.iter().filter(|p| p["state"] == "queued").min_by_key(|p| p["created"].as_i64().unwrap_or(0)).cloned());
    let Some(p) = cur else { return };
    let id = p["plan_id"].as_str().unwrap_or("").to_string();
    let bounds = effective_bounds(&x, &p["requested_bounds"]);
    if p["state"] == "queued" {
        // re-validate right before it starts: age, prices, limits on the projected state
        match super::policy::revalidate(&p, &x) {
            Ok(()) => {
                let steps = steps_of(&p, worker);
                if steps.is_empty() {
                    let _ = plan::transition(&id, "executing", "started", json!({"steps": [], "effective_bounds": bounds}));
                    let _ = plan::transition(&id, "confirmed", "nothing to do: already at the targets", json!({}));
                    return;
                }
                if plan::transition(&id, "executing", "started", json!({"steps": steps, "effective_bounds": bounds, "started": super::now()})).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = plan::transition(&id, "expired", &format!("re-validation failed: {e}"), json!({}));
                super::notify::alert("normal", "Plan expired", &format!("plan {id}: {e}"));
                return;
            }
        }
    }
    let Some(p) = plan::get(&id) else { return };
    let steps = p["steps"].as_array().cloned().unwrap_or_default();
    for (k, s) in steps.iter().enumerate() {
        match s["state"].as_str().unwrap_or("") {
            "done" => continue,
            "pending" => {
                // a strategy is applied between runs: one in flight would save its old state over it
                let _idle = if ["strategy", "add_pool"].contains(&s["kind"].as_str().unwrap_or("")) {
                    match worker.idle_guard() {
                        Some(g) => Some(g),
                        None => return, // the next pass, once the worker is between runs
                    }
                } else {
                    None
                };
                // stored as submitted before the work is queued: a restart never queues it twice
                let _ = plan::update(&id, |pp| {
                    pp["steps"][k]["state"] = json!("submitted");
                    pp["steps"][k]["submitted"] = json!(super::now());
                    pp["steps"][k]["attempts"] = json!(1);
                });
                match start_step(&p, s, &bounds, live) {
                    Ok(req) => {
                        let _ = plan::update(&id, |pp| pp["steps"][k]["request"] = json!(req));
                        super::journal::record("plan_step", json!({"plan_id": id, "step": s["step_no"], "kind": s["kind"], "pool": s["pool"], "request": req}));
                        worker.request(false);
                    }
                    Err(e) => {
                        let _ = plan::update(&id, |pp| { pp["steps"][k]["state"] = json!("failed"); pp["steps"][k]["error"] = json!(e.clone()); });
                        finish(&id, &p, "partially_completed", &format!("step {} failed: {e}", k + 1));
                    }
                }
                return;
            }
            "submitted" => {
                if s["kind"] == "add" && s["gone_at"].is_null() && s["request"].as_str().map(|r| !request_pending(r)).unwrap_or(false) {
                    // its tokens are assigned: the deploy follows in this cycle or the next
                    let _ = plan::update(&id, |pp| pp["steps"][k]["gone_at"] = json!(super::now()));
                    return;
                }
                if step_done(s, worker, live) {
                    let _ = plan::update(&id, |pp| { pp["steps"][k]["state"] = json!("done"); pp["steps"][k]["done"] = json!(super::now()); });
                    continue;
                }
                let since = s["submitted"].as_i64().unwrap_or(0);
                if super::now() - since > STEP_TIMEOUT_S {
                    let attempts = s["attempts"].as_i64().unwrap_or(1);
                    if attempts >= MAX_ATTEMPTS {
                        if let Some(r) = s["request"].as_str() {
                            remove_request(r);
                        }
                        let _ = plan::update(&id, |pp| pp["steps"][k]["state"] = json!("failed"));
                        finish(&id, &p, "partially_completed", &format!("step {} not done after {attempts} attempts; what was released stays Unallocated", k + 1));
                    } else {
                        let _idle = if ["strategy", "add_pool"].contains(&s["kind"].as_str().unwrap_or("")) {
                            match worker.idle_guard() {
                                Some(g) => Some(g),
                                None => return,
                            }
                        } else {
                            None
                        };
                        let old = s["request"].as_str().map(String::from);
                        // an add whose tokens were assigned is not added again: it waits for its deploy
                        if s["kind"] == "add" && old.as_deref().map(|r| !request_pending(r)).unwrap_or(false) {
                            let _ = plan::update(&id, |pp| {
                                pp["steps"][k]["attempts"] = json!(attempts + 1);
                                pp["steps"][k]["submitted"] = json!(super::now());
                            });
                            worker.request(false);
                            return;
                        }
                        // a withdrawal that was partly done is queued again for the rest only
                        let mut s = s.clone();
                        if let Some(left) = old.as_deref().and_then(outstanding) {
                            s["usd"] = json!(left);
                        }
                        if let Some(r) = &old {
                            remove_request(r);
                        }
                        let s = &s;
                        let req = match start_step(&p, s, &bounds, live) {
                            Ok(r) => r,
                            Err(e) => {
                                crate::warn!("agent", "plan {id} step {}: re-queue failed ({e}); tried again on the next timeout", k + 1);
                                None
                            }
                        };
                        let _ = plan::update(&id, |pp| {
                            pp["steps"][k]["attempts"] = json!(attempts + 1);
                            pp["steps"][k]["submitted"] = json!(super::now());
                            pp["steps"][k]["request"] = json!(req);
                            pp["steps"][k]["gone_at"] = Value::Null;
                        });
                        worker.request(false);
                    }
                }
                return;
            }
            _ => return,
        }
    }
    finish(&id, &p, "confirmed", "every step done");
}

/// Remove a queued request and the per-pool releases a withdrawal was split into (id-0, id-1, …).
fn remove_request(id: &str) {
    for r in crate::capital::requests() {
        if let Some(x) = r["id"].as_str() {
            if x == id || x.starts_with(&format!("{id}-")) {
                crate::capital::remove(x);
            }
        }
    }
}

fn clear_bounds(p: &Value) {
    for c in p["changes"].as_array().into_iter().flatten() {
        if let Some(pool) = c["pool"].as_str() {
            set_bound(pool, None);
        }
    }
}

fn finish(id: &str, p: &Value, state: &str, why: &str) {
    let _ = plan::transition(id, state, why, json!({"finished": super::now()}));
    clear_bounds(p);
    if state != "confirmed" {
        super::notify::alert("severe", &format!("Plan {state}"), &format!("plan {id}: {why}"));
        super::sched::trigger("plan_ended", "severe", &format!("plan {id} {state}: {why}"));
    } else {
        super::notify::alert("info", "Plan done", &format!("plan {id}: {}", p["summary"].as_str().unwrap_or("")));
    }
}

pub fn start(worker: Arc<crate::worker::Worker>) {
    std::thread::Builder::new().name("coordinator".into()).spawn(move || loop {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pass(&worker)));
        std::thread::sleep(std::time::Duration::from_secs(30));
    }).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_are_the_stricter() {
        let x = super::super::defaults();
        let b = effective_bounds(&x, &json!({"max_cost_bp": 40.0, "plan_age_h": 2.0}));
        assert_eq!(b["max_cost_bp"], json!(15.0));
        assert_eq!(b["plan_age_h"], json!(2.0));
        assert_eq!(effective_bounds(&x, &json!(null))["max_cost_bp"], json!(15.0));
    }
}
