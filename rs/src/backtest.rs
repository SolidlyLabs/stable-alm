//! Backtests on the shared event store (history.rs): a custom strategy, the ladder and holding,
//! replayed over the same swaps with the same decision code the live app runs.
//!
//! The replay asks the strategy at every 5-minute bar what it wants, exactly as the worker does:
//! custom mode through `custom::decide` (with its timers, cutoffs, exits and the depeg guard),
//! the ladder through `ladder::decide_at` on its re-solve schedule and turnover threshold. A move
//! burns everything, swaps toward the ratio the new ranges need (paying `swap_bps` on the swapped
//! value) and mints; every transaction pays gas. Between moves the ranges are fixed and earn, per
//! swap, `fee × ours / active` with ours = our liquidity at the swap's tick (averaged over the ticks
//! before and after it) and active = the pool's liquidity that earned the fee as it happened: the
//! fee per unit of liquidity the pool paid, adjusted for how much flow follows depth: the flow
//! grows by ((others + ours) / active)^beta and our share of it is ours / (others + ours), others
//! being the pool without the wallet's own positions. beta is measured on the pool's last week
//! (elasticity.rs): at 1 the fee APR does not depend on the capital, at 0 volume is fixed and a
//! larger position dilutes itself. The flow never exceeds MAX_FLOW times the swap's real flow. Emissions are a fixed budget per pool and accrue by time in
//! ours / (others + ours): they do dilute. Fees and emissions are paid out (not compounded) and
//! reduced by the haircuts.
//!
//! Every figure is in the numeraire token. The result splits the change against holding:
//!   value − hodl = fees + emissions − swap costs − gas + IL,
//! where hodl holds the token amounts of the first deposit (before its swap cost) and IL is the
//! rest: what the position's inventory lost (or gained) against those amounts as the price moved.

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::custom::{self, Decision, ExitTo, Rules};
use crate::data::{self, Market};
use crate::ladder::{self, Params};

const MAX_DAYS: i64 = 365;
const WARMUP_S: i64 = 8 * 86400;
const MAX_JOBS: usize = 2;
/// The most flow a position may draw, as a multiple of the swap's real flow (deeper liquidity
/// attracts volume, but not without limit).
const MAX_FLOW: f64 = 2.0;
const MAX_EVENTS: usize = 400;

#[derive(Clone, Debug)]
pub struct Costs {
    pub swap_bps: f64,
    pub gas_usd: f64,
    pub fee_haircut: f64,
    pub emission_haircut: f64,
}

impl Costs {
    pub fn from(v: &Value) -> Costs {
        let n = |k: &str, d: f64| v[k].as_f64().filter(|x| x.is_finite() && *x >= 0.0).unwrap_or(d);
        Costs {
            swap_bps: n("swap_bps", 0.3).min(500.0),
            gas_usd: n("gas_usd", 0.02).min(100.0),
            fee_haircut: n("fee_haircut", 0.82).min(1.0),
            emission_haircut: n("emission_haircut", 0.5).min(1.0),
        }
    }
    fn json(&self) -> Value {
        json!({"swap_bps": self.swap_bps, "gas_usd": self.gas_usd, "fee_haircut": self.fee_haircut, "emission_haircut": self.emission_haircut})
    }
}

pub enum Strategy {
    Custom(Rules),
    Graph(Box<crate::graph::Graph>),
    Ladder { p: Params, max_ranges: usize },
    Hodl,
}

/// The pool, in human units: price P = token1 per token0.
pub struct Ctx<'a> {
    pub m: &'a Market,
    pub spacing: i64,
    pub num1: bool,
    /// numeraire tokens per USD (gas and emissions are priced in USD)
    pub num_per_usd: f64,
    /// emissions in USD per hour, by hour start
    pub emis_usd_h: HashMap<i64, f64>,
    pub sqrt_px: f64,
    /// how far flow follows liquidity (elasticity.rs): a position deepening the pool by r draws r^beta
    pub beta: f64,
}

impl Ctx<'_> {
    fn s(&self, i: usize) -> f64 {
        self.m.sp[i] * self.sqrt_px
    }
    fn s_tick(&self, t: i64) -> f64 {
        1.0001f64.powf(t as f64 / 2.0) * self.sqrt_px
    }
    /// Token amounts of liquidity `l` on [tl, tu) at sqrt price s.
    fn amounts(&self, l: f64, s: f64, tl: i64, tu: i64) -> (f64, f64) {
        let (a, b) = (self.s_tick(tl), self.s_tick(tu));
        if s <= a {
            (l * (b - a) / (a * b), 0.0)
        } else if s >= b {
            (0.0, l * (b - a))
        } else {
            (l * (b - s) / (s * b), l * (s - a))
        }
    }
    /// Value in the numeraire of token amounts at sqrt price s.
    fn value(&self, x: f64, y: f64, s: f64) -> f64 {
        let p = s * s;
        if self.num1 {
            x * p + y
        } else {
            x + y / p
        }
    }
}

#[derive(Default)]
struct Sim {
    ranges: Vec<(i64, i64, f64)>,
    x: f64,
    y: f64,
    cash: f64,
    fees: f64,
    emissions: f64,
    /// fees the flow cap kept back (a position too large for the pool's flow)
    flow_capped: f64,
    swap_costs: f64,
    gas: f64,
    actions: usize,
    t_in: f64,
    t_tot: f64,
    hodl: Option<(f64, f64)>,
    events: Vec<Value>,
    segments: Vec<Value>,
    seg_start: i64,
}

impl Sim {
    fn liq_at(&self, t: i64) -> f64 {
        self.ranges.iter().filter(|(a, b, l)| *a <= t && t < *b && *l > 0.0).map(|r| r.2).sum()
    }

    fn inventory(&self, c: &Ctx, s: f64) -> (f64, f64) {
        let (mut x, mut y) = (self.x, self.y);
        for (a, b, l) in &self.ranges {
            let (dx, dy) = c.amounts(*l, s, *a, *b);
            x += dx;
            y += dy;
        }
        (x, y)
    }

