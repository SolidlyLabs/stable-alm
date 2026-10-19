//! The API spend allowance (plan section 13), in data/agent/budget.json.
//!
//! - The month's allowance is `agent.budget_usd_month` plus what was left last month (at most one
//!   month's worth carries over).
//! - `agent.reserve_pct` of the allowance is kept for the daily review and severe triggers: a
//!   routine or normal review may not dip into it.
//! - Every call reserves its worst case first (its input estimate plus `max_tokens` of output, plus
//!   its web searches); the call is not made when that does not fit. The reservation is settled
//!   with the real usage from the response.
//! - At zero the agent stops (hard stop) until the next month or a higher allowance.
//!
//! Prices (Claude Opus 5.5, per million tokens): input $4, output $20, cache write $5 (5-minute),
//! cache read $0.20; web search $10 per 1,000.

use serde_json::{json, Value};
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());
const FILE: &str = "budget.json";

pub const IN: f64 = 4.0;
pub const OUT: f64 = 20.0;
pub const CACHE_WRITE: f64 = 5.0;
pub const CACHE_READ: f64 = 0.20;
pub const SEARCH: f64 = 10.0 / 1000.0;

fn month_of(ts: i64) -> String {
    let (y, m, ..) = crate::log::civil(ts);
    format!("{y}-{m:02}")
}

/// USD of a response's usage block.
pub fn cost(usage: &Value) -> f64 {
    let n = |k: &str| usage[k].as_f64().unwrap_or(0.0);
    n("input_tokens") * IN / 1e6 + n("output_tokens") * OUT / 1e6 + n("cache_creation_input_tokens") * CACHE_WRITE / 1e6
        + n("cache_read_input_tokens") * CACHE_READ / 1e6 + usage["server_tool_use"]["web_search_requests"].as_f64().unwrap_or(0.0) * SEARCH
}

/// Worst case of a call: all input uncached, all output used, every search used.
pub fn worst_case(input_chars: usize, max_tokens: u64, searches: u64) -> f64 {
    (input_chars as f64 / 3.0) * CACHE_WRITE / 1e6 + max_tokens as f64 * OUT / 1e6 + searches as f64 * SEARCH
}

fn load(x: &Value, now: i64) -> Value {
    let mut b = super::read_json(FILE, json!({}));
    let m = month_of(now);
    if b["month"].as_str() != Some(&m) {
        let monthly = x["agent"]["budget_usd_month"].as_f64().unwrap_or(0.0);
        let left = if b["month"].is_string() { (allowance_of(&b, monthly) - b["spent"].as_f64().unwrap_or(0.0)).max(0.0).min(monthly) } else { 0.0 };
        let mut hist = b["history"].as_array().cloned().unwrap_or_default();
        if b["month"].is_string() {
            hist.push(json!({"month": b["month"], "spent": b["spent"], "calls": b["calls"]}));
        }
        let n = hist.len();
        b = json!({"month": m, "spent": 0.0, "carry": left, "reserved": {}, "calls": 0, "history": hist[n.saturating_sub(24)..]});
    }
    b
}

fn allowance_of(b: &Value, monthly: f64) -> f64 {
    monthly + b["carry"].as_f64().unwrap_or(0.0)
}

/// What is left for a review of this kind (routine and normal reviews leave the reserve alone).
pub fn available(x: &Value, kind: &str) -> f64 {
    let _g = LOCK.lock().unwrap();
    left(&load(x, super::now()), x, kind)
}

fn left(b: &Value, x: &Value, kind: &str) -> f64 {
    let monthly = x["agent"]["budget_usd_month"].as_f64().unwrap_or(0.0);
    let all = allowance_of(b, monthly);
    let reserved: f64 = b["reserved"].as_object().map(|o| o.values().filter_map(|v| v.as_f64()).sum()).unwrap_or(0.0);
    let mut l = all - b["spent"].as_f64().unwrap_or(0.0) - reserved;
    if !matches!(kind, "daily" | "severe_trigger" | "manual") {
        l -= all * x["agent"]["reserve_pct"].as_f64().unwrap_or(25.0) / 100.0;
    }
    l
}

/// Reserve a call's worst case; Err when it does not fit.
pub fn reserve(x: &Value, id: &str, usd: f64, kind: &str) -> Result<(), String> {
    let _g = LOCK.lock().unwrap();
    let mut b = load(x, super::now());
    let l = left(&b, x, kind);
    if usd > l {
        return Err(format!("the API budget has ${:.2} left for a {kind} review; this call may cost up to ${usd:.2}", l.max(0.0)));
    }
    b["reserved"][id] = json!(usd);
    let _ = super::write_json(FILE, &b);
    Ok(())
}

/// Settle a reservation with the real cost (None: the call failed before it was billed).
pub fn settle(x: &Value, id: &str, usage: Option<&Value>) -> f64 {
    let _g = LOCK.lock().unwrap();
    let mut b = load(x, super::now());
    if let Some(o) = b["reserved"].as_object_mut() {
        o.remove(id);
    }
    let c = usage.map(cost).unwrap_or(0.0);
    b["spent"] = json!(b["spent"].as_f64().unwrap_or(0.0) + c);
    b["calls"] = json!(b["calls"].as_u64().unwrap_or(0) + usage.map(|_| 1).unwrap_or(0));
    let _ = super::write_json(FILE, &b);
    c
}

pub fn status(x: &Value) -> Value {
    let _g = LOCK.lock().unwrap();
    let mut b = load(x, super::now());
    let monthly = x["agent"]["budget_usd_month"].as_f64().unwrap_or(0.0);
    b["allowance"] = json!(allowance_of(&b, monthly));
    b["left_routine"] = json!(left(&b, x, "routine"));
    b["left_daily"] = json!(left(&b, x, "daily"));
    b
}

/// API spend since a time (the months it covers, whole; the shadow report's cost).
pub fn spent_since(x: &Value, ts: i64) -> f64 {
    let b = status(x);
    let from = month_of(ts);
    b["history"].as_array().into_iter().flatten().filter(|h| h["month"].as_str().map(|m| m >= from.as_str()).unwrap_or(false))
        .filter_map(|h| h["spent"].as_f64()).sum::<f64>() + b["spent"].as_f64().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs_and_reserve() {
        let u = json!({"input_tokens": 1_000_000, "output_tokens": 100_000, "cache_read_input_tokens": 1_000_000, "cache_creation_input_tokens": 0,
                       "server_tool_use": {"web_search_requests": 3}});
        assert!((cost(&u) - (4.0 + 2.0 + 0.2 + 0.03)).abs() < 1e-9);
        let x = json!({"agent": {"budget_usd_month": 100.0, "reserve_pct": 25.0}});
        let b = json!({"month": "2026-10", "spent": 50.0, "carry": 0.0, "reserved": {"a": 10.0}});
        assert!((left(&b, &x, "daily") - 40.0).abs() < 1e-9);
        assert!((left(&b, &x, "routine") - 15.0).abs() < 1e-9);
        assert_eq!(month_of(1_792_000_000), "2026-10");
    }
}
