//! Node strategies: a range and its triggers built from connected nodes in the Strategy lab
//! (strategy type "graph"). The same evaluation runs live (worker) and in backtests.
//!
//! Nodes are sources (price, bar high/low, fees, constants), indicators (ta.rs: averages, bands,
//! channels, oscillators and trend scores), maths and conditions, and sinks:
//!   range     (lower, upper)  the range wanted at each bar (this or a ladder is required);
//!   ladder    ()              instead of a range: the app's ladder (ladder.rs), capital spread over
//!                             single ticks around the trailing median where fees were paid,
//!                             re-solved every `every_hours` (and whenever the rebalance condition
//!                             holds), moved only when at least `turnover` of the value would move;
//!                             with a `guard` (ticks, 0 = off) it holds while the price is further
//!                             than that from its 7-day median (the depeg guard of pegged pairs).
//!                             Each setting has an input of the same name (hours, span,
//!                             every_hours, turnover, guard, width): connected, its value at the
//!                             decision overrides the setting (clamped to the setting's limits; a
//!                             value still warming up leaves the setting), so other blocks can pick
//!                             the median window, the span or the re-solve schedule bar by bar;
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
//!
//! "Wait out the move" (reports/DIRECTIONAL.md) is built from three nodes made for it: `breakout`
//! (the side of the last n-hour breakout, and whether a new extreme came in the last hours: the move
//! is on), `volatility` (the daily sigma, to size a band) and `fee_vs_arb` (the pool's trailing fees
//! over its loss to arbitrage, F/LVR: below 1 the pool does not pay for what arbitrage takes). The
//! exit sink's optional `to` input picks the token to wait in (+1 token0, -1 token1).
//!
//! Other tokens: `token_price` is another token's USD price (external.rs: Solidly pools with enough
//! active liquidity, through a paired token when there is no stable pool), in ticks. Its `stale`
//! output is true while there is no reliable price (no such pool, or none traded for 12 hours), so
//! a strategy can hold or exit instead of acting on a price it does not have.
//!
//! Timeframes: any node that is not a position condition or a sink may run on candles of 15 or 30
//! minutes, 1 or 4 hours or a day ("tf", in minutes), built in memory from the 5-minute bars: its
//! inputs are taken at each candle's close (highs and lows over the candle), lookbacks count in
//! hours (explicit `*_periods` smoothing parameters count candles), and its value holds from that close until the next one. A candle counts
//! once it has closed, so a decision never sees the candle still forming.
//! 1-minute candles are finer than the replay: they are built from the swaps themselves (the last
//! swap at or before each minute's end, highs, lows and fees over the minute), 5-minute inputs reach
//! them from the last bar closed by then, and a decision (every 5 minutes) reads the minute that
//! ends with its bar.

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

/// Periods up to a year (slow trends, 6–12 month momentum; best on daily candles): the live replay
/// and backtests' warm-up hold what the graph needs (Graph::replay_days, backtest.rs warmup_s).
const H: (&str, f64, f64, f64) = ("hours", 24.0, 0.1, 8760.0);
/// The long-window nodes (breakout, volatility, fee vs arbitrage) reach back up to a year too.
const LONG: f64 = 8760.0;
pub const KINDS: &[Spec] = &[
    ("price", &[], &["tick"], &[]),
    ("high_low", &[], &["high", "low"], &[]),
    ("fees", &[], &["out"], &[]),
    ("const", &[], &["value"], &[("value", 10.0, -1e6, 1e6)]),
    ("sma", &["in"], &["out"], &[H]),
    ("ema", &["in"], &["out"], &[H]),
    ("median", &["in"], &["out"], &[H]),
    ("stdev", &["in"], &["out"], &[H]),
    // Parkinson sigma in ticks per candle (not annualized); invariant to the pool's quote direction.
    ("parkinson", &[], &["out"], &[H]),
    ("efficiency_ratio", &["in"], &["out"], &[H]),
    // KAMA's fast/slow are smoothing lengths in candles; only the efficiency window is in hours.
    ("kama", &["in"], &["out"], &[H, ("fast_periods", 2.0, 1.0, 1000.0), ("slow_periods", 30.0, 1.0, 1000.0)]),
    // slope is input units per hour, so its scale is consistent across candle timeframes.
    ("linreg", &["in"], &["slope", "r2"], &[H]),
    ("drawdown", &["in"], &["out"], &[H]),
    ("bollinger", &["in"], &["upper", "mid", "lower"], &[H, ("k", 2.0, 0.0, 20.0)]),
    ("atr", &[], &["out"], &[H]),
    ("keltner", &[], &["upper", "mid", "lower"], &[H, ("k", 2.0, 0.0, 20.0)]),
    ("donchian", &[], &["upper", "lower"], &[H]),
    ("rsi", &["in"], &["out"], &[("hours", 14.0, 0.1, 168.0)]),
    ("lag", &["in"], &["out"], &[("hours", 1.0, 0.0, 8760.0)]),
    ("change", &["in"], &["out"], &[("hours", 1.0, 0.1, 8760.0)]),
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
    ("breakout", &[], &["side", "hot"], &[("hours", 336.0, 1.0, LONG), ("hot_hours", 24.0, 0.0, LONG)]),
    ("volatility", &[], &["out"], &[("hours", 168.0, 2.0, LONG)]),
    ("fee_vs_arb", &[], &["out"], &[("hours", 168.0, 24.0, LONG)]),
    ("token_price", &[], &["tick", "high", "low", "stale"], &[]),
    // the pool's volatile token (the one that is not the numeraire) priced in the numeraire, in ticks:
    // rises when that token does, whichever way round the pool is; 'token' is +1 when it is token0
    // and -1 when it is token1 (into an exit's 'to': wait in the volatile token)
    ("vol_price", &[], &["tick", "token"], &[]),
    // what a short of the hedged token earns on Hyperliquid, % a year (backtests only)
    ("funding", &[], &["apr"], &[]),
    ("add", &["a", "b"], &["out"], &[]),
    ("sub", &["a", "b"], &["out"], &[]),
    ("mul", &["a", "b"], &["out"], &[]),
    // a / b (NaN where b is 0): e.g. a target volatility over the measured one
    ("div", &["a", "b"], &["out"], &[]),
    ("scale", &["in"], &["out"], &[("k", 2.0, -1e6, 1e6)]),
    ("offset", &["in"], &["out"], &[("value", 10.0, -1e6, 1e6)]),
    ("min", &["a", "b"], &["out"], &[]),
    ("max", &["a", "b"], &["out"], &[]),
    ("abs", &["in"], &["out"], &[]),
    // prices are ticks (log price, base 1.0001): to_price turns one into the price itself (1.0001^in),
    // for arithmetic-price indicators (a mean or z-score of the price, not of its log); to_tick back
    ("to_price", &["in"], &["out"], &[]),
    ("to_tick", &["in"], &["out"], &[]),
    ("around", &["center", "width"], &["lower", "upper"], &[("below", 0.5, 0.0, 1.0)]),
    ("switch", &["cond", "a", "b"], &["out"], &[]),
    ("gt", &["a", "b"], &["out"], &[]),
    ("lt", &["a", "b"], &["out"], &[]),
    ("cross_above", &["a", "b"], &["out"], &[]),
    ("cross_below", &["a", "b"], &["out"], &[]),
    ("between", &["in", "lower", "upper"], &["out"], &[]),
    ("distance_gt", &["a", "b"], &["out"], &[("value", 20.0, 0.0, 1e6)]),
    ("and", &["a", "b"], &["out"], &[]),
    // true from a bar where 'set' is true until one where 'reset' is (set wins when both are)
    ("latch", &["set", "reset"], &["out"], &[]),
    ("or", &["a", "b"], &["out"], &[]),
    ("not", &["in"], &["out"], &[]),
    ("out_of_range", &[], &["out"], &[("below", 0.0, -1e6, 1e6), ("above", 0.0, -1e6, 1e6)]),
    ("range_shift", &[], &["out"], &[("value", 10.0, 0.0, 1e6)]),
    ("range", &["lower", "upper"], &[], &[]),
    ("ladder", LADDER_INPUTS, &[], &[("hours", 24.0, 1.0, 168.0), ("span", 3.0, 0.0, 1e6), ("every_hours", 6.0, 0.1, 168.0), ("turnover", 0.4, 0.0, 1.0), ("guard", 0.0, 0.0, 1e6), ("width", 1.0, 1.0, 1e6)]),
    ("rebalance", &["when"], &[], &[("delay_min", 0.0, 0.0, 10080.0), ("cooldown_min", 60.0, 0.0, 43200.0)]),
    // min_out_hours: stay out at least this long after the exit; confirm_hours: the re-entry
    // condition must hold this long without a break before depositing again (0: at once)
    ("exit", &["when", "reenter", "to"], &[], &[("delay_min", 0.0, 0.0, 10080.0), ("min_out_hours", 0.0, 0.0, 8760.0), ("confirm_hours", 0.0, 0.0, 8760.0)]),
    ("hold", &["when"], &[], &[]),
    // a perp short on Hyperliquid against the pool's volatile token: `ratio` of
    // the pool's exposure, traded back when it drifts `band_pct` from the target and by at least
    // `min_usd`; margin at `leverage` (effective) comes out of the pool's capital. `size` adds a
    // position of its own in the same perp, as a share of the strategy's whole value: + long the
    // volatile token, − short it (a direction, not a hedge; 0: none)
    ("hedge", &["when", "ratio", "size"], &[], &[("ratio", 1.0, 0.0, 1.5), ("band_pct", 10.0, 1.0, 50.0), ("min_usd", 25.0, 0.0, 1e6), ("leverage", 2.0, 1.0, 5.0), ("smooth_h", 0.0, 0.0, 168.0), ("size", 0.0, -2.0, 2.0)]),
];
/// The ladder's inputs: one per setting, overriding it when connected (`ladder_value`).
const LADDER_INPUTS: &[&str] = &["hours", "span", "every_hours", "turnover", "guard", "width"];
/// Inputs that may stay unconnected (the sink's default applies).
const OPTIONAL: &[(&str, &str)] = &[("rebalance", "when"), ("exit", "reenter"), ("exit", "to"), ("hedge", "when"), ("hedge", "ratio"), ("hedge", "size"),
    ("ladder", "hours"), ("ladder", "span"), ("ladder", "every_hours"), ("ladder", "turnover"), ("ladder", "guard"), ("ladder", "width")];
/// Params measured in ticks (scaled by the node's unit).
const TICK_PARAMS: &[(&str, &str)] =
    &[("const", "value"), ("offset", "value"), ("distance_gt", "value"), ("out_of_range", "below"), ("out_of_range", "above"), ("range_shift", "value"), ("ladder", "span"), ("ladder", "guard"), ("ladder", "width")];
