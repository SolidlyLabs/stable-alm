//! Running a perp portfolio (`portfolio.rs`) live, in dry run: once a day, after the trade hour's
//! bar closes, the worker computes each coin's target leverage, reads the Hyperliquid account's
//! positions (public info API, by address) and writes the orders that would bring it to the targets.
//!
//! Nothing is signed or sent: placing orders needs Hyperliquid's agent-wallet signing and an API
//! key, which is not built. The plan is in `logs/perps_plan.json` (the latest) and
//! `logs/perps_plan.jsonl` (every one), and on the Perps API.
//!
//! Config, `config/perps.json`: {enabled, strategy_id (a saved "u:" perp portfolio), capital_usd
//! (used when no account is set, or as a cap on the account's value), account (0x Hyperliquid address)}.
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};

const INFO: &str = "https://api.hyperliquid.xyz/info";
/// Orders smaller than this are left out (Hyperliquid's minimum order value is $10).
const MIN_ORDER_USD: f64 = 11.0;
static BUSY: AtomicBool = AtomicBool::new(false);

fn path() -> std::path::PathBuf {
    crate::paths::config().join("perps.json")
}

pub fn config() -> Value {
    std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({"enabled": false, "strategy_id": null, "capital_usd": 10000.0, "account": null}))
}

fn valid_addr(a: &str) -> bool {
    a.len() == 42 && a.starts_with("0x") && a[2..].chars().all(|c| c.is_ascii_hexdigit())
}

pub fn set_config(b: &Value) -> Result<Value, String> {
    let mut c = config();
    if let Some(e) = b.get("enabled").and_then(|v| v.as_bool()) {
        c["enabled"] = json!(e);
    }
    if let Some(id) = b.get("strategy_id") {
        let id = id.as_str().ok_or("strategy_id: a saved strategy's id")?;
        strategy_of(id)?;
        c["strategy_id"] = json!(id);
    }
    if let Some(x) = b.get("capital_usd") {
        let x = x.as_f64().filter(|x| (100.0..=10_000_000.0).contains(x)).ok_or("capital_usd: 100 to 10,000,000")?;
        c["capital_usd"] = json!(x);
    }
    if let Some(a) = b.get("account") {
        match a.as_str().map(|s| s.trim().to_lowercase()) {
            None => c["account"] = Value::Null,
            Some(s) if s.is_empty() => c["account"] = Value::Null,
            Some(s) if valid_addr(&s) => c["account"] = json!(s),
            _ => return Err("account: a 0x address".into()),
        }
    }
    if c["enabled"] == true && c["strategy_id"].as_str().is_none() {
        return Err("choose the perp portfolio to run first".into());
    }
    crate::settings::write_atomic(&path(), &c).map_err(|e| e.to_string())?;
    Ok(c)
}

fn strategy_of(id: &str) -> Result<serde_json::Map<String, Value>, String> {
    let lib = crate::library::get();
    let all = lib["mine"].as_array().cloned().unwrap_or_default();
    let x = all.iter().find(|x| x["id"] == id).ok_or(format!("no saved strategy {id}"))?;
    let s = x["strategy"].as_object().cloned().ok_or("strategy")?;
    crate::portfolio::parse(&s)?;
    Ok(s)
}

