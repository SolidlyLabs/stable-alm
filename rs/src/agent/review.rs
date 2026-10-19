//! One review (plan section 10): snapshot → model → tools → outcome.
//!
//! | Outcome | When |
//! |---|---|
//! | hold_decision | the model called hold |
//! | escalated | the model called escalate (an alert goes to the user) |
//! | plan_submitted | a plan passed the policy gate (queued, or waiting for approval) |
//! | plan_rejected | a plan failed the policy gate (the reasons are kept) |
//! | incomplete | no decision within `agent.max_tool_calls` or `agent.review_minutes` |
//! | refused | the model refused |
//! | api_error | the API failed (after its retries) |
//! | budget_skipped | the next call's worst case did not fit the budget |
//! | skipped | off, no key, or another review running |
//!
//! Each review is kept in data/agent/reviews/<id>.json (the whole exchange) and summarised in
//! reviews.jsonl and the journal.

use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};

pub const PROMPT_VERSION: &str = "agent_system_v1";
const SYSTEM: &str = include_str!("../../prompts/agent_system_v1.md");
const MAX_TOKENS: u64 = 16_000;
static RUNNING: AtomicBool = AtomicBool::new(false);

pub fn running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

fn rand6() -> String {
    let mut b = [0u8; 3];
    let _ = getrandom::getrandom(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn finish(rec: &mut Value, outcome: &str, detail: Value) -> Value {
    rec["outcome"] = json!(outcome);
    rec["detail"] = detail;
    rec["ended"] = json!(super::now());
    let id = rec["review_id"].as_str().unwrap_or("").to_string();
    let dir = super::dir().join("reviews");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join(format!("{id}.json")), serde_json::to_string_pretty(rec).unwrap_or_default());
    // keep the last 200 transcripts
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        files.sort();
        let n = files.len();
        for f in files.into_iter().take(n.saturating_sub(200)) {
            let _ = std::fs::remove_file(f);
        }
    }
    let short = json!({"review_id": id, "ts": rec["started"], "ended": rec["ended"], "kind": rec["kind"], "effort": rec["effort"], "outcome": outcome,
                       "detail": rec["detail"], "cost_usd": rec["cost_usd"], "tool_calls": rec["tool_calls"], "snapshot_id": rec["snapshot_id"],
                       "plan_id": rec["plan_id"], "model": rec["model"]});
    super::append("reviews.jsonl", &short);
    super::journal::record("review", short.clone());
    short
}

pub fn recent(n: usize) -> Vec<Value> {
    super::tail("reviews.jsonl", n)
}

pub fn get(id: &str) -> Option<Value> {
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    std::fs::read_to_string(super::dir().join("reviews").join(format!("{id}.json"))).ok().and_then(|s| serde_json::from_str(&s).ok())
}

/// Run one review (blocking; called from the scheduler's thread).
pub fn run(worker: &crate::worker::Worker, kind: &str, effort: &str, triggers: &[Value]) -> Value {
    let x = super::settings();
    let now = super::now();
    let model = x["agent"]["model"].as_str().unwrap_or("claude-opus-5-5").to_string();
    let id = format!("r{now}-{}", rand6());
    let mut rec = json!({"review_id": id, "kind": kind, "effort": effort, "started": now, "model": model, "triggers": triggers,
                         "prompt_version": PROMPT_VERSION, "cost_usd": 0.0, "tool_calls": 0, "plan_id": null});
    if !super::enabled_in(&x) || x["agent"]["mode"].as_str().unwrap_or("off") == "off" {
        return finish(&mut rec, "skipped", json!("the agent is off"));
    }
    if !super::claude::has_key() {
        return finish(&mut rec, "skipped", json!("no API key set"));
    }
    if RUNNING.swap(true, Ordering::SeqCst) {
        return json!({"review_id": id, "outcome": "skipped", "detail": "another review is running"});
    }
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_inner(worker, &x, &mut rec, kind, effort, triggers, &model)))
        .unwrap_or_else(|_| finish(&mut rec.clone(), "api_error", json!("the review crashed")));
    RUNNING.store(false, Ordering::SeqCst);
    out
}