    /// What the ranges earned from swap i0 (exclusive) to i1 and from time t0 to t1.
    fn accrue(&mut self, c: &Ctx, cost: &Costs, i0: usize, i1: usize, t0: i64, t1: i64) {
        if self.ranges.is_empty() || t1 <= t0 {
            self.t_tot += (t1 - t0).max(0) as f64;
            return;
        }
        let m = c.m;
        let mut fees = 0.0;
        for k in (i0 + 1)..=i1 {
            let ours = 0.5 * (self.liq_at(m.tick[k]) + self.liq_at(m.tick[k - 1]));
            if ours > 0.0 {
                let pool = m.l_others[k] + ours;
                // the flow this much liquidity draws, as a multiple of what the swap really was
                let flow = if m.l_active[k] > 0.0 { (pool / m.l_active[k]).powf(c.beta) } else { 1.0 };
                if flow > MAX_FLOW {
                    self.flow_capped += m.fee[k] * (flow - MAX_FLOW) * ours / pool;
                }
                fees += m.fee[k] * flow.min(MAX_FLOW) * ours / pool;
            }
        }
        let mut emis = 0.0;
        for k in i0..=i1 {
            let next = if k < i1 { m.ts[k + 1] } else { t1 };
            let dt = (next.min(t1) - m.ts[k].max(t0)).max(0) as f64;
            if dt == 0.0 {
                continue;
            }
            self.t_tot += dt;
            let held = self.liq_at(m.tick[k]);
            if held > 0.0 {
                self.t_in += dt;
                let rate = c.emis_usd_h.get(&(m.ts[k].max(t0).div_euclid(3600) * 3600)).copied().unwrap_or(0.0) / 3600.0;
                emis += rate * c.num_per_usd * dt * held / (m.l_others[k] + held);
            }
        }
        self.fees += fees * cost.fee_haircut;
        self.emissions += emis * cost.emission_haircut;
        self.cash += fees * cost.fee_haircut + emis * cost.emission_haircut;
    }

    fn close_segment(&mut self, t: i64) {
        if let Some((a, b)) = self.span() {
            self.segments.push(json!([self.seg_start, t, a, b]));
        }
    }

    fn span(&self) -> Option<(i64, i64)> {
        (!self.ranges.is_empty()).then(|| (self.ranges.iter().map(|r| r.0).min().unwrap(), self.ranges.iter().map(|r| r.1).max().unwrap()))
    }

    fn pay_gas(&mut self, c: &Ctx, cost: &Costs, n_tx: usize) {
        let g = n_tx as f64 * cost.gas_usd * c.num_per_usd;
        self.gas += g;
        self.cash -= g;
    }

    /// Burn everything, swap to the ratio `target` (ranges with value weights) needs, mint it.
    fn rebalance(&mut self, c: &Ctx, cost: &Costs, i: usize, t: i64, target: &[(i64, i64, f64)], why: &str) {
        let s = c.s(i);
        let p = s * s;
        let (x, y) = self.inventory(c, s);
        let mut n_tx = if self.ranges.is_empty() { 0 } else { 1 } + target.len();
        let wsum: f64 = target.iter().map(|r| r.2).sum::<f64>().max(1e-300);
        // value in token1 of everything, and the share token0 must hold
        let v1 = x * p + y;
        let unit: Vec<(f64, f64)> = target.iter().map(|(a, b, _)| c.amounts(1.0, s, *a, *b)).collect();
        let f0: f64 = target
            .iter()
            .zip(&unit)
            .map(|((_, _, w), (ux, uy))| w / wsum * if ux * p + uy > 0.0 { ux * p / (ux * p + uy) } else { 0.0 })
            .sum();
        if self.hodl.is_none() {
            self.hodl = Some((f0 * v1 / p, (1.0 - f0) * v1));
        }
        let swapped1 = (f0 * v1 / p - x).abs() * p;
        let mut v1n = v1;
        if swapped1 > v1 * 1e-9 {
            let c1 = swapped1 * cost.swap_bps / 1e4;
            v1n -= c1;
            self.swap_costs += if c.num1 { c1 } else { c1 / p };
            n_tx += 1;
        }
        self.close_segment(t);
        self.ranges = target
            .iter()
            .zip(&unit)
            .map(|((a, b, w), (ux, uy))| (*a, *b, w / wsum * v1n / (ux * p + uy).max(1e-300)))
            .collect();
        self.x = 0.0;
        self.y = 0.0;
        self.seg_start = t;
        self.pay_gas(c, cost, n_tx);
        self.actions += 1;
        if self.events.len() < MAX_EVENTS {
            let (a, b) = self.span().unwrap_or((0, 0));
            self.events.push(json!({"ts": t, "kind": "move", "tl": a, "tu": b, "ranges": target.len(), "reason": why}));
        }
    }

    fn exit(&mut self, c: &Ctx, cost: &Costs, i: usize, t: i64, to: ExitTo, why: &str) {
        let s = c.s(i);
        let p = s * s;
        let (mut x, mut y) = self.inventory(c, s);
        let mut n_tx = if self.ranges.is_empty() { 0 } else { 1 };
        let swapped1 = match to {
            ExitTo::Token0 => y,
            ExitTo::Token1 => x * p,
            ExitTo::Keep => 0.0,
        };
        if swapped1 > 0.0 {
            let c1 = swapped1 * cost.swap_bps / 1e4;
            self.swap_costs += if c.num1 { c1 } else { c1 / p };
            n_tx += 1;
            let v1 = x * p + y - c1;
            (x, y) = if matches!(to, ExitTo::Token0) { (v1 / p, 0.0) } else { (0.0, v1) };
        }
        self.close_segment(t);
        self.ranges.clear();
        self.x = x;
        self.y = y;
        self.pay_gas(c, cost, n_tx);
        self.actions += 1;
        if self.events.len() < MAX_EVENTS {
            self.events.push(json!({"ts": t, "kind": "exit", "reason": why}));
        }
    }
}

