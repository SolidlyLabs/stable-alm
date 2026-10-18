//! The strategy library of the Strategy lab: built-in examples, and the strategies the user saved
//! under a name (config/strategies.json). Built-ins use units that fit any pool: tick spacings,
//! percent, or indicators.

use serde_json::{json, Value};

const MAX_SAVED: usize = 200;

fn n(id: &str, kind: &str, x: i64, y: i64, p: Value, inputs: Value) -> Value {
    let mut v = json!({"id": id, "kind": kind, "x": x, "y": y, "p": p, "in": inputs});
    if p.get("unit").is_some() {
        v["unit"] = p["unit"].clone();
        v["p"].as_object_mut().unwrap().remove("unit");
    }
    v
}

fn graph(name: &str, nodes: Vec<Value>) -> Value {
    json!({"type": "graph", "name": name, "nodes": nodes})
}

/// The price node, a "price left the range" trigger and the rebalance sink, shared by most examples.
fn moves(delay: f64, cooldown: f64, buffer_spacings: f64) -> Vec<Value> {
    vec![
        n("oor", "out_of_range", 700, 330, json!({"below": buffer_spacings, "above": buffer_spacings, "unit": "spacings"}), json!({})),
        n("move", "rebalance", 960, 330, json!({"delay_min": delay, "cooldown_min": cooldown}), json!({"when": ["oor", "out"]})),
    ]
}

