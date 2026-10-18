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
    let ex = |id: &str, name: &str, desc: &str, nodes: Vec<Value>| json!({"id": format!("b:{id}"), "category": "Indicators", "name": name, "description": desc, "strategy": graph(name, nodes)});
    // the ladder as blocks: the same solve, with exits, holds, triggers and the optimizer around it
    let v = vec![n("lad", "ladder", 960, 60, json!({"hours": 24, "span": 3, "every_hours": 6, "turnover": 0.4, "unit": "spacings"}), json!({}))];
    let mut s = ex("ladder_nodes", "Ladder (blocks)", "The app's ladder as a block: capital spread over single ticks within 3 spacings of the 24-hour median price, weighted by where fees were paid and how much liquidity others hold there, re-solved every 6 hours and moved when at least 40% of the value would move. Add triggers, holds or exits around it, or let the optimizer tune it.", v);
    s["category"] = json!("Ladder");
    out.push(s);
    let mut v = vec![price(), n("bb", "bollinger", 240, 40, json!({"hours": 24, "k": 2}), json!({"in": ["p", "tick"]})),
                     n("r", "range", 960, 60, json!({}), json!({"lower": ["bb", "lower"], "upper": ["bb", "upper"]}))];
    v.extend(moves(15.0, 120.0, 0.0));
    out.push(ex("bollinger", "Bollinger band range", "The range is the 24-hour Bollinger band (mean ± 2 standard deviations of the price). It widens when the price gets noisy and tightens when it is calm. It moves when the price leaves the range.", v));

    // the "wait out the move" strategies share their band, breakout and fee gate
    let waitout = |hot: [&str; 2]| -> Vec<Value> {
        let mut v = vec![price(), n("vol", "volatility", 240, 160, json!({"hours": 168}), json!({})),
                     n("k", "scale", 470, 160, json!({"k": 10}), json!({"in": ["vol", "out"]})),
                     n("minw", "const", 240, 260, json!({"value": 4, "unit": "spacings"}), json!({})),
                     n("w", "max", 470, 260, json!({}), json!({"a": ["k", "out"], "b": ["minw", "value"]})),
                     n("ar", "around", 700, 60, json!({"below": 0.5}), json!({"center": ["p", "tick"], "width": ["w", "out"]})),
                     n("r", "range", 960, 60, json!({}), json!({"lower": ["ar", "lower"], "upper": ["ar", "upper"]})),
                     n("brk", "breakout", 240, 460, json!({"hours": 336, "hot_hours": 24}), json!({})),
                     n("fa", "fee_vs_arb", 240, 600, json!({"hours": 168}), json!({})),
                     n("one", "const", 240, 700, json!({"value": 1}), json!({})),
                     n("poor", "lt", 470, 640, json!({}), json!({"a": ["fa", "out"], "b": ["one", "value"]})),
                     n("out", "or", 700, 520, json!({}), json!({"a": hot, "b": ["poor", "out"]})),
                     n("zero", "const", 470, 760, json!({"value": 0}), json!({})),
                     n("side", "switch", 700, 640, json!({}), json!({"cond": hot, "a": ["brk", "side"], "b": ["zero", "value"]})),
                     n("x", "exit", 960, 560, json!({"delay_min": 0}), json!({"when": ["out", "out"], "to": ["side", "out"]}))];
        v.extend(moves(240.0, 60.0, 0.0));
        v
    };
    let v = waitout(["brk", "hot"]);
    let mut s = graph("Wait out the move", v);
    s["park"] = json!(false);
    out.push(json!({"id": "b:waitout", "category": "Directional", "name": "Wait out the move",
                    "description": "For volatile pairs. A band of ±5 daily sigmas, re-centred after 4 hours out of range. For 24 hours after each new 2-week high or low it withdraws everything and waits in the token the move favours, then farms again. It also stays out while the pool's 7-day fees do not cover its loss to arbitrage (F/LVR below 1). With parking on, the waiting token earns in a whitelisted pegged pool. Research: reports/DIRECTIONAL.md.",
                    "strategy": s}));

    // other tokens: WETH's own 7-day highs and lows, on Base, also count as a move; when WETH has no
    // reliable price (no pool with enough liquidity traded for 12 hours) the position holds still
    let mut v = waitout(["hot", "out"]);
    let weth = json!({"chain_id": 8453, "token": "0x4200000000000000000000000000000000000006", "tsym": "WETH", "label": "WETH price", "tf": 60});
    let mut eth = n("eth", "token_price", 240, 980, json!({}), json!({}));
    for (k, x) in weth.as_object().unwrap() {
        eth[k] = x.clone();
    }
    v.extend([eth,
              n("ehi", "highest", 470, 960, json!({"hours": 168}), json!({"in": ["eth", "tick"]})),
              n("elo", "lowest", 470, 1040, json!({"hours": 168}), json!({"in": ["eth", "tick"]})),
              n("ehl", "lag", 700, 960, json!({"hours": 1}), json!({"in": ["ehi", "out"]})),
              n("ell", "lag", 700, 1040, json!({"hours": 1}), json!({"in": ["elo", "out"]})),
              n("eup", "gt", 960, 960, json!({}), json!({"a": ["eth", "tick"], "b": ["ehl", "out"]})),
              n("edn", "lt", 960, 1040, json!({}), json!({"a": ["eth", "tick"], "b": ["ell", "out"]})),
              n("emv", "or", 1180, 1000, json!({}), json!({"a": ["eup", "out"], "b": ["edn", "out"]})),
              n("emh", "highest", 1180, 1100, json!({"hours": 24}), json!({"in": ["emv", "out"]})),
              n("half", "const", 960, 1140, json!({"value": 0.5}), json!({})),
              n("ehot", "gt", 1180, 1200, json!({}), json!({"a": ["emh", "out"], "b": ["half", "value"]})),
              n("hot", "or", 470, 460, json!({}), json!({"a": ["brk", "hot"], "b": ["ehot", "out"]})),
              n("still", "hold", 960, 760, json!({}), json!({"when": ["eth", "stale"]}))]);
    for x in v.iter_mut().filter(|x| ["ehi", "elo", "ehl", "ell"].contains(&x["id"].as_str().unwrap_or(""))) {
        x["tf"] = json!(60);
    }
    let mut s = graph("Wait out the move, watching WETH", v);
    s["park"] = json!(false);
    out.push(json!({"id": "b:waitout_weth", "category": "Directional", "name": "Wait out the move, watching WETH",
                    "description": "Wait out the move, plus the market's own moves: for 24 hours after WETH makes a new 7-day high or low (its price from Base pools with at least $500 of active liquidity, on hourly candles) the position waits in the winning token too, even when this pool has not broken out yet. While WETH has no reliable price the position holds still. For pools of tokens that move with ETH (AERO/WETH and the like). On Base over 90 days: AERO/WETH +5.4% against holding where the plain one lost 5.3%, WETH/USDC +7.9% (plain +5.3%); one quarter of data, so a lead to test, not a result.",
                    "strategy": s}));

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
