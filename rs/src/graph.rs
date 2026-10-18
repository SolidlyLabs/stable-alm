//! Node strategies: a range and its triggers built from connected nodes in the Strategy lab
//! (strategy type "graph"). The same evaluation runs live (worker) and in backtests.
//!
//! Nodes are sources (price, bar high/low, fees, constants), indicators (ta.rs: averages, bands,
//! channels, oscillators and trend scores), maths and conditions, and sinks:
//!   range     (lower, upper)  the range wanted at each bar (required);
//!   rebalance (when)          move to the wanted range when true (default: price left the range),
//!                             after a delay and not within the cooldown;
//!   exit      (when, reenter) withdraw everything; deposit again when `reenter` (default: not
//!                             `when`) holds;
//!   hold      (when)          no action at all while true (a freeze, like the depeg guard).
//! Every indicator at bar b uses bars up to b only, so a backtest never sees the future. Values
//! are ticks for prices (1 tick = 0.01%), plain numbers otherwise; a condition is true above 0.5,
//! and an indicator still warming up is NaN (a condition on it is false, a range on it waits).
//! Two conditions depend on the position and are evaluated at decision time: `out_of_range` and
//! `range_shift`.

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

use crate::custom::{Decision, ExitTo, State};
use crate::data::Market;
use crate::ta;

pub const BARS_PER_HOUR: f64 = 12.0;
const MAX_NODES: usize = 60;