/// Replay one strategy over bars [b0, b1] with `capital` (numeraire units, all in the numeraire).
pub fn run(c: &Ctx, strat: &Strategy, cost: &Costs, capital: f64, b0: usize, b1: usize) -> Value {
    let m = c.m;
    let mut sim = Sim::default();
    let i_start = m.bar_idx[b0];
    if c.num1 {
        sim.y = capital;
    } else {
        sim.x = capital;
    }
    let med = data::rolling_median_trailing(&m.bar_tick, data::WEEK_BARS, 12);
    let guard_dev = |b: usize| (m.bar_tick[b] as f64 - med[b]).abs();
    let mut cst = custom::State::default();
    let mut last_solve: Option<usize> = None;
    let (mut last_i, mut last_t) = (i_start, m.bar_end[b0]);
    let mut series = vec![];
    let mut next_mark = m.bar_end[b0];
    let label;
    match strat {
        Strategy::Hodl => {
            // hold the first deposit's mix: what the ladder and custom mode are measured against
            label = "hold".to_string();
        }
        Strategy::Custom(r) => label = r.label(),
        Strategy::Graph(g) => label = g.label(),
        Strategy::Ladder { p, .. } => label = p.label(),
    }
    let mut guard_frozen = 0usize;
    let gev = match strat {
        Strategy::Graph(g) => Some(crate::graph::evaluate(g, m)),
        _ => None,
    };
    for b in b0..=b1 {
        let i = m.bar_idx[b];
        let t = m.bar_end[b];
        sim.accrue(c, cost, last_i, i, last_t, t);
        (last_i, last_t) = (i, t);
        let tick = m.tick[i];
        let s = c.s(i);
        match strat {
            Strategy::Hodl => {
                if sim.hodl.is_none() {
                    // the mix a range of ±10% around the price would take: half and half by value
                    let p = s * s;
                    let v1 = if c.num1 { capital } else { capital * p };
                    sim.hodl = Some((0.5 * v1 / p, 0.5 * v1));
                    (sim.x, sim.y) = sim.hodl.unwrap();
                }
            }
            Strategy::Custom(r) => {
                if r.guard.map(|g| guard_dev(b) > g).unwrap_or(false) {
                    guard_frozen += 1;
                } else {
                    let range = sim.span();
                    match custom::decide(r, &mut cst, t, tick, range, c.spacing) {
                        Decision::Move { tl, tu, reason } => {
                            sim.rebalance(c, cost, i, t, &[(tl, tu, 1.0)], &reason);
                            cst.moved(t);
                        }
                        Decision::Exit(reason) => {
                            sim.exit(c, cost, i, t, r.exit_to, &reason);
                            cst.exited();
                        }
                        Decision::Wait(_) | Decision::Keep(_) => {}
                    }
                }
            }
            Strategy::Graph(g) => {
                let range = sim.span();
                match crate::graph::decide(g, gev.as_ref().unwrap(), &mut cst, t, b, tick, range, c.spacing) {
                    Decision::Move { tl, tu, reason } => {
                        sim.rebalance(c, cost, i, t, &[(tl, tu, 1.0)], &reason);
                        cst.moved(t);
                    }
                    Decision::Exit(reason) => {
                        sim.exit(c, cost, i, t, g.exit_to, &reason);
                        cst.exited();
                    }
                    Decision::Wait(r) => {
                        if r.starts_with("hold") {
                            guard_frozen += 1;
                        }
                    }
                    Decision::Keep(_) => {}
                }
            }
            Strategy::Ladder { p, max_ranges } => {
                let due = sim.ranges.is_empty() || last_solve.map(|lb| b - lb >= p.every as usize).unwrap_or(true);
                let frozen = p.guard.map(|g| guard_dev(b) > g).unwrap_or(false);
                if frozen {
                    guard_frozen += 1;
                } else if due {
                    let (x, y) = sim.inventory(c, s);
                    let budget0 = x + y / (s * s);
                    let target = crate::engine::cap_ranges(
                        crate::engine::snap_ranges(ladder::decide_at(m, p, budget0, m.sp[i], tick, i), c.spacing),
                        *max_ranges,
                    );
                    if sim.ranges.is_empty() || turnover(c, &sim, &target, s) >= p.turnover {
                        sim.rebalance(c, cost, i, t, &target, "ladder re-solve");
                    }
                    last_solve = Some(b);
                }
            }
        }
        if t >= next_mark || b == b1 {
            let (x, y) = sim.inventory(c, s);
            let hodl = sim.hodl.map(|(hx, hy)| c.value(hx, hy, s)).unwrap_or(capital);
            series.push(json!([t, round6(c.value(x, y, s) + sim.cash), round6(hodl)]));
            next_mark = (t.div_euclid(3600) + 1) * 3600;
        }
    }
    let t_end = m.bar_end[b1];
    sim.close_segment(t_end);
    let s_end = c.s(m.bar_idx[b1]);
    let (x, y) = sim.inventory(c, s_end);
    let inventory = c.value(x, y, s_end);
    let value = inventory + sim.cash;
    let hodl = sim.hodl.map(|(hx, hy)| c.value(hx, hy, s_end)).unwrap_or(capital);
    let il = inventory - hodl + sim.swap_costs;
    json!({
        "label": label,
        "start_value": capital,
        "value": round6(value),
        "pnl": round6(value - capital),
        "hodl_value": round6(hodl),
        "vs_hodl": round6(value - hodl),
        "fees": round6(sim.fees),
        "emissions": round6(sim.emissions),
        "flow_capped": round6(sim.flow_capped * cost.fee_haircut),
        "swap_costs": round6(sim.swap_costs),
        "gas": round6(sim.gas),
        "costs": round6(sim.swap_costs + sim.gas),
        "il": round6(il),
        "actions": sim.actions,
        "time_in_range": if sim.t_tot > 0.0 { sim.t_in / sim.t_tot } else { 0.0 },
        "guard_bars": guard_frozen,
        "series": series,
        "segments": sim.segments,
        "events": sim.events,
    })
}