/// Outputs that are prices (ticks): drawn on the chart.
const PRICE_LIKE: &[&str] = &["price", "high_low", "sma", "ema", "median", "bollinger", "keltner", "donchian", "around", "offset", "add", "sub", "min", "max", "switch",
    "lag", "highest", "lowest", "percentile", "psar", "ichimoku", "heikin_ashi", "gaussian", "kama"];
/// Kinds a decision can compute at one bar from other values: the only ones a node fed by a
/// position condition may be.
const POINTWISE: &[&str] = &["and", "or", "not", "switch", "gt", "lt", "add", "sub", "mul", "div", "scale", "offset", "min", "max", "abs", "to_price", "to_tick", "between", "distance_gt",
    "range", "ladder", "rebalance", "exit", "hold", "hedge"];
/// History a graph may need before its first decision. Backtests warm up 16 days; the live replay
/// keeps 10 days, or more for a graph that needs it (`Graph::replay_days`).
pub const MAX_LOOKBACK_HOURS: f64 = 8800.0;
const STATEFUL: &[&str] = &["out_of_range", "range_shift"];
/// The outputs: no timeframe, at most one of each.
const SINKS: &[&str] = &["range", "ladder", "rebalance", "exit", "hold", "hedge"];

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
    /// candle length in seconds (300: the replay's own bars)
    pub tf_s: i64,
    /// token_price: the token (and its chain, when not the pool's)
    pub token: Option<(Option<u64>, String)>,
}

/// Timeframes a node may run on, in minutes.
pub const TIMEFRAMES: &[i64] = &[1, 5, 15, 30, 60, 240, 1440];

#[derive(Clone, Debug)]
pub struct Graph {
    /// in evaluation order (every input before its node)
    pub nodes: Vec<Node>,
    pub exit_to: ExitTo,
    pub name: String,
    stateful: HashSet<usize>,
    /// hours of history the graph needs before its first decision
    pub lookback: f64,
    /// the token an exit waits in is parked in a pegged pool meanwhile (parking.rs)
    pub park: bool,
    /// a ladder block's most ranges (the pool's setting; the backtest's request)
    pub max_ranges: usize,
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
            if sp.0 == "kama" && p["fast_periods"] > p["slow_periods"] {
                return Err(format!("node {id}: fast_periods must be at most slow_periods"));
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
            let tf = n["tf"].as_i64().unwrap_or(5);
            if !TIMEFRAMES.contains(&tf) {
                return Err(format!("node {id}: timeframe must be one of {TIMEFRAMES:?} minutes"));
            }
            if tf != 5 && (STATEFUL.contains(&sp.0) || SINKS.contains(&sp.0)) {
                return Err(format!("node {id}: position conditions and outputs run on every bar (no timeframe)"));
            }
            let token = if sp.0 == "token_price" {
                let a = n["token"].as_str().unwrap_or("").to_lowercase();
                if !(a.len() == 42 && a.starts_with("0x") && a[2..].chars().all(|c| c.is_ascii_hexdigit())) {
                    return Err(format!("node {id}: pick the token to watch"));
                }
                Some((n["chain_id"].as_u64(), a))
            } else {
                None
            };
            nodes.push(Node { id, kind: sp.0, p, inputs, label, tf_s: tf * 60, token });
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
        if count("range") + count("ladder") != 1 {
            return Err("node strategy: needs exactly one Range or Ladder node".into());
        }
        for k in ["rebalance", "exit", "hold", "hedge"] {
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
            look[i] = own_lookback(&sorted[i]) + (sorted[i].tf_s - 300).max(0) as f64 / 3600.0 + sorted[i].inputs.values().map(|(s, _)| look[*s]).fold(0.0, f64::max);
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
            "stable" => ExitTo::Stable,
            _ => ExitTo::Keep,
        };
        let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("").chars().take(60).collect();
        let lookback = look.iter().copied().fold(0.0, f64::max);
        let park = s.get("park").and_then(|v| v.as_bool()).unwrap_or(false);
        Ok(Graph { nodes: sorted, exit_to, name, stateful, lookback, park, max_ranges: 5 })
    }

    /// Other tokens the graph watches: (node index, chain or None for the pool's, token).
    pub fn externals(&self) -> Vec<(usize, Option<u64>, String)> {
        self.nodes.iter().enumerate().filter_map(|(i, n)| n.token.as_ref().map(|(c, t)| (i, *c, t.clone()))).collect()
    }

    /// Days of swaps the live replay must hold for this graph (10 at least, as before).
    /// The replay's bar, and so how often the strategy decides: every minute when any block works on
    /// 1-minute candles (its value changes every minute), else every 5 minutes (the price bar every
    /// position condition reads). The backtest replays on these bars and the worker checks the pool
    /// this often, so both decide at the same moments.
    pub fn bar_s(&self) -> i64 {
        if self.nodes.iter().any(|n| n.tf_s == 60) { 60 } else { crate::data::BAR_S }
    }

    pub fn replay_days(&self) -> i64 {
        ((self.lookback / 24.0).ceil() as i64 + 1).max(10)
    }

    pub fn label(&self) -> String {
        if self.name.is_empty() { format!("nodes({})", self.nodes.len()) } else { format!("nodes: {}", self.name) }
    }

    /// The ladder block's settings in the ladder's own terms (ladder.rs), when the graph has one.
    pub fn ladder(&self) -> Option<crate::ladder::Params> {
        let i = self.sink("ladder")?;
        Some(crate::ladder::Params {
            lookback: (self.param(i, "hours") * 3600.0).round() as i64,
            span: self.param(i, "span").round().max(0.0) as i64,
            every: (self.param(i, "every_hours") * BARS_PER_HOUR).round().max(1.0) as i64,
            turnover: self.param(i, "turnover"),
            guard: Some(self.param(i, "guard")).filter(|g| *g > 0.0),
            width: self.param(i, "width").round().max(1.0) as i64,
        })
    }

    /// The hedge block's settings, when the graph has one.
    pub fn hedge(&self) -> Option<HedgeCfg> {
        let i = self.sink("hedge")?;
        Some(HedgeCfg { ratio: self.param(i, "ratio"), band: self.param(i, "band_pct") / 100.0, min_usd: self.param(i, "min_usd"), leverage: self.param(i, "leverage"),
                        smooth_h: self.param(i, "smooth_h"), size: self.param(i, "size"),
                        sized: self.nodes[i].inputs.contains_key("size") })
    }

    fn sink(&self, kind: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.kind == kind)
    }

    /// Whether an exit may wait in the dollar stable (the strategy's exit_to).
    pub fn may_exit_to_stable(&self) -> bool {
        self.exit_to == ExitTo::Stable
    }

    fn param(&self, i: usize, k: &str) -> f64 {
        self.nodes[i].p.get(k).copied().unwrap_or(0.0)
    }
}

// -- series --------------------------------------------------------------------------------------------

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
        "parkinson" | "efficiency_ratio" | "kama" | "linreg" | "drawdown" => {
            // Account for rounding hours to candles, especially a sub-day window on daily candles.
            // ER/KAMA need n changes (n+1 closes); the other windows are conservatively one close extra.
            let step = n.tf_s as f64 / 3600.0;
            let min_periods = if n.kind == "linreg" { 2.0 } else { 1.0 };
            (p("hours") / step).round().max(min_periods) * step + bar
        }
        // a window an input sets may be as long as the setting allows
        "ladder" if n.inputs.contains_key("hours") => 168.0 + bar,
        "sma" | "ema" | "median" | "stdev" | "bollinger" | "keltner" | "highest" | "lowest" | "percentile" | "zscore" | "cci" | "lag" | "change" | "ladder" => p("hours") + bar,
        "atr" | "rsi" | "donchian" | "stochastic" => p("hours") + p("d_hours") + bar,
        "adx" => 2.0 * p("hours") + bar,
        "trix" => 3.0 * p("hours") + bar,
        "macd" => p("fast").max(p("slow")) + p("signal"),
        "stoch_rsi" => p("hours") + p("stoch_hours") + bar,
        "ichimoku" => p("tenkan").max(p("kijun")).max(p("span_b")) + p("shift"),
        "gaussian" => 5.0 * p("hours"),
        "trend" => 50.0 * p("unit_hours"),
        "momentum" => p("hours") + p("window") + p("smooth") + bar,
        "breakout" | "volatility" | "fee_vs_arb" => p("hours") + 1.0 + bar,
        _ => 0.0,
    }
}

fn truth(v: f64) -> bool {
    v.is_finite() && v > 0.5
}

fn b(v: bool) -> f64 {
    if v { 1.0 } else { 0.0 }
}

/// The breakout state over `n` bars (a turtle): side +1 from a close above the previous n bars' highest
/// close until one below their lowest (and the mirror); hot = 1 while the last new extreme on the
/// side's own direction came within `hot` bars (the move is on). NaN until n bars are in.
fn breakout(close: &[f64], n: usize, hot: usize) -> (Vec<f64>, Vec<f64>) {
    let hi = ta::lag(&ta::extreme(close, n, true), 1);
    let lo = ta::lag(&ta::extreme(close, n, false), 1);
    let (mut side, mut on) = (vec![f64::NAN; close.len()], vec![f64::NAN; close.len()]);
    let (mut st, mut last) = (0.0f64, None::<usize>);
    for j in 0..close.len() {
        if !hi[j].is_finite() || !lo[j].is_finite() || !close[j].is_finite() {
            continue;
        }
        if close[j] > hi[j] {
            st = 1.0;
            last = Some(j);
        } else if close[j] < lo[j] {
            st = -1.0;
            last = Some(j);
        }
        side[j] = st;
        on[j] = b(st != 0.0 && last.map(|l| j - l <= hot).unwrap_or(false));
    }
    (side, on)
}

/// The pool's fees over its loss to arbitrage (F/LVR, data::fee_vs_lvr) over the last `n` bars, per
/// unit of at-the-money liquidity: F sums each swap's fee over the pool's active liquidity, LVR sums
/// r^2 * sqrt(P) / 4 over hourly closes. NaN until n bars are in, or when the price did not move.
fn fee_vs_arb(m: &Market, n: usize) -> Vec<f64> {
    let nb = m.bar_tick.len();
    let ln_tick = 1.0001f64.ln();
    let (mut f, mut l) = (vec![0.0; nb], vec![0.0; nb]);
    let mut prev_hour: Option<i64> = None;
    for j in 0..nb {
        let from = if j == 0 { m.bar_idx[0] } else { m.bar_idx[j - 1] + 1 };
        for k in from..=m.bar_idx[j] {
            if k < m.fee.len() && m.l_active[k] > 0.0 {
                f[j] += m.sig_fee(k) / m.l_active[k];
            }
        }
        if m.bar_end[j] % 3600 == 0 {
            let t = m.bar_tick[j];
            if let Some(pt) = prev_hour {
                let r = (t - pt) as f64 * ln_tick;
                let p = 1.0001f64.powf(t as f64) * m.px_scale;
                let root = if m.num1 { p.sqrt() } else { 1.0 / p.sqrt() };
                l[j] = r * r * root / 4.0;
            }
            prev_hour = Some(t);
        }
    }
    let (mut sf, mut sl) = (0.0, 0.0);
    let mut out = vec![f64::NAN; nb];
    for j in 0..nb {
        sf += f[j];
        sl += l[j];
        if j >= n {
            sf -= f[j - n];
            sl -= l[j - n];
        }
        if j + 1 >= n && sl > 0.0 {
            out[j] = sf / sl;
        }
    }
    out
}

