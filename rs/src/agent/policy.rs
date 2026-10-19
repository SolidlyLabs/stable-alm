//! The policy gate (plan sections 3 and 5). A submitted plan is checked by code, never by the model:
//!
//! | Check | Fails the plan | Needs approval |
//! |---|---|---|
//! | the snapshot is older than `execution.plan_age_h` | ✓ | |
//! | a pool is locked, or an emergency is on | ✓ | |
//! | a pool is not configured (other than add_pool) or its chain is off | ✓ | |
//! | more plans on a pool in 24 h than `limits.actions_per_pool_day` | ✓ | |
//! | a strategy put back within `limits.no_reversal_h` of being replaced | ✓ | |
//! | capital moved in 24 h above `limits.move_pct_day` of the account | ✓ | |
//! | a new pool's start above `limits.new_pool_pct` | ✓ | |
//! | a new pool with less than `limits.new_pool_days` of stored history | ✓ | |
//! | an exposure limit broken by the projected state (and not already broken now) | ✓ | |
//! | the projected state cannot be exited within `execution.max_emergency_bp` | ✓ | |
//! | a strategy change fails the strategy gate (sgate) | ✓ | |
//! | agent mode "advise" | | ✓ |
//! | a token not in the approved list nor in a configured pool | | ✓ |
//! | exit capacity not verified (no quote) or an advisory flag from the strategy gate | | ✓ |

use serde_json::{json, Value};

use super::exposure::{self, Holding, Move};

pub struct Verdict {
    pub ok: bool,
    pub failures: Vec<String>,
    pub approval: Vec<String>,
    pub report: Value,
}

fn pool_cfg(addr: &str) -> Option<Value> {
    crate::settings::load_pools().into_iter().find(|p| p["pool"].as_str().map(|a| a.eq_ignore_ascii_case(addr)).unwrap_or(false))
}

/// Plans touching a pool that started in the last `secs`.
fn recent_on(pool: &str, secs: i64, except: &str) -> Vec<Value> {
    let now = super::now();
    super::plan::all().into_iter().filter(|p| {
        p["plan_id"] != except && matches!(p["state"].as_str(), Some("queued" | "executing" | "confirmed" | "partially_completed"))
            && now - p["created"].as_i64().unwrap_or(0) < secs
            && p["changes"].as_array().into_iter().flatten().any(|c| c["pool"].as_str().map(|a| a.eq_ignore_ascii_case(pool)).unwrap_or(false))
    }).collect()
}

/// USD a plan moves (capital changes, new pools' starts, exits).
pub fn moved_usd(p: &Value, capital_of: &dyn Fn(&str) -> f64) -> f64 {
    p["changes"].as_array().into_iter().flatten().map(|c| {
        let pool = c["pool"].as_str().unwrap_or("");
        match c["type"].as_str().unwrap_or("") {
            "capital" => (c["target_usd"].as_f64().unwrap_or(0.0) - capital_of(pool)).abs(),
            "add_pool" => c["start_usd"].as_f64().unwrap_or(0.0),
            "exit_pool" => capital_of(pool),
            _ => 0.0,
        }
    }).sum()
}

/// The projected moves of a plan.
pub fn moves_of(p: &Value, capital_of: &dyn Fn(&str) -> f64) -> Vec<Move> {
    let mut out = vec![];
    for c in p["changes"].as_array().into_iter().flatten() {
        let pool = c["pool"].as_str().unwrap_or("").to_lowercase();
        let chain = c["chain_id"].as_u64().or_else(|| pool_cfg(&pool).map(|x| crate::worker::pool_chain(&x))).unwrap_or(crate::chains::BASE);
        let cur = capital_of(&pool);
        match c["type"].as_str().unwrap_or("") {
            "capital" => {
                let t = c["target_usd"].as_f64().unwrap_or(cur);
                if t < cur {
                    out.push(Move { chain, from: Some(pool), to: None, usd: cur - t });
                } else if t > cur {
                    out.push(Move { chain, from: None, to: Some(pool), usd: t - cur });
                }
            }
            "add_pool" => out.push(Move { chain, from: None, to: Some(pool), usd: c["start_usd"].as_f64().unwrap_or(0.0) }),
            "exit_pool" => out.push(Move { chain, from: Some(pool), to: None, usd: cur }),
            _ => {}
        }
    }
    // releases first, so what a plan frees can fund what it adds
    out.sort_by_key(|m| m.to.is_some());
    out
}

fn tokens_of(cid: u64, pool: &str) -> Option<[(String, String); 2]> {
    let (t0, t1) = crate::chain::pool_tokens(cid, pool).ok()?;
    let s0 = crate::prices::meta(cid, &t0).ok()?.symbol;
    let s1 = crate::prices::meta(cid, &t1).ok()?.symbol;
    Some([(t0.to_lowercase(), s0), (t1.to_lowercase(), s1)])
}