fn round6(x: f64) -> f64 {
    if x.is_finite() {
        (x * 1e6).round() / 1e6
    } else {
        0.0
    }
}

/// Share of value that moves between the current ranges and `target` (0 = same, 1 = all).
fn turnover(c: &Ctx, sim: &Sim, target: &[(i64, i64, f64)], s: f64) -> f64 {
    let mut cur: HashMap<i64, f64> = HashMap::new();
    for (a, b, l) in &sim.ranges {
        let (x, y) = c.amounts(*l, s, *a, *b);
        let v = x + y / (s * s);
        for t in *a..*b {
            *cur.entry(t).or_default() += v / (b - a) as f64;
        }
    }
    let total: f64 = cur.values().sum::<f64>();
    let total = if total > 0.0 { total } else { 1.0 };
    let wsum: f64 = target.iter().map(|r| r.2).sum::<f64>().max(1e-300);
    let mut new: HashMap<i64, f64> = HashMap::new();
    for (a, b, w) in target {
        for t in *a..*b {
            *new.entry(t).or_default() += w / wsum / (b - a) as f64;
        }
    }
    let keys: std::collections::HashSet<i64> = cur.keys().chain(new.keys()).cloned().collect();
    keys.iter().map(|t| (new.get(t).unwrap_or(&0.0) - cur.get(t).unwrap_or(&0.0) / total).abs()).sum::<f64>() / 2.0
}

// -- jobs ----------------------------------------------------------------------------------------------

static JOBS: Mutex<Option<HashMap<u64, Value>>> = Mutex::new(None);
/// The last replay market (rebuilt only when new events were stored or the request changed), and
/// the baselines run on it: a change to the custom rules replays only the custom rules.
static LAST: Mutex<Option<(String, std::sync::Arc<Market>)>> = Mutex::new(None);
static BASELINES: Mutex<Option<Vec<(String, Value)>>> = Mutex::new(None);
const MAX_COMPARE: usize = 4;
/// The library list's backtests: at most this many strategies, run by this many threads.
const MAX_BATCH: usize = 80;
const BATCH_WORKERS: usize = 4;
/// The overfit check: the strategy and this many neighbours, over this many time slices.
const WF_VARIANTS: usize = 25;
const WF_SLICES: usize = 8;
static NEXT: AtomicUsize = AtomicUsize::new(1);
static RUNNING: AtomicUsize = AtomicUsize::new(0);

fn set_job(id: u64, v: Value) {
    let mut g = JOBS.lock().unwrap();
    let jobs = g.get_or_insert_with(HashMap::new);
    jobs.insert(id, v);
    // keep the last 20
    if jobs.len() > 20 {
        let oldest = *jobs.keys().min().unwrap();
        jobs.remove(&oldest);
    }
}

pub fn job(id: u64) -> Option<Value> {
    let mut j = JOBS.lock().unwrap().get_or_insert_with(HashMap::new).get(&id).cloned()?;
    if j["state"] == "running" && j["step"] == "loading history" {
        if let Some(p) = j["chain_id"].as_u64().and_then(crate::history::progress) {
            j["step"] = json!(format!("loading {p}"));
        }
    }
    Some(j)
}

/// Start a backtest in the background; returns its id. Request:
/// {chain_id, pool, days (1–365), capital, strategy: {type: "custom", ...rules} | {type: "ladder", ...},
///  costs: {swap_bps, gas_usd, fee_haircut, emission_haircut}, compare: true}
pub fn start(req: Value, wallet: String) -> Result<u64, String> {
    let cid = req["chain_id"].as_u64().ok_or("chain_id is required")?;
    crate::history::blockchain_name(cid).ok_or("chain not supported")?;
    let pool = req["pool"].as_str().unwrap_or("").to_lowercase();
    if !(pool.len() == 42 && pool.starts_with("0x") && pool[2..].chars().all(|c| c.is_ascii_hexdigit())) {
        return Err("pool must be a 0x address".into());
    }
    let days = req["days"].as_i64().unwrap_or(30);
    if !(1..=MAX_DAYS).contains(&days) {
        return Err(format!("days must be 1 to {MAX_DAYS}"));
    }
    if RUNNING.load(Ordering::SeqCst) >= MAX_JOBS {
        return Err(format!("{MAX_JOBS} backtests are already running; try again when one finishes"));
    }
    RUNNING.fetch_add(1, Ordering::SeqCst);
    let id = NEXT.fetch_add(1, Ordering::SeqCst) as u64;
    set_job(id, json!({"id": id, "state": "running", "step": "loading history", "chain_id": cid}));
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(id, cid, &pool, days, &req, &wallet)));
        let v = match r {
            Ok(Ok(v)) => json!({"id": id, "state": "done", "result": v}),
            Ok(Err(e)) => json!({"id": id, "state": "error", "error": e}),
            Err(_) => json!({"id": id, "state": "error", "error": "the backtest crashed"}),
        };
        set_job(id, v);
        RUNNING.fetch_sub(1, Ordering::SeqCst);
    });
    Ok(id)
}

fn step(id: u64, s: &str) {
    set_job(id, json!({"id": id, "state": "running", "step": s}));
}