pub fn builtin() -> Vec<Value> {
    let price = || n("p", "price", 20, 40, json!({}), json!({}));
    let mut out = vec![
        json!({"id": "b:ladder", "category": "Ladder", "name": "Ladder (app default)",
               "description": "The app's own strategy: capital spread over single ticks where fees were paid, re-solved every 6 hours, depeg guard on pegged pairs.",
               "strategy": {"type": "ladder"}}),
    ];
    for (id, name, desc, s) in [
        ("tight", "Tight range", "A range two spacings wide, moved as soon as the price leaves it (30 min cooldown). Most fees per unit while in range, most moves.",
         json!({"type": "custom", "unit": "spacings", "width": 2, "delay_min": 0, "cooldown_min": 30})),
        ("balanced", "Balanced", "Six spacings wide with a one-spacing buffer, a 15 minute delay and a 2 hour cooldown.",
         json!({"type": "custom", "unit": "spacings", "width": 6, "buffer_lo": 1, "buffer_hi": 1, "delay_min": 15, "cooldown_min": 120})),
        ("patient", "Patient", "Wide (14 spacings), a big buffer and long timers: few moves, a lower share of fees.",
         json!({"type": "custom", "unit": "spacings", "width": 14, "buffer_lo": 3, "buffer_hi": 3, "delay_min": 60, "cooldown_min": 720})),
        ("follow", "Follow the trend", "Keeps the width and re-places the range from the edge the price crossed, so a trend is followed without re-centring.",
         json!({"type": "custom", "unit": "spacings", "placement": "follow", "width": 8, "offset_up": 2, "offset_down": 2, "delay_min": 10, "cooldown_min": 60})),
    ] {
        out.push(json!({"id": format!("b:{id}"), "category": "Simple rules", "name": name, "description": desc, "strategy": s}));
    }
    let ex = |id: &str, name: &str, desc: &str, nodes: Vec<Value>| json!({"id": format!("b:{id}"), "category": "Indicators", "name": name, "description": desc, "strategy": graph(name, nodes)});
    let mut v = vec![price(), n("bb", "bollinger", 240, 40, json!({"hours": 24, "k": 2}), json!({"in": ["p", "tick"]})),
                     n("r", "range", 960, 60, json!({}), json!({"lower": ["bb", "lower"], "upper": ["bb", "upper"]}))];
    v.extend(moves(15.0, 120.0, 0.0));
    out.push(ex("bollinger", "Bollinger band range", "The range is the 24-hour Bollinger band (mean ± 2 standard deviations of the price). It widens when the price gets noisy and tightens when it is calm. It moves when the price leaves the range.", v));

    let mut v = vec![price(), n("atr", "atr", 240, 160, json!({"hours": 12}), json!({})),
                     n("k", "scale", 470, 160, json!({"k": 4}), json!({"in": ["atr", "out"]})),
                     n("minw", "const", 240, 260, json!({"value": 2, "unit": "spacings"}), json!({})),
                     n("w", "max", 470, 260, json!({}), json!({"a": ["k", "out"], "b": ["minw", "value"]})),
                     n("ar", "around", 700, 60, json!({"below": 0.5}), json!({"center": ["p", "tick"], "width": ["w", "out"]})),
                     n("r", "range", 960, 60, json!({}), json!({"lower": ["ar", "lower"], "upper": ["ar", "upper"]}))];
    v.extend(moves(0.0, 60.0, 0.0));
    out.push(ex("atr", "ATR-sized range", "Centred on the price, four times the 12-hour average true range wide (at least two spacings): wide when bars are big, narrow when they are small.", v));

    let mut v = vec![n("kc", "keltner", 240, 40, json!({"hours": 24, "k": 2}), json!({})),
                     n("r", "range", 960, 60, json!({}), json!({"lower": ["kc", "lower"], "upper": ["kc", "upper"]}))];
    v.extend(moves(30.0, 240.0, 0.0));
    out.push(ex("keltner", "Keltner channel", "The range is the 24-hour Keltner channel: an exponential average of the price ± 2 average true ranges. Smoother than Bollinger bands; a 30 minute delay filters spikes.", v));

    let v = vec![n("dc", "donchian", 240, 40, json!({"hours": 48}), json!({})),
                 n("r", "range", 960, 60, json!({}), json!({"lower": ["dc", "lower"], "upper": ["dc", "upper"]})),
                 n("oor", "out_of_range", 470, 300, json!({"below": 0, "above": 0}), json!({})),
                 n("sh", "range_shift", 470, 400, json!({"value": 3, "unit": "spacings"}), json!({})),
                 n("any", "or", 700, 340, json!({}), json!({"a": ["oor", "out"], "b": ["sh", "out"]})),
                 n("move", "rebalance", 960, 340, json!({"delay_min": 0, "cooldown_min": 360}), json!({"when": ["any", "out"]}))];
    out.push(ex("donchian", "Donchian channel", "The range spans the highest high and lowest low of the last 48 hours. It follows the channel when it drifts more than three spacings, or when the price breaks out (6 hour cooldown).", v));

    let mut v = vec![price(), n("ema", "ema", 240, 40, json!({"hours": 6}), json!({"in": ["p", "tick"]})),
                     n("w", "const", 240, 160, json!({"value": 6, "unit": "spacings"}), json!({})),
                     n("ar", "around", 700, 60, json!({"below": 0.5}), json!({"center": ["ema", "out"], "width": ["w", "value"]})),
                     n("r", "range", 960, 60, json!({}), json!({"lower": ["ar", "lower"], "upper": ["ar", "upper"]}))];
    v.extend(moves(10.0, 60.0, 1.0));
    out.push(ex("ema", "Trend average", "Six spacings around the 6-hour exponential average instead of the spot price, so single spikes do not drag the range. Moves when the price is a spacing outside it.", v));

    let v = vec![price(), n("med", "median", 240, 40, json!({"hours": 168}), json!({"in": ["p", "tick"]})),
                 n("w", "const", 240, 160, json!({"value": 4, "unit": "spacings"}), json!({})),
                 n("ar", "around", 700, 60, json!({"below": 0.5}), json!({"center": ["med", "out"], "width": ["w", "value"]})),
                 n("r", "range", 960, 60, json!({}), json!({"lower": ["ar", "lower"], "upper": ["ar", "upper"]})),
                 n("oor", "out_of_range", 470, 300, json!({"below": 1, "above": 1, "unit": "spacings"}), json!({})),
                 n("move", "rebalance", 960, 300, json!({"delay_min": 30, "cooldown_min": 240}), json!({"when": ["oor", "out"]})),
                 n("far", "distance_gt", 470, 460, json!({"value": 0.5, "unit": "percent"}), json!({"a": ["p", "tick"], "b": ["med", "out"]})),
                 n("x", "exit", 960, 460, json!({"delay_min": 15}), json!({"when": ["far", "out"]}))];
    out.push(ex("peg", "Peg keeper", "For pegged pairs: a range around the 7-day median price, not the spot. Withdraws everything when the price is more than 0.5% from the median for 15 minutes, and returns once it is back.", v));

    let mut v = vec![price(), n("bb", "bollinger", 240, 40, json!({"hours": 12, "k": 2}), json!({"in": ["p", "tick"]})),
                     n("r", "range", 960, 60, json!({}), json!({"lower": ["bb", "lower"], "upper": ["bb", "upper"]})),
                     n("a1", "atr", 240, 460, json!({"hours": 1}), json!({})),
                     n("a24", "atr", 240, 560, json!({"hours": 24}), json!({})),
                     n("k", "scale", 470, 560, json!({"k": 3}), json!({"in": ["a24", "out"]})),
                     n("spike", "gt", 700, 500, json!({}), json!({"a": ["a1", "out"], "b": ["k", "out"]})),
                     n("h", "hold", 960, 500, json!({}), json!({"when": ["spike", "out"]}))];
    v.extend(moves(0.0, 60.0, 0.0));
    out.push(ex("volfreeze", "Bollinger with a volatility freeze", "A 12-hour Bollinger range that does nothing while the last hour's bars are three times bigger than the day's average: no chasing a spike.", v));

    let v = vec![price(), n("ema", "ema", 240, 40, json!({"hours": 24}), json!({"in": ["p", "tick"]})),
                 n("below", "lt", 470, 40, json!({}), json!({"a": ["p", "tick"], "b": ["ema", "out"]})),
                 n("near", "const", 240, 180, json!({"value": 2, "unit": "spacings"}), json!({})),
                 n("far", "const", 240, 280, json!({"value": 6, "unit": "spacings"}), json!({})),
                 n("l1", "sub", 470, 160, json!({}), json!({"a": ["p", "tick"], "b": ["near", "value"]})),
                 n("u1", "add", 470, 240, json!({}), json!({"a": ["p", "tick"], "b": ["far", "value"]})),
                 n("l2", "sub", 470, 320, json!({}), json!({"a": ["p", "tick"], "b": ["far", "value"]})),
                 n("u2", "add", 470, 400, json!({}), json!({"a": ["p", "tick"], "b": ["near", "value"]})),
                 n("lo", "switch", 700, 160, json!({}), json!({"cond": ["below", "out"], "a": ["l1", "out"], "b": ["l2", "out"]})),
                 n("hi", "switch", 700, 300, json!({}), json!({"cond": ["below", "out"], "a": ["u1", "out"], "b": ["u2", "out"]})),
                 n("r", "range", 960, 200, json!({}), json!({"lower": ["lo", "out"], "upper": ["hi", "out"]})),
                 n("oor", "out_of_range", 700, 460, json!({"below": 0, "above": 0}), json!({})),
                 n("move", "rebalance", 960, 460, json!({"delay_min": 15, "cooldown_min": 120}), json!({"when": ["oor", "out"]}))];
    out.push(ex("revert", "Mean-reversion tilt", "Expects the price to return to its 24-hour average: below the average the range reaches further up (2 spacings below the price, 6 above), above it further down.", v));
    let mut v = vec![price(), n("bb", "bollinger", 240, 40, json!({"hours": 12, "k": 2}), json!({"in": ["p", "tick"]})),
                     n("r", "range", 960, 60, json!({}), json!({"lower": ["bb", "lower"], "upper": ["bb", "upper"]})),
                     n("adx", "adx", 240, 300, json!({"hours": 14}), json!({})),
                     n("c30", "const", 240, 460, json!({"value": 30}), json!({})),
                     n("c20", "const", 240, 540, json!({"value": 20}), json!({})),
                     n("strong", "gt", 470, 340, json!({}), json!({"a": ["adx", "adx"], "b": ["c30", "value"]})),
                     n("calm", "lt", 470, 460, json!({}), json!({"a": ["adx", "adx"], "b": ["c20", "value"]})),
                     n("x", "exit", 960, 400, json!({"delay_min": 30}), json!({"when": ["strong", "out"], "reenter": ["calm", "out"]}))];
    v.extend(moves(0.0, 60.0, 0.0));
    out.push(ex("adx", "Calm markets only (ADX)", "A 12-hour Bollinger range that withdraws when the 14-hour ADX is above 30 for half an hour (a strong trend, where a range mostly collects losses) and returns once the ADX is below 20.", v));

    let mut v = vec![price(), n("ich", "ichimoku", 240, 40, json!({"tenkan": 9, "kijun": 26, "span_b": 52, "shift": 26}), json!({})),
                     n("th", "sub", 470, 200, json!({}), json!({"a": ["ich", "span_a"], "b": ["ich", "span_b"]})),
                     n("thk", "abs", 470, 300, json!({}), json!({"in": ["th", "out"]})),
                     n("minw", "const", 470, 400, json!({"value": 4, "unit": "spacings"}), json!({})),
                     n("w", "max", 700, 300, json!({}), json!({"a": ["thk", "out"], "b": ["minw", "value"]})),
                     n("ar", "around", 700, 60, json!({"below": 0.5}), json!({"center": ["ich", "kijun"], "width": ["w", "out"]})),
                     n("r", "range", 960, 60, json!({}), json!({"lower": ["ar", "lower"], "upper": ["ar", "upper"]}))];
    v.extend(moves(30.0, 240.0, 0.0));
    out.push(ex("ichimoku", "Ichimoku base line", "Centred on the Ichimoku base line (the midpoint of the last 26 hours' high and low), as wide as the cloud is thick (at least four spacings): a thick cloud means an uncertain market and a wider range.", v));

    let mut v = vec![price(), n("tr", "trend", 240, 300, json!({"unit_hours": 0.5}), json!({})),
                     n("c40", "const", 240, 420, json!({"value": 40}), json!({})),
                     n("cm40", "const", 240, 500, json!({"value": -40}), json!({})),
                     n("up", "gt", 470, 340, json!({}), json!({"a": ["tr", "out"], "b": ["c40", "value"]})),
                     n("dn", "lt", 470, 440, json!({}), json!({"a": ["tr", "out"], "b": ["cm40", "value"]})),
                     n("s1", "offset", 240, 40, json!({"value": -1, "unit": "spacings"}), json!({"in": ["p", "tick"]})),
                     n("s3", "offset", 240, 120, json!({"value": -3, "unit": "spacings"}), json!({"in": ["p", "tick"]})),
                     n("s5", "offset", 240, 200, json!({"value": -5, "unit": "spacings"}), json!({"in": ["p", "tick"]})),
                     n("lo1", "switch", 470, 160, json!({}), json!({"cond": ["dn", "out"], "a": ["s5", "out"], "b": ["s3", "out"]})),
                     n("lo", "switch", 700, 60, json!({}), json!({"cond": ["up", "out"], "a": ["s1", "out"], "b": ["lo1", "out"]})),
                     n("hi", "offset", 700, 200, json!({"value": 6, "unit": "spacings"}), json!({"in": ["lo", "out"]})),
                     n("r", "range", 960, 60, json!({}), json!({"lower": ["lo", "out"], "upper": ["hi", "out"]}))];
    v.extend(moves(15.0, 120.0, 0.0));
    out.push(ex("trend", "Trend lean", "Six spacings wide, leaning into the trend: when the trend score (EMAs, MACD, ADX and RSI combined) is above 40 most of the range sits above the price, below -40 most of it sits below, otherwise it is centred.", v));

    let mut v = vec![price(), n("atr", "atr", 240, 160, json!({"hours": 12}), json!({})),
                     n("k", "scale", 470, 160, json!({"k": 4}), json!({"in": ["atr", "out"]})),
                     n("minw", "const", 240, 260, json!({"value": 2, "unit": "spacings"}), json!({})),
                     n("w", "max", 470, 260, json!({}), json!({"a": ["k", "out"], "b": ["minw", "value"]})),
                     n("ar", "around", 700, 60, json!({"below": 0.5}), json!({"center": ["p", "tick"], "width": ["w", "out"]})),
                     n("r", "range", 960, 60, json!({}), json!({"lower": ["ar", "lower"], "upper": ["ar", "upper"]})),
                     n("macd", "macd", 240, 420, json!({"fast": 2, "slow": 6, "signal": 2}), json!({"in": ["p", "tick"]})),
                     n("z", "zscore", 470, 420, json!({"hours": 48}), json!({"in": ["macd", "hist"]})),
                     n("za", "abs", 470, 520, json!({}), json!({"in": ["z", "out"]})),
                     n("c2", "const", 470, 600, json!({"value": 2.5}), json!({})),
                     n("burst", "gt", 700, 520, json!({}), json!({"a": ["za", "out"], "b": ["c2", "value"]})),
                     n("h", "hold", 960, 520, json!({}), json!({"when": ["burst", "out"]}))];
    v.extend(moves(0.0, 60.0, 0.0));
    out.push(ex("macd", "Momentum pause (MACD)", "An ATR-sized range that holds still while the MACD histogram is unusually far from normal (more than 2.5 standard deviations of the last two days): it waits out sudden momentum instead of moving into it.", v));

    out
}