fn run_inner(worker: &crate::worker::Worker, x: &Value, rec: &mut Value, kind: &str, effort: &str, triggers: &[Value], model: &str) -> Value {
    let id = rec["review_id"].as_str().unwrap_or("").to_string();
    let routine = kind == "routine";
    if super::budget::available(x, kind) <= 0.0 {
        return finish(rec, "budget_skipped", json!("the month's API budget is used up"));
    }
    let snap = super::snapshot::build(worker);
    rec["snapshot_id"] = snap["snapshot_id"].clone();
    rec["snapshot_hash"] = snap["hash"].clone();
    let tools = super::tools::definitions(x, routine);
    let system = json!([{"type": "text", "text": SYSTEM, "cache_control": {"type": "ephemeral"}}]);
    let intro = format!(
        "Review kind: {kind} (effort {effort}).{}\nAgent mode: {} ({}).\nTriggers since the last review:\n{}\n\nSnapshot {} (built by code at {}):\n{}",
        if routine { " A routine review may only hold or escalate." } else { "" },
        x["agent"]["mode"].as_str().unwrap_or("off"),
        if x["agent"]["mode"] == "auto" { "plans that pass the policy gate run without approval" } else { "every plan waits for the user's approval" },
        if triggers.is_empty() { "none".to_string() } else { serde_json::to_string_pretty(triggers).unwrap_or_default() },
        snap["snapshot_id"].as_str().unwrap_or(""), snap["blocks"]["now_utc"].as_str().unwrap_or(""),
        snap["blocks"]);
    let mut messages: Vec<Value> = vec![json!({"role": "user", "content": [{"type": "text", "text": intro}]})];
    let max_calls = x["agent"]["max_tool_calls"].as_f64().unwrap_or(40.0) as u64;
    let deadline = super::now() + (x["agent"]["review_minutes"].as_f64().unwrap_or(10.0) * 60.0) as i64;
    let searches = if x["agent"]["web_search"].as_bool().unwrap_or(true) { x["agent"]["searches_per_review"].as_f64().unwrap_or(5.0) as u64 } else { 0 };
    let mut nudged = false;
    let mut turn = 0;
    loop {
        turn += 1;
        if super::now() > deadline {
            return finish(rec, "incomplete", json!(format!("no decision within {} minutes", x["agent"]["review_minutes"])));
        }
        // the conversation's cache breakpoint moves to the newest user turn
        for m in messages.iter_mut() {
            for b in m["content"].as_array_mut().into_iter().flatten() {
                if let Some(o) = b.as_object_mut() {
                    o.remove("cache_control");
                }
            }
        }
        if let Some(b) = messages.last_mut().and_then(|m| m["content"].as_array_mut()).and_then(|a| a.last_mut()) {
            b["cache_control"] = json!({"type": "ephemeral"});
        }
        let body = json!({"model": model, "max_tokens": MAX_TOKENS, "system": system, "tools": tools, "messages": messages,
                          "output_config": {"effort": effort}});
        let rid = format!("{id}-{turn}");
        let worst = super::budget::worst_case(body.to_string().len(), MAX_TOKENS, if turn == 1 { searches } else { 0 });
        if let Err(e) = super::budget::reserve(x, &rid, worst, kind) {
            return finish(rec, "budget_skipped", json!(e));
        }
        let resp = match super::claude::call(&body) {
            Ok(r) => r,
            Err(e) => {
                // a call that failed in transit may still have been billed: count its worst case
                let usage = if e.billed { Some(json!({"output_tokens": MAX_TOKENS})) } else { None };
                let c = super::budget::settle(x, &rid, usage.as_ref());
                rec["cost_usd"] = json!(rec["cost_usd"].as_f64().unwrap_or(0.0) + c);
                return finish(rec, "api_error", json!(format!("{}{}", e.status.map(|s| format!("HTTP {s}: ")).unwrap_or_default(), e.message)));
            }
        };
        let c = super::budget::settle(x, &rid, Some(&resp["usage"]));
        rec["cost_usd"] = json!(rec["cost_usd"].as_f64().unwrap_or(0.0) + c);
        rec["usage"] = json!(rec["usage"].as_array().cloned().unwrap_or_default().into_iter().chain([resp["usage"].clone()]).collect::<Vec<_>>());
        let content = resp["content"].as_array().cloned().unwrap_or_default();
        messages.push(json!({"role": "assistant", "content": content}));
        rec["messages"] = json!(messages);
        let stop = resp["stop_reason"].as_str().unwrap_or("");
        if stop == "refusal" {
            return finish(rec, "refused", json!("the model refused"));
        }
        if stop == "pause_turn" {
            continue;
        }
        let uses: Vec<Value> = content.iter().filter(|b| b["type"] == "tool_use").cloned().collect();
        if uses.is_empty() {
            if stop == "max_tokens" || nudged {
                return finish(rec, "incomplete", json!(format!("the model stopped without a decision ({stop})")));
            }
            nudged = true;
            messages.push(json!({"role": "user", "content": [{"type": "text", "text": "End the review with one decision tool: hold, escalate or submit_plan."}]}));
            continue;
        }
        // a decision ends the review (the first one, if several)
        if let Some(d) = uses.iter().find(|u| super::tools::DECISIONS.contains(&u["name"].as_str().unwrap_or(""))) {
            let input = &d["input"];
            match d["name"].as_str().unwrap_or("") {
                "hold" => return finish(rec, "hold_decision", input["reason"].clone()),
                "escalate" => {
                    let sev = if input["severity"] == "severe" { "severe" } else { "normal" };
                    super::notify::alert(sev, "The agent asks you to look", input["reason"].as_str().unwrap_or(""));
                    return finish(rec, "escalated", input["reason"].clone());
                }
                _ => {
                    if routine {
                        return finish(rec, "incomplete", json!("a routine review may not submit a plan"));
                    }
                    let body = match super::tools::plan_body(input) {
                        Ok(b) => b,
                        Err(e) => return finish(rec, "plan_rejected", json!(e)),
                    };
                    let meta = json!({"snapshot_id": snap["snapshot_id"], "snapshot_ts": snap["ts"], "account_version": snap["account_version"],
                                      "prompt_version": PROMPT_VERSION, "model": model, "prices": snap["prices"]});
                    let tuid = d["id"].as_str().unwrap_or("");
                    let p = match super::plan::submit(&id, tuid, &body, &meta) {
                        Ok((p, _)) => p,
                        Err(e) => return finish(rec, "plan_rejected", json!(e)),
                    };
                    let pid = p["plan_id"].as_str().unwrap_or("").to_string();
                    rec["plan_id"] = json!(pid);
                    return match super::policy::gate(&pid, worker) {
                        Ok(p) if p["state"] == "rejected" => finish(rec, "plan_rejected", p["failures"].clone()),
                        Ok(p) => finish(rec, "plan_submitted", json!(p["state"])),
                        Err(e) => finish(rec, "plan_rejected", json!(e)),
                    };
                }
            }
        }
        // the other tools run, and their results go back
        let mut results = vec![];
        for u in &uses {
            let n = rec["tool_calls"].as_u64().unwrap_or(0) + 1;
            rec["tool_calls"] = json!(n);
            if n > max_calls {
                return finish(rec, "incomplete", json!(format!("more than {max_calls} tool calls")));
            }
            let r = super::tools::run(u["name"].as_str().unwrap_or(""), &u["input"], worker);
            let (text, err) = match r {
                Ok(v) => (v.to_string(), false),
                Err(e) => (e, true),
            };
            let text: String = text.chars().take(40_000).collect();
            results.push(json!({"type": "tool_result", "tool_use_id": u["id"], "content": text, "is_error": err}));
        }
        messages.push(json!({"role": "user", "content": results}));
    }
}