fn execute(id: u64, cid: u64, pool: &str, days: i64, req: &Value, wallet: &str) -> Result<Value, String> {
    let now = crate::engine::now() as i64;
    let from = now - days * 86400;
    let t0 = std::time::Instant::now();
    // topped up at most every two minutes: rule edits in a row replay the same events
    static ENSURED: Mutex<Option<HashMap<String, (std::time::Instant, i64)>>> = Mutex::new(None);
    let ekey = format!("{cid}:{pool}");
    let recent = ENSURED.lock().unwrap().get_or_insert_with(HashMap::new).get(&ekey)
        .map(|(t, f)| t.elapsed().as_secs() < 120 && *f <= from - WARMUP_S).unwrap_or(false);
    // the fee history downloads while the events do
    let apr_job = {
        let pool = pool.to_string();
        std::thread::spawn(move || apr_rows(cid, &pool, from - WARMUP_S - 3600))
    };
    let rep = if recent {
        crate::history::Report::default()
    } else {
        let r = crate::history::ensure(cid, &[pool.to_string()], from - WARMUP_S)?;
        ENSURED.lock().unwrap().as_mut().unwrap().insert(ekey, (std::time::Instant::now(), from - WARMUP_S));
        r
    };
    step(id, "loading fee history");
    let (tok0, tok1, spacing) = crate::chain::pool_info(cid, pool)?;
    let (m0, m1) = (crate::prices::meta(cid, &tok0)?, crate::prices::meta(cid, &tok1)?);
    let num1 = req["numeraire"].as_i64().map(|n| n == 1).unwrap_or_else(|| {
        ["WETH", "ETH"].contains(&m1.symbol.to_uppercase().as_str()) && !["USDC", "USDT"].contains(&m0.symbol.to_uppercase().as_str())
    });
    let apr_rows = apr_job.join().map_err(|_| "the fee history download failed".to_string())??;
    let mut apr: HashMap<i64, (f64, f64)> = HashMap::new();
    for r in &apr_rows {
        let n = |v: &Value| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0);
        let ts = n(&r["timestamp"]) as i64;
        let vol = n(&r["volumeUSD"]);
        let rate = if vol > 0.0 { (n(&r["feesUSD"]) / vol).clamp(0.0, 0.01) } else { 0.0 };
        apr.entry(ts.div_euclid(3600) * 3600).or_insert((if rate.is_nan() { 0.0 } else { rate }, n(&r["emissionsUSD"])));
    }
    step(id, "replaying");
    let cov_to = crate::history::coverage(cid, pool).map(|c| c.to_block).unwrap_or(0);
    // `from` moves with the clock: bucketed to the hour, so a minute later still reuses the market
    let mkey = format!("{cid}:{pool}:{days}:{}:{num1}:{wallet}:{cov_to}:{}", from.div_euclid(3600), apr_rows.len());
    // one replay is built at a time: a second job for the same period waits and reuses it (a year of a
    // busy pool takes about 2 GB while it is built)
    static BUILD: Mutex<()> = Mutex::new(());
    let _build = BUILD.lock().unwrap_or_else(|e| e.into_inner());
    let cached = LAST.lock().unwrap().as_ref().filter(|(k, _)| *k == mkey).map(|(_, m)| m.clone());
    let m = match cached {
        Some(m) => m,
        None => {
            let events = crate::history::read(cid, pool, from.div_euclid(3600) * 3600 - WARMUP_S, now + 3600);
            let m = std::sync::Arc::new(data::build_market(events, &wallet.to_lowercase(), &apr, m0.decimals as i32, m1.decimals as i32, if num1 { 1 } else { 0 })?);
            *LAST.lock().unwrap() = Some((mkey.clone(), m.clone()));
            m
        }
    };
    drop(_build);
    let m: &Market = &m;
    let from = from.div_euclid(3600) * 3600;
    let b0 = m.bar_end.partition_point(|t| *t < from);
    if m.bar_end.is_empty() || b0 >= m.bar_end.len() {
        return Err("not enough swaps in the period".into());
    }
    let b1 = m.bar_end.len() - 1;
    let num_tok = if num1 { &tok1 } else { &tok0 };
    let usd = crate::prices::usd(cid, num_tok).unwrap_or(if num1 { 0.0 } else { 1.0 });
    let num_per_usd = if usd > 0.0 { 1.0 / usd } else { 0.0 };
    // how much the pool's flow follows its liquidity, from its last week (or as the request says)
    let mut elasticity = crate::elasticity::estimate(m, m.bar_end[b1], 7);
    if let Some(b) = req["flow_beta"].as_f64().filter(|b| (0.0..=1.0).contains(b)) {
        elasticity["beta"] = json!(b);
        elasticity["method"] = json!("set");
    }
    let beta = elasticity["beta"].as_f64().unwrap_or(0.5);
    let c = Ctx {
        m,
        spacing,
        num1,
        num_per_usd,
        emis_usd_h: apr.iter().map(|(h, (_, e))| (*h, *e)).collect(),
        sqrt_px: m.px_scale.sqrt(),
        beta,
    };
    let cost = Costs::from(&req["costs"]);
    // capital: in USD (capital_usd, what the lab sends: you bring USDC) or in the numeraire. USDC
    // becomes the numeraire at today's price; where the numeraire is not a dollar the conversion is
    // a swap and pays swap_bps (entry_cost), then the replay's own first swap splits it into the pair.
    let stable_num = ["USDC", "USDT", "USDC.E", "USDBC", "DAI"].contains(&(if num1 { &m1.symbol } else { &m0.symbol }).to_uppercase().as_str());
    let capital_usd = req["capital_usd"].as_f64().filter(|x| *x > 0.0 && x.is_finite());
    let (capital, entry_cost) = match capital_usd {
        Some(u) if num_per_usd > 0.0 => {
            let gross = u * num_per_usd;
            let fee = if stable_num { 0.0 } else { gross * cost.swap_bps / 10_000.0 };
            (gross - fee, fee)
        }
        Some(_) => return Err("no USD price for the pool's tokens: give the capital in the numeraire".into()),
        None => (req["capital"].as_f64().filter(|x| *x > 0.0 && x.is_finite()).unwrap_or(10_000.0), 0.0),
    };
    let pegged = crate::market_data::is_pegged(&m0.symbol, &m1.symbol, None);
    let strat_v = req["strategy"].as_object().cloned().unwrap_or_default();
    let max_ranges = req["max_ranges"].as_u64().unwrap_or(5) as usize;
    // what the replay really covers (a pool younger than the period starts at its first swap)
    let span_days = ((m.bar_end[b1] - m.bar_end[b0]) as f64 / 86400.0).max(1.0 / 24.0);
    if let Some(items) = req["batch"].as_array() {
        // the library list: every strategy on the same replay, BATCH_WORKERS at a time
        let items: Vec<Value> = items.iter().take(MAX_BATCH).cloned().collect();
        let done = AtomicUsize::new(0);
        let next = AtomicUsize::new(0);
        let results: Mutex<Vec<Option<Value>>> = Mutex::new(vec![None; items.len()]);
        std::thread::scope(|sc| {
            for _ in 0..BATCH_WORKERS.min(items.len().max(1)) {
                sc.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    if k >= items.len() {
                        break;
                    }
                    let it = &items[k];
                    let sv = it["strategy"].as_object().cloned().unwrap_or_default();
                    let key = format!("{mkey}:{capital}:{beta}:{}:{}", cost.json(), Value::Object(sv.clone()));
                    let cached = BASELINES.lock().unwrap().get_or_insert_with(Vec::new).iter().find(|(k2, _)| *k2 == key).map(|(_, v)| v.clone());
                    let r = match cached {
                        Some(v) => Ok(v),
                        None => strategy(&sv, spacing, max_ranges, pegged).map(|st| {
                            let v = run(&c, &st, &cost, capital, b0, b1);
                            let mut g = BASELINES.lock().unwrap();
                            let list = g.get_or_insert_with(Vec::new);
                            list.push((key, v.clone()));
                            if list.len() > 80 {
                                list.remove(0);
                            }
                            v
                        }),
                    };
                    let row = match r {
                        Ok(v) => {
                            let pnl = v["pnl"].as_f64().unwrap_or(0.0);
                            json!({"id": it["id"], "pnl": pnl, "apr": pnl / capital * 365.0 / span_days * 100.0, "vs_hodl": v["vs_hodl"],
                                   "fees": v["fees"], "actions": v["actions"], "time_in_range": v["time_in_range"]})
                        }
                        Err(e) => json!({"id": it["id"], "error": e}),
                    };
                    results.lock().unwrap()[k] = Some(row);
                    let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                    set_job(id, json!({"id": id, "state": "running", "step": format!("backtested {d} of {}", items.len()), "partial": results.lock().unwrap().iter().flatten().cloned().collect::<Vec<_>>()}));
                });
            }
        });
        let rows: Vec<Value> = results.into_inner().unwrap().into_iter().flatten().collect();
        return Ok(json!({"chain_id": cid, "pool": pool, "days": days, "span_days": span_days, "from": m.bar_end[b0], "to": m.bar_end[b1], "capital": capital,
                         "numeraire_symbol": if num1 { &m1.symbol } else { &m0.symbol }, "swaps": m.ts.len(), "history_requests": rep.requests,
                         "elapsed_ms": t0.elapsed().as_millis() as u64, "elasticity": elasticity, "batch": rows}));
    }
    if req["walkforward"].as_bool().unwrap_or(false) {
        // the overfit check: the strategy and its neighbours (every setting 1/2 to 2x) on the same
        // replay, BATCH_WORKERS at a time, then walk-forward and PBO over time slices (overfit.rs)
        let base = if strat_v.get("type").and_then(|t| t.as_str()) == Some("ladder") { ladder_params(&strat_v, spacing, pegged) } else { strat_v.clone() };
        let vs = crate::overfit::variants(&base, WF_VARIANTS);
        let (f0, f1) = (m.bar_end[b0], m.bar_end[b1]);
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let rows: Mutex<Vec<Option<Result<Vec<f64>, String>>>> = Mutex::new(vec![None; vs.len()]);
        std::thread::scope(|sc| {
            for _ in 0..BATCH_WORKERS.min(vs.len()) {
                sc.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    if k >= vs.len() {
                        break;
                    }
                    let r = strategy(&vs[k], spacing, max_ranges, pegged).map(|st| {
                        let v = run(&c, &st, &cost, capital, b0, b1);
                        crate::overfit::slice_excess(v["series"].as_array().map(|a| a.as_slice()).unwrap_or(&[]), f0, f1, WF_SLICES, capital)
                    });
                    rows.lock().unwrap()[k] = Some(r);
                    let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                    step(id, &format!("overfit check: {d} of {} runs", vs.len()));
                });
            }
        });
        let rows = rows.into_inner().unwrap();
        let mut matrix = vec![];
        for (k, r) in rows.into_iter().enumerate() {
            match r {
                Some(Ok(x)) => matrix.push(x),
                Some(Err(e)) if k == 0 => return Err(e),
                _ => {} // a neighbour the checks refuse (a chain too long, say) is left out
            }
        }
        let mut st = crate::overfit::stats(&matrix);
        if let Some(o) = st.as_object_mut() {
            for (k, v) in [("chain_id", json!(cid)), ("pool", json!(pool)), ("days", json!(days)), ("span_days", json!(span_days)), ("from", json!(f0)), ("to", json!(f1)),
                           ("capital", json!(capital)), ("elapsed_ms", json!(t0.elapsed().as_millis() as u64))] {
                o.insert(k.into(), v);
            }
        }
        return Ok(json!({"walkforward": st}));
    }
    let main = strategy(&strat_v, spacing, max_ranges, pegged)?;
    let mut out = Map::new();
    // "main": false runs only the comparisons (one added to the list), not the strategy again
    let only_compare = req["main"].as_bool() == Some(false);
    if !only_compare {
        out.insert("main".into(), run(&c, &main, &cost, capital, b0, b1));
    }
    // what to compare with: strategies from the library (default: the ladder), and holding
    let picks: Vec<Value> = match &req["compare"] {
        Value::Array(a) => a.iter().take(MAX_COMPARE).cloned().collect(),
        Value::Bool(false) => vec![],
        _ => vec![json!({"id": "ladder", "name": "Ladder", "strategy": {"type": "ladder"}})],
    };
    let mut compare = vec![];
    for (k, pick) in picks.iter().enumerate() {
        let sv = pick["strategy"].as_object().cloned().unwrap_or_default();
        let key = format!("{mkey}:{capital}:{beta}:{}:{}", cost.json(), Value::Object(sv.clone()));
        let cached = BASELINES.lock().unwrap().get_or_insert_with(Vec::new).iter().find(|(k2, _)| *k2 == key).map(|(_, v)| v.clone());
        let mut r = match cached {
            Some(v) => v,
            None => {
                let st = strategy(&sv, spacing, max_ranges, pegged).map_err(|e| format!("comparison {}: {e}", k + 1))?;
                let v = run(&c, &st, &cost, capital, b0, b1);
                let mut g = BASELINES.lock().unwrap();
                let list = g.get_or_insert_with(Vec::new);
                list.push((key, v.clone()));
                if list.len() > 24 {
                    list.remove(0);
                }
                v
            }
        };
        r["id"] = pick["id"].clone();
        r["name"] = json!(pick["name"].as_str().unwrap_or("").chars().take(60).collect::<String>());
        compare.push(r);
    }
    out.insert("compare".into(), Value::Array(compare));
    if req["hold"].as_bool().unwrap_or(true) {
        out.insert("hold".into(), run(&c, &Strategy::Hodl, &cost, capital, b0, b1));
    }
    // hourly samples for the charts
    let mut idx = vec![];
    let mut next = 0;
    for b in b0..=b1 {
        if m.bar_end[b] >= next {
            idx.push(b);
            next = (m.bar_end[b].div_euclid(3600) + 1) * 3600;
        }
    }
    let price: Vec<Value> = idx.iter().map(|b| json!([m.bar_end[*b], m.bar_tick[*b], m.bar_hi[*b], m.bar_lo[*b]])).collect();
    let overlays = match &main {
        _ if only_compare => Value::Null,
        Strategy::Graph(g) => crate::graph::overlays(g, &crate::graph::evaluate(g, m), &idx, spacing),
        _ => Value::Null,
    };
    Ok(json!({
        "chain_id": cid, "pool": pool, "from": m.bar_end[b0], "to": m.bar_end[b1], "days": days, "span_days": span_days,
        "symbols": [m0.symbol, m1.symbol], "decimals": [m0.decimals, m1.decimals], "tick_spacing": spacing,
        "numeraire": if num1 { 1 } else { 0 }, "numeraire_symbol": if num1 { &m1.symbol } else { &m0.symbol },
        "numeraire_usd": usd, "capital": capital, "capital_usd": capital_usd, "entry_cost": entry_cost, "costs": cost.json(),
        "swaps": m.ts.len(), "history_requests": rep.requests,
        "elapsed_ms": t0.elapsed().as_millis() as u64,
        "price": price, "overlays": overlays, "runs": Value::Object(out), "elasticity": elasticity,
        "notes": [format!("Fees: the pool's fee per unit of liquidity, with flow following depth by the pool's measured elasticity (β = {beta:.2}; 1 would make the fee APR independent of capital), never more than twice a swap's real flow. Emissions are a fixed budget per pool and dilute; gas is a fixed cost per move."),
                  "Fees and emissions are paid out, not compounded, and reduced by the haircuts.",
                  "Emissions and gas use today's USD price of the numeraire."],
    }))
}