fn approved_token(x: &Value, cid: u64, token: &str) -> bool {
    let t = token.to_lowercase();
    if exposure::safe_assets(cid).contains(&t) {
        return true;
    }
    if x["agent"]["approved_tokens"].as_array().into_iter().flatten()
        .any(|e| e["chain_id"].as_u64() == Some(cid) && e["address"].as_str().map(|a| a.eq_ignore_ascii_case(&t)).unwrap_or(false)) {
        return true;
    }
    crate::settings::load_pools().iter().filter(|p| crate::worker::pool_chain(p) == cid)
        .filter_map(|p| tokens_of(cid, p["pool"].as_str()?))
        .any(|[a, b]| a.0 == t || b.0 == t)
}

/// Check a plan against everything above.
pub fn check(p: &Value, x: &Value, worker: &crate::worker::Worker) -> Verdict {
    let mut fail: Vec<String> = vec![];
    let mut appr: Vec<String> = vec![];
    let mut report = serde_json::Map::new();
    let now = super::now();
    let lim = &x["limits"];
    let lf = |k: &str, d: f64| lim[k].as_f64().unwrap_or(d);
    let id = p["plan_id"].as_str().unwrap_or("");
    let age_h = (now - p["snapshot_ts"].as_i64().unwrap_or(now)) as f64 / 3600.0;
    if age_h > x["execution"]["plan_age_h"].as_f64().unwrap_or(6.0) {
        fail.push(format!("the snapshot is {age_h:.1} h old"));
    }
    if let Some(e) = super::locks::emergency() {
        fail.push(format!("an emergency is on: {}", e["reason"].as_str().unwrap_or("")));
    }
    let cap = |pool: &str| super::exec::capital_of(worker, pool).unwrap_or(0.0);
    let st = worker.status.lock().unwrap().clone();
    let holdings: Vec<Holding> = exposure::holdings(&st["account"], &st["capital"]);
    let total: f64 = holdings.iter().map(|h| h.usd).sum();
    report.insert("account_usd".into(), json!(total));
    for (i, c) in p["changes"].as_array().into_iter().flatten().enumerate() {
        let n = i + 1;
        let pool = c["pool"].as_str().unwrap_or("").to_lowercase();
        let cfg = pool_cfg(&pool);
        let cid = c["chain_id"].as_u64().or_else(|| cfg.as_ref().map(crate::worker::pool_chain)).unwrap_or(crate::chains::BASE);
        if c["type"] != "add_pool" && cfg.is_none() {
            fail.push(format!("change {n}: pool {pool} is not configured"));
            continue;
        }
        if !crate::chains::get(cid).enabled {
            fail.push(format!("change {n}: chain {cid} is off"));
        }
        if let Some(l) = super::locks::active(cid, Some(&pool)) {
            if c["type"] != "exit_pool" && !(c["type"] == "capital" && c["target_usd"].as_f64().unwrap_or(0.0) < cap(&pool)) {
                fail.push(format!("change {n}: the pool is locked ({})", l["reason"].as_str().unwrap_or("")));
            }
        }
        let recent = recent_on(&pool, 86_400, id);
        if recent.len() as f64 >= lf("actions_per_pool_day", 3.0) {
            fail.push(format!("change {n}: {} plans on this pool in 24 h already", recent.len()));
        }
        if c["strategy"].is_object() {
            let h = super::plan::strategy_hash(&c["strategy"]);
            let back = recent_on(&pool, (lf("no_reversal_h", 12.0) * 3600.0) as i64, id).iter().any(|q| {
                q["changes"].as_array().into_iter().flatten().any(|qc| qc["pool"].as_str().map(|a| a.eq_ignore_ascii_case(&pool)).unwrap_or(false)
                    && qc["previous_strategy_hash"].as_str() == Some(h.as_str()))
            });
            if back {
                fail.push(format!("change {n}: puts back a strategy replaced less than {} h ago", lf("no_reversal_h", 12.0)));
            }
        }
        if c["type"] == "add_pool" {
            let start = c["start_usd"].as_f64().unwrap_or(0.0);
            if total > 0.0 && start > lf("new_pool_pct", 5.0) / 100.0 * total + 1e-6 {
                fail.push(format!("change {n}: a new pool starts with at most {}% of the account (${:.0})", lf("new_pool_pct", 5.0), lf("new_pool_pct", 5.0) / 100.0 * total));
            }
            let days = crate::history::coverage(cid, &pool).map(|c| (c.to_ts - c.from_ts) as f64 / 86_400.0).unwrap_or(0.0);
            if days < lf("new_pool_days", 14.0) {
                fail.push(format!("change {n}: only {days:.0} days of history stored (needs {})", lf("new_pool_days", 14.0)));
            }
            match tokens_of(cid, &pool) {
                Some([a, b]) => {
                    for t in [a, b] {
                        if !approved_token(x, cid, &t.0) {
                            appr.push(format!("token {} ({}) is not approved", t.1, t.0));
                        }
                    }
                }
                None => fail.push(format!("change {n}: the pool's tokens could not be read")),
            }
        }
        if c["strategy"].is_object() {
            match super::sgate::check(cid, &pool, &c["strategy"], cfg.as_ref().map(|v| v["strategy"].clone()), c["start_usd"].as_f64().unwrap_or(cap(&pool)).max(100.0)) {
                Ok(r) => {
                    if r["pass"] != true {
                        fail.push(format!("change {n}: strategy gate: {}", r["why"].as_str().unwrap_or("failed")));
                    }
                    for a in r["advisory"].as_array().into_iter().flatten() {
                        appr.push(format!("change {n}: {}", a.as_str().unwrap_or("")));
                    }
                    report.insert(format!("sgate_{n}"), r);
                }
                Err(e) => fail.push(format!("change {n}: strategy gate could not run: {e}")),
            }
        }
    }
    // capital moved in 24 h
    let moved = moved_usd(p, &cap);
    let before: f64 = super::plan::all().iter()
        .filter(|q| q["plan_id"] != id && now - q["created"].as_i64().unwrap_or(0) < 86_400 && matches!(q["state"].as_str(), Some("queued" | "executing" | "confirmed" | "partially_completed")))
        .map(|q| q["moved_usd"].as_f64().unwrap_or(0.0)).sum();
    report.insert("moved_usd".into(), json!(moved));
    if total > 0.0 && moved + before > lf("move_pct_day", 20.0) / 100.0 * total + 1.0 {
        fail.push(format!("moves ${:.0} (with ${before:.0} already today), above {}% of the account a day", moved, lf("move_pct_day", 20.0)));
    }
    // exposure on the projected state
    let cid0 = p["changes"][0]["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
    let safe_tok = exposure::safe_assets(cid0).into_iter().next().unwrap_or_default();
    let safe = (safe_tok.clone(), crate::prices::meta(cid0, &safe_tok).map(|m| m.symbol).unwrap_or_else(|_| "USDC".into()));
    let moves = moves_of(p, &cap);
    let after = exposure::project(&holdings, &moves, &safe, &tokens_of);
    let (s0, s1) = (exposure::summary(&holdings, &[safe_tok.clone()]), exposure::summary(&after, &[safe_tok.clone()]));
    let (b0, b1) = (exposure::breaches(&s0, lim), exposure::breaches(&s1, lim));
    for b in &b1 {
        let was = b0.iter().find(|o| o["limit"] == b["limit"] && o["key"] == b["key"]).and_then(|o| o["share"].as_f64());
        let s = b["share"].as_f64().unwrap_or(0.0);
        if was.map(|w| s > w + 1e-6).unwrap_or(true) {
            fail.push(format!("{} {}: {:.1}% after the plan (max {:.1}%)", b["limit"].as_str().unwrap_or(""), b["key"].as_str().unwrap_or(""), s * 100.0, b["max"].as_f64().unwrap_or(0.0) * 100.0));
        }
    }
    report.insert("exposure_after".into(), s1);
    // exit capacity: every non-safe token of the projected state, sold to the safe asset
    let max_bp = x["execution"]["max_emergency_bp"].as_f64().unwrap_or(100.0);
    let need = lf("exit_capacity_pct", 90.0) / 100.0;
    let mut by_tok: std::collections::BTreeMap<(u64, String), f64> = Default::default();
    for h in &after {
        if !exposure::safe_assets(h.chain).contains(&h.token.to_lowercase()) {
            *by_tok.entry((h.chain, h.token.to_lowercase())).or_default() += h.usd;
        }
    }
    let risky: f64 = by_tok.values().sum();
    let mut exitable = 0.0;
    let mut unverified = false;
    let mut quotes = vec![];
    for ((cid, t), usd) in &by_tok {
        if *usd < 1.0 {
            exitable += usd;
            continue;
        }
        let safe = exposure::safe_assets(*cid).into_iter().next().unwrap_or_default();
        match super::refdata::swap_cost(*cid, t, &safe, *usd) {
            Ok(q) => {
                let bp = q["cost_bp"].as_f64().unwrap_or(f64::INFINITY);
                if bp <= max_bp {
                    exitable += usd;
                }
                quotes.push(json!({"token": t, "usd": usd, "cost_bp": bp}));
            }
            Err(e) => {
                unverified = true;
                quotes.push(json!({"token": t, "usd": usd, "error": e}));
            }
        }
    }
    report.insert("exit_quotes".into(), json!(quotes));
    if risky > 1.0 && !unverified && exitable / risky < need {
        fail.push(format!("only {:.0}% of the non-safe holdings can be sold within {max_bp} bp (needs {:.0}%)", exitable / risky * 100.0, need * 100.0));
    }
    if unverified {
        appr.push("exit capacity not verified (no quote for a token)".into());
    }
    if x["agent"]["mode"] != "auto" {
        appr.insert(0, "agent mode is advise".into());
    }
    Verdict { ok: fail.is_empty(), failures: fail, approval: appr, report: Value::Object(report) }
}

/// Validate a submitted plan and move it on: rejected, awaiting approval, or queued.
pub fn gate(id: &str, worker: &crate::worker::Worker) -> Result<Value, String> {
    let x = super::settings();
    let p = super::plan::get(id).ok_or("no such plan")?;
    let v = check(&p, &x, worker);
    let cap = |pool: &str| super::exec::capital_of(worker, pool).unwrap_or(0.0);
    let moved = moved_usd(&p, &cap);
    if !v.ok {
        return super::plan::transition(id, "rejected", &v.failures.join("; "), json!({"policy": v.report, "failures": v.failures}));
    }
    super::plan::transition(id, "validated", "passed the policy gate", json!({"policy": v.report, "moved_usd": moved, "approval_reasons": v.approval}))?;
    if v.approval.is_empty() {
        super::plan::transition(id, "queued", "no approval needed (auto)", json!({}))
    } else {
        let p = super::plan::transition(id, "awaiting_approval", &v.approval.join("; "), json!({}))?;
        super::notify::alert("normal", "Plan waits for approval", &format!("{}: {}", id, p["summary"].as_str().unwrap_or("")));
        Ok(p)
    }
}

/// The check right before a queued plan starts: its age, the prices since the snapshot, locks.
pub fn revalidate(p: &Value, x: &Value) -> Result<(), String> {
    let now = super::now();
    let b = super::exec::effective_bounds(x, &p["requested_bounds"]);
    let age_h = (now - p["snapshot_ts"].as_i64().unwrap_or(now)) as f64 / 3600.0;
    if age_h > b["plan_age_h"].as_f64().unwrap_or(6.0) {
        return Err(format!("the snapshot is {age_h:.1} h old"));
    }
    if super::locks::emergency().is_some() {
        return Err("an emergency is on".into());
    }
    let max_move = b["price_move_pct"].as_f64().unwrap_or(1.0);
    for (k, v) in p["prices_at_snapshot"].as_object().into_iter().flatten() {
        let Some((cid, tok)) = k.split_once(':') else { continue };
        let (Ok(cid), Some(then)) = (cid.parse::<u64>(), v.as_f64()) else { continue };
        if let Ok(now_px) = crate::prices::usd(cid, tok) {
            let mv = (now_px / then - 1.0).abs() * 100.0;
            if then > 0.0 && mv > max_move {
                return Err(format!("{tok} moved {mv:.2}% since the snapshot (max {max_move}%)"));
            }
        }
    }
    for c in p["changes"].as_array().into_iter().flatten() {
        let pool = c["pool"].as_str().unwrap_or("");
        let cid = c["chain_id"].as_u64().or_else(|| pool_cfg(pool).map(|x| crate::worker::pool_chain(&x))).unwrap_or(crate::chains::BASE);
        let down = c["type"] == "exit_pool" || (c["type"] == "capital" && c["target_usd"].as_f64().unwrap_or(f64::INFINITY) < 1.0);
        if !down {
            if let Some(l) = super::locks::active(cid, Some(pool)) {
                return Err(format!("{pool} is locked: {}", l["reason"].as_str().unwrap_or("")));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_release_first() {
        let p = json!({"changes": [
            {"type": "capital", "pool": "0xaa00000000000000000000000000000000000000", "chain_id": 8453, "target_usd": 300.0},
            {"type": "capital", "pool": "0xbb00000000000000000000000000000000000000", "chain_id": 8453, "target_usd": 50.0},
        ]});
        let cap = |p: &str| if p.starts_with("0xaa") { 100.0 } else { 250.0 };
        let m = moves_of(&p, &cap);
        assert_eq!(m.len(), 2);
        assert!(m[0].to.is_none() && (m[0].usd - 200.0).abs() < 1e-9);
        assert!(m[1].from.is_none() && (m[1].usd - 200.0).abs() < 1e-9);
        assert!((moved_usd(&p, &cap) - 400.0).abs() < 1e-9);
    }
}