/// (kind, inputs, outputs, number params with (default, min, max)). Units of a "ticks" param
/// follow the node's "unit": ticks, spacings or percent.
type Spec = (&'static str, &'static [&'static str], &'static [&'static str], &'static [(&'static str, f64, f64, f64)]);

/// Periods up to a week: the live replay keeps 10 days, backtests warm up for 8.
const H: (&str, f64, f64, f64) = ("hours", 24.0, 0.1, 168.0);
pub const KINDS: &[Spec] = &[
    ("price", &[], &["tick"], &[]),
    ("high_low", &[], &["high", "low"], &[]),
    ("fees", &[], &["out"], &[]),
    ("const", &[], &["value"], &[("value", 10.0, -1e6, 1e6)]),
    ("sma", &["in"], &["out"], &[H]),
    ("ema", &["in"], &["out"], &[H]),
    ("median", &["in"], &["out"], &[H]),
    ("stdev", &["in"], &["out"], &[H]),
    ("bollinger", &["in"], &["upper", "mid", "lower"], &[H, ("k", 2.0, 0.0, 20.0)]),
    ("atr", &[], &["out"], &[H]),
    ("keltner", &[], &["upper", "mid", "lower"], &[H, ("k", 2.0, 0.0, 20.0)]),
    ("donchian", &[], &["upper", "lower"], &[H]),
    ("rsi", &["in"], &["out"], &[("hours", 14.0, 0.1, 168.0)]),
    ("lag", &["in"], &["out"], &[("hours", 1.0, 0.0, 168.0)]),
    ("change", &["in"], &["out"], &[("hours", 1.0, 0.1, 168.0)]),
    ("highest", &["in"], &["out"], &[H]),
    ("lowest", &["in"], &["out"], &[H]),
    ("percentile", &["in"], &["out"], &[H, ("q", 0.9, 0.0, 1.0)]),
    ("zscore", &["in"], &["out"], &[H]),
    ("macd", &["in"], &["line", "signal", "hist"], &[("fast", 12.0, 0.1, 96.0), ("slow", 26.0, 0.1, 96.0), ("signal", 9.0, 0.1, 48.0)]),
    ("stochastic", &[], &["k", "d"], &[("hours", 14.0, 0.1, 168.0), ("d_hours", 0.25, 0.1, 24.0)]),
    ("stoch_rsi", &["in"], &["out"], &[("hours", 14.0, 0.1, 96.0), ("stoch_hours", 14.0, 0.1, 96.0)]),
    ("cci", &[], &["out"], &[("hours", 20.0, 0.1, 168.0)]),
    ("adx", &[], &["plus_di", "minus_di", "adx"], &[("hours", 14.0, 0.1, 96.0)]),
    ("trix", &["in"], &["out"], &[("hours", 15.0, 0.1, 60.0)]),
    ("psar", &[], &["out"], &[("af", 0.02, 0.001, 0.5), ("step", 0.02, 0.001, 0.5), ("max_af", 0.2, 0.01, 1.0)]),
    ("ichimoku", &[], &["tenkan", "kijun", "span_a", "span_b"], &[("tenkan", 9.0, 0.1, 96.0), ("kijun", 26.0, 0.1, 96.0), ("span_b", 52.0, 0.1, 96.0), ("shift", 26.0, 0.0, 96.0)]),
    ("heikin_ashi", &[], &["open", "high", "low", "close"], &[]),
    ("gaussian", &["in"], &["upper", "mid", "lower"], &[("hours", 12.0, 0.1, 37.0), ("k", 2.0, 0.0, 20.0)]),
    ("trend", &[], &["out"], &[("unit_hours", 1.0, 0.1, 3.5)]),
    ("momentum", &[], &["out"], &[("hours", 0.5, 0.1, 24.0), ("window", 42.0, 1.0, 120.0), ("smooth", 1.2, 0.1, 24.0)]),
    ("add", &["a", "b"], &["out"], &[]),
    ("sub", &["a", "b"], &["out"], &[]),
    ("mul", &["a", "b"], &["out"], &[]),
    ("scale", &["in"], &["out"], &[("k", 2.0, -1e6, 1e6)]),
    ("offset", &["in"], &["out"], &[("value", 10.0, -1e6, 1e6)]),
    ("min", &["a", "b"], &["out"], &[]),
    ("max", &["a", "b"], &["out"], &[]),
    ("abs", &["in"], &["out"], &[]),
    ("around", &["center", "width"], &["lower", "upper"], &[("below", 0.5, 0.0, 1.0)]),
    ("switch", &["cond", "a", "b"], &["out"], &[]),
    ("gt", &["a", "b"], &["out"], &[]),
    ("lt", &["a", "b"], &["out"], &[]),
    ("cross_above", &["a", "b"], &["out"], &[]),
    ("cross_below", &["a", "b"], &["out"], &[]),
    ("between", &["in", "lower", "upper"], &["out"], &[]),
    ("distance_gt", &["a", "b"], &["out"], &[("value", 20.0, 0.0, 1e6)]),
    ("and", &["a", "b"], &["out"], &[]),
    ("or", &["a", "b"], &["out"], &[]),
    ("not", &["in"], &["out"], &[]),
    ("out_of_range", &[], &["out"], &[("below", 0.0, -1e6, 1e6), ("above", 0.0, -1e6, 1e6)]),
    ("range_shift", &[], &["out"], &[("value", 10.0, 0.0, 1e6)]),
    ("range", &["lower", "upper"], &[], &[]),
    ("rebalance", &["when"], &[], &[("delay_min", 0.0, 0.0, 10080.0), ("cooldown_min", 60.0, 0.0, 43200.0)]),
    ("exit", &["when", "reenter"], &[], &[("delay_min", 0.0, 0.0, 10080.0)]),
    ("hold", &["when"], &[], &[]),
];
/// Inputs that may stay unconnected (the sink's default applies).
const OPTIONAL: &[(&str, &str)] = &[("rebalance", "when"), ("exit", "reenter")];
/// Params measured in ticks (scaled by the node's unit).
const TICK_PARAMS: &[(&str, &str)] =
    &[("const", "value"), ("offset", "value"), ("distance_gt", "value"), ("out_of_range", "below"), ("out_of_range", "above"), ("range_shift", "value")];
/// Outputs that are prices (ticks): drawn on the chart.
const PRICE_LIKE: &[&str] = &["price", "high_low", "sma", "ema", "median", "bollinger", "keltner", "donchian", "around", "offset", "add", "sub", "min", "max", "switch",
    "lag", "highest", "lowest", "percentile", "psar", "ichimoku", "heikin_ashi", "gaussian"];
/// Kinds a decision can compute at one bar from other values: the only ones a node fed by a
/// position condition may be.
const POINTWISE: &[&str] = &["and", "or", "not", "switch", "gt", "lt", "add", "sub", "mul", "scale", "offset", "min", "max", "abs", "between", "distance_gt",
    "range", "rebalance", "exit", "hold"];
/// History loaded before the first decision (backtests warm up 8 days, the live replay keeps 10).
pub const MAX_LOOKBACK_HOURS: f64 = 190.0;
const STATEFUL: &[&str] = &["out_of_range", "range_shift"];

fn spec(kind: &str) -> Option<&'static Spec> {
    KINDS.iter().find(|s| s.0 == kind)
}

#[derive(Clone, Debug)]
pub struct Node {
    pub id: String,
    pub kind: &'static str,
    pub p: HashMap<String, f64>,
    pub inputs: HashMap<String, (usize, String)>,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Graph {
    /// in evaluation order (every input before its node)
    pub nodes: Vec<Node>,
    pub exit_to: ExitTo,
    pub name: String,
    stateful: HashSet<usize>,
}

fn unit_ticks(unit: &str, v: f64, spacing: i64) -> f64 {
    match unit {
        "spacings" => v * spacing.max(1) as f64,
        "percent" => (1.0 + v / 100.0).max(1e-9).ln() / 1.0001f64.ln(),
        _ => v,
    }
}

impl Graph {
    pub fn parse(s: &Map<String, Value>, spacing: i64) -> Result<Graph, String> {
        let raw = s.get("nodes").and_then(|v| v.as_array()).ok_or("node strategy: no nodes")?;
        if raw.is_empty() || raw.len() > MAX_NODES {
            return Err(format!("node strategy: 1 to {MAX_NODES} nodes"));
        }
        let mut ids: HashMap<String, usize> = HashMap::new();
        for (i, n) in raw.iter().enumerate() {
            let id = n["id"].as_str().unwrap_or("").to_string();
            if id.is_empty() || id.len() > 24 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                return Err(format!("node strategy: bad node id {id:?}"));
            }
            if ids.insert(id.clone(), i).is_some() {
                return Err(format!("node strategy: duplicate node id {id}"));
            }
        }
        let mut nodes = vec![];
        for n in raw {
            let id = n["id"].as_str().unwrap().to_string();
            let kind = n["kind"].as_str().unwrap_or("");
            let sp = spec(kind).ok_or(format!("node {id}: unknown kind {kind:?}"))?;
            let unit = n["unit"].as_str().unwrap_or("ticks");
            if !["ticks", "spacings", "percent"].contains(&unit) {
                return Err(format!("node {id}: unit must be ticks, spacings or percent"));
            }
            let mut p = HashMap::new();
            for (k, d, lo, hi) in sp.3 {
                let v = n["p"][*k].as_f64().unwrap_or(*d);
                if !v.is_finite() || v < *lo || v > *hi {
                    return Err(format!("node {id}: {k} must be {lo} to {hi}"));
                }
                let v = if TICK_PARAMS.contains(&(sp.0, *k)) { unit_ticks(unit, v, spacing) } else { v };
                p.insert(k.to_string(), v);
            }
            let mut inputs = HashMap::new();
            for port in sp.1 {
                match n["in"][*port].as_array() {
                    Some(a) if a.len() == 2 => {
                        let (src, out) = (a[0].as_str().unwrap_or(""), a[1].as_str().unwrap_or(""));
                        let si = *ids.get(src).ok_or(format!("node {id}: input {port} comes from unknown node {src}"))?;
                        let ssp = spec(raw[si]["kind"].as_str().unwrap_or("")).unwrap();
                        if !ssp.2.contains(&out) {
                            return Err(format!("node {id}: node {src} has no output {out}"));
                        }
                        inputs.insert(port.to_string(), (si, out.to_string()));
                    }
                    _ if OPTIONAL.contains(&(sp.0, *port)) => {}
                    _ => return Err(format!("node {id} ({kind}): connect its {port} input")),
                }
            }
            let label = n["label"].as_str().unwrap_or("").chars().take(40).collect::<String>();
            nodes.push(Node { id, kind: sp.0, p, inputs, label });
        }
        // order: inputs first; a cycle is refused
        let mut order = vec![];
        let mut state = vec![0u8; nodes.len()];
        fn visit(i: usize, nodes: &[Node], state: &mut [u8], order: &mut Vec<usize>) -> Result<(), String> {
            match state[i] {
                2 => return Ok(()),
                1 => return Err(format!("node strategy: a loop through node {}", nodes[i].id)),
                _ => {}
            }
            state[i] = 1;
            for (src, _) in nodes[i].inputs.values() {
                visit(*src, nodes, state, order)?;
            }
            state[i] = 2;
            order.push(i);
            Ok(())
        }
        for i in 0..nodes.len() {
            visit(i, &nodes, &mut state, &mut order)?;
        }
        let remap: HashMap<usize, usize> = order.iter().enumerate().map(|(new, old)| (*old, new)).collect();
        let mut sorted: Vec<Node> = order.iter().map(|i| nodes[*i].clone()).collect();
        for n in &mut sorted {
            for v in n.inputs.values_mut() {
                v.0 = remap[&v.0];
            }
        }
        let count = |k: &str| sorted.iter().filter(|n| n.kind == k).count();
        if count("range") != 1 {
            return Err("node strategy: needs exactly one Range node".into());
        }
        for k in ["rebalance", "exit", "hold"] {
            if count(k) > 1 {
                return Err(format!("node strategy: at most one {k} node"));
            }
        }
        let mut stateful = HashSet::new();
        for (i, n) in sorted.iter().enumerate() {
            if STATEFUL.contains(&n.kind) || n.inputs.values().any(|(s, _)| stateful.contains(s)) {
                stateful.insert(i);
            }
        }
        for (i, n) in sorted.iter().enumerate() {
            if stateful.contains(&i) && !STATEFUL.contains(&n.kind) && !POINTWISE.contains(&n.kind) {
                return Err(format!("node {}: an indicator cannot take a position condition (out of range / range shift) as input", n.id));
            }
        }
        let mut look = vec![0.0f64; sorted.len()];
        for i in 0..sorted.len() {
            look[i] = own_lookback(&sorted[i]) + sorted[i].inputs.values().map(|(s, _)| look[*s]).fold(0.0, f64::max);
            if look[i] > MAX_LOOKBACK_HOURS {
                return Err(format!(
                    "node {}: needs {:.0} hours of history before its first value (chained indicators add up); at most {MAX_LOOKBACK_HOURS:.0}",
                    sorted[i].id, look[i]
                ));
            }
        }
        for (i, n) in sorted.iter().enumerate() {
            if n.kind == "range" && stateful.contains(&i) {
                return Err("node strategy: the range cannot depend on the position (out of range / range shift)".into());
            }
        }
        let exit_to = match s.get("exit_to").and_then(|v| v.as_str()).unwrap_or("keep") {
            "token0" => ExitTo::Token0,
            "token1" => ExitTo::Token1,
            _ => ExitTo::Keep,
        };
        let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("").chars().take(60).collect();
        Ok(Graph { nodes: sorted, exit_to, name, stateful })
    }

    pub fn label(&self) -> String {
        if self.name.is_empty() { format!("nodes({})", self.nodes.len()) } else { format!("nodes: {}", self.name) }
    }

    fn sink(&self, kind: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.kind == kind)
    }

    fn param(&self, i: usize, k: &str) -> f64 {
        self.nodes[i].p.get(k).copied().unwrap_or(0.0)
    }
}

// -- series --------------------------------------------------------------------------------------------

fn bars(hours: f64) -> usize {
    ((hours * BARS_PER_HOUR).round() as usize).max(1)
}

fn median(x: &[f64], n: usize) -> Vec<f64> {
    // the store's trailing median on integers (milli-units keep three decimals)
    if x.iter().any(|v| !v.is_finite()) {
        let mut out = vec![f64::NAN; x.len()];
        for i in 0..x.len() {
            if i + 1 >= n {
                let mut w: Vec<f64> = x[i + 1 - n..=i].iter().copied().filter(|v| v.is_finite()).collect();
                if w.len() == n {
                    out[i] = crate::data::median(&mut w);
                }
            }
        }
        return out;
    }
    let xi: Vec<i64> = x.iter().map(|v| (v * 1000.0).round() as i64).collect();
    crate::data::rolling_median_trailing(&xi, n, n).into_iter().map(|v| v / 1000.0).collect()
}

/// Hours of history a node needs before its first value (on top of its inputs').
fn own_lookback(n: &Node) -> f64 {
    let p = |k: &str| n.p.get(k).copied().unwrap_or(0.0);
    let bar = 1.0 / BARS_PER_HOUR;
    match n.kind {
        "sma" | "ema" | "median" | "stdev" | "bollinger" | "keltner" | "highest" | "lowest" | "percentile" | "zscore" | "cci" | "lag" | "change" => p("hours") + bar,
        "atr" | "rsi" | "donchian" | "stochastic" => p("hours") + p("d_hours") + bar,
        "adx" => 2.0 * p("hours") + bar,
        "trix" => 3.0 * p("hours") + bar,
        "macd" => p("fast").max(p("slow")) + p("signal"),
        "stoch_rsi" => p("hours") + p("stoch_hours") + bar,
        "ichimoku" => p("tenkan").max(p("kijun")).max(p("span_b")) + p("shift"),
        "gaussian" => 5.0 * p("hours"),
        "trend" => 50.0 * p("unit_hours"),
        "momentum" => p("hours") + p("window") + p("smooth") + bar,
        _ => 0.0,
    }
}

fn truth(v: f64) -> bool {
    v.is_finite() && v > 0.5
}

fn b(v: bool) -> f64 {
    if v { 1.0 } else { 0.0 }
}

/// Every stateless output over all bars of `m`.
pub struct Eval {
    pub series: HashMap<(usize, String), Vec<f64>>,
}

pub fn evaluate(g: &Graph, m: &Market) -> Eval {
    let n = m.bar_tick.len();
    let close: Vec<f64> = m.bar_tick.iter().map(|t| *t as f64).collect();
    let hi: Vec<f64> = m.bar_hi.iter().map(|t| *t as f64).collect();
    let lo: Vec<f64> = m.bar_lo.iter().map(|t| *t as f64).collect();
    let mut s: HashMap<(usize, String), Vec<f64>> = HashMap::new();
    for (i, node) in g.nodes.iter().enumerate() {
        if g.stateful.contains(&i) || node.inputs.is_empty() && STATEFUL.contains(&node.kind) {
            continue;
        }
        let inp = |port: &str| -> Vec<f64> { node.inputs.get(port).and_then(|(src, out)| s.get(&(*src, out.clone())).cloned()).unwrap_or_else(|| vec![f64::NAN; n]) };
        let hours = node.p.get("hours").copied().unwrap_or(24.0);
        let k = node.p.get("k").copied().unwrap_or(0.0);
        let val = node.p.get("value").copied().unwrap_or(0.0);
        let par = |k: &str| node.p.get(k).copied().unwrap_or(0.0);
        let zip = |a: Vec<f64>, c: Vec<f64>, f: &dyn Fn(f64, f64) -> f64| -> Vec<f64> { a.iter().zip(&c).map(|(x, y)| f(*x, *y)).collect() };
        let mut outs: Vec<(String, Vec<f64>)> = vec![];
        let mut put = |port: &str, v: Vec<f64>| outs.push((port.to_string(), v));
        match node.kind {
            "price" => put("tick", close.clone()),
            "high_low" => {
                put("high", hi.clone());
                put("low", lo.clone());
            }
            "fees" => put("out", m.bar_fee.clone()),
            "const" => put("value", vec![val; n]),
            "sma" => put("out", ta::sma(&inp("in"), bars(hours))),
            "ema" => put("out", ta::ema(&inp("in"), bars(hours))),
            "median" => put("out", median(&inp("in"), bars(hours))),
            "stdev" => put("out", ta::stdev(&inp("in"), bars(hours))),
            "bollinger" => {
                let x = inp("in");
                let (mid, sd) = (ta::sma(&x, bars(hours)), ta::stdev(&x, bars(hours)));
                put("upper", mid.iter().zip(&sd).map(|(a, d)| a + k * d).collect());
                put("lower", mid.iter().zip(&sd).map(|(a, d)| a - k * d).collect());
                put("mid", mid);
            }
            "atr" => put("out", ta::atr(&close, &hi, &lo, bars(hours))),
            "keltner" => {
                let (mid, a) = (ta::ema(&close, bars(hours)), ta::atr(&close, &hi, &lo, bars(hours)));
                put("upper", mid.iter().zip(&a).map(|(x, d)| x + k * d).collect());
                put("lower", mid.iter().zip(&a).map(|(x, d)| x - k * d).collect());
                put("mid", mid);
            }
            "donchian" => {
                // the bars before this one, so a close above the channel is a breakout
                put("upper", ta::lag(&ta::extreme(&hi, bars(hours), true), 1));
                put("lower", ta::lag(&ta::extreme(&lo, bars(hours), false), 1));
            }
            "rsi" => put("out", ta::rsi(&inp("in"), bars(hours))),
            "lag" => put("out", ta::lag(&inp("in"), (hours * BARS_PER_HOUR).round() as usize)),
            "change" => {
                let x = inp("in");
                let l = ta::lag(&x, bars(hours));
                put("out", zip(x, l, &|a, c| a - c));
            }
            "highest" => put("out", ta::extreme(&inp("in"), bars(hours), true)),
            "lowest" => put("out", ta::extreme(&inp("in"), bars(hours), false)),
            "percentile" => put("out", ta::quantile(&inp("in"), bars(hours), par("q"))),
            "zscore" => put("out", ta::zscore(&inp("in"), bars(hours))),
            "macd" => {
                let (l, sg, h) = ta::macd(&inp("in"), bars(par("fast")), bars(par("slow")), bars(par("signal")));
                put("line", l);
                put("signal", sg);
                put("hist", h);
            }
            "stochastic" => {
                let kk = ta::stoch_of(&close, &hi, &lo, bars(hours));
                put("d", ta::sma(&kk, bars(par("d_hours"))));
                put("k", kk);
            }
            "stoch_rsi" => {
                let r = ta::rsi(&inp("in"), bars(hours));
                put("out", ta::stoch_of(&r, &r, &r, bars(par("stoch_hours"))));
            }
            "cci" => put("out", ta::cci(&close, &hi, &lo, bars(hours))),
            "adx" => {
                let (pd, md, a) = ta::adx(&close, &hi, &lo, bars(hours));
                put("plus_di", pd);
                put("minus_di", md);
                put("adx", a);
            }
            "trix" => put("out", ta::trix(&inp("in"), bars(hours))),
            "psar" => put("out", ta::psar(&close, &hi, &lo, par("af"), par("step"), par("max_af"))),
            "ichimoku" => {
                let shift = (par("shift") * BARS_PER_HOUR).round() as usize;
                let (t, kj, sa, sb) = ta::ichimoku(&hi, &lo, bars(par("tenkan")), bars(par("kijun")), bars(par("span_b")), shift);
                put("tenkan", t);
                put("kijun", kj);
                put("span_a", sa);
                put("span_b", sb);
            }
            "heikin_ashi" => {
                let (o, h, l, c) = ta::heikin_ashi(&close, &hi, &lo);
                put("open", o);
                put("high", h);
                put("low", l);
                put("close", c);
            }
            "gaussian" => {
                let (u, c, l) = ta::gaussian(&inp("in"), bars(hours), k);
                put("upper", u);
                put("mid", c);
                put("lower", l);
            }
            "trend" => put("out", ta::trend(&close, &hi, &lo, bars(par("unit_hours")))),
            "momentum" => put("out", ta::momentum(&close, &hi, &lo, bars(hours), bars(par("window")), bars(par("smooth")))),
            "add" => put("out", zip(inp("a"), inp("b"), &|x, y| x + y)),
            "sub" => put("out", zip(inp("a"), inp("b"), &|x, y| x - y)),
            "mul" => put("out", zip(inp("a"), inp("b"), &|x, y| x * y)),
            "scale" => put("out", inp("in").iter().map(|x| x * k).collect()),
            "offset" => put("out", inp("in").iter().map(|x| x + val).collect()),
            "min" => put("out", zip(inp("a"), inp("b"), &|x, y| if x.is_nan() || y.is_nan() { f64::NAN } else { x.min(y) })),
            "max" => put("out", zip(inp("a"), inp("b"), &|x, y| if x.is_nan() || y.is_nan() { f64::NAN } else { x.max(y) })),
            "abs" => put("out", inp("in").iter().map(|x| x.abs()).collect()),
            "around" => {
                let below = node.p.get("below").copied().unwrap_or(0.5);
                let (c, w) = (inp("center"), inp("width"));
                put("lower", c.iter().zip(&w).map(|(c, w)| c - w.abs() * below).collect());
                put("upper", c.iter().zip(&w).map(|(c, w)| c + w.abs() * (1.0 - below)).collect());
            }
            "switch" => {
                let (c, a, bb) = (inp("cond"), inp("a"), inp("b"));
                put("out", (0..n).map(|j| if truth(c[j]) { a[j] } else { bb[j] }).collect());
            }
            "gt" => put("out", zip(inp("a"), inp("b"), &|x, y| b(x > y))),
            "lt" => put("out", zip(inp("a"), inp("b"), &|x, y| b(x < y))),
            "cross_above" | "cross_below" => {
                let (a, c) = (inp("a"), inp("b"));
                let up = node.kind == "cross_above";
                put("out", (0..n).map(|j| b(j > 0 && if up { a[j] > c[j] && a[j - 1] <= c[j - 1] } else { a[j] < c[j] && a[j - 1] >= c[j - 1] })).collect());
            }
            "between" => {
                let (x, l, u) = (inp("in"), inp("lower"), inp("upper"));
                put("out", (0..n).map(|j| b(x[j] >= l[j] && x[j] < u[j])).collect());
            }
            "distance_gt" => put("out", zip(inp("a"), inp("b"), &|x, y| b((x - y).abs() > val))),
            "and" => put("out", zip(inp("a"), inp("b"), &|x, y| b(truth(x) && truth(y)))),
            "or" => put("out", zip(inp("a"), inp("b"), &|x, y| b(truth(x) || truth(y)))),
            "not" => put("out", inp("in").iter().map(|x| b(!truth(*x))).collect()),
            _ => {}
        }
        for (port, v) in outs {
            s.insert((i, port), v);
        }
    }
    Eval { series: s }
}

/// What a decision at one bar needs about the position.
pub struct At {
    pub bar: usize,
    pub tick: i64,
    pub range: Option<(i64, i64)>,
    pub wanted: Option<(i64, i64)>,
}

/// The value of node `i`'s output `port` at the bar (stateful nodes computed from the position).
fn value(g: &Graph, ev: &Eval, i: usize, port: &str, at: &At) -> f64 {
    if !g.stateful.contains(&i) {
        return ev.series.get(&(i, port.to_string())).map(|v| v[at.bar]).unwrap_or(f64::NAN);
    }
    let node = &g.nodes[i];
    let inp = |p: &str| node.inputs.get(p).map(|(s, o)| value(g, ev, *s, o, at)).unwrap_or(f64::NAN);
    let val = node.p.get("value").copied().unwrap_or(0.0);
    match node.kind {
        "out_of_range" => match at.range {
            Some((tl, tu)) => {
                let (lo, hi) = (tl as f64 - node.p["below"], tu as f64 + node.p["above"]);
                b(!((at.tick as f64) >= lo && (at.tick as f64) < hi))
            }
            None => 1.0,
        },
        "range_shift" => match (at.range, at.wanted) {
            (Some((a, c)), Some((x, y))) => b((((a + c) - (x + y)) as f64 / 2.0).abs() > val),
            _ => 1.0,
        },
        "and" => b(truth(inp("a")) && truth(inp("b"))),
        "or" => b(truth(inp("a")) || truth(inp("b"))),
        "not" => b(!truth(inp("in"))),
        "switch" => if truth(inp("cond")) { inp("a") } else { inp("b") },
        "gt" => b(inp("a") > inp("b")),
        "lt" => b(inp("a") < inp("b")),
        "add" => inp("a") + inp("b"),
        "sub" => inp("a") - inp("b"),
        "mul" => inp("a") * inp("b"),
        "scale" => inp("in") * node.p.get("k").copied().unwrap_or(0.0),
        "offset" => inp("in") + val,
        "min" => inp("a").min(inp("b")),
        "max" => inp("a").max(inp("b")),
        "abs" => inp("in").abs(),
        "between" => {
            let x = inp("in");
            b(x >= inp("lower") && x < inp("upper"))
        }
        "distance_gt" => b((inp("a") - inp("b")).abs() > val),
        _ => f64::NAN,
    }
}

fn input(g: &Graph, ev: &Eval, sink: usize, port: &str, at: &At) -> Option<f64> {
    g.nodes[sink].inputs.get(port).map(|(s, o)| value(g, ev, *s, o, at))
}

/// The range the graph wants at `bar`, snapped to the spacing (None while indicators warm up).
pub fn wanted(g: &Graph, ev: &Eval, bar: usize, spacing: i64) -> Option<(i64, i64)> {
    let r = g.sink("range")?;
    let at = At { bar, tick: 0, range: None, wanted: None };
    let (lo, hi) = (input(g, ev, r, "lower", &at)?, input(g, ev, r, "upper", &at)?);
    if !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    let (lo, hi) = (lo.min(hi), lo.max(hi));
    let sp = spacing.max(1);
    let tl = ((lo / sp as f64).floor() as i64) * sp;
    let mut tu = ((hi / sp as f64).ceil() as i64) * sp;
    if tu - tl < sp {
        tu = tl + sp;
    }
    Some((tl.clamp(-887_200, 887_200 - sp), tu.clamp(-887_200 + sp, 887_200)))
}

fn mins(s: i64) -> String {
    if s >= 5400 { format!("{:.1} h", s as f64 / 3600.0) } else { format!("{} min", (s + 59) / 60) }
}

/// The graph's decision at `bar` (time `now`, price `tick`, current range).
pub fn decide(g: &Graph, ev: &Eval, st: &mut State, now: i64, bar: usize, tick: i64, range: Option<(i64, i64)>, spacing: i64) -> Decision {
    let want = wanted(g, ev, bar, spacing);
    let at = At { bar, tick, range, wanted: want };
    if let Some(h) = g.sink("hold") {
        if input(g, ev, h, "when", &at).map(truth).unwrap_or(false) {
            return Decision::Wait("hold condition is true: no action".into());
        }
    }
    if let Some(x) = g.sink("exit") {
        let when = input(g, ev, x, "when", &at).map(truth).unwrap_or(false);
        if when && !st.exited {
            let since = *st.exit_since.get_or_insert(now);
            let delay = (g.param(x, "delay_min") * 60.0) as i64;
            if now - since < delay {
                return Decision::Wait(format!("exit condition true; exiting in {}", mins(delay - (now - since))));
            }
            return Decision::Exit(format!("exit condition true (tick {tick})"));
        }
        if !when {
            st.exit_since = None;
        }
        if st.exited {
            let back = input(g, ev, x, "reenter", &at).map(truth).unwrap_or(!when);
            if !back {
                return Decision::Keep("exited; waiting for the re-entry condition".into());
            }
            return match want {
                Some((tl, tu)) => Decision::Move { tl, tu, reason: "re-entry condition true: depositing again".into() },
                None => Decision::Wait("re-entry: indicators still warming up".into()),
            };
        }
    } else if st.exited {
        st.exited = false;
    }
    let Some((wl, wu)) = want else {
        return Decision::Wait("indicators still warming up".into());
    };
    let Some((tl, tu)) = range else {
        return Decision::Move { tl: wl, tu: wu, reason: "no position yet: opening the wanted range".into() };
    };
    let reb = g.sink("rebalance");
    let trigger = match reb.and_then(|r| input(g, ev, r, "when", &at)) {
        Some(v) => truth(v),
        None => !(tl <= tick && tick < tu),
    };
    if !trigger {
        st.outside_since = None;
        return Decision::Keep(if tl <= tick && tick < tu { "in range".into() } else { "out of range; the rebalance condition is false".into() });
    }
    let since = *st.outside_since.get_or_insert(now);
    let (delay, cool) = reb.map(|r| ((g.param(r, "delay_min") * 60.0) as i64, (g.param(r, "cooldown_min") * 60.0) as i64)).unwrap_or((0, 3600));
    if now - since < delay {
        return Decision::Wait(format!("rebalance condition true; moving in {}", mins(delay - (now - since))));
    }
    if st.last_move > 0 && now - st.last_move < cool {
        return Decision::Wait(format!("cooldown: next move allowed in {}", mins(cool - (now - st.last_move))));
    }
    if (wl, wu) == (tl, tu) {
        return Decision::Keep("the wanted range equals the current one".into());
    }
    Decision::Move { tl: wl, tu: wu, reason: format!("rebalance condition true: moving to [{wl},{wu})") }
}

/// Chart lines: every price-like output, sampled at `bars`, plus the wanted range.
pub fn overlays(g: &Graph, ev: &Eval, idx: &[usize], spacing: i64) -> Value {
    let mut out = vec![];
    for (i, n) in g.nodes.iter().enumerate() {
        if g.stateful.contains(&i) || !PRICE_LIKE.contains(&n.kind) || n.kind == "price" {
            continue;
        }
        for port in spec(n.kind).map(|s| s.2).unwrap_or(&[]) {
            if let Some(v) = ev.series.get(&(i, port.to_string())) {
                let name = format!("{}{}", if n.label.is_empty() { format!("{} {}", n.kind, n.id) } else { n.label.clone() }, if spec(n.kind).unwrap().2.len() > 1 { format!(" {port}") } else { String::new() });
                out.push(json!({"name": name, "node": n.id, "port": port, "v": idx.iter().map(|b| v.get(*b).copied().filter(|x| x.is_finite()).map(|x| (x * 10.0).round() / 10.0)).collect::<Vec<_>>()}));
            }
        }
    }
    let want: Vec<Value> = idx.iter().map(|b| wanted(g, ev, *b, spacing).map(|(a, c)| json!([a, c])).unwrap_or(Value::Null)).collect();
    json!({"lines": out, "wanted": want})
}

/// The node kinds for the editor: ports and params with defaults and bounds.
pub fn catalog() -> Value {
    Value::Array(
        KINDS
            .iter()
            .map(|(k, i, o, p)| {
                json!({"kind": k, "inputs": i, "outputs": o, "optional": OPTIONAL.iter().filter(|x| x.0 == *k).map(|x| x.1).collect::<Vec<_>>(),
                       "params": p.iter().map(|(n, d, lo, hi)| json!({"name": n, "default": d, "min": lo, "max": hi, "ticks": TICK_PARAMS.contains(&(*k, *n))})).collect::<Vec<_>>(),
                       "price": PRICE_LIKE.contains(k), "stateful": STATEFUL.contains(k)})
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market(ticks: &[i64]) -> Market {
        let n = ticks.len();
        let ts: Vec<i64> = (0..n as i64).map(|k| 1_000_000 + k * 300).collect();
        let bar_end: Vec<i64> = ts.iter().map(|t| t + 1).collect();
        let bar_idx: Vec<usize> = (0..n).collect();
        let fee = vec![1.0; n];
        let (bar_hi, bar_lo, bar_fee) = crate::data::bar_ranges(ticks, &fee, &bar_idx);
        Market { ts, tick: ticks.to_vec(), sp: vec![1.0; n], fee, l_others: vec![1.0; n], l_active: vec![1.0; n], bar_end, bar_idx, bar_tick: ticks.to_vec(), bar_hi, bar_lo, bar_fee,
                 decimals: 6, px_scale: 1.0, liq_scale: 1.0 }
    }

    fn graph(v: Value) -> Result<Graph, String> {
        Graph::parse(v.as_object().unwrap(), 1)
    }

    fn bollinger() -> Value {
        json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"},
            {"id": "bb", "kind": "bollinger", "p": {"hours": 1, "k": 2}, "in": {"in": ["p", "tick"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["bb", "lower"], "upper": ["bb", "upper"]}},
            {"id": "m", "kind": "rebalance", "p": {"delay_min": 0, "cooldown_min": 0}}
        ]})
    }

    #[test]
    fn indicators_only_look_back() {
        // every indicator at bar i is the same whether or not later bars exist
        let c: Vec<f64> = (0..900).map(|i| ((i * i % 17) as f64) + (i as f64 / 9.0).sin() * 30.0).collect();
        let h: Vec<f64> = c.iter().enumerate().map(|(i, v)| v + (i % 3) as f64).collect();
        let l: Vec<f64> = c.iter().enumerate().map(|(i, v)| v - (i % 4) as f64).collect();
        let all = |c: &[f64], h: &[f64], l: &[f64]| -> Vec<Vec<f64>> {
            let (m1, m2, m3) = ta::macd(c, 6, 13, 4);
            let (a1, a2, a3) = ta::adx(c, h, l, 14);
            let (i1, i2, i3, i4) = ta::ichimoku(h, l, 9, 26, 52, 26);
            let (k1, k2, k3, k4) = ta::heikin_ashi(c, h, l);
            let (g1, g2, g3) = ta::gaussian(c, 10, 2.0);
            vec![ta::sma(c, 5), ta::ema(c, 5), ta::stdev(c, 5), median(c, 5), ta::wilder(c, 7), ta::extreme(h, 5, true), ta::quantile(c, 9, 0.8),
                 ta::atr(c, h, l, 14), ta::rsi(c, 14), ta::stoch_of(c, h, l, 14), ta::cci(c, h, l, 20), ta::trix(c, 5), ta::psar(c, h, l, 0.02, 0.02, 0.2),
                 ta::zscore(c, 20), ta::trend(c, h, l, 1), ta::momentum(c, h, l, 5, 100, 14), m1, m2, m3, a1, a2, a3, i1, i2, i3, i4, k1, k2, k3, k4, g1, g2, g3]
        };
        let full = all(&c, &h, &l);
        let cut = all(&c[..600], &h[..600], &l[..600]);
        for (k, (f, s)) in full.iter().zip(&cut).enumerate() {
            for i in 0..600 {
                assert!(f[i] == s[i] || (f[i].is_nan() && s[i].is_nan()), "indicator {k}, index {i}: {} vs {}", f[i], s[i]);
            }
            assert!(f[899].is_finite(), "indicator {k} never warmed up");
        }
        assert!(ta::sma(&c, 5)[3].is_nan() && ta::sma(&c, 5)[4].is_finite());
        assert_eq!(ta::extreme(&[1.0, 5.0, 2.0, 3.0, 0.0], 2, true)[2..], [5.0, 3.0, 3.0]);
        assert_eq!(ta::sma(&[1.0, 2.0, 3.0, 4.0], 2)[3], 3.5);
    }

    #[test]
    fn every_new_kind_parses_and_long_chains_are_refused() {
        for (kind, ins, _, _) in KINDS.iter() {
            if ["range", "rebalance", "exit", "hold"].contains(kind) {
                continue;
            }
            let mut inputs = serde_json::Map::new();
            for p in *ins {
                inputs.insert(p.to_string(), json!(["p", "tick"]));
            }
            let g = graph(json!({"type": "graph", "nodes": [
                {"id": "p", "kind": "price"}, {"id": "x", "kind": kind, "in": inputs},
                {"id": "w", "kind": "const", "p": {"value": 10}},
                {"id": "ar", "kind": "around", "in": {"center": ["p", "tick"], "width": ["w", "value"]}},
                {"id": "r", "kind": "range", "in": {"lower": ["ar", "lower"], "upper": ["ar", "upper"]}}]}));
            assert!(g.is_ok(), "{kind}: {:?}", g.err());
            let m = market(&(0..3000).map(|i| ((i as f64 / 50.0).sin() * 40.0) as i64).collect::<Vec<_>>());
            let g = g.unwrap();
            let xi = g.nodes.iter().position(|n| n.id == "x").unwrap();
            let ev = evaluate(&g, &m);
            if !STATEFUL.contains(kind) {
                let out = ev.series.iter().filter(|((i, _), _)| *i == xi).map(|(_, v)| v).collect::<Vec<_>>();
                assert!(!out.is_empty() && out.iter().all(|v| v[2999].is_finite()), "{kind} produced no value");
            }
        }
        let long = graph(json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"}, {"id": "a", "kind": "sma", "p": {"hours": 120}, "in": {"in": ["p", "tick"]}},
            {"id": "b", "kind": "sma", "p": {"hours": 120}, "in": {"in": ["a", "out"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["b", "out"], "upper": ["p", "tick"]}}]}));
        assert!(long.unwrap_err().contains("hours of history"));
        let bad = graph(json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"}, {"id": "o", "kind": "out_of_range"}, {"id": "e", "kind": "ema", "in": {"in": ["o", "out"]}},
            {"id": "x", "kind": "exit", "in": {"when": ["e", "out"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["p", "tick"], "upper": ["p", "tick"]}}]}));
        assert!(bad.unwrap_err().contains("position condition"));
    }

    #[test]
    fn bollinger_range_follows_the_band() {
        let ticks: Vec<i64> = (0..200).map(|i| ((i as f64 / 7.0).sin() * 10.0) as i64).collect();
        let m = market(&ticks);
        let g = graph(bollinger()).unwrap();
        let ev = evaluate(&g, &m);
        assert!(wanted(&g, &ev, 5, 1).is_none(), "12 bars of warm-up");
        let (a, c) = wanted(&g, &ev, 100, 1).unwrap();
        assert!(a < c);
        let mut st = State::default();
        assert!(matches!(decide(&g, &ev, &mut st, 0, 100, ticks[100], None, 1), Decision::Move { .. }));
        assert!(matches!(decide(&g, &ev, &mut st, 0, 100, (a + c) / 2, Some((a, c)), 1), Decision::Keep(_)));
        assert!(matches!(decide(&g, &ev, &mut st, 0, 101, c + 50, Some((a, c)), 1), Decision::Move { .. }));
    }

    #[test]
    fn bad_graphs_are_refused() {
        let mut v = bollinger();
        v["nodes"][2]["in"]["upper"] = json!(["bb", "nope"]);
        assert!(graph(v).is_err());
        let mut v = bollinger();
        v["nodes"].as_array_mut().unwrap().remove(2);
        assert!(graph(v).unwrap_err().contains("Range"));
        let loop_ = json!({"nodes": [{"id": "a", "kind": "abs", "in": {"in": ["b", "out"]}}, {"id": "b", "kind": "abs", "in": {"in": ["a", "out"]}},
                                     {"id": "r", "kind": "range", "in": {"lower": ["a", "out"], "upper": ["b", "out"]}}]});
        assert!(graph(loop_).unwrap_err().contains("loop"));
        let mut v = bollinger();
        v["nodes"][1]["p"]["hours"] = json!(1e9);
        assert!(graph(v).is_err());
    }

    #[test]
    fn exit_and_reentry() {
        let v = json!({"nodes": [
            {"id": "p", "kind": "price"},
            {"id": "w", "kind": "const", "p": {"value": 10}},
            {"id": "ar", "kind": "around", "in": {"center": ["p", "tick"], "width": ["w", "value"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["ar", "lower"], "upper": ["ar", "upper"]}},
            {"id": "lim", "kind": "const", "p": {"value": -50}},
            {"id": "c", "kind": "lt", "in": {"a": ["p", "tick"], "b": ["lim", "value"]}},
            {"id": "x", "kind": "exit", "in": {"when": ["c", "out"]}}
        ]});
        let g = graph(v).unwrap();
        let ticks = vec![0, 0, -60, -60, 0];
        let m = market(&ticks);
        let ev = evaluate(&g, &m);
        let mut st = State::default();
        assert!(matches!(decide(&g, &ev, &mut st, 0, 2, -60, Some((-5, 5)), 1), Decision::Exit(_)));
        st.exited();
        assert!(matches!(decide(&g, &ev, &mut st, 10, 3, -60, None, 1), Decision::Keep(_)));
        assert!(matches!(decide(&g, &ev, &mut st, 20, 4, 0, None, 1), Decision::Move { .. }));
    }

    #[test]
    fn position_conditions_and_units() {
        let v = json!({"nodes": [
            {"id": "p", "kind": "price"},
            {"id": "w", "kind": "const", "unit": "spacings", "p": {"value": 4}},
            {"id": "ar", "kind": "around", "in": {"center": ["p", "tick"], "width": ["w", "value"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["ar", "lower"], "upper": ["ar", "upper"]}},
            {"id": "o", "kind": "out_of_range", "unit": "spacings", "p": {"below": 1, "above": 1}},
            {"id": "m", "kind": "rebalance", "p": {"cooldown_min": 0}, "in": {"when": ["o", "out"]}}
        ]});
        let g = Graph::parse(v.as_object().unwrap(), 10).unwrap();
        let m = market(&[0, 0, 0]);
        let ev = evaluate(&g, &m);
        assert_eq!(wanted(&g, &ev, 0, 10), Some((-20, 20)));
        let mut st = State::default();
        // range [0, 40) widened by one spacing each side is [-10, 50): 55 is outside, 45 inside
        assert!(matches!(decide(&g, &ev, &mut st, 0, 1, 45, Some((0, 40)), 10), Decision::Keep(_)));
        assert!(matches!(decide(&g, &ev, &mut st, 0, 1, 55, Some((0, 40)), 10), Decision::Move { .. }));
        let pct = json!({"nodes": [{"id": "c", "kind": "const", "unit": "percent", "p": {"value": 1}},
                                   {"id": "r", "kind": "range", "in": {"lower": ["c", "value"], "upper": ["c", "value"]}}]});
        let g = Graph::parse(pct.as_object().unwrap(), 1).unwrap();
        assert!((g.nodes[0].p["value"] - 99.5).abs() < 0.1);
    }
}