fn strategy(s: &Map<String, Value>, spacing: i64, max_ranges: usize, pegged: bool) -> Result<Strategy, String> {
    match s.get("type").and_then(|t| t.as_str()).unwrap_or("custom") {
        "custom" => Ok(Strategy::Custom(Rules::from(s, spacing)?)),
        "graph" => Ok(Strategy::Graph(Box::new(crate::graph::Graph::parse(s, spacing)?))),
        "ladder" => Ok(Strategy::Ladder { p: Params::from(&ladder_params(s, spacing, pegged))?, max_ranges: max_ranges.clamp(1, 50) }),
        t => Err(format!("unknown strategy type {t}")),
    }
}

/// A ladder's settings: the app's defaults for this pool (as adding it would set them), then what was given.
fn ladder_params(s: &Map<String, Value>, spacing: i64, pegged: bool) -> Map<String, Value> {
    let mut lp = crate::settings::pool_defaults()["strategy"].as_object().cloned().unwrap_or_default();
    if pegged {
        let span = lp.get("span").and_then(|v| v.as_i64()).unwrap_or(3).max(3 * spacing);
        lp.insert("span".into(), json!(span));
    } else {
        lp.insert("span".into(), json!(60 * spacing));
        lp.insert("guard".into(), Value::Null);
    }
    for (k, v) in s {
        lp.insert(k.clone(), v.clone());
    }
    lp.insert("type".into(), json!("ladder"));
    lp
}