pub fn last_plan() -> Value {
    std::fs::read_to_string(crate::paths::logs().join("perps_plan.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
}

pub fn status() -> Value {
    json!({"config": config(), "plan": last_plan(), "busy": BUSY.load(Ordering::SeqCst),
           "execution": "dry run: orders are planned, not sent (Hyperliquid signing is not built)"})
}

/// Size decimals per coin (Hyperliquid rounds order sizes to them).
fn sz_decimals() -> std::collections::HashMap<String, u32> {
    let mut m = std::collections::HashMap::new();
    if let Ok(v) = crate::rpc::post_json(INFO, &json!({"type": "meta"}), &[]) {
        for u in v["universe"].as_array().cloned().unwrap_or_default() {
            if let (Some(n), Some(d)) = (u["name"].as_str(), u["szDecimals"].as_u64()) {
                m.insert(n.to_string(), d as u32);
            }
        }
    }
    m
}

/// Compute today's targets and the orders that would reach them. Writes the plan files.
pub fn plan() -> Result<Value, String> {
    let c = config();
    let id = c["strategy_id"].as_str().ok_or("no perp portfolio chosen")?.to_string();
    let st = strategy_of(&id)?;
    let spec = crate::portfolio::parse(&st)?;
    let now = crate::engine::now() as i64;
    let d = crate::portfolio::load(&spec, now)?;
    let r = crate::portfolio::run(&spec, &d)?;
    let targets = crate::portfolio::targets(&spec, &r);
    let mids = crate::rpc::post_json(INFO, &json!({"type": "allMids"}), &[])?;
    let cap = c["capital_usd"].as_f64().unwrap_or(10_000.0);
    let (equity, positions, src) = match c["account"].as_str() {
        Some(a) => {
            let s = crate::rpc::post_json(INFO, &json!({"type": "clearinghouseState", "user": a}), &[])?;
            let eq: f64 = s["marginSummary"]["accountValue"].as_str().and_then(|x| x.parse().ok()).unwrap_or(0.0);
            let mut pos = std::collections::HashMap::new();
            for p in s["assetPositions"].as_array().cloned().unwrap_or_default() {
                let q = &p["position"];
                if let (Some(coin), Some(szi)) = (q["coin"].as_str(), q["szi"].as_str().and_then(|x| x.parse::<f64>().ok())) {
                    pos.insert(coin.to_string(), szi);
                }
            }
            (eq.min(cap), pos, "account")
        }
        None => (cap, Default::default(), "capital (no account set: positions taken as 0)"),
    };
    let dec = sz_decimals();
    let mut orders = vec![];
    let mut rows = vec![];
    let mut gross = 0.0;
    for (coin, lev, at) in &targets {
        let mid: f64 = mids[coin].as_str().and_then(|x| x.parse().ok()).unwrap_or(f64::NAN);
        let cur = positions.get(coin).copied().unwrap_or(0.0);
        let want = lev * equity / mid;
        let dd = dec.get(coin).copied().unwrap_or(3);
        let p10 = 10f64.powi(dd as i32);
        let delta = ((want - cur) * p10).round() / p10;
        gross += lev.abs();
        rows.push(json!({"coin": coin, "target_leverage": lev, "signal_at": at, "mid": mid, "position": cur, "target_size": want, "delta": delta}));
        if mid.is_finite() && (delta * mid).abs() >= MIN_ORDER_USD {
            orders.push(json!({"coin": coin, "side": if delta > 0.0 { "buy" } else { "sell" }, "size": delta.abs(), "usd": delta.abs() * mid,
                               "reduce_only": cur != 0.0 && want.abs() < cur.abs() && want.signum() == cur.signum()}));
        }
    }
    let plan = json!({"ts": now, "strategy_id": id, "strategy": spec.name, "equity_usd": equity, "equity_from": src, "gross_leverage": gross,
                      "overlay": r.overlay.last(), "coins": rows, "orders": orders, "sent": false,
                      "note": "dry run: these orders were not sent"});
    let logs = crate::paths::logs();
    let _ = std::fs::create_dir_all(&logs);
    crate::settings::write_atomic(&logs.join("perps_plan.json"), &plan).map_err(|e| e.to_string())?;
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(logs.join("perps_plan.jsonl")) {
        let _ = writeln!(f, "{plan}");
    }
    Ok(plan)
}

/// Run `plan` in the background when it is due: enabled, the trade hour's bar has closed today, and
/// no plan yet since. Called by the worker every cycle; returns at once.
pub fn maybe_run() {
    let c = config();
    if c["enabled"] != true {
        return;
    }
    let Some(id) = c["strategy_id"].as_str() else { return };
    let Ok(st) = strategy_of(id) else { return };
    let Ok(spec) = crate::portfolio::parse(&st) else { return };
    let now = crate::engine::now() as i64;
    let due = now - now.rem_euclid(86400) + spec.trade_hour_utc * 3600;
    if now < due || last_plan()["ts"].as_i64().map(|t| t >= due).unwrap_or(false) {
        return;
    }
    if BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        match plan() {
            Ok(p) => crate::info!("perps", "planned {} orders (dry run), gross {:.2}x", p["orders"].as_array().map(|a| a.len()).unwrap_or(0), p["gross_leverage"].as_f64().unwrap_or(0.0)),
            Err(e) => crate::warn!("perps", "plan failed: {e}"),
        }
        BUSY.store(false, Ordering::SeqCst);
    });
}

/// POST /api/perps: {action: "config", ...fields} or {action: "plan"} (plan now).
pub fn post(b: &Value) -> Result<Value, String> {
    match b["action"].as_str().unwrap_or("config") {
        "config" => set_config(b).map(|_| status()),
        "plan" => {
            if BUSY.swap(true, Ordering::SeqCst) {
                return Err("a plan is already being computed".into());
            }
            let r = plan();
            BUSY.store(false, Ordering::SeqCst);
            r.map(|_| status())
        }
        a => Err(format!("unknown action {a}")),
    }
}