/// Every stateless output over all bars of `m`.
pub struct Eval {
    pub series: HashMap<(usize, String), Vec<f64>>,
}

/// Other tokens' prices by node index, at the replay's 5-minute bars (external.rs).
pub type Externals = HashMap<usize, crate::external::Series>;

/// Load the prices of the tokens the graph watches over the bars of `m` (chain `cid` unless a node
/// names another). A token that cannot be priced stays out (its node is NaN) and is reported.
pub fn load_externals(g: &Graph, cid: u64, m: &Market) -> (Externals, Vec<String>) {
    let (mut out, mut errs) = (HashMap::new(), vec![]);
    let minutes = minute_ends(m);
    for (i, c, t) in g.externals() {
        // a block finer than the replay's bars gets its own minute series from the watched pools' swaps
        let ends = if g.nodes[i].tf_s < m.bar_s { &minutes } else { &m.bar_end };
        match crate::external::load(c.unwrap_or(cid), &t, ends) {
            Ok(s) => {
                out.insert(i, s);
            }
            Err(e) => errs.push(format!("node {}: {e}", g.nodes[i].id)),
        }
    }
    (out, errs)
}

/// The bars a node sees: the replay's own, longer candles, or 1-minute candles.
struct Frame {
    close: Vec<f64>,
    hi: Vec<f64>,
    lo: Vec<f64>,
    fee: Vec<f64>,
    /// candles per hour
    bph: f64,
    /// the 5-minute bar that closes each candle (None: the bars themselves)
    idx: Option<Vec<usize>>,
    /// 1-minute candles: for each, the last 5-minute bar closed by its end (its 5-minute inputs)
    fine: Option<Vec<Option<usize>>>,
    /// for each 5-minute bar, the last candle closed at or before it
    pos: Vec<Option<usize>>,
}

/// The end of every minute of the replay: one per minute of each bar, the last one at the bar's end.
pub fn minute_ends(m: &Market) -> Vec<i64> {
    let per = (m.bar_s / 60).max(1);
    m.bar_end.iter().flat_map(|e| (0..per).rev().map(move |k| e - 60 * k)).collect()
}

/// 1-minute candles from the swaps: the last swap at or before each minute's end (NaN before the
/// first swap), the high and low swap ticks and the fees in the minute.
fn minute_frame(m: &Market) -> Frame {
    let ends = minute_ends(m);
    let n = ends.len();
    let mut close = vec![f64::NAN; n];
    let (mut hi, mut lo, mut fee) = (vec![f64::NAN; n], vec![f64::NAN; n], vec![0.0; n]);
    if !m.ts.is_empty() {
        let last: Vec<Option<usize>> = ends.iter().map(|e| m.ts.partition_point(|t| t <= e).checked_sub(1)).collect();
        let mut prev: Option<usize> = None;
        for (k, l) in last.iter().enumerate() {
            let Some(i) = *l else { continue };
            close[k] = m.tick[i] as f64;
            let from = prev.map(|p| p + 1).unwrap_or(i);
            let (mut h, mut lw, mut f) = (m.tick[i], m.tick[i], 0.0);
            for s in from..=i {
                h = h.max(m.tick[s]);
                lw = lw.min(m.tick[s]);
                f += m.sig_fee(s);
            }
            if prev.is_some() && from > i {
                f = 0.0; // no swap in this minute: the close carries, no fees
            }
            hi[k] = h as f64;
            lo[k] = lw as f64;
            fee[k] = f;
            prev = Some(i);
        }
    }
    // 5-minute inputs: the bar that closed at or before the minute's end (the bar ending with it)
    let fine: Vec<Option<usize>> = ends.iter().map(|e| m.bar_end.partition_point(|b| b <= e).checked_sub(1)).collect();
    let per = (m.bar_s / 60).max(1) as usize;
    let pos = (0..m.bar_end.len()).map(|j| Some(per * j + per - 1)).collect();
    Frame { close, hi, lo, fee, bph: 60.0, idx: None, fine: Some(fine), pos }
}

fn group<F: Fn(&[f64]) -> f64>(v: &[f64], idx: &[usize], f: F) -> Vec<f64> {
    let mut start = 0;
    idx.iter().map(|&j| {
        let r = f(&v[start..=j]);
        start = j + 1;
        r
    }).collect()
}

fn frame(m: &Market, close: &[f64], hi: &[f64], lo: &[f64], tf_s: i64) -> Frame {
    let n = close.len();
    if tf_s < m.bar_s {
        return minute_frame(m);
    }
    if tf_s == m.bar_s {
        return Frame { close: close.to_vec(), hi: hi.to_vec(), lo: lo.to_vec(), fee: m.bar_fee.clone(), bph: 3600.0 / m.bar_s as f64, idx: None, fine: None, pos: (0..n).map(Some).collect() };
    }
    let idx: Vec<usize> = (0..n).filter(|j| m.bar_end[*j] % tf_s == 0).collect();
    let mut pos = vec![None; n];
    let mut k: Option<usize> = None;
    let mut next = 0;
    for (j, p) in pos.iter_mut().enumerate() {
        while next < idx.len() && idx[next] <= j {
            k = Some(next);
            next += 1;
        }
        *p = k;
    }
    Frame {
        close: idx.iter().map(|j| close[*j]).collect(),
        hi: group(hi, &idx, |w| w.iter().copied().fold(f64::NEG_INFINITY, f64::max)),
        lo: group(lo, &idx, |w| w.iter().copied().fold(f64::INFINITY, f64::min)),
        fee: group(&m.bar_fee, &idx, |w| w.iter().sum()),
        bph: 3600.0 / tf_s as f64,
        idx: Some(idx),
        fine: None,
        pos,
    }
}

impl Frame {
    /// A 5-minute series at this frame's candle closes.
    fn sample(&self, v: Vec<f64>) -> Vec<f64> {
        if let Some(f) = &self.fine {
            return f.iter().map(|j| j.map(|j| v[j]).unwrap_or(f64::NAN)).collect();
        }
        match &self.idx {
            None => v,
            Some(ix) => ix.iter().map(|j| v[*j]).collect(),
        }
    }
    /// A series on this frame's candles, back on the 5-minute bars (each candle's value from its close on).
    fn expand(&self, v: Vec<f64>) -> Vec<f64> {
        if self.idx.is_none() && self.fine.is_none() {
            return v;
        }
        self.pos.iter().map(|k| k.map(|k| v[k]).unwrap_or(f64::NAN)).collect()
    }
}

/// Node outputs at every 5-minute bar of `m`, without other tokens' prices (token_price is NaN).

/// A tick's price, 1.0001^tick
fn to_price(t: f64) -> f64 {
    1.0001f64.powf(t)
}

/// A price's tick, log base 1.0001 (NaN for a price of zero or less)
fn to_tick(p: f64) -> f64 {
    if p > 0.0 { p.ln() / 1.0001f64.ln() } else { f64::NAN }
}

#[cfg(test)]
pub fn evaluate(g: &Graph, m: &Market) -> Eval {
    evaluate_ext(g, m, &HashMap::new())
}