/// The processor's hourly fee and emission history for the pool back to `since`: the kept copy when
/// it is under an hour old and reaches back far enough, else topped up (data::apr_history reads
/// only the missing pages).
fn apr_rows(cid: u64, pool: &str, since: i64) -> Result<Vec<Value>, String> {
    let dir = data::pool_dir(cid, pool);
    let p = dir.join("apr_hourly.json");
    let fresh = std::fs::metadata(&p).and_then(|m| m.modified()).map(|t| t.elapsed().map(|d| d.as_secs() < 3600).unwrap_or(false)).unwrap_or(false);
    let reach = std::fs::read_to_string(dir.join("apr_hourly.from")).ok().and_then(|s| s.trim().parse::<i64>().ok());
    if fresh && reach.map(|r| r <= since).unwrap_or(false) {
        if let Some(v) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok()) {
            return Ok(v);
        }
    }
    data::apr_history(cid, pool, since)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pool with equal decimals, price at tick `t(k)`, every swap paying `fee`.
    fn market(ticks: &[i64], fee: f64, others: f64) -> Market {
        let n = ticks.len();
        let ts: Vec<i64> = (0..n as i64).map(|k| 1_000_000 + k * 60).collect();
        let sp: Vec<f64> = ticks.iter().map(|t| 1.0001f64.powf(*t as f64 / 2.0)).collect();
        let bar_end: Vec<i64> = (0..).map(|i| (ts[0].div_euclid(300) + 1) * 300 + i * 300).take_while(|t| *t < ts[n - 1]).collect();
        let bar_idx: Vec<usize> = bar_end.iter().map(|b| ts.partition_point(|t| t <= b) - 1).collect();
        let bar_tick = bar_idx.iter().map(|i| ticks[*i]).collect();
        let fees = vec![fee; n];
        let (bar_hi, bar_lo, bar_fee) = data::bar_ranges(ticks, &fees, &bar_idx);
        Market { ts, tick: ticks.to_vec(), sp, fee: fees, l_others: vec![others; n], l_active: vec![others; n], bar_end, bar_idx, bar_tick, bar_hi, bar_lo, bar_fee, decimals: 6, px_scale: 1.0, liq_scale: 1e6 }
    }

    fn ctx(m: &Market) -> Ctx<'_> {
        Ctx { m, spacing: 1, num1: false, num_per_usd: 1.0, emis_usd_h: HashMap::new(), sqrt_px: 1.0, beta: 1.0 }
    }

    fn no_costs() -> Costs {
        Costs { swap_bps: 0.0, gas_usd: 0.0, fee_haircut: 1.0, emission_haircut: 1.0 }
    }

    fn custom(v: Value) -> Strategy {
        Strategy::Custom(Rules::from(v.as_object().unwrap(), 1).unwrap())
    }

    #[test]
    fn flat_price_earns_fees_and_matches_hodl() {
        let m = market(&vec![0; 2000], 1.0, 1e9);
        let c = ctx(&m);
        let r = run(&c, &custom(json!({"width": 10})), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        assert_eq!(r["actions"], 1);
        assert!(r["fees"].as_f64().unwrap() > 0.0);
        assert!(r["il"].as_f64().unwrap().abs() < 1e-6, "{}", r["il"]);
        assert!((r["value"].as_f64().unwrap() - 10_000.0 - r["fees"].as_f64().unwrap()).abs() < 1e-6);
        assert!((r["time_in_range"].as_f64().unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn the_fee_rate_does_not_depend_on_the_capital_until_the_flow_cap() {
        let m = market(&vec![0; 2000], 1.0, 1e9);
        let c = ctx(&m);
        let rate = |cap: f64| {
            let r = run(&c, &custom(json!({"width": 10})), &no_costs(), cap, 0, m.bar_end.len() - 1);
            (r["fees"].as_f64().unwrap() / cap, r["flow_capped"].as_f64().unwrap())
        };
        let (small, sc) = rate(100.0);
        let (mid, mc) = rate(10_000.0);
        assert!(sc == 0.0 && mc == 0.0);
        assert!((small - mid).abs() / mid < 1e-5, "{small} vs {mid}"); // round6 on the small one
        // a position many times the pool draws at most twice the flow: its rate falls, and says so
        let (huge, hc) = rate(1e9);
        assert!(huge < mid * 0.5 && hc > 0.0, "{huge} vs {mid}");
    }

    #[test]
    fn the_identity_holds_with_costs_and_moves() {
        let ticks: Vec<i64> = (0..3000).map(|k| ((k as f64 / 120.0).sin() * 40.0) as i64).collect();
        let m = market(&ticks, 0.5, 5e8);
        let c = ctx(&m);
        let cost = Costs { swap_bps: 5.0, gas_usd: 0.5, fee_haircut: 0.8, emission_haircut: 0.5 };
        let r = run(&c, &custom(json!({"width": 10, "cooldown_min": 30})), &cost, 10_000.0, 0, m.bar_end.len() - 1);
        assert!(r["actions"].as_u64().unwrap() > 2);
        let f = |k: &str| r[k].as_f64().unwrap();
        let lhs = f("value") - f("hodl_value");
        let rhs = f("fees") + f("emissions") - f("swap_costs") - f("gas") + f("il");
        assert!((lhs - rhs).abs() < 1e-4, "{lhs} vs {rhs}");
        assert!(f("il") < 0.0, "moving ranges in a swinging price loses against holding");
        assert!(f("gas") > 0.0 && f("swap_costs") > 0.0);
    }

    #[test]
    fn exit_to_token0_stops_earning() {
        let mut ticks = vec![0i64; 600];
        ticks.extend(vec![-200i64; 1400]);
        let m = market(&ticks, 1.0, 1e9);
        let c = ctx(&m);
        let r = run(&c, &custom(json!({"width": 10, "exit_lo": -100, "exit_to": "token0"})), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        assert_eq!(r["actions"], 2);
        assert_eq!(r["events"][1]["kind"], "exit");
        let tir = r["time_in_range"].as_f64().unwrap();
        assert!(tir > 0.2 && tir < 0.4, "{tir}");
        // all in token0 after the exit: the value no longer moves with the price
        let s = r["series"].as_array().unwrap();
        let (a, b) = (s[s.len() - 2][1].as_f64().unwrap(), s[s.len() - 1][1].as_f64().unwrap());
        assert!((a - b).abs() < 1e-6);
    }

    #[test]
    fn ladder_and_hold_run() {
        let ticks: Vec<i64> = (0..3000).map(|k| ((k as f64 / 300.0).sin() * 3.0) as i64).collect();
        let m = market(&ticks, 0.5, 5e8);
        let c = ctx(&m);
        let p = Params { lookback: 86400, span: 3, every: 72, turnover: 0.4, guard: None };
        let l = run(&c, &Strategy::Ladder { p, max_ranges: 5 }, &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        assert!(l["actions"].as_u64().unwrap() >= 1);
        assert!(l["fees"].as_f64().unwrap() > 0.0);
        let h = run(&c, &Strategy::Hodl, &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        assert_eq!(h["actions"], 0);
        assert!((h["value"].as_f64().unwrap() - h["hodl_value"].as_f64().unwrap()).abs() < 1e-6);
    }
}