fn path() -> std::path::PathBuf {
    crate::paths::config().join("strategies.json")
}

pub fn saved() -> Vec<Value> {
    std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok()).unwrap_or_default()
}

fn write(list: &[Value]) -> Result<(), String> {
    let _ = std::fs::create_dir_all(crate::paths::config());
    crate::settings::write_atomic(&path(), &Value::Array(list.to_vec())).map_err(|e| e.to_string())
}

/// A strategy the lab can run: a known type that parses (spacing 1: units are checked, not applied).
/// Err: not a strategy at all. Ok(Some(why)): a node strategy that is not finished yet (an input left
/// unconnected, no range…): kept as a draft, which can be opened and edited but not applied.
fn check(s: &Value) -> Result<Option<String>, String> {
    let o = s.as_object().ok_or("the strategy must be an object")?;
    if serde_json::to_string(s).map(|x| x.len()).unwrap_or(0) > 64 * 1024 {
        return Err("strategy too large".into());
    }
    match o.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "custom" => crate::custom::Rules::from(o, 1).map(|_| None),
        "graph" => {
            if !o.get("nodes").map(|n| n.is_array()).unwrap_or(false) {
                return Err("a node strategy needs a list of nodes".into());
            }
            Ok(crate::graph::Graph::parse(o, 1).err())
        }
        "ladder" => Ok(None),
        t => Err(format!("unknown strategy type {t:?}")),
    }
}