/// Node outputs at every 5-minute bar of `m`, with the watched tokens' prices.
pub fn evaluate_ext(g: &Graph, m: &Market, ext: &Externals) -> Eval {
    let nb = m.bar_tick.len();
    let close0: Vec<f64> = m.bar_tick.iter().map(|t| *t as f64).collect();
    let hi0: Vec<f64> = m.bar_hi.iter().map(|t| *t as f64).collect();
    let lo0: Vec<f64> = m.bar_lo.iter().map(|t| *t as f64).collect();
    let mut frames: HashMap<i64, Frame> = HashMap::new();
    let mut s: HashMap<(usize, String), Vec<f64>> = HashMap::new();
    // 1-minute outputs on their own minutes, for 1-minute nodes that read them
    let mut fine_s: HashMap<(usize, String), Vec<f64>> = HashMap::new();
    for (i, node) in g.nodes.iter().enumerate() {
        // a ladder's own series (its median, the price, the guard's distance) never depend on its
        // inputs: computed even when an input follows the position
        if g.stateful.contains(&i) && node.kind != "ladder" || node.inputs.is_empty() && STATEFUL.contains(&node.kind) {
            continue;
        }
        let fr = frames.entry(node.tf_s).or_insert_with(|| frame(m, &close0, &hi0, &lo0, node.tf_s));
        let n = fr.close.len();
        let (close, hi, lo) = (&fr.close, &fr.hi, &fr.lo);
        let bph = fr.bph;
        let bars = |hours: f64| ((hours * bph).round() as usize).max(1);
        let inp = |port: &str| -> Vec<f64> {
            if fr.fine.is_some() {
                if let Some(v) = node.inputs.get(port).and_then(|(src, out)| fine_s.get(&(*src, out.clone()))) {
                    return v.clone();
                }
            }
            fr.sample(node.inputs.get(port).and_then(|(src, out)| s.get(&(*src, out.clone())).cloned()).unwrap_or_else(|| vec![f64::NAN; nb]))
        };
        let hours = node.p.get("hours").copied().unwrap_or(24.0);
        let k = node.p.get("k").copied().unwrap_or(0.0);
        let val = node.p.get("value").copied().unwrap_or(0.0);
        let par = |k: &str| node.p.get(k).copied().unwrap_or(0.0);
        let zip = |a: Vec<f64>, c: Vec<f64>, f: &dyn Fn(f64, f64) -> f64| -> Vec<f64> { a.iter().zip(&c).map(|(x, y)| f(*x, *y)).collect() };
        let mut outs: Vec<(String, Vec<f64>)> = vec![];
        let mut put = |port: &str, v: Vec<f64>| outs.push((port.to_string(), v));
        match node.kind {
            "price" => put("tick", close.clone()),
            "vol_price" => {
                put("tick", if m.num1 { close.clone() } else { close.iter().map(|x| -x).collect() });
                put("token", vec![if m.num1 { 1.0 } else { -1.0 }; n]);
            }
            "high_low" => {
                put("high", hi.clone());
                put("low", lo.clone());
            }
            "fees" => put("out", fr.fee.clone()),
            "const" => put("value", vec![val; n]),
            // the ladder's centre: the median price over its lookback (what the wanted range is drawn around)
            "ladder" => {
                put("mid", median(close, bars(hours)));
                // a window set by an input: the median is taken at the decision, over this
                if node.inputs.contains_key("hours") {
                    put("close", close.clone());
                }
                // the depeg guard: distance of the bar's tick from its 7-day median, as ladder.rs
                if par("guard") > 0.0 || node.inputs.contains_key("guard") {
                    let med = crate::data::rolling_median_trailing(&m.bar_tick, crate::data::week_bars(m), 12);
                    put("guard_dev", m.bar_tick.iter().zip(&med).map(|(t, md)| (*t as f64 - md).abs()).collect());
                }
            }
            "sma" => put("out", ta::sma(&inp("in"), bars(hours))),
            "ema" => put("out", ta::ema(&inp("in"), bars(hours))),
            "median" => put("out", median(&inp("in"), bars(hours))),
            "stdev" => put("out", ta::stdev(&inp("in"), bars(hours))),
            "parkinson" => put("out", ta::parkinson(hi, lo, bars(hours))),
            "efficiency_ratio" => put("out", ta::efficiency_ratio(&inp("in"), bars(hours))),
            "kama" => put("out", ta::kama(&inp("in"), bars(hours), par("fast_periods").round() as usize, par("slow_periods").round() as usize)),
            "linreg" => {
                let (slope, r2) = ta::linreg(&inp("in"), bars(hours).max(2));
                put("slope", slope.into_iter().map(|v| v * bph).collect());
                put("r2", r2);
            }
            "drawdown" => put("out", ta::drawdown(&inp("in"), bars(hours))),
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
            "lag" => put("out", ta::lag(&inp("in"), (hours * bph).round() as usize)),
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
                let shift = (par("shift") * bph).round() as usize;
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
            "breakout" => {
                let (side, hot) = breakout(&close, bars(hours), bars(par("hot_hours")));
                put("side", side);
                put("hot", hot);
            }
            "volatility" => {
                // the spread of hourly moves (or of each candle's, when longer), in ticks (1 tick = 0.01%), scaled to a day
                let step = bars(1.0);
                let step_h = step as f64 / bph;
                let r: Vec<f64> = (0..n).map(|j| if j >= step { close[j] - close[j - step] } else { f64::NAN }).collect();
                put("out", ta::stdev(&r, bars(hours)).iter().map(|s| s * (24.0 / step_h).sqrt()).collect());
            }
            // from the swaps themselves, then taken at the candle closes (its hourly LVR needs bars)
            "fee_vs_arb" => put("out", fr.sample(fee_vs_arb(m, ((hours * 3600.0 / m.bar_s as f64).round() as usize).max(1)))),
            // the backtester puts the hedge's funding in `ext` (backtest.rs funding_series); empty without it
            "funding" => match ext.get(&i) {
                Some(x) if fr.fine.is_none() && x.close.len() == nb => put("apr", fr.sample(x.close.clone())),
                _ => put("apr", vec![f64::NAN; n]),
            },
            "token_price" => match ext.get(&i) {
                // 1-minute candles: loaded on the minutes already
                Some(x) if fr.fine.is_some() && x.close.len() == n => {
                    put("stale", x.close.iter().map(|v| b(!v.is_finite())).collect());
                    put("high", x.hi.clone());
                    put("low", x.lo.clone());
                    put("tick", x.close.clone());
                }
                Some(x) if fr.fine.is_none() && x.close.len() == nb => {
                    let c = fr.sample(x.close.clone());
                    put("stale", c.iter().map(|v| b(!v.is_finite())).collect());
                    match &fr.idx {
                        None => {
                            put("high", x.hi.clone());
                            put("low", x.lo.clone());
                        }
                        Some(ix) => {
                            put("high", group(&x.hi, ix, |w| w.iter().copied().fold(f64::NEG_INFINITY, f64::max)));
                            put("low", group(&x.lo, ix, |w| w.iter().copied().fold(f64::INFINITY, f64::min)));
                        }
                    }
                    put("tick", c);
                }
                _ => {
                    for port in ["tick", "high", "low"] {
                        put(port, vec![f64::NAN; n]);
                    }
                    put("stale", vec![1.0; n]);
                }
            },
            "latch" => {
                // as the live engine sees it: each cycle it rebuilds the graph from the last
                // replay_days of history, the latch off at its start, so a latch remembers only what
                // happened in that window. On while the last set or reset in the window was a set
                // (Oct 2026 review: unbounded, a backtest stayed latched where live had forgotten)
                let (st, rs) = (inp("set"), inp("reset"));
                // its inputs are empty for their own look-back at the start of live's window, so
                // live sees events in the last replay_days × 24 − lookback hours only
                let wb = bars((g.replay_days() as f64 * 24.0 - g.lookback).max(1.0));
                let mut last: Option<(usize, bool)> = None;
                put("out", st.iter().zip(&rs).enumerate().map(|(j, (a, r))| {
                    if truth(*a) {
                        last = Some((j, true));
                    } else if truth(*r) {
                        last = Some((j, false));
                    }
                    b(matches!(last, Some((e, true)) if j - e < wb))
                }).collect());
            }
            "add" => put("out", zip(inp("a"), inp("b"), &|x, y| x + y)),
            "sub" => put("out", zip(inp("a"), inp("b"), &|x, y| x - y)),
            "mul" => put("out", zip(inp("a"), inp("b"), &|x, y| x * y)),
            "div" => put("out", zip(inp("a"), inp("b"), &|x, y| if y == 0.0 { f64::NAN } else { x / y })),
            "scale" => put("out", inp("in").iter().map(|x| x * k).collect()),
            "offset" => put("out", inp("in").iter().map(|x| x + val).collect()),
            "min" => put("out", zip(inp("a"), inp("b"), &|x, y| if x.is_nan() || y.is_nan() { f64::NAN } else { x.min(y) })),
            "max" => put("out", zip(inp("a"), inp("b"), &|x, y| if x.is_nan() || y.is_nan() { f64::NAN } else { x.max(y) })),
            "abs" => put("out", inp("in").iter().map(|x| x.abs()).collect()),
            "to_price" => put("out", inp("in").iter().map(|x| to_price(*x)).collect()),
            "to_tick" => put("out", inp("in").iter().map(|x| to_tick(*x)).collect()),
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
        let fr = &frames[&node.tf_s];
        for (port, v) in outs {
            if fr.fine.is_some() {
                fine_s.insert((i, port.clone()), v.clone());
            }
            s.insert((i, port), fr.expand(v));
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
        "div" => {
            let b = inp("b");
            if b == 0.0 { f64::NAN } else { inp("a") / b }
        }
        "scale" => inp("in") * node.p.get("k").copied().unwrap_or(0.0),
        "offset" => inp("in") + val,
        "min" => inp("a").min(inp("b")),
        "max" => inp("a").max(inp("b")),
        "abs" => inp("in").abs(),
        "to_price" => to_price(inp("in")),
        "to_tick" => to_tick(inp("in")),
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

/// The range the graph wants at `bar`, snapped to the spacing (None while indicators warm up). A
/// ladder wants the ticks it spreads over: its span around the median price of its lookback.
pub fn wanted(g: &Graph, ev: &Eval, bar: usize, spacing: i64) -> Option<(i64, i64)> {
    let at = At { bar, tick: 0, range: None, wanted: None };
    let (lo, hi) = match g.sink("range") {
        Some(r) => (input(g, ev, r, "lower", &at)?, input(g, ev, r, "upper", &at)?),
        None => {
            let l = g.sink("ladder")?;
            let mid = ladder_mid(g, ev, l, &at)?.trunc();
            let span = ladder_value(g, ev, l, "span", &at).round();
            (mid - span, mid + span + 1.0)
        }
    };
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

/// A ladder setting at a decision: its input when connected and decided (clamped to the setting's
/// limits), else the block's own.
fn ladder_value(g: &Graph, ev: &Eval, l: usize, name: &str, at: &At) -> f64 {
    let own = g.param(l, name);
    match input(g, ev, l, name, at) {
        Some(v) if v.is_finite() => {
            let (lo, hi) = spec("ladder").and_then(|s| s.3.iter().find(|p| p.0 == name)).map(|p| (p.2, p.3)).unwrap_or((f64::MIN, f64::MAX));
            v.clamp(lo, hi)
        }
        _ => own,
    }
}

/// The ladder's centre at a decision: the median price over its window, the one its `hours`
/// input sets when connected (whole 5-minute bars), else the precomputed one.
fn ladder_mid(g: &Graph, ev: &Eval, l: usize, at: &At) -> Option<f64> {
    if !g.nodes[l].inputs.contains_key("hours") {
        return ev.series.get(&(l, "mid".to_string())).and_then(|v| v.get(at.bar).copied()).filter(|v| v.is_finite());
    }
    let close = ev.series.get(&(l, "close".to_string()))?;
    let n = ((ladder_value(g, ev, l, "hours", at) * BARS_PER_HOUR).round() as usize).max(1);
    if at.bar + 1 < n {
        return None;
    }
    let mut w: Vec<f64> = close[at.bar + 1 - n..=at.bar].iter().copied().filter(|v| v.is_finite()).collect();
    (w.len() == n).then(|| crate::data::median(&mut w))
}

/// The ladder's settings at `bar` (ladder.rs terms): each from its input when connected, else the
/// block's own. None: the graph has no ladder.
pub fn ladder_at(g: &Graph, ev: &Eval, bar: usize, tick: i64, range: Option<(i64, i64)>) -> Option<crate::ladder::Params> {
    let l = g.sink("ladder")?;
    let at = At { bar, tick, range, wanted: None };
    let v = |name: &str| ladder_value(g, ev, l, name, &at);
    Some(crate::ladder::Params {
        lookback: (v("hours") * 3600.0).round() as i64,
        span: v("span").round().max(0.0) as i64,
        every: (v("every_hours") * BARS_PER_HOUR).round().max(1.0) as i64,
        turnover: v("turnover"),
        guard: Some(v("guard")).filter(|g| *g > 0.0),
        width: v("width").round().max(1.0) as i64,
    })
}

/// A hedge block's settings (the ratio is its default; `hedge_ratio` gives the one at a bar).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HedgeCfg {
    pub ratio: f64,
    /// the drift from the target that trades, as a fraction of the target
    pub band: f64,
    pub min_usd: f64,
    /// effective leverage: margin = notional / leverage
    pub leverage: f64,
    /// hours over which the exposure is averaged before it is hedged (0: as it is)
    pub smooth_h: f64,
    /// a position of its own, as a share of the strategy's value: + long, − short
    pub size: f64,
    /// whether a signal sets the size (its `size` input is connected)
    pub sized: bool,
}

/// The hedge at `bar`: (the share of the pool's exposure to short, the position of its own as a
/// share of the strategy's value, + long). Both 0 while the hedge's `when` input is connected and
/// false; else each its input when connected (ratio clamped to 0–1.5, size to ±2; NaN while it warms
/// up: the block's own), else the block's. None: the graph has no hedge.
pub fn hedge_ratio(g: &Graph, ev: &Eval, bar: usize, tick: i64, range: Option<(i64, i64)>) -> Option<(f64, f64)> {
    let h = g.sink("hedge")?;
    let at = At { bar, tick, range, wanted: None };
    if let Some(w) = input(g, ev, h, "when", &at) {
        if !truth(w) {
            return Some((0.0, 0.0));
        }
    }
    let ratio = match input(g, ev, h, "ratio", &at) {
        Some(r) if r.is_finite() => r.clamp(0.0, 1.5),
        _ => g.param(h, "ratio"),
    };
    let size = match input(g, ev, h, "size", &at) {
        Some(r) if r.is_finite() => r.clamp(-2.0, 2.0),
        _ => g.param(h, "size"),
    };
    Some((ratio, size))
}

/// The token an exit at `bar` waits in: the exit sink's `to` input (+1 token0, -1 token1) when
/// connected and decided, else the strategy's exit_to (which may be the dollar stable).
pub fn exit_to(g: &Graph, ev: &Eval, bar: usize, tick: i64, range: Option<(i64, i64)>) -> ExitTo {
    let at = At { bar, tick, range, wanted: None };
    match g.sink("exit").and_then(|x| input(g, ev, x, "to", &at)) {
        Some(v) if v > 0.5 => ExitTo::Token0,
        Some(v) if v < -0.5 => ExitTo::Token1,
        _ => g.exit_to,
    }
}

/// How an exit's token is kept in the state: 1 token0, -1 token1, 2 the dollar stable, 0 as it was.
pub fn side_code(to: ExitTo) -> i8 {
    match to {
        ExitTo::Token0 => 1,
        ExitTo::Token1 => -1,
        ExitTo::Stable => 2,
        ExitTo::Keep => 0,
    }
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
    if let Some(l) = g.sink("ladder") {
        let guard = ladder_value(g, ev, l, "guard", &at);
        let dev = ev.series.get(&(l, "guard_dev".to_string())).and_then(|v| v.get(bar).copied()).unwrap_or(f64::NAN);
        // NaN (too little history) never freezes, as in ladder.rs
        if guard > 0.0 && dev > guard {
            return Decision::Wait(format!("hold: depeg guard (tick {tick} is {dev:.0} ticks from the 7-day median)"));
        }
    }
    if let Some(x) = g.sink("exit") {
        let when = input(g, ev, x, "when", &at).map(truth).unwrap_or(false);
        if when && st.exited {
            // out and waiting in one token, and the move turned: wait in the other one
            let to = exit_to(g, ev, bar, tick, range);
            if to != ExitTo::Keep && side_code(to) != st.exit_side {
                let what = match to {
                    ExitTo::Token0 => "token0",
                    ExitTo::Token1 => "token1",
                    _ => "the dollar stable",
                };
                return Decision::Exit(format!("the move turned: waiting in {what} now"));
            }
        }
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
            // when the exit was done: the first decision after it (one bar or cycle later at most)
            // set at the exit (backtest and engine); a state saved before that was recorded starts now
            let out = *st.out_since.get_or_insert(now);
            let back = input(g, ev, x, "reenter", &at).map(truth).unwrap_or(!when);
            if !back {
                st.back_since = None;
                return Decision::Keep("exited; waiting for the re-entry condition".into());
            }
            let since = *st.back_since.get_or_insert(now);
            let min_out = (g.param(x, "min_out_hours") * 3600.0) as i64;
            if now - out < min_out {
                return Decision::Keep(format!("exited; out at least another {}", mins(min_out - (now - out))));
            }
            let confirm = (g.param(x, "confirm_hours") * 3600.0) as i64;
            if now - since < confirm {
                return Decision::Keep(format!("re-entry condition true; confirming for another {}", mins(confirm - (now - since))));
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
    let ladder = g.sink("ladder");
    if let Some(l) = ladder {
        // a ladder re-solves on its schedule; the caller moves only when the solve differs enough
        let every = (ladder_value(g, ev, l, "every_hours", &at) * 3600.0) as i64;
        if now - st.last_move >= every {
            return Decision::Move { tl: wl, tu: wu, reason: format!("ladder re-solve (every {})", mins(every)) };
        }
    }
    let trigger = match reb.and_then(|r| input(g, ev, r, "when", &at)) {
        Some(v) => truth(v),
        // a ladder without a rebalance condition waits for its schedule
        None if ladder.is_some() => {
            st.outside_since = None;
            return Decision::Keep(format!("ladder: next re-solve in {}", mins((ladder_value(g, ev, ladder.unwrap(), "every_hours", &at) * 3600.0) as i64 - (now - st.last_move))));
        }
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
    if ladder.is_some() {
        return Decision::Move { tl: wl, tu: wu, reason: "rebalance condition true: re-solving the ladder".into() };
    }
    if (wl, wu) == (tl, tu) {
        return Decision::Keep("the wanted range equals the current one".into());
    }
    Decision::Move { tl: wl, tu: wu, reason: format!("rebalance condition true: moving to [{wl},{wu})") }
}

/// Chart lines: every price-like output, sampled at `bars`, plus the wanted range.
/// Which price each node's output is in, as bits: 1 the pool's own (its ticks: decimals and
/// orientation of the pool), 2 a watched token's USD price (`token_price`, ticks of the USD price),
/// 0 not a price at all (a width, a volatility, a flag). The two prices are different scales: a
/// chart can only draw a line against the one it is in, and a width not at all.
fn frames(g: &Graph) -> Vec<u8> {
    let mut f = vec![0u8; g.nodes.len()];
    for (i, n) in g.nodes.iter().enumerate() {
        let src = |port: &str| -> u8 {
            n.inputs.get(port).map(|(j, out)| if g.nodes[*j].kind == "token_price" && out == "stale" { 0 } else { f[*j] }).unwrap_or(0)
        };
        f[i] = match n.kind {
            "token_price" => 2,
            "price" | "high_low" => 1,
            // a price ± a width is a price; the width says nothing about which
            "around" => src("center"),
            // a price less a price is a distance, not a price
            "sub" if src("a") != 0 && src("b") != 0 => 0,
            k if PRICE_LIKE.contains(&k) => n.inputs.keys().map(|p| src(p)).fold(0, |a, b| a | b),
            _ => 0,
        };
    }
    f
}

pub fn overlays(g: &Graph, ev: &Eval, idx: &[usize], spacing: i64) -> Value {
    let mut out = vec![];
    let fr = frames(g);
    for (i, n) in g.nodes.iter().enumerate() {
        if g.stateful.contains(&i) || !PRICE_LIKE.contains(&n.kind) || n.kind == "price" || fr[i] == 0 {
            continue; // not a price (a width, a flag): nothing to draw against the price
        }
        for port in spec(n.kind).map(|s| s.2).unwrap_or(&[]) {
            if let Some(v) = ev.series.get(&(i, port.to_string())) {
                let name = format!("{}{}", if n.label.is_empty() { format!("{} {}", n.kind, n.id) } else { n.label.clone() }, if spec(n.kind).unwrap().2.len() > 1 { format!(" {port}") } else { String::new() });
                let frame = match fr[i] {
                    2 => "usd",
                    3 => "mixed",
                    _ => "pool",
                };
                out.push(json!({"name": name, "node": n.id, "port": port, "frame": frame, "v": idx.iter().map(|b| v.get(*b).copied().filter(|x| x.is_finite()).map(|x| (x * 10.0).round() / 10.0)).collect::<Vec<_>>()}));
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
        Market { ts, tick: ticks.to_vec(), sp: vec![1.0; n], fee, fee_sig: vec![], l_others: vec![1.0; n], l_active: vec![1.0; n], bar_end, bar_idx, bar_tick: ticks.to_vec(), bar_hi, bar_lo, bar_fee,
                 decimals: 6, px_scale: 1.0, liq_scale: 1.0, num1: true, bar_s: 300 }
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

    fn volatile_indicators(tf: i64, hours: f64) -> Value {
        json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price", "tf": tf},
            {"id": "pk", "kind": "parkinson", "tf": tf, "p": {"hours": hours}},
            {"id": "er", "kind": "efficiency_ratio", "tf": tf, "p": {"hours": hours}, "in": {"in": ["p", "tick"]}},
            {"id": "ka", "kind": "kama", "tf": tf, "p": {"hours": hours}, "in": {"in": ["p", "tick"]}},
            {"id": "lr", "kind": "linreg", "tf": tf, "p": {"hours": hours}, "in": {"in": ["p", "tick"]}},
            {"id": "dd", "kind": "drawdown", "tf": tf, "p": {"hours": hours}, "in": {"in": ["p", "tick"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["p", "tick"], "upper": ["p", "tick"]}}
        ]})
    }

    #[test]
    fn volatile_indicator_blocks_match_the_ta_math() {
        let ticks: Vec<i64> = (0..100).map(|i| 1000 + i * 2 + (i % 7) * 3).collect();
        let mut m = market(&ticks);
        m.bar_hi = ticks.iter().map(|t| t + 3).collect();
        m.bar_lo = ticks.iter().map(|t| t - 2).collect();
        let c: Vec<f64> = ticks.iter().map(|t| *t as f64).collect();
        let h: Vec<f64> = m.bar_hi.iter().map(|t| *t as f64).collect();
        let l: Vec<f64> = m.bar_lo.iter().map(|t| *t as f64).collect();
        let g = graph(volatile_indicators(5, 0.5)).unwrap();
        let ev = evaluate(&g, &m);
        let (slope, r2) = ta::linreg(&c, 6);
        let expected = [
            ("pk", "out", ta::parkinson(&h, &l, 6), 5),
            ("er", "out", ta::efficiency_ratio(&c, 6), 6),
            ("ka", "out", ta::kama(&c, 6, 2, 30), 6),
            ("lr", "slope", slope.into_iter().map(|v| v * 12.0).collect(), 5),
            ("lr", "r2", r2, 5),
            ("dd", "out", ta::drawdown(&c, 6), 5),
        ];
        for (id, port, want, first) in expected {
            let ni = g.nodes.iter().position(|n| n.id == id).unwrap();
            let got = &ev.series[&(ni, port.to_string())];
            assert!(got[..first].iter().all(|v| v.is_nan()), "{id}:{port} warms up");
            assert!(got[first].is_finite(), "{id}:{port} first value");
            for (i, (a, b)) in got.iter().zip(&want).enumerate() {
                assert!(a == b || (a.is_nan() && b.is_nan()), "{id}:{port} bar {i}: {a} vs {b}");
            }
        }
        assert!(g.lookback >= 7.0 / 12.0, "ER and KAMA need six changes, seven closes");
    }

    #[test]
    fn volatile_indicator_defaults_bounds_and_lookback_are_registered() {
        let mut v = volatile_indicators(5, 24.0);
        for n in v["nodes"].as_array_mut().unwrap() {
            n.as_object_mut().unwrap().remove("p");
        }
        let g = graph(v.clone()).unwrap();
        for kind in ["parkinson", "efficiency_ratio", "kama", "linreg", "drawdown"] {
            let n = g.nodes.iter().find(|n| n.kind == kind).unwrap();
            assert_eq!(n.p["hours"], 24.0);
        }
        let ki = v["nodes"].as_array().unwrap().iter().position(|n| n["kind"] == "kama").unwrap();
        let k = g.nodes.iter().find(|n| n.kind == "kama").unwrap();
        assert_eq!((k.p["fast_periods"], k.p["slow_periods"]), (2.0, 30.0));
        v["nodes"][ki]["p"] = json!({"fast_periods": 31, "slow_periods": 30});
        assert!(graph(v.clone()).unwrap_err().contains("fast_periods must be at most slow_periods"));
        v["nodes"][ki]["p"] = json!({"fast_periods": 0});
        assert!(graph(v).unwrap_err().contains("fast_periods must be 1 to 1000"));
        let daily = graph(volatile_indicators(1440, 0.1)).unwrap();
        let lr = daily.nodes.iter().find(|n| n.kind == "linreg").unwrap();
        assert_eq!(own_lookback(lr), 48.0 + 1.0 / BARS_PER_HOUR, "regression needs at least two candles");
        let long = graph(volatile_indicators(5, 8760.0)).unwrap();
        assert!(long.lookback > 8760.0 && long.lookback <= MAX_LOOKBACK_HOURS);
        assert!(long.replay_days() >= 366);
        assert!(graph(volatile_indicators(5, 8761.0)).is_err());
    }

    #[test]
    fn volatile_indicators_on_timeframes_do_not_see_future_candles() {
        let ticks: Vec<i64> = (0..120).collect();
        let make_market = |count: usize| {
            let mut m = market(&ticks[..count]);
            m.bar_end = (1..=count as i64).map(|k| 3600 * 1000 + k * 300).collect();
            m.bar_hi = ticks[..count].iter().map(|t| t + 3).collect();
            m.bar_lo = ticks[..count].iter().map(|t| t - 2).collect();
            m
        };
        for tf in [5, 60] {
            let g = graph(volatile_indicators(tf, 2.0)).unwrap();
            let full = evaluate(&g, &make_market(120));
            let prefix = evaluate(&g, &make_market(41)); // ends before the fourth hourly candle closes
            for (key, short) in &prefix.series {
                for (i, (a, b)) in full.series[key].iter().zip(short).enumerate() {
                    assert!(a == b || (a.is_nan() && b.is_nan()), "tf {tf}, {key:?}, bar {i}");
                }
            }
            if tf == 60 {
                let at = |id: &str, port: &str, bar: usize| {
                    full.series[&(g.nodes.iter().position(|n| n.id == id).unwrap(), port.to_string())][bar]
                };
                for (id, port) in [("pk", "out"), ("er", "out"), ("ka", "out"), ("lr", "slope"), ("lr", "r2"), ("dd", "out")] {
                    assert!(at(id, port, 22).is_nan(), "{id}:{port} needs two closed candles");
                }
                let sigma = 16.0 / (4.0 * 2.0f64.ln()).sqrt();
                assert!((at("pk", "out", 23) - sigma).abs() < 1e-12);
                assert!((at("lr", "slope", 23) - 12.0).abs() < 1e-12, "ticks per hour");
                assert!((at("lr", "r2", 23) - 1.0).abs() < 1e-12);
                assert_eq!(at("dd", "out", 23), 0.0);
                assert!(at("er", "out", 34).is_nan() && at("ka", "out", 34).is_nan());
                assert_eq!(at("er", "out", 35), 1.0);
                assert!(at("ka", "out", 35).is_finite());
                assert_eq!(at("pk", "out", 34), at("pk", "out", 23), "hold until next candle closes");
            }
        }
    }

    #[test]
    fn a_price_block_gives_the_price_itself_and_back() {
        let ticks: Vec<i64> = (0..600).map(|k| 6931 + (k % 7) * 1000).collect();
        let m = market(&ticks);
        let g = graph(json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"},
            {"id": "px", "kind": "to_price", "in": {"in": ["p", "tick"]}},
            {"id": "back", "kind": "to_tick", "in": {"in": ["px", "out"]}},
            {"id": "lad", "kind": "ladder", "p": {"hours": 1, "span": 3, "every_hours": 6, "turnover": 0.4}}]})).unwrap();
        let ev = evaluate(&g, &m);
        let id = |s: &str| g.nodes.iter().position(|n| n.id == s).unwrap();
        let px = &ev.series[&(id("px"), "out".to_string())];
        let back = &ev.series[&(id("back"), "out".to_string())];
        for b in [0usize, 1, 6, 599] {
            assert!((px[b] - 1.0001f64.powf(ticks[b] as f64)).abs() < 1e-9 * px[b], "bar {b}");
            assert!((back[b] - ticks[b] as f64).abs() < 1e-6, "bar {b}");
        }
        assert!((px[0] - 2.0).abs() < 1e-3);
        assert!(to_tick(0.0).is_nan());
    }

    #[test]
    fn a_ladders_inputs_override_its_settings_at_the_decision() {
        // 280 bars at tick 100, then 20 at 140: the 24-hour median is still 100, the last hour's 140
        let mut ticks = vec![100i64; 280];
        ticks.extend([140; 20]);
        let m = market(&ticks);
        let b = 299;
        let lad = |inputs: Value, extra: Vec<Value>| {
            let mut nodes = vec![json!({"id": "lad", "kind": "ladder", "p": {"hours": 24, "span": 3, "every_hours": 6, "turnover": 0.4}, "in": inputs})];
            nodes.extend(extra);
            graph(json!({"type": "graph", "nodes": nodes})).unwrap()
        };
        let k = |id: &str, v: f64| json!({"id": id, "kind": "const", "p": {"value": v}});
        // no inputs: the settings as they are
        let plain = lad(json!({}), vec![]);
        let ev = evaluate(&plain, &m);
        assert_eq!(wanted(&plain, &ev, b, 1), Some((97, 104)));
        let p = ladder_at(&plain, &ev, b, 140, None).unwrap();
        assert_eq!((p.lookback, p.span, p.turnover), (24 * 3600, 3, 0.4));
        // a one-hour window from an input: the median follows the price to 140
        let short = lad(json!({"hours": ["h", "value"], "span": ["s", "value"]}), vec![k("h", 1.0), k("s", 5.0)]);
        let ev = evaluate(&short, &m);
        assert_eq!(wanted(&short, &ev, b, 1), Some((135, 146)));
        let p = ladder_at(&short, &ev, b, 140, None).unwrap();
        assert_eq!((p.lookback, p.span), (3600, 5));
        // an input past the setting's limits is clamped to them: 1000 h is the 168 h maximum, which
        // this history cannot fill yet (no centre), and a turnover of 5 is all of it
        let long = lad(json!({"hours": ["h", "value"], "turnover": ["t", "value"]}), vec![k("h", 1000.0), k("t", 5.0)]);
        let ev = evaluate(&long, &m);
        assert_eq!(wanted(&long, &ev, b, 1), None);
        let p = ladder_at(&long, &ev, b, 140, None).unwrap();
        assert_eq!((p.lookback, p.turnover), (168 * 3600, 1.0));
        // the schedule from an input: half an hour, so an hour after the last move it re-solves
        let fast = lad(json!({"every_hours": ["e", "value"]}), vec![k("e", 0.5)]);
        let ev = evaluate(&fast, &m);
        let now = 10_000_000;
        let mut st = State::default();
        st.moved(now - 3600);
        assert!(matches!(decide(&fast, &ev, &mut st, now, b, 140, Some((97, 104)), 1), Decision::Move { .. }));
        let ev = evaluate(&plain, &m);
        assert!(matches!(decide(&plain, &ev, &mut st, now, b, 140, Some((97, 104)), 1), Decision::Keep(_)));
        // an input that follows the position: a low turnover while the price is out of the range
        let follow = lad(json!({"turnover": ["sw", "out"]}), vec![
            json!({"id": "o", "kind": "out_of_range"}), k("lo", 0.1), k("hi", 0.4),
            json!({"id": "sw", "kind": "switch", "in": {"cond": ["o", "out"], "a": ["lo", "value"], "b": ["hi", "value"]}})]);
        let ev = evaluate(&follow, &m);
        assert_eq!(ladder_at(&follow, &ev, b, 140, Some((97, 104))).unwrap().turnover, 0.1);
        assert_eq!(ladder_at(&follow, &ev, b, 100, Some((97, 104))).unwrap().turnover, 0.4);
        // and its own centre is still there
        assert_eq!(wanted(&follow, &ev, b, 1), Some((97, 104)));
    }

    #[test]
    fn an_exit_stays_out_its_minimum_and_re_enters_only_after_a_confirmed_signal() {
        let g = |back: f64| graph(json!({"type": "graph", "nodes": [
            {"id": "lad", "kind": "ladder", "p": {"hours": 1, "span": 3, "every_hours": 6, "turnover": 0.4}},
            {"id": "no", "kind": "const", "p": {"value": 0}}, {"id": "back", "kind": "const", "p": {"value": back}},
            {"id": "x", "kind": "exit", "p": {"min_out_hours": 48, "confirm_hours": 24}, "in": {"when": ["no", "value"], "reenter": ["back", "value"]}}]})).unwrap();
        let (yes, no) = (g(1.0), g(0.0));
        let m = market(&[100; 300]);
        let (ey, en) = (evaluate(&yes, &m), evaluate(&no, &m));
        let (b, h) = (299, 3600);
        let now = 10_000_000;
        let mut st = State::default();
        st.exited_to(ExitTo::Token1);
        let go = |st: &mut State, g: &Graph, ev: &Eval, t: i64| matches!(decide(g, ev, st, t, b, 100, None, 1), Decision::Move { .. });
        // out since now; the signal is back at once, but the minimum time out holds
        assert!(!go(&mut st, &yes, &ey, now));
        assert!(!go(&mut st, &yes, &ey, now + 47 * h));
        // the signal breaks at 30 h: its 24 h confirmation starts again from 31 h
        let mut st2 = State::default();
        st2.exited_to(ExitTo::Token1);
        assert!(!go(&mut st2, &yes, &ey, now));
        assert!(!go(&mut st2, &no, &en, now + 30 * h));
        assert!(!go(&mut st2, &yes, &ey, now + 31 * h));
        assert!(!go(&mut st2, &yes, &ey, now + 50 * h), "48 h out but only 19 h confirmed");
        assert!(go(&mut st2, &yes, &ey, now + 55 * h));
        // without the break it re-enters at 48 h
        assert!(go(&mut st, &yes, &ey, now + 48 * h));
        st.moved(now + 48 * h);
        assert_eq!((st.out_since, st.back_since), (None, None));
        // the clock starts at the exit as recorded, not at the first decision after it
        let mut st3 = State::default();
        st3.exited_to(ExitTo::Token1);
        st3.out_since = Some(now);
        assert!(!go(&mut st3, &yes, &ey, now + 10 * h));
        assert!(go(&mut st3, &yes, &ey, now + 48 * h), "48 h after the exit, 38 h confirmed");
    }

    #[test]
    fn a_ladder_block_re_solves_on_its_schedule_and_on_its_trigger() {
        let lad = |extra: Vec<Value>| -> Result<Graph, String> {
            let mut nodes = vec![json!({"id": "lad", "kind": "ladder", "p": {"hours": 1, "span": 3, "every_hours": 6, "turnover": 0.4}})];
            nodes.extend(extra);
            graph(json!({"type": "graph", "nodes": nodes}))
        };
        let g = lad(vec![]).unwrap();
        let p = g.ladder().unwrap();
        assert_eq!((p.lookback, p.span, p.every), (3600, 3, 72), "the ladder's own units: seconds, ticks, 5-minute bars");
        let m = market(&[100; 300]);
        let ev = evaluate(&g, &m);
        let b = 299;
        assert_eq!(wanted(&g, &ev, b, 1), Some((97, 104)), "the span around the median price");
        let now = 10_000_000;
        let mut st = State::default();
        assert!(matches!(decide(&g, &ev, &mut st, now, b, 100, None, 1), Decision::Move { .. }), "no position: open");
        st.moved(now);
        // a plain ladder waits for its schedule, even with the price outside its ticks
        assert!(matches!(decide(&g, &ev, &mut st, now + 3600, b, 120, Some((97, 104)), 1), Decision::Keep(_)));
        assert!(matches!(decide(&g, &ev, &mut st, now + 6 * 3600, b, 120, Some((97, 104)), 1), Decision::Move { .. }));
        // with a trigger wired to Rebalance it also re-solves when that holds
        let g2 = lad(vec![json!({"id": "oor", "kind": "out_of_range"}),
                          json!({"id": "m", "kind": "rebalance", "p": {"delay_min": 0, "cooldown_min": 0}, "in": {"when": ["oor", "out"]}})]).unwrap();
        let ev2 = evaluate(&g2, &m);
        let mut st = State::default();
        st.moved(now);
        assert!(matches!(decide(&g2, &ev2, &mut st, now + 3600, b, 120, Some((97, 104)), 1), Decision::Move { .. }));
        assert!(matches!(decide(&g2, &ev2, &mut st, now + 3600, b, 100, Some((97, 104)), 1), Decision::Keep(_)));
        // one output to place liquidity: a range or a ladder, not both
        let both = lad(vec![json!({"id": "p", "kind": "price"}), json!({"id": "r", "kind": "range", "in": {"lower": ["p", "tick"], "upper": ["p", "tick"]}})]);
        assert!(both.is_err());
    }

    #[test]
    fn overlay_lines_say_which_price_they_are_in() {
        let g = graph(json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"},
            {"id": "s", "kind": "sma", "p": {"hours": 1}, "in": {"in": ["p", "tick"]}},
            {"id": "eth", "kind": "token_price", "token": "0x4200000000000000000000000000000000000006", "chain_id": 8453},
            {"id": "ehi", "kind": "highest", "p": {"hours": 1}, "in": {"in": ["eth", "tick"]}},
            {"id": "w", "kind": "const", "p": {"value": 20}},
            {"id": "eo", "kind": "offset", "p": {"value": 5}, "in": {"in": ["ehi", "out"]}},
            {"id": "mix", "kind": "max", "in": {"a": ["s", "out"], "b": ["eth", "tick"]}},
            {"id": "ar", "kind": "around", "in": {"center": ["s", "out"], "width": ["w", "value"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["ar", "lower"], "upper": ["ar", "upper"]}},
            {"id": "m", "kind": "rebalance", "p": {"delay_min": 0, "cooldown_min": 0}}
        ]})).unwrap();
        let f = frames(&g);
        let at = |id: &str| f[g.nodes.iter().position(|n| n.id == id).unwrap()];
        assert_eq!((at("s"), at("ar")), (1, 1), "the pool's price, also through a constant width");
        assert_eq!((at("ehi"), at("eo")), (2, 2), "the watched token's USD price");
        assert_eq!(at("mix"), 3, "both: no one scale to draw it on");
        assert_eq!(at("w"), 0, "a constant width is not a price");
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
    fn the_volatile_price_rises_with_the_volatile_token_either_way_round() {
        let ticks: Vec<i64> = (0..600).map(|i| i / 3).collect();
        let g = graph(json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"}, {"id": "v", "kind": "vol_price"},
            {"id": "w", "kind": "const", "p": {"value": 10}},
            {"id": "ar", "kind": "around", "in": {"center": ["p", "tick"], "width": ["w", "value"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["ar", "lower"], "upper": ["ar", "upper"]}}]})).unwrap();
        let vi = g.nodes.iter().position(|n| n.id == "v").unwrap();
        for num1 in [true, false] {
            let mut m = market(&ticks);
            m.num1 = num1;
            let v = &evaluate(&g, &m).series[&(vi, "tick".to_string())];
            // numeraire token1: the volatile token is token0, priced by the tick; else its inverse
            assert_eq!(v[599], if num1 { 199.0 } else { -199.0 });
            assert_eq!(evaluate(&g, &m).series[&(vi, "token".to_string())][599], if num1 { 1.0 } else { -1.0 });
        }
    }

    #[test]
    fn a_latch_stays_on_from_set_to_reset() {
        // set while the price is above 30, reset once it is below -30: on through the band between
        let ticks: Vec<i64> = (0..600).map(|i| ((i as f64 / 50.0).sin() * 40.0) as i64).collect();
        let g = graph(json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"}, {"id": "hi", "kind": "const", "p": {"value": 30}}, {"id": "lo", "kind": "const", "p": {"value": -30}},
            {"id": "up", "kind": "gt", "in": {"a": ["p", "tick"], "b": ["hi", "value"]}},
            {"id": "dn", "kind": "lt", "in": {"a": ["p", "tick"], "b": ["lo", "value"]}},
            {"id": "l", "kind": "latch", "in": {"set": ["up", "out"], "reset": ["dn", "out"]}},
            {"id": "w", "kind": "const", "p": {"value": 10}},
            {"id": "ar", "kind": "around", "in": {"center": ["p", "tick"], "width": ["w", "value"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["ar", "lower"], "upper": ["ar", "upper"]}}]})).unwrap();
        let m = market(&ticks);
        let ev = evaluate(&g, &m);
        let li = g.nodes.iter().position(|n| n.id == "l").unwrap();
        let v = &ev.series[&(li, "out".to_string())];
        let mut on = false;
        for (k, t) in ticks.iter().enumerate() {
            if *t > 30 {
                on = true;
            } else if *t < -30 {
                on = false;
            }
            assert_eq!(v[k] == 1.0, on, "bar {k}, tick {t}");
        }
    }

    #[test]
    fn a_latch_forgets_a_set_older_than_the_live_window() {
        // set in the first bars only, never reset: on for the live engine's window (10 days of
        // 5-minute bars), then off, as live, rebuilding from the last 10 days, would see it
        let ticks: Vec<i64> = (0..4000).map(|i| if i < 10 { 50 } else { 0 }).collect();
        let g = graph(json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"}, {"id": "hi", "kind": "const", "p": {"value": 30}}, {"id": "lo", "kind": "const", "p": {"value": -30}},
            {"id": "up", "kind": "gt", "in": {"a": ["p", "tick"], "b": ["hi", "value"]}},
            {"id": "dn", "kind": "lt", "in": {"a": ["p", "tick"], "b": ["lo", "value"]}},
            {"id": "l", "kind": "latch", "in": {"set": ["up", "out"], "reset": ["dn", "out"]}},
            {"id": "w", "kind": "const", "p": {"value": 10}},
            {"id": "ar", "kind": "around", "in": {"center": ["p", "tick"], "width": ["w", "value"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["ar", "lower"], "upper": ["ar", "upper"]}}]})).unwrap();
        let m = market(&ticks);
        let ev = evaluate(&g, &m);
        let li = g.nodes.iter().position(|n| n.id == "l").unwrap();
        let v = &ev.series[&(li, "out".to_string())];
        let wb = (g.replay_days() * 24 * 12) as usize;
        assert_eq!(v[9], 1.0);
        assert_eq!(v[9 + wb - 1], 1.0);
        assert_eq!(v[9 + wb], 0.0);
        assert_eq!(v[3999], 0.0);
    }

    #[test]
    fn every_new_kind_parses_and_long_chains_are_refused() {
        for (kind, ins, _, _) in KINDS.iter() {
            if ["range", "ladder", "rebalance", "exit", "hold", "hedge", "token_price", "funding"].contains(kind) {
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
            // longer than this market, or needing hourly bars (or, to_tick, a positive price, not the
            // ticks it is fed here): tested on their own
            if !STATEFUL.contains(kind) && !["breakout", "fee_vs_arb", "to_tick"].contains(kind) {
                let out = ev.series.iter().filter(|((i, _), _)| *i == xi).map(|(_, v)| v).collect::<Vec<_>>();
                assert!(!out.is_empty() && out.iter().all(|v| v[2999].is_finite()), "{kind} produced no value");
            }
        }
        let long = graph(json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"}, {"id": "a", "kind": "sma", "p": {"hours": 4000}, "in": {"in": ["p", "tick"]}},
            {"id": "b", "kind": "sma", "p": {"hours": 4000}, "in": {"in": ["a", "out"]}},
            {"id": "c", "kind": "sma", "p": {"hours": 1000}, "in": {"in": ["b", "out"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["c", "out"], "upper": ["p", "tick"]}}]}));
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
    fn one_minute_strategies_decide_on_minute_bars() {
        let g = graph(json!({"nodes": [{"id": "p", "kind": "price", "tf": 1}, {"id": "q", "kind": "price"},
            {"id": "r", "kind": "range", "in": {"lower": ["q", "tick"], "upper": ["p", "tick"]}}]})).unwrap();
        assert_eq!(g.bar_s(), 60, "a 1-minute block makes the strategy decide every minute");
        let five = graph(json!({"nodes": [{"id": "q", "kind": "price", "tf": 60},
            {"id": "r", "kind": "range", "in": {"lower": ["q", "tick"], "upper": ["q", "tick"]}}]})).unwrap();
        assert_eq!(five.bar_s(), 300);
        // 1-minute bars; every fifth one ends a 5-minute candle
        let ticks: Vec<i64> = (0..20).collect();
        let mut m = market(&ticks);
        m.bar_s = 60;
        m.bar_end = (1..=20).map(|k| 300 * 1000 + k * 60).collect();
        let ev = evaluate(&g, &m);
        let at = |id: &str, j: usize| ev.series[&(g.nodes.iter().position(|x| x.id == id).unwrap(), "tick".to_string())][j];
        assert_eq!(at("p", 7), 7.0, "the 1-minute price is the bar's own");
        assert!(at("q", 3).is_nan(), "no 5-minute candle closed yet");
        assert_eq!(at("q", 4), 4.0);
        assert_eq!(at("q", 8), 4.0, "the 5-minute candle holds until the next close");
        assert_eq!(at("q", 9), 9.0);
    }

    #[test]
    fn minute_candles_come_from_the_swaps() {
        // one 5-minute bar ending at 1_000_300 with swaps inside it at 1_000_010 (tick 5), 1_000_070
        // (tick 9), 1_000_080 (tick 7) and 1_000_250 (tick 3); a second bar with none
        let mut m = market(&[0, 0]);
        m.ts = vec![1_000_010, 1_000_070, 1_000_080, 1_000_250];
        m.tick = vec![5, 9, 7, 3];
        m.fee = vec![1.0, 2.0, 4.0, 8.0];
        m.bar_end = vec![1_000_300, 1_000_600];
        m.bar_tick = vec![3, 3];
        let g = graph(json!({"nodes": [
            {"id": "p", "kind": "price", "tf": 1},
            {"id": "hl", "kind": "high_low", "tf": 1},
            {"id": "f", "kind": "fees", "tf": 1},
            {"id": "c", "kind": "change", "tf": 1, "p": {"hours": 0.1}, "in": {"in": ["p", "tick"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["p", "tick"], "upper": ["p", "tick"]}}]})).unwrap();
        let fr = minute_frame(&m);
        // minutes end at 1_000_060, 120, 180, 240, 300, then 360 … 600
        assert_eq!(fr.close[..5].to_vec(), vec![5.0, 7.0, 7.0, 7.0, 3.0]);
        assert_eq!(fr.hi[1], 9.0, "the minute's high");
        assert_eq!(fr.lo[1], 7.0);
        assert_eq!(fr.fee[..5].to_vec(), vec![1.0, 6.0, 0.0, 0.0, 8.0]);
        assert_eq!(fr.close[9], 3.0, "no swap: the close carries");
        let ev = evaluate(&g, &m);
        let at = |id: &str, port: &str, j: usize| ev.series[&(g.nodes.iter().position(|x| x.id == id).unwrap(), port.to_string())][j];
        // a decision reads the minute that ends with its bar
        assert_eq!(at("p", "tick", 0), 3.0);
        assert_eq!(at("f", "out", 0), 8.0, "fees of the bar's last minute");
        assert_eq!(at("hl", "high", 0), 3.0);
        // 0.1 hours = 6 one-minute candles back: from minute 3 (tick 7) to minute 9 (tick 3)
        assert_eq!(at("c", "out", 1), -4.0);
    }

    #[test]
    fn timeframes_use_closed_candles_only() {
        // 5-minute bars ending on the hour every 12 bars; the price is the bar's index
        let n = 12 * 10;
        let ticks: Vec<i64> = (0..n as i64).collect();
        let mut m = market(&ticks);
        m.bar_end = (1..=n as i64).map(|k| 3600 * 1000 + k * 300).collect();
        m.bar_hi = ticks.iter().map(|t| t + 3).collect();
        let g = graph(json!({"nodes": [
            {"id": "p", "kind": "price", "tf": 60},
            {"id": "s", "kind": "sma", "tf": 60, "p": {"hours": 2}, "in": {"in": ["p", "tick"]}},
            {"id": "hl", "kind": "high_low", "tf": 60},
            {"id": "r", "kind": "range", "in": {"lower": ["s", "out"], "upper": ["p", "tick"]}}]})).unwrap();
        let ev = evaluate(&g, &m);
        let at = |id: &str, port: &str, j: usize| ev.series[&(g.nodes.iter().position(|x| x.id == id).unwrap(), port.to_string())][j];
        // the first hourly candle closes at bar 11 (bar_end on the hour)
        assert!(at("p", "tick", 10).is_nan(), "no candle has closed yet");
        assert_eq!(at("p", "tick", 11), 11.0);
        assert_eq!(at("p", "tick", 22), 11.0, "the next candle is still forming");
        assert_eq!(at("p", "tick", 23), 23.0);
        assert!(at("s", "out", 23).is_finite() && (at("s", "out", 23) - 17.0).abs() < 1e-9, "average of the two hourly closes 11 and 23");
        assert_eq!(at("hl", "high", 23), 26.0, "the candle's high over its twelve bars");
        assert_eq!(at("hl", "low", 23), 12.0);
        // a minute value never changes what an earlier bar saw
        let mut m2 = market(&ticks[..60]);
        m2.bar_end = m.bar_end[..60].to_vec();
        m2.bar_hi = m.bar_hi[..60].to_vec();
        let ev2 = evaluate(&g, &m2);
        let si = g.nodes.iter().position(|x| x.id == "s").unwrap();
        for j in 0..60 {
            let (a, b) = (ev.series[&(si, "out".to_string())][j], ev2.series[&(si, "out".to_string())][j]);
            assert!(a == b || (a.is_nan() && b.is_nan()), "bar {j}");
        }
        assert!(graph(json!({"nodes": [{"id": "p", "kind": "price", "tf": 7},
            {"id": "r", "kind": "range", "in": {"lower": ["p", "tick"], "upper": ["p", "tick"]}}]})).unwrap_err().contains("timeframe"));
        assert!(graph(json!({"nodes": [{"id": "p", "kind": "price"}, {"id": "o", "kind": "out_of_range", "tf": 60},
            {"id": "r", "kind": "range", "in": {"lower": ["p", "tick"], "upper": ["p", "tick"]}}]})).is_err());
    }

    #[test]
    fn breakout_side_and_hot_window() {
        // flat, a rise to a new high, a pause, then a fall through the low
        let mut c = vec![0.0; 30];
        c.extend((1..=10).map(|k| k as f64 * 5.0));
        c.extend(vec![50.0; 20]);
        c.extend((1..=20).map(|k| 50.0 - k as f64 * 6.0));
        let (side, hot) = breakout(&c, 10, 5);
        assert!(side[5].is_nan(), "warming up");
        assert_eq!((side[25], hot[25]), (0.0, 0.0));
        assert_eq!((side[31], hot[31]), (1.0, 1.0), "a new 10-bar high");
        assert_eq!((side[39], hot[39]), (1.0, 1.0));
        assert_eq!((side[45], hot[45]), (1.0, 0.0), "no new high for 6 bars: still up, the move is over");
        let flip = (60..c.len()).find(|j| side[*j] == -1.0).unwrap();
        assert_eq!(hot[flip], 1.0, "a new low turns the side and starts a move");
    }

    #[test]
    fn fee_vs_arb_is_fees_over_hourly_lvr() {
        // 12 bars an hour; the price alternates by 10 ticks each hour, each bar pays fee 1 on liquidity 1
        let n = 12 * 48;
        let ticks: Vec<i64> = (0..n).map(|i| if (i / 12) % 2 == 0 { 0 } else { 10 }).collect();
        let mut m = market(&ticks);
        m.bar_end = (0..n as i64).map(|k| 3600 * 1000 + (k + 1) * 300).collect();
        let v = fee_vs_arb(&m, 12 * 24);
        let r = 10.0 * 1.0001f64.ln();
        let lvr_h = r * r * 1.0 / 4.0; // sqrt(P) ~ 1 near tick 0..10
        let want = (12.0 * 24.0) / (24.0 * lvr_h);
        let got = v[n - 1];
        assert!((got / want - 1.0).abs() < 0.01, "{got} vs {want}");
        assert!(v[12 * 24 - 2].is_nan());
    }

    #[test]
    fn exit_waits_in_the_side_and_switches_when_the_move_turns() {
        let v = json!({"nodes": [
            {"id": "p", "kind": "price"},
            {"id": "w", "kind": "const", "p": {"value": 10}},
            {"id": "ar", "kind": "around", "in": {"center": ["p", "tick"], "width": ["w", "value"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["ar", "lower"], "upper": ["ar", "upper"]}},
            {"id": "lim", "kind": "const", "p": {"value": 50}},
            {"id": "far", "kind": "distance_gt", "p": {"value": 50}, "in": {"a": ["p", "tick"], "b": ["w", "value"]}},
            {"id": "s", "kind": "sub", "in": {"a": ["p", "tick"], "b": ["w", "value"]}},
            {"id": "x", "kind": "exit", "in": {"when": ["far", "out"], "to": ["s", "out"]}}
        ]});
        let g = graph(v).unwrap();
        let ticks = vec![0, 100, 100, -100, -100, 0];
        let m = market(&ticks);
        let ev = evaluate(&g, &m);
        let mut st = State::default();
        assert!(matches!(decide(&g, &ev, &mut st, 0, 1, 100, Some((-5, 5)), 1), Decision::Exit(_)));
        let to = exit_to(&g, &ev, 1, 100, Some((-5, 5)));
        assert_eq!(to, ExitTo::Token0);
        st.exited_to(to);
        assert!(matches!(decide(&g, &ev, &mut st, 10, 2, 100, None, 1), Decision::Keep(_)));
        assert!(matches!(decide(&g, &ev, &mut st, 20, 3, -100, None, 1), Decision::Exit(_)), "the side turned");
        assert_eq!(exit_to(&g, &ev, 3, -100, None), ExitTo::Token1);
        st.exited_to(ExitTo::Token1);
        assert!(matches!(decide(&g, &ev, &mut st, 30, 4, -100, None, 1), Decision::Keep(_)));
        assert!(matches!(decide(&g, &ev, &mut st, 40, 5, 0, None, 1), Decision::Move { .. }), "back in");
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