/// Save (new, or by id) or delete a user strategy. Returns the saved item.
pub fn post(body: &Value) -> Result<Value, String> {
    let mut list = saved();
    match body["action"].as_str().unwrap_or("save") {
        "delete" => {
            let id = body["id"].as_str().unwrap_or("");
            let before = list.len();
            list.retain(|x| x["id"] != id);
            if list.len() == before {
                return Err("no such saved strategy".into());
            }
            write(&list)?;
            Ok(json!({"ok": true}))
        }
        "save" => {
            let name: String = body["name"].as_str().unwrap_or("").trim().chars().take(60).collect();
            if name.is_empty() {
                return Err("give the strategy a name".into());
            }
            let strategy = body["strategy"].clone();
            let draft = check(&strategy)?;
            let id = body["id"].as_str().filter(|s| s.starts_with("u:")).map(String::from);
            if list.iter().any(|x| x["name"].as_str().map(|s| s.eq_ignore_ascii_case(&name)).unwrap_or(false) && x["id"].as_str() != id.as_deref()) {
                return Err(format!("you already have a strategy named {name:?}"));
            }
            let now = crate::engine::now();
            let desc: String = body["description"].as_str().unwrap_or("").chars().take(400).collect();
            let mut strategy = strategy;
            if strategy["type"] == "graph" {
                strategy["name"] = json!(name);
            }
            let item = match id.and_then(|id| list.iter_mut().find(|x| x["id"] == id.as_str())) {
                Some(x) => {
                    x["name"] = json!(name);
                    x["description"] = json!(desc);
                    x["strategy"] = strategy;
                    x["updated"] = json!(now);
                    x["draft"] = json!(draft);
                    x.clone()
                }
                None => {
                    if list.len() >= MAX_SAVED {
                        return Err(format!("at most {MAX_SAVED} saved strategies"));
                    }
                    let x = json!({"id": format!("u:{now}{:04}", list.len() % 10_000), "category": "Mine", "name": name, "description": desc,
                                   "strategy": strategy, "created": now, "updated": now, "draft": draft});
                    list.push(x.clone());
                    x
                }
            };
            write(&list)?;
            Ok(item)
        }
        a => Err(format!("unknown action {a}")),
    }
}

pub fn get() -> Value {
    json!({"builtin": builtin(), "mine": saved(), "catalog": crate::graph::catalog()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_parses_on_any_spacing() {
        for b in builtin() {
            let s = b["strategy"].as_object().unwrap();
            for spacing in [1, 10, 60, 200] {
                match s["type"].as_str().unwrap() {
                    "graph" => {
                        crate::graph::Graph::parse(s, spacing).unwrap_or_else(|e| panic!("{}: {e}", b["id"]));
                    }
                    "custom" => {
                        crate::custom::Rules::from(s, spacing).unwrap_or_else(|e| panic!("{}: {e}", b["id"]));
                    }
                    _ => {}
                }
            }
        }
    }
}
