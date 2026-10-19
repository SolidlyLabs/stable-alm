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

/// Two years: the longest period the lab offers (a pool younger than that starts at its first swap).
const MAX_DAYS: i64 = 730;
/// History before the period: the longest graph lookback (graph.rs MAX_LOOKBACK_HOURS) plus a day.
const WARMUP_S: i64 = 16 * 86400;
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
    /// an exit to the dollar stable and its buy-back: a swap through the aggregator between the
    /// pair and USDC, dearer than a swap inside the pool (route fees and impact)
    pub stable_swap_bps: f64,
}

impl Costs {
    pub fn from(v: &Value) -> Costs {
        let n = |k: &str, d: f64| v[k].as_f64().filter(|x| x.is_finite() && *x >= 0.0).unwrap_or(d);
        Costs {
            swap_bps: n("swap_bps", 0.3).min(500.0),
            gas_usd: n("gas_usd", 0.02).min(100.0),
            fee_haircut: n("fee_haircut", 0.82).min(1.0),
            emission_haircut: n("emission_haircut", 0.5).min(1.0),
            stable_swap_bps: n("stable_swap_bps", 1.0).min(500.0),
        }
    }
    fn json(&self) -> Value {
        json!({"swap_bps": self.swap_bps, "gas_usd": self.gas_usd, "fee_haircut": self.fee_haircut, "emission_haircut": self.emission_haircut,
               "stable_swap_bps": self.stable_swap_bps})
    }
}

pub enum Strategy {
    Custom(Rules),
    Graph(Box<crate::graph::Graph>),
    Ladder { p: Params, max_ranges: usize },
    Hodl,
}

/// What a dollar parked in a token's whitelisted pegged pool earns a year (a fraction), day by day
/// from that pool's own hourly history: nothing before its first hour of history (the pool was not
/// there yet, so nothing could park in it) and nothing on a day it has none.
#[derive(Clone, Debug, Default)]
pub struct ParkRate {
    pub pool: String,
    /// the first hour of the pool's history (None: no history at all)
    first: Option<i64>,
    by_day: HashMap<i64, f64>,
    /// one rate throughout (tests)
    flat: Option<f64>,
    /// why there is no history, when a download failed
    pub error: Option<String>,
}

impl ParkRate {
    #[cfg(test)]
    pub fn flat(apr: f64) -> ParkRate {
        ParkRate { flat: Some(apr), ..Default::default() }
    }
    /// The rate on the day of `t`.
    pub fn at(&self, t: i64) -> f64 {
        if let Some(a) = self.flat {
            return a;
        }
        match self.first {
            Some(f) if t >= f => self.by_day.get(&t.div_euclid(86400)).copied().unwrap_or(0.0),
            _ => 0.0,
        }
    }
    /// The average rate over [from, to), days with nothing counting as nothing.
    pub fn mean(&self, from: i64, to: i64) -> f64 {
        if let Some(a) = self.flat {
            return a;
        }
        let (a, b) = (from.div_euclid(86400), (to - 1).div_euclid(86400));
        if b < a {
            return 0.0;
        }
        (a..=b).map(|d| self.at((d * 86400).max(from))).sum::<f64>() / (b - a + 1) as f64
    }
    /// From the processor's hourly rows: per day, the fees and emissions (at the haircuts) over the
    /// average active liquidity they were shared with, ours (`size_usd`) included, over a year.
    /// Income over liquidity, not an average of hourly APRs: a quiet hour of a small pool shows a
    /// huge APR that no position earns.
    fn from_rows(pool: &str, rows: &[Value], cost: &Costs, size_usd: f64) -> ParkRate {
        let n = |v: &Value| v.as_f64().or_else(|| v.as_str().and_then(|x| x.parse().ok())).unwrap_or(f64::NAN);
        // day -> (income, liquidity summed over the hours, hours)
        let mut days: HashMap<i64, (f64, f64, f64)> = HashMap::new();
        let mut first: Option<i64> = None;
        for r in rows {
            let ts = n(&r["timestamp"]);
            if !ts.is_finite() {
                continue;
            }
            let ts = ts as i64;
            first = Some(first.map_or(ts, |f| f.min(ts)));
            let tvl = Some(n(&r["activeTvlUSD"])).filter(|x| x.is_finite() && *x > 0.0).unwrap_or_else(|| n(&r["tvlUSD"]));
            let inc = n(&r["feesUSD"]).max(0.0) * cost.fee_haircut + n(&r["emissionsUSD"]).max(0.0) * cost.emission_haircut;
            if !(tvl.is_finite() && tvl > 0.0 && inc.is_finite()) {
                continue;
            }
            let d = days.entry(ts.div_euclid(86400)).or_default();
            *d = (d.0 + inc, d.1 + tvl + size_usd.max(0.0), d.2 + 1.0);
        }
        let by_day = days.into_iter().filter(|(_, d)| d.1 > 0.0).map(|(k, (inc, liq, h))| (k, inc / (liq / h) * 365.0)).collect();
        ParkRate { pool: pool.to_string(), first, by_day, flat: None, error: None }
    }
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
    /// what token0 and token1 earn parked in their whitelisted pegged pools while a strategy with
    /// parking on waits in them (parking.rs), day by day; nothing when not whitelisted
    pub park: [ParkRate; 2],
    /// the pool's chain (other tokens a node strategy watches are priced on it)
    pub cid: u64,
    /// the numeraire token's address (an exit to the dollar stable is valued with its USD price)
    pub num_token: String,
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
    /// parking: whether this run parks, the tokens parked now (token0, token1), and what they earned
    park_on: bool,
    /// the tokens the last exit left waiting (each parks once its pegged pool is there)
    waiting: [bool; 2],
    parked: [bool; 2],
    park_yield: f64,
    /// dollars an exit to the stable holds, and the numeraire's USD price at the current bar
    usd: f64,
    upn: f64,
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
        // a token waiting out of the pool parks as soon as its pegged pool is there, as the live
        // worker parks idle tokens every cycle
        if self.park_on && self.ranges.is_empty() {
            for sd in 0..2 {
                if self.waiting[sd] && !self.parked[sd] && c.park[sd].at(t0) > 0.0 {
                    let s = c.s(i0);
                    let v = if sd == 0 { c.value(self.x, 0.0, s) } else { c.value(0.0, self.y, s) };
                    if v <= 0.0 {
                        continue;
                    }
                    let fee = self.park_move(c, cost, v);
                    let k = (1.0 - fee / v).max(0.0);
                    if sd == 0 { self.x *= k } else { self.y *= k }
                    self.parked[sd] = true;
                }
            }
        }
        if self.parked.iter().any(|p| *p) {
            // the waiting tokens earn their pegged pools' APRs over the interval
            let s = c.s(i1);
            let v = |side: usize| if side == 0 { c.value(self.x, 0.0, s) } else { c.value(0.0, self.y, s) };
            let y = (0..2).filter(|sd| self.parked[*sd]).map(|sd| v(sd) * c.park[sd].at(t0)).sum::<f64>() * (t1 - t0).max(0) as f64 / (365.0 * 86400.0);
            self.park_yield += y;
            self.cash += y;
        }
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

    /// Moving a parked token in or out of its pegged pool: half of it swapped, a swap and a mint (or
    /// a withdrawal and a swap back). Returns the swap fee (numeraire), which the caller takes from
    /// the tokens, as every swap cost is.
    fn park_move(&mut self, c: &Ctx, cost: &Costs, value: f64) -> f64 {
        let fee = value * 0.5 * cost.swap_bps / 1e4;
        self.swap_costs += fee;
        self.pay_gas(c, cost, 2);
        fee
    }

    /// Value (numeraire) of the idle tokens parked now.
    fn parked_value(&self, c: &Ctx, s: f64) -> f64 {
        (if self.parked[0] { c.value(self.x, 0.0, s) } else { 0.0 }) + (if self.parked[1] { c.value(0.0, self.y, s) } else { 0.0 })
    }

    fn pay_gas(&mut self, c: &Ctx, cost: &Costs, n_tx: usize) {
        let g = n_tx as f64 * cost.gas_usd * c.num_per_usd;
        self.gas += g;
        self.cash -= g;
    }

    /// Burn everything, swap to the ratio `target` (ranges with value weights) needs, mint it.
    fn rebalance(&mut self, c: &Ctx, cost: &Costs, i: usize, t: i64, target: &[(i64, i64, f64)], why: &str) {
        self.from_usd(c, cost);
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
        let mut v1n = v1;
        if self.parked.iter().any(|p| *p) {
            let fee = self.park_move(c, cost, self.parked_value(c, s));
            self.parked = [false; 2];
            v1n -= if c.num1 { fee } else { fee * p };
        }
        self.waiting = [false; 2];
        let swapped1 = (f0 * v1 / p - x).abs() * p;
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

    /// Dollars an exit to the stable holds, bought back into the numeraire (a swap) at the bar's price.
    fn from_usd(&mut self, c: &Ctx, cost: &Costs) {
        if self.usd <= 0.0 {
            return;
        }
        let v = self.usd / self.upn;
        let fee = v * cost.stable_swap_bps / 1e4;
        self.swap_costs += fee;
        if c.num1 {
            self.y += v - fee;
        } else {
            self.x += v - fee;
        }
        self.usd = 0.0;
        self.pay_gas(c, cost, 1);
    }

    /// Value (numeraire) of the dollars held now.
    fn usd_value(&self) -> f64 {
        if self.usd > 0.0 { self.usd / self.upn } else { 0.0 }
    }

    /// An exit into the dollar stable: everything withdrawn and both tokens sold for dollars.
    fn exit_stable(&mut self, c: &Ctx, cost: &Costs, i: usize, t: i64, why: &str) {
        let s = c.s(i);
        let (x, y) = self.inventory(c, s);
        let mut n_tx = if self.ranges.is_empty() { 0 } else { 1 } + (x > 0.0) as usize + (y > 0.0) as usize;
        let mut v = c.value(x, y, s);
        if self.parked.iter().any(|p| *p) {
            v -= self.park_move(c, cost, self.parked_value(c, s));
            n_tx += 1;
        }
        let fee = v * cost.stable_swap_bps / 1e4;
        self.swap_costs += fee;
        self.usd += (v - fee).max(0.0) * self.upn;
        self.close_segment(t);
        self.ranges.clear();
        self.x = 0.0;
        self.y = 0.0;
        self.waiting = [false; 2];
        self.parked = [false; 2];
        self.pay_gas(c, cost, n_tx);
        self.actions += 1;
        if self.events.len() < MAX_EVENTS {
            self.events.push(json!({"ts": t, "kind": "exit", "reason": why, "to": "stable"}));
        }
    }

    fn exit(&mut self, c: &Ctx, cost: &Costs, i: usize, t: i64, to: ExitTo, why: &str) {
        if to == ExitTo::Stable {
            return self.exit_stable(c, cost, i, t, why);
        }
        // from the stable into a token: the dollars buy the numeraire back first
        self.from_usd(c, cost);
        let s = c.s(i);
        let p = s * s;
        let (mut x, mut y) = self.inventory(c, s);
        let mut n_tx = if self.ranges.is_empty() { 0 } else { 1 };
        let swapped1 = match to {
            ExitTo::Token0 => y,
            ExitTo::Token1 => x * p,
            ExitTo::Keep | ExitTo::Stable => 0.0,
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
        // parking: out of the old pegged pools (a turn), into the new tokens'; kept as they are,
        // each token held waits in its own pegged pool
        let v = c.value(x, y, s);
        let mut fee = 0.0;
        if self.parked.iter().any(|p| *p) {
            fee += self.park_move(c, cost, self.parked_value(c, s));
        }
        self.waiting = match to {
            ExitTo::Token0 => [true, false],
            ExitTo::Token1 => [false, true],
            ExitTo::Keep | ExitTo::Stable => [x > 0.0, y > 0.0],
        };
        self.parked = self.waiting;
        for sd in 0..2 {
            // only into a pegged pool that is there (and earning) on the day
            self.parked[sd] &= self.park_on && c.park[sd].at(t) > 0.0;
        }
        if self.parked.iter().any(|p| *p) {
            fee += self.park_move(c, cost, self.parked_value(c, s));
        }
        if fee > 0.0 && v > 0.0 {
            let k = (1.0 - fee / v).max(0.0);
            self.x *= k;
            self.y *= k;
        }
        if self.events.len() < MAX_EVENTS {
            self.events.push(json!({"ts": t, "kind": "exit", "reason": why}));
        }
    }
}

/// Replay one strategy over bars [b0, b1] with `capital` (numeraire units, all in the numeraire).
pub fn run(c: &Ctx, strat: &Strategy, cost: &Costs, capital: f64, b0: usize, b1: usize) -> Value {
    let m = c.m;
    let mut sim = Sim::default();
    let park = match strat {
        Strategy::Graph(g) => g.park,
        Strategy::Custom(r) => r.park,
        _ => false,
    };
    if park {
        sim.park_on = true;
    }
    let i_start = m.bar_idx[b0];
    if c.num1 {
        sim.y = capital;
    } else {
        sim.x = capital;
    }
    let med = data::rolling_median_trailing(&m.bar_tick, data::week_bars(m), 12);
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
    let mut watch_errors: Vec<String> = vec![];
    let glad = match strat {
        Strategy::Graph(g) => g.ladder(),
        _ => None,
    };
    let gev = match strat {
        Strategy::Graph(g) => {
            let (ext, errs) = crate::graph::load_externals(g, c.cid, m);
            watch_errors = errs;
            Some(crate::graph::evaluate_ext(g, m, &ext))
        }
        _ => None,
    };
    // an exit to the dollar stable holds dollars: valued with the numeraire's USD price bar by bar
    // (from the chain's pools, as a watched token is), or today's price when that is not available
    let flat_upn = if c.num_per_usd > 0.0 { 1.0 / c.num_per_usd } else { 1.0 };
    let stable_exit = match strat {
        Strategy::Graph(g) => g.may_exit_to_stable(),
        Strategy::Custom(r) => r.exit_to == ExitTo::Stable,
        _ => false,
    };
    let upn: Vec<f64> = if !stable_exit || c.num_token.is_empty() {
        vec![]
    } else {
        match crate::external::load(c.cid, &c.num_token, &m.bar_end) {
            Ok(sr) => {
                let mut last = flat_upn;
                sr.close
                    .iter()
                    .map(|t| {
                        if t.is_finite() {
                            last = 1.0001f64.powf(*t);
                        }
                        last
                    })
                    .collect()
            }
            Err(e) => {
                watch_errors.push(format!("the numeraire's USD price (exit to stable): {e}; today's price is used"));
                vec![]
            }
        }
    };
    sim.upn = flat_upn;
    for b in b0..=b1 {
        let i = m.bar_idx[b];
        let t = m.bar_end[b];
        if let Some(u) = upn.get(b) {
            sim.upn = *u;
        }
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
                        match &glad {
                            // a ladder block: the ladder's own solve, taken when it moves enough of the value
                            Some(p) => {
                                sim.from_usd(c, cost);
                                let (x, y) = sim.inventory(c, s);
                                let target = crate::engine::cap_ranges(
                                    crate::engine::snap_ranges(ladder::decide_at(m, p, x + y / (s * s), m.sp[i], tick, i), c.spacing),
                                    g.max_ranges,
                                );
                                if sim.ranges.is_empty() || turnover(c, &sim, &target, s) >= p.turnover {
                                    sim.rebalance(c, cost, i, t, &target, &reason);
                                }
                            }
                            None => sim.rebalance(c, cost, i, t, &[(tl, tu, 1.0)], &reason),
                        }
                        cst.moved(t);
                    }
                    Decision::Exit(reason) => {
                        let to = crate::graph::exit_to(g, gev.as_ref().unwrap(), b, tick, range);
                        sim.exit(c, cost, i, t, to, &reason);
                        cst.exited_to(to);
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
            series.push(json!([t, round6(c.value(x, y, s) + sim.usd_value() + sim.cash), round6(hodl)]));
            next_mark = (t.div_euclid(3600) + 1) * 3600;
        }
    }
    let t_end = m.bar_end[b1];
    sim.close_segment(t_end);
    let s_end = c.s(m.bar_idx[b1]);
    let (x, y) = sim.inventory(c, s_end);
    let inventory = c.value(x, y, s_end) + sim.usd_value();
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
        "park_yield": round6(sim.park_yield),
        "watch_errors": watch_errors,
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
static LAST: Mutex<Vec<(String, std::sync::Arc<Market>)>> = Mutex::new(Vec::new());
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
/// {chain_id, pool, days (1–730), until (optional: the period ends then, unix seconds, else now), capital, strategy: {type: "custom", ...rules} | {type: "ladder", ...},
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
    // the period ends now, or at `until` (unix seconds) for a period in the past
    let end = req["until"].as_i64().filter(|u| *u > 0 && *u < now).unwrap_or(now);
    let from = end - days * 86400;
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
    // the replay on bars of `bar_s` seconds: 5 minutes, or 1 minute for a strategy that decides every
    // minute (graph.rs Graph::bar_s); the last two are kept
    let market_at = |bar_s: i64| -> Result<std::sync::Arc<Market>, String> {
        let _build = BUILD.lock().unwrap_or_else(|e| e.into_inner());
        let key = format!("{mkey}:{bar_s}");
        if let Some(m) = LAST.lock().unwrap().iter().find(|(k, _)| *k == key).map(|(_, m)| m.clone()) {
            return Ok(m);
        }
        let events = crate::history::read(cid, pool, from.div_euclid(3600) * 3600 - WARMUP_S, now + 3600);
        let m = std::sync::Arc::new(data::build_market_bars(events, &wallet.to_lowercase(), &apr, m0.decimals as i32, m1.decimals as i32, if num1 { 1 } else { 0 }, bar_s)?);
        let mut g = LAST.lock().unwrap();
        g.push((key, m.clone()));
        while g.len() > 2 {
            g.remove(0);
        }
        Ok(m)
    };
    let m_arc = market_at(data::BAR_S)?;
    let m: &Market = &m_arc;
    let from = from.div_euclid(3600) * 3600;
    let b0 = m.bar_end.partition_point(|t| *t < from);
    if m.bar_end.is_empty() || b0 >= m.bar_end.len() {
        return Err("not enough swaps in the period".into());
    }
    let b1 = m.bar_end.partition_point(|t| *t <= end).saturating_sub(1);
    if b1 <= b0 {
        return Err("not enough swaps in the period".into());
    }
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
        park: park_rates(cid, [tok0.as_str(), tok1.as_str()], from - WARMUP_S, &Costs::from(&req["costs"]), req["capital_usd"].as_f64().unwrap_or(0.0)),
        cid,
        num_token: num_tok.to_lowercase(),
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
    // the 1-minute replay, when any strategy here decides every minute
    let wants_minutes = |s: &Value| s.as_object().map(|o| bar_of(o, spacing) == 60).unwrap_or(false);
    let need60 = wants_minutes(&req["strategy"])
        || req["compare"].as_array().map(|a| a.iter().any(|p| wants_minutes(&p["strategy"]))).unwrap_or(false)
        || req["batch"].as_array().map(|a| a.iter().any(|p| wants_minutes(&p["strategy"]))).unwrap_or(false)
        || req["optimize"]["params"].as_array().map(|a| a.iter().any(|d| d["choices"].as_array().map(|c| c.iter().any(|x| x.as_f64() == Some(1.0))).unwrap_or(false))).unwrap_or(false);
    let m60_arc = if need60 { Some(market_at(60)?) } else { None };
    let rp5 = Rp { c: Ctx { m, ..c }, b0, b1 };
    let rp60 = match &m60_arc {
        Some(mm) => {
            let mm: &Market = mm;
            let a = mm.bar_end.partition_point(|t| *t < from);
            if mm.bar_end.is_empty() || a >= mm.bar_end.len() {
                return Err("not enough swaps in the period".into());
            }
            Some(Rp { c: Ctx { m: mm, spacing, num1, num_per_usd, emis_usd_h: rp5.c.emis_usd_h.clone(), sqrt_px: mm.px_scale.sqrt(), beta, park: rp5.c.park.clone(), cid, num_token: rp5.c.num_token.clone() }, b0: a, b1: mm.bar_end.partition_point(|t| *t <= end).saturating_sub(1).max(a) })
        }
        None => None,
    };
    // the replay a strategy runs on: its own decision interval
    let rp_for = |s: &Map<String, Value>| pick(&rp5, &rp60, s, spacing);
    let c = &rp5.c;
    if req["optimize"].is_object() {
        return optimize(id, &req["optimize"], &strat_v, &rp5, &rp60, &cost, capital, spacing, max_ranges, pegged);
    }
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
                            let r = rp_for(&sv);
                            let v = run(&r.c, &st, &cost, capital, r.b0, r.b1);
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
                            let ann = |k: &str| v[k].as_f64().unwrap_or(0.0) / capital * 365.0 / span_days * 100.0;
                            json!({"id": it["id"], "pnl": pnl, "apr": pnl / capital * 365.0 / span_days * 100.0, "vs_hodl": v["vs_hodl"],
                                   "vs_hold_apr": ann("vs_hodl"), "fees_apr": ann("fees"), "il_apr": ann("il"), "costs_apr": ann("swap_costs") + ann("gas"),
                                   "park_apr": ann("park_yield"), "fees": v["fees"], "actions": v["actions"], "time_in_range": v["time_in_range"]})
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
        let rp = rp_for(&strat_v);
        let (c, b0, b1, m) = (&rp.c, rp.b0, rp.b1, rp.c.m);
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
                        let v = run(c, &st, &cost, capital, b0, b1);
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
    let main_rp = rp_for(&strat_v);
    let mut out = Map::new();
    // "main": false runs only the comparisons (one added to the list), not the strategy again
    let only_compare = req["main"].as_bool() == Some(false);
    if !only_compare {
        let mut v = run(&main_rp.c, &main, &cost, capital, main_rp.b0, main_rp.b1);
        v["decides_every_s"] = json!(main_rp.c.m.bar_s);
        out.insert("main".into(), v);
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
                let r = rp_for(&sv);
                let v = run(&r.c, &st, &cost, capital, r.b0, r.b1);
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
        out.insert("hold".into(), run(c, &Strategy::Hodl, &cost, capital, b0, b1));
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
        Strategy::Graph(g) => {
            // on the strategy's own replay, at the same hours as the price
            let mm = main_rp.c.m;
            let ix: Vec<usize> = idx.iter().map(|b| mm.bar_end.partition_point(|t| *t <= m.bar_end[*b]).saturating_sub(1)).collect();
            crate::graph::overlays(g, &crate::graph::evaluate_ext(g, mm, &crate::graph::load_externals(g, cid, mm).0), &ix, spacing)
        }
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
                  "Emissions and gas use today's USD price of the numeraire.".to_string(),
                  format!("Parking (strategies with parking on): a token waiting out of the pool earns what its whitelisted pegged pool paid that day (its fees and emissions after the haircuts, shared with its active liquidity and ours), nothing before that pool's history begins ({}); each move in or out costs two transactions and a swap of half the value.{}",
                          [(&m0.symbol, &c.park[0]), (&m1.symbol, &c.park[1])].iter().map(|(sym, r)| park_note(sym, r, m.bar_end[b0], m.bar_end[b1])).collect::<Vec<_>>().join("; "),
                          if crate::parking::enabled(&crate::settings::load_settings()) { "" } else { " Live parking is off (Settings › Options): the backtest counts it, the worker will not park until it is on." })],
    }))
}

/// Dynamic optimization (optimize.rs) on the replay: the search sees the first three quarters of the
/// period when the hold-out is on, and the best settings, and the current ones, are then run on the
/// last quarter. Returns are annualised, in percent of the capital.
#[allow(clippy::too_many_arguments)]
fn optimize(id: u64, spec: &Value, base: &Map<String, Value>, rp5: &Rp, rp60: &Option<Rp>, cost: &Costs, capital: f64,
            spacing: i64, max_ranges: usize, pegged: bool) -> Result<Value, String> {
    use crate::optimize as opt;
    let rp_for = |s: &Map<String, Value>| pick(rp5, rp60, s, spacing);
    let auto = spec["auto"].as_bool().unwrap_or(false);
    let o = opt::Opts::from(spec);
    let vs_hold = spec["objective"].as_str() == Some("vs_hold");
    // auto mode always keeps the last quarter back: its suggestion rests on it
    let holdout = auto || spec["holdout"].as_bool().unwrap_or(true);
    // the split is a moment in time, the same on either replay
    let r0 = rp_for(base);
    let (t_a, t_b) = (r0.c.m.bar_end[r0.b0], r0.c.m.bar_end[r0.b1]);
    let t_split = if holdout { t_a + (t_b - t_a) * 3 / 4 } else { t_b };
    let days = |a: i64, b: i64| ((b - a) as f64 / 86400.0).max(1.0 / 24.0);
    if days(t_a, t_split) < 1.0 || (holdout && days(t_split, t_b) < 0.5) {
        return Err("the period is too short to optimize: pick a longer one".into());
    }
    let row = |v: &Value, d: f64| -> (f64, Value) {
        let ann = |k: &str| v[k].as_f64().unwrap_or(f64::NAN) / capital * 365.0 / d * 100.0;
        let (apr, vh) = (ann("pnl"), ann("vs_hodl"));
        (if vs_hold { vh } else { apr }, json!({"apr": apr, "vs_hold_apr": vh, "fees_apr": ann("fees"), "actions": v["actions"], "time_in_range": v["time_in_range"]}))
    };
    // walk-forward: the training part in consecutive folds, each run on its own (at least a day each)
    let folds = (spec["folds"].as_u64().unwrap_or(4) as usize).clamp(1, 24).min(days(t_a, t_split).floor().max(1.0) as usize);
    let cuts: Vec<i64> = (0..=folds).map(|k| t_a + (t_split - t_a) * k as i64 / folds as i64).collect();
    // [from, to) of the period on the strategy's own replay
    let run_span = |s: &Map<String, Value>, from: i64, to: i64| -> Result<(f64, Value), String> {
        let st = strategy(s, spacing, max_ranges, pegged)?;
        let r = rp_for(s);
        let a = r.c.m.bar_end.partition_point(|t| *t < from).clamp(r.b0, r.b1);
        let b = r.c.m.bar_end.partition_point(|t| *t < to).clamp(r.b0, r.b1);
        if b <= a {
            return Err("a fold with no bars".into());
        }
        Ok(row(&run(&r.c, &st, cost, capital, a, b), days(r.c.m.bar_end[a], r.c.m.bar_end[b])))
    };
    let run_on = |s: &Map<String, Value>, test: bool| -> Result<(f64, Value), String> {
        if test { run_span(s, t_split, t_b) } else { run_span(s, t_a, t_split) }
    };
    // the search's score: the reliable return over the folds (optimize.rs `composite`)
    let eval = |s: &Map<String, Value>| -> Result<(f64, Value), String> {
        let mut vals = vec![];
        let mut rows = vec![];
        for k in 0..folds {
            let (v, r) = run_span(s, cuts[k], cuts[k + 1])?;
            vals.push(v);
            rows.push(r);
        }
        let (score, parts) = opt::composite(&vals);
        let mean = |k: &str| rows.iter().map(|r| r[k].as_f64().unwrap_or(f64::NAN)).sum::<f64>() / rows.len() as f64;
        let actions: u64 = rows.iter().map(|r| r["actions"].as_u64().unwrap_or(0)).sum();
        Ok((score, json!({"score": score, "wf": parts, "apr": mean("apr"), "vs_hold_apr": mean("vs_hold_apr"), "fees_apr": mean("fees_apr"),
                          "actions": actions, "time_in_range": mean("time_in_range")})))
    };
    let flag = opt::flag(id);
    if auto {
        let r = if spec["mode"].as_str() == Some("walk") {
            walk_forward(id, spec, base, o.workers, &|s, a, b| run_span(s, a, b), (t_a, t_b), vs_hold, &flag)
        } else {
            auto_attempts(id, spec, base, o.workers, &|s, a, b| run_span(s, a, b), (t_a, t_b), vs_hold, &flag)
        };
        opt::forget(id);
        let mut r = r?;
        r["objective"] = json!(if vs_hold { "vs_hold" } else { "return" });
        r["score"] = json!("reliable return: mean − 1.64·sd/√n of the annualised return across walk-forward folds");
        r["workers"] = json!(o.workers);
        return Ok(json!({"optimize": r}));
    }
    let dims = opt::dims(base, &spec["params"])?;
    let total = o.runs;
    let mut r = opt::search(base, &dims, &o, &eval, &flag, &|n, top| {
        set_job(id, json!({"id": id, "state": "running", "step": format!("optimizing: {n} of {total} runs"), "partial": {"done": n, "total": total, "top": top}}));
    });
    opt::forget(id);
    // the current settings, and the best ones on the quarter the search never saw
    let mut current = eval(base).map(|x| x.1).unwrap_or_else(|e| json!({"error": e}));
    if holdout {
        step(id, "checking the best settings on the held-out quarter");
        let tops: Vec<Map<String, Value>> = r["top"].as_array().map(|a| a.iter().map(|t| {
            let x: Vec<f64> = dims.iter().map(|d| t["values"][d.key().as_str()].as_f64().unwrap_or(f64::NAN)).collect();
            opt::apply(base, &dims, &x)
        }).collect()).unwrap_or_default();
        let tests: Mutex<Vec<Option<Value>>> = Mutex::new(vec![None; tops.len()]);
        let next = AtomicUsize::new(0);
        std::thread::scope(|sc| {
            for _ in 0..o.workers.min(tops.len().max(1)) {
                sc.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    if k >= tops.len() {
                        break;
                    }
                    let v = run_on(&tops[k], true).map(|x| x.1).unwrap_or_else(|e| json!({"error": e}));
                    tests.lock().unwrap()[k] = Some(v);
                });
            }
        });
        // over all periods: the training folds and the held-out quarter, scored alike; the
        // finalists are ranked on it (the search itself never saw the held-out quarter)
        let key = if vs_hold { "vs_hold_apr" } else { "apr" };
        let all = |t: &Value| -> Value {
            let mut v: Vec<f64> = t["wf"]["folds"].as_array().map(|a| a.iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect()).unwrap_or_default();
            v.push(t["test"][key].as_f64().unwrap_or(f64::NAN));
            opt::composite(&v).1
        };
        if let Some(a) = r["top"].as_array_mut() {
            for (t, v) in a.iter_mut().zip(tests.into_inner().unwrap()) {
                t["test"] = v.unwrap_or(Value::Null);
                t["all"] = all(t);
            }
            let sc = |t: &Value| t["all"]["score"].as_f64().filter(|x| x.is_finite()).unwrap_or(f64::NEG_INFINITY);
            a.sort_by(|x, y| sc(y).partial_cmp(&sc(x)).unwrap_or(std::cmp::Ordering::Equal));
        }
        current["test"] = run_on(base, true).map(|x| x.1).unwrap_or_else(|e| json!({"error": e}));
        current["all"] = all(&current);
    }
    r["current"] = current;
    r["objective"] = json!(if vs_hold { "vs_hold" } else { "return" });
    r["train"] = json!({"from": t_a, "to": t_split, "days": days(t_a, t_split)});
    r["folds"] = json!({"n": folds, "cuts": cuts, "days": days(t_a, t_split) / folds as f64});
    r["score"] = json!("reliable return: mean − 1.64·sd/√n of the annualised return across walk-forward folds");
    if holdout {
        r["test"] = json!({"from": t_split, "to": t_b, "days": days(t_split, t_b)});
    }
    r["workers"] = json!(o.workers);
    Ok(json!({"optimize": r}))
}

/// A run of a strategy over [from, to): the objective (annualised, %) and its row.
type Span<'a> = dyn Fn(&Map<String, Value>, i64, i64) -> Result<(f64, Value), String> + Sync + 'a;

/// How auto mode cuts the period: the last eighth is the exam, which no search, screening or
/// validation ever runs on; the rest is cut into WINDOWS windows, and each attempt validates on one
/// of them (the most recent first, a fresh one every attempt) and searches on the others, its folds.
const WINDOWS: usize = 5;
/// Candidates that may take the exam: each look makes a lucky pass likelier, so few, and the second
/// must pass each half of it.
const EXAMS: usize = 2;

/// The reliable return over `spans` (optimize.rs `composite`), and the row the lab shows.
fn eval_over(span: &Span, spans: &[(i64, i64)], s: &Map<String, Value>) -> Result<(f64, Value), String> {
    let mut vals = vec![];
    let mut rows = vec![];
    for (a, b) in spans {
        let (v, r) = span(s, *a, *b)?;
        vals.push(v);
        rows.push(r);
    }
    let (score, parts) = crate::optimize::composite(&vals);
    let mean = |k: &str| rows.iter().map(|r| r[k].as_f64().unwrap_or(f64::NAN)).sum::<f64>() / rows.len().max(1) as f64;
    let actions: u64 = rows.iter().map(|r| r["actions"].as_u64().unwrap_or(0)).sum();
    Ok((score, json!({"score": score, "wf": parts, "apr": mean("apr"), "vs_hold_apr": mean("vs_hold_apr"), "fees_apr": mean("fees_apr"),
                      "actions": actions, "time_in_range": mean("time_in_range")})))
}

/// The windows of auto mode over [t_a, t_b): the exam, and per attempt k (1-based) its validation
/// window and its folds (the other windows, each cut in `per` pieces).
fn auto_windows(t_a: i64, t_b: i64, per: usize) -> ((i64, i64), Vec<((i64, i64), Vec<(i64, i64)>)>) {
    let t_e = t_b - (t_b - t_a) / 8;
    let w: Vec<(i64, i64)> = (0..WINDOWS).map(|j| (t_a + (t_e - t_a) * j as i64 / WINDOWS as i64, t_a + (t_e - t_a) * (j + 1) as i64 / WINDOWS as i64)).collect();
    let attempts = (0..WINDOWS).map(|k| {
        let v = WINDOWS - 1 - k;
        let folds = (0..WINDOWS).filter(|j| *j != v).flat_map(|j| {
            let (a, b) = w[j];
            (0..per).map(move |q| (a + (b - a) * q as i64 / per as i64, a + (b - a) * (q + 1) as i64 / per as i64))
        }).collect();
        (w[v], folds)
    }).collect();
    ((t_e, t_b), attempts)
}

/// Auto optimization: attempts, each a different way to look for settings that hold up, until one
/// passes or none is left:
///   1. the settings that matter: every setting screened over half to twice its value, the few that
///      move the score tuned together;
///   2. wider ranges: those that mattered, less any the search showed changed nothing, plus any whose
///      best sat at an end of its range, screened again over a quarter to four times their value;
///   3. small steps: the three that mattered most, within ±30% of where they are (a small change is
///      less able to fit noise than a large one);
///   4. changes to the blocks: each single change `optimize::variants` offers, run as it is;
///   5. the best change, then tuned as in 1.
/// Overfitting is what this guards against, with windows that move (`auto_windows`): each attempt
/// searches on its folds and is judged on its own validation window, one no earlier attempt was
/// judged on; a best that passes there (`optimize::suggest`) then takes the exam, the last eighth,
/// which nothing else ever ran on, and is suggested only if it beats the current settings there too.
/// At most EXAMS candidates take it; the second must win each half. `span` runs [from, to).
#[allow(clippy::too_many_arguments)]
fn auto_attempts(id: u64, spec: &Value, base: &Map<String, Value>, workers: usize, span: &Span, period: (i64, i64),
                 vs_hold: bool, flag: &std::sync::atomic::AtomicBool) -> Result<Value, String> {
    use crate::optimize as opt;
    let key = if vs_hold { "vs_hold_apr" } else { "apr" };
    let (t_a, t_b) = period;
    let day = |a: i64, b: i64| (b - a) as f64 / 86400.0;
    if day(t_a, t_b) < 10.0 {
        return Err("auto-optimize needs at least 10 days: pick a longer period".into());
    }
    let only: Vec<String> = spec["nodes"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let max_attempts = spec["attempts"].as_u64().unwrap_or(5).clamp(1, WINDOWS as u64) as usize;
    let per = ((spec["folds"].as_u64().unwrap_or(4) as usize).clamp(1, 24) + WINDOWS - 2) / (WINDOWS - 1);
    let (exam, plan) = auto_windows(t_a, t_b, per.max(1));
    let runs_for = |n: usize| spec["runs"].as_u64().map(|r| r as usize).unwrap_or((25 * n).clamp(60, 400)).clamp(10, opt::MAX_RUNS);
    let stopped = || flag.load(Ordering::SeqCst);
    let sc_all = |t: &Value| t["all"]["score"].as_f64().filter(|x| x.is_finite()).unwrap_or(f64::NEG_INFINITY);
    let report = |k: usize, label: &str, phase: &str, n: usize, total: usize, top: Value| {
        set_job(id, json!({"id": id, "state": "running", "step": format!("attempt {k} of {max_attempts} ({label}): {phase}, {n} of {total} runs"),
                           "partial": {"phase": phase, "attempt": k, "of": max_attempts, "label": label, "done": n, "total": total, "top": top}}));
    };
    // a run with its two halves
    let halves = |s: &Map<String, Value>, (a, b): (i64, i64)| -> Value {
        let m = a + (b - a) / 2;
        let mut v = span(s, a, b).map(|x| x.1).unwrap_or_else(|e| json!({"error": e}));
        if let (Ok(x), Ok(y)) = (span(s, a, m), span(s, m, b)) {
            v["halves"] = json!([x.0, y.0]);
        }
        v
    };

    // everything one attempt needs: its folds, its validation window, the current settings on them
    struct Att<'a> {
        k: usize,
        val: (i64, i64),
        eval: Box<opt::Eval<'a>>,
        current: Value,
    }
    let attempt = |k: usize| -> Att {
        let (val, folds) = plan[k - 1].clone();
        let f2 = folds.clone();
        let eval: Box<opt::Eval> = Box::new(move |s: &Map<String, Value>| eval_over(span, &f2, s));
        let mut current = eval(base).map(|x| x.1).unwrap_or_else(|e| json!({"error": e}));
        current["test"] = span(base, val.0, val.1).map(|x| x.1).unwrap_or_else(|e| json!({"error": e}));
        current["all"] = all_periods(&current, key);
        Att { k, val, eval, current }
    };
    // finalists: each run on the attempt's validation window (in parallel), scored over all its
    // periods and ranked on it, rows alike merged; each keeps the whole strategy it stands for
    let finish = |at: &Att, mut rows: Vec<Value>, strats: Vec<Map<String, Value>>| -> Vec<Value> {
        let tests: Mutex<Vec<Option<Value>>> = Mutex::new(vec![None; strats.len()]);
        let next = AtomicUsize::new(0);
        std::thread::scope(|sc| {
            for _ in 0..workers.min(strats.len().max(1)) {
                sc.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    if k >= strats.len() {
                        break;
                    }
                    let v = span(&strats[k], at.val.0, at.val.1).map(|x| x.1).unwrap_or_else(|e| json!({"error": e}));
                    tests.lock().unwrap()[k] = Some(v);
                });
            }
        });
        for ((t, v), st) in rows.iter_mut().zip(tests.into_inner().unwrap()).zip(strats) {
            t["test"] = v.unwrap_or(Value::Null);
            t["all"] = all_periods(t, key);
            t["strategy"] = Value::Object(st);
        }
        rows.sort_by(|x, y| sc_all(y).partial_cmp(&sc_all(x)).unwrap_or(std::cmp::Ordering::Equal));
        opt::dedupe(rows)
    };
    // screening on base `sb`: the candidates as dimensions, the current score and the report
    let screen = |at: &Att, label: &str, sb: &Map<String, Value>, specs: Vec<Value>| -> Result<(Vec<opt::Dim>, f64, Vec<Value>), String> {
        if specs.is_empty() || stopped() {
            return Ok((vec![], f64::NAN, vec![]));
        }
        let cands = opt::dims_upto(sb, &Value::Array(specs), opt::MAX_SCREEN)?;
        let (b, rep) = opt::screen(sb, &cands, workers, &*at.eval, flag, &|n, total| report(at.k, label, "screening", n, total, Value::Null));
        Ok((cands, b, rep))
    };
    let screen_runs = |rep: &[Value]| if rep.is_empty() { 0 } else { 1 + rep.iter().map(|r| r["points"].as_array().map(|a| a.len().saturating_sub(1)).unwrap_or(0)).sum::<usize>() };
    let picked_of = |cands: &[opt::Dim], rep: &[Value]| -> (Vec<opt::Dim>, Vec<String>) {
        let imp: Vec<f64> = rep.iter().map(|r| r["importance"].as_f64().unwrap_or(0.0)).collect();
        let p = opt::pick(&imp);
        (p.iter().map(|i| cands[*i].clone()).collect(), p.iter().map(|i| rep[*i]["key"].as_str().unwrap_or("").to_string()).collect())
    };
    // a search on base `sb`, its finalists checked on the attempt's validation window
    let tune = |at: &Att, label: &str, sb: &Map<String, Value>, dims: &[opt::Dim], runs: usize| -> Value {
        let o = opt::Opts { runs, workers };
        let mut r = opt::search(sb, dims, &o, &*at.eval, flag, &|n, top| report(at.k, label, "search", n, runs, top));
        let rows: Vec<Value> = r["top"].as_array().cloned().unwrap_or_default();
        r["flat"] = json!(opt::flat_keys(&rows, dims));
        let strats: Vec<Map<String, Value>> = rows.iter().map(|t| {
            let x: Vec<f64> = dims.iter().map(|d| t["values"][d.key().as_str()].as_f64().unwrap_or(f64::NAN)).collect();
            opt::apply(sb, dims, &x)
        }).collect();
        r["top"] = Value::Array(finish(at, rows, strats));
        r
    };

    // the exam, run at most EXAMS times: the current settings once, then each candidate
    let cur_exam = std::cell::OnceCell::new();
    let mut exams = 0usize;
    let mut attempts: Vec<Value> = vec![];
    let mut shown: Vec<Value> = vec![];
    let mut passed: Option<usize> = None;
    let mut total_runs = 0usize;
    // an attempt's outcome: judged on its window, a pass taking the exam; whether to stop here
    let mut settle = |at: &Att, kind: &str, label: &str, keys: Vec<String>, r: Option<Value>, note: &str, extra_runs: usize,
                      attempts: &mut Vec<Value>, shown: &mut Vec<Value>| -> bool {
        let mut r = r.unwrap_or_else(|| json!({"top": [], "runs": 0, "rounds": 0}));
        let top: Vec<Value> = r["top"].as_array().cloned().unwrap_or_default();
        let mut sug = if top.is_empty() { json!({"apply": null, "why": note}) } else { opt::suggest(&at.current, &top, key) };
        // here the check before the exam is the attempt's own window
        if let Some(w) = sug["why"].as_str().map(|w| w.replace("the held-out quarter", "its validation window")) {
            sug["why"] = json!(w);
        }
        let mut exam_res = Value::Null;
        if sug["apply"] == 0 {
            if exams >= EXAMS {
                sug = json!({"apply": null, "why": format!("passed its window, but the exam has been taken {EXAMS} times already: another look would be luck")});
            } else {
                exams += 1;
                let ce: &Value = cur_exam.get_or_init(|| halves(base, exam));
                let be = halves(top[0]["strategy"].as_object().unwrap_or(base), exam);
                let f = |v: &Value, k: &str| v[k].as_f64().filter(|x| x.is_finite());
                let h = |v: &Value, i: usize| v["halves"][i].as_f64().filter(|x| x.is_finite());
                let (b, c) = (f(&be, key), f(ce, key));
                let ok = matches!((b, c), (Some(b), Some(c)) if b > c)
                    && (exams == 1 || matches!((h(&be, 0), h(ce, 0), h(&be, 1), h(ce, 1)), (Some(b0), Some(c0), Some(b1), Some(c1)) if b0 > c0 && b1 > c1));
                let (bv, cv) = (b.unwrap_or(f64::NAN), c.unwrap_or(f64::NAN));
                exam_res = json!({"best": be, "current": ce, "passed": ok, "look": exams});
                if ok {
                    let short = if day(exam.0, exam.1) < 14.0 { " (a short exam: a longer period gives a firmer check)" } else { "" };
                    sug["why"] = json!(format!("{}; and on the exam, the last {:.0} days nothing had run on, {bv:+.1} vs {cv:+.1} %/yr{}{short}",
                                               sug["why"].as_str().unwrap_or(""), day(exam.0, exam.1), if exams > 1 { ", in each half of it" } else { "" }));
                } else {
                    sug = json!({"apply": null, "why": format!("passed its validation window but not the exam, the last {:.0} days nothing had run on ({bv:+.1} vs {cv:+.1} %/yr{}): it fit the past",
                                                               day(exam.0, exam.1), if exams > 1 { ", or not in each half" } else { "" })});
                }
            }
        }
        let runs = r["runs"].as_u64().unwrap_or(0) as usize + extra_runs;
        total_runs += runs;
        let ran = !top.is_empty();
        attempts.push(json!({"n": at.k, "kind": kind, "label": label, "picked": keys, "runs": runs, "suggestion": sug, "ran": ran, "exam": exam_res,
                             "window": {"from": at.val.0, "to": at.val.1, "days": day(at.val.0, at.val.1)},
                             "current": {"test": at.current["test"][key], "all": at.current["all"]["score"]},
                             "best": top.first().map(|b| json!({"all": b["all"]["score"], "test": b["test"][key], "values": b["values"], "change": b["change"]}))}));
        r["picked"] = json!(keys);
        r["suggestion"] = sug.clone();
        r["attempt"] = json!(at.k);
        r["label"] = json!(label);
        r["kind"] = json!(kind);
        r["current"] = at.current.clone();
        r["test"] = json!({"from": at.val.0, "to": at.val.1, "days": day(at.val.0, at.val.1)});
        r["exam"] = exam_res;
        if ran {
            shown.push(r);
            if sug["apply"] == 0 {
                passed = Some(shown.len() - 1);
            }
        }
        passed.is_some() || stopped() || attempts.len() >= max_attempts
    };

    // 1. the settings that matter
    let l1 = "the settings that matter";
    let a1 = attempt(1);
    let (c1, b1, rep1) = screen(&a1, l1, base, opt::candidates(base, &only))?;
    let (dims1, keys1) = picked_of(&c1, &rep1);
    let r1 = (!dims1.is_empty() && !stopped()).then(|| tune(&a1, l1, base, &dims1, runs_for(dims1.len())));
    let flat1: Vec<String> = r1.as_ref().and_then(|r| r["flat"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())).unwrap_or_default();
    let best1 = r1.as_ref().and_then(|r| r["top"].get(0).cloned());
    'tries: {
        if settle(&a1, "tune", l1, keys1.clone(), r1, "no setting moved the reliable return by a point or more over its range", screen_runs(&rep1), &mut attempts, &mut shown) {
            break 'tries;
        }
        // 2. wider ranges
        let l2 = "wider ranges";
        let a2 = attempt(2);
        let mut keys2: Vec<String> = keys1.iter().filter(|k| !flat1.contains(k)).cloned().collect();
        for e in opt::edge_keys(&rep1, best1.as_ref(), &dims1) {
            if !flat1.contains(&e) && !keys2.contains(&e) {
                keys2.push(e);
            }
        }
        let specs2: Vec<Value> = opt::candidates_by(base, &only, 4.0).into_iter()
            .filter(|c| c["param"] != "tf" && keys2.contains(&format!("{}.{}", c["node"].as_str().unwrap_or(""), c["param"].as_str().unwrap_or("")))).collect();
        let (c2, _, rep2) = screen(&a2, l2, base, specs2)?;
        let (dims2, k2) = picked_of(&c2, &rep2);
        let r2 = (!dims2.is_empty() && !stopped()).then(|| tune(&a2, l2, base, &dims2, runs_for(dims2.len())));
        if settle(&a2, "wider", l2, k2, r2, "nothing to widen: no setting that mattered moved the score over a wider range", screen_runs(&rep2), &mut attempts, &mut shown) {
            break 'tries;
        }
        // 3. small steps
        let l3 = "small steps";
        let a3 = attempt(3);
        let mut by_imp: Vec<&Value> = rep1.iter().filter(|r| r["choices"].is_null() && r["importance"].as_f64().unwrap_or(0.0) >= 1.0
            && !flat1.iter().any(|f| r["key"] == f.as_str())).collect();
        by_imp.sort_by(|a, b| b["importance"].as_f64().unwrap_or(0.0).partial_cmp(&a["importance"].as_f64().unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal));
        let keys3: Vec<String> = by_imp.iter().take(3).filter_map(|r| r["key"].as_str().map(String::from)).collect();
        let specs3: Vec<Value> = opt::candidates_by(base, &only, 1.3).into_iter()
            .filter(|c| c["param"] != "tf" && keys3.contains(&format!("{}.{}", c["node"].as_str().unwrap_or(""), c["param"].as_str().unwrap_or("")))).collect();
        let dims3 = if specs3.is_empty() { vec![] } else { opt::dims_upto(base, &Value::Array(specs3), opt::MAX_SCREEN)? };
        let r3 = (!dims3.is_empty() && !stopped()).then(|| tune(&a3, l3, base, &dims3, runs_for(dims3.len()).min(90)));
        if settle(&a3, "closer", l3, keys3, r3, "no setting moved the score enough to step it", 0, &mut attempts, &mut shown) {
            break 'tries;
        }
        // 4. changes to the blocks, one at a time
        let l4 = "changes to the blocks";
        let a4 = attempt(4);
        let vars = opt::variants(base, &only);
        let scored: Mutex<Vec<Option<(f64, Value)>>> = Mutex::new(vec![None; vars.len()]);
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        std::thread::scope(|sc| {
            for _ in 0..workers.min(vars.len().max(1)) {
                sc.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    if k >= vars.len() || stopped() {
                        break;
                    }
                    let v = (a4.eval)(&vars[k].1).ok().filter(|x| x.0.is_finite());
                    scored.lock().unwrap()[k] = v;
                    report(4, l4, "trying", done.fetch_add(1, Ordering::SeqCst) + 1, vars.len(), Value::Null);
                });
            }
        });
        let cur_score = a4.current["score"].as_f64().unwrap_or(f64::NEG_INFINITY);
        let mut better: Vec<(f64, Value, Map<String, Value>)> = scored.into_inner().unwrap().into_iter().zip(&vars)
            .filter_map(|(s, (d, st))| s.filter(|s| s.0 > cur_score).map(|(sc, mut row)| {
                row["change"] = d.clone();
                (sc, row, st.clone())
            })).collect();
        better.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        better.truncate(12);
        let lead = better.first().map(|b| (b.1["change"].clone(), b.2.clone()));
        let r4 = (!better.is_empty()).then(|| {
            let (rows, strats): (Vec<Value>, Vec<Map<String, Value>>) = better.into_iter().map(|(_, r, s)| (r, s)).unzip();
            json!({"top": finish(&a4, rows, strats), "runs": vars.len(), "rounds": 1, "variants": vars.len()})
        });
        let note4 = if vars.is_empty() { "no change to the blocks to try: no exit, hold or optional input, and no block with a stand-in".to_string() }
                    else { format!("none of {} single changes to the blocks did better over the folds", vars.len()) };
        // its runs: every variant tried (the rows count none of their own)
        let tried = if r4.is_some() { 0 } else { vars.len() };
        if settle(&a4, "structure", l4, vec![], r4, &note4, tried, &mut attempts, &mut shown) {
            break 'tries;
        }
        // 5. the best change, then tuned
        let l5 = "a change, then tuned";
        let a5 = attempt(5);
        let Some((change, base5)) = lead else {
            settle(&a5, "structure_tuned", l5, vec![], None, "no change to the blocks did better over the folds to start from", 0, &mut attempts, &mut shown);
            break 'tries;
        };
        let (c5, _, rep5) = screen(&a5, l5, &base5, opt::candidates(&base5, &only))?;
        let (dims5, keys5) = picked_of(&c5, &rep5);
        let r5 = (!dims5.is_empty() && !stopped()).then(|| {
            let mut r = tune(&a5, l5, &base5, &dims5, runs_for(dims5.len()));
            if let Some(a) = r["top"].as_array_mut() {
                for t in a {
                    t["change"] = change.clone();
                }
            }
            r
        });
        settle(&a5, "structure_tuned", l5, keys5, r5, "after the change no setting moved the score", screen_runs(&rep5), &mut attempts, &mut shown);
    }
    // every period, the five windows and the exam, for the current settings, each attempt's best and
    // two references nothing was tuned on: holding, and the strategy without its exits and holds. The
    // current settings may have been picked on this very history (then every period is in-sample for
    // them), so how each fares across all periods, and against the references, is shown side by side.
    let mut cols: Vec<(i64, i64)> = plan.iter().map(|p| p.0).collect();
    cols.sort();
    cols.push(exam);
    let mut subjects: Vec<(String, Map<String, Value>)> = vec![("current settings".into(), base.clone())];
    for sh in &shown {
        if let Some(st) = sh["top"][0]["strategy"].as_object() {
            subjects.push((format!("attempt {} best", sh["attempt"]), st.clone()));
        }
    }
    let mut band = base.clone();
    if let Some(a) = band.get_mut("nodes").and_then(|n| n.as_array_mut()) {
        a.retain(|n| !["exit", "hold"].contains(&n["kind"].as_str().unwrap_or("")));
    }
    if band != *base && crate::graph::Graph::parse(&band, 10).is_ok() {
        subjects.push(("without its exits".into(), band));
    }
    let cells: Vec<(usize, usize)> = (0..subjects.len()).flat_map(|i| (0..cols.len()).map(move |j| (i, j))).collect();
    let got: Mutex<Vec<Option<Value>>> = Mutex::new(vec![None; cells.len()]);
    let next = AtomicUsize::new(0);
    if !stopped() {
        step(id, "running every period for the comparison");
        std::thread::scope(|sc| {
            for _ in 0..workers.min(cells.len().max(1)) {
                sc.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    if k >= cells.len() {
                        break;
                    }
                    let (i, j) = cells[k];
                    let v = span(&subjects[i].1, cols[j].0, cols[j].1).ok().map(|x| x.1);
                    got.lock().unwrap()[k] = v;
                });
            }
        });
    }
    let got = got.into_inner().unwrap();
    let cell = |i: usize, j: usize, k: &str| got[i * cols.len() + j].as_ref().and_then(|v| v[k].as_f64()).filter(|x| x.is_finite());
    let mut prow: Vec<Value> = subjects.iter().enumerate().map(|(i, (name, _))| {
        let v: Vec<f64> = (0..cols.len()).map(|j| cell(i, j, key).unwrap_or(f64::NAN)).collect();
        json!({"name": name, "values": v.iter().map(|x| if x.is_finite() { json!(x) } else { Value::Null }).collect::<Vec<_>>(), "score": opt::composite(&v).1})
    }).collect();
    if !vs_hold {
        // holding the first deposit's tokens: the return less the return over holding, of the current settings' runs
        let v: Vec<f64> = (0..cols.len()).map(|j| match (cell(0, j, "apr"), cell(0, j, "vs_hold_apr")) { (Some(a), Some(h)) => a - h, _ => f64::NAN }).collect();
        prow.push(json!({"name": "holding", "values": v.iter().map(|x| if x.is_finite() { json!(x) } else { Value::Null }).collect::<Vec<_>>(), "score": opt::composite(&v).1}));
    }
    let periods = json!({"cols": cols.iter().map(|c| json!({"from": c.0, "to": c.1, "days": day(c.0, c.1), "exam": *c == exam})).collect::<Vec<_>>(), "rows": prow});
    let ran = attempts.iter().filter(|a| a["ran"] == true).count();
    // the attempt shown: the one that passed, else the one whose best did best over its periods
    let pick_i = passed.or_else(|| (0..shown.len()).max_by(|a, b| {
        let f = |i: usize| shown[i]["top"].get(0).map(|t| sc_all(t)).unwrap_or(f64::NEG_INFINITY);
        f(*a).partial_cmp(&f(*b)).unwrap_or(std::cmp::Ordering::Equal)
    }));
    let mut r = match pick_i {
        Some(i) => shown[i].clone(),
        None => json!({"top": [], "runs": 0, "rounds": 0, "current": a1.current.clone()}),
    };
    if passed.is_none() {
        let closest = pick_i.map(|i| format!(" The closest was attempt {} ({}): {}.", shown[i]["attempt"], shown[i]["label"].as_str().unwrap_or(""),
                                                shown[i]["suggestion"]["why"].as_str().unwrap_or("").trim_start_matches("keep the current settings: ")))
            .unwrap_or_default();
        r["suggestion"] = json!({"apply": null, "why": format!("keep the current settings: {} found nothing that holds up where it was not tuned.{closest}",
                                                               match ran { 0 => "screening".to_string(), 1 => "the attempt".to_string(), n => format!("{n} attempts") })});
    }
    let fd = plan[0].1.first().map(|f| day(f.0, f.1)).unwrap_or(0.0);
    r["auto"] = json!(true);
    r["attempts"] = json!(attempts);
    r["screen"] = json!(rep1);
    r["periods"] = periods;
    r["screen_base"] = json!(b1);
    r["runs_total"] = json!(total_runs);
    r["stopped"] = json!(stopped());
    r["windows"] = json!({"exam": {"from": exam.0, "to": exam.1, "days": day(exam.0, exam.1)}, "n": WINDOWS, "days": day(plan[0].0 .0, plan[0].0 .1),
                          "fold_days": fd, "exams": exams, "period": {"from": t_a, "to": t_b}});
    r["folds"] = json!({"n": plan[0].1.len(), "days": fd});
    r["train"] = json!({"from": t_a, "to": exam.0, "days": day(t_a, exam.0)});
    Ok(r)
}

/// Periods a walk-forward may cut the span into (a month each over three years).
const MAX_PERIODS: usize = 36;

/// The periods of a walk-forward over [t_a, t_b): `n` equal periods, the first test one, and the
/// training periods of test period t (the `train` before it, or every one before it when 0).
fn walk_plan(t_a: i64, t_b: i64, n: usize, train: usize) -> (Vec<(i64, i64)>, usize, impl Fn(usize) -> std::ops::Range<usize>) {
    let per: Vec<(i64, i64)> = (0..n).map(|k| (t_a + (t_b - t_a) * k as i64 / n as i64, t_a + (t_b - t_a) * (k + 1) as i64 / n as i64)).collect();
    let first = if train == 0 { (n / 4).max(2) } else { train.max(2) };
    (per, first, move |t: usize| if train == 0 { 0..t } else { t.saturating_sub(train)..t })
}

/// Walk-forward of the tuning itself, the honest test of whether tuning helps: the span is cut into
/// `periods` equal periods (a month each by default). At every period from the first test one on,
/// the settings are tuned on the periods before it only (the last `train` of them, each its own
/// fold; 0: all before), then run on that period, which nothing had seen. Joined up, those runs are
/// what re-tuning every period would have earned. The current settings and two references nothing
/// was tuned on (the strategy without its exits and holds, and holding) run on the same periods.
/// Which settings are tuned is screened once, on the first training span: before any test period.
/// Re-tuning is suggested, with the settings tuned on the most recent periods, only if it beats the
/// current settings and both references out of sample, over most periods. The current settings may
/// have been picked on this very history: then their numbers here are in-sample and flattering.
#[allow(clippy::too_many_arguments)]
fn walk_forward(id: u64, spec: &Value, base: &Map<String, Value>, workers: usize, span: &Span, period: (i64, i64),
                vs_hold: bool, flag: &std::sync::atomic::AtomicBool) -> Result<Value, String> {
    use crate::optimize as opt;
    let key = if vs_hold { "vs_hold_apr" } else { "apr" };
    let (t_a, t_b) = period;
    let day = |a: i64, b: i64| (b - a) as f64 / 86400.0;
    let total = day(t_a, t_b);
    let n = spec["periods"].as_u64().map(|x| x as usize).unwrap_or(((total / 30.4).round() as usize).max(4)).clamp(4, MAX_PERIODS);
    if total / (n as f64) < 3.0 {
        return Err(format!("{n} periods of {:.1} days are too short to trade on: fewer periods or a longer span", total / n as f64));
    }
    let train = (spec["train"].as_u64().unwrap_or(6) as usize).min(n - 2);
    let (per, first, train_of) = walk_plan(t_a, t_b, n, train);
    if first >= n {
        return Err("no period left to test on: fewer training periods or more periods".into());
    }
    let steps = n - first;
    let runs = spec["runs"].as_u64().map(|r| r as usize).unwrap_or(60).clamp(10, 400);
    let only: Vec<String> = spec["nodes"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let stopped = || flag.load(Ordering::SeqCst);
    let report = |phase: &str, step: usize, k: usize, total: usize| {
        let what = if step == 0 { "screening which settings matter".to_string() } else if step > steps { "tuning on the most recent periods".to_string() } else { format!("period {step} of {steps}: tuning on the periods before it") };
        set_job(id, json!({"id": id, "state": "running", "step": format!("walk-forward, {what}: {k} of {total} runs"),
                           "partial": {"mode": "walk", "phase": phase, "step": step, "steps": steps, "done": k, "total": total}}));
    };
    let folds_of = |t: usize| -> Vec<(i64, i64)> { train_of(t).map(|k| per[k]).collect() };

    // which settings to tune: screened on the first training span only
    let f0 = folds_of(first);
    let ev0 = |s: &Map<String, Value>| eval_over(span, &f0, s);
    let cands = opt::dims_upto(base, &Value::Array(opt::candidates(base, &only)), opt::MAX_SCREEN)?;
    let (b0, rep) = opt::screen(base, &cands, workers, &ev0, flag, &|k, t| report("screening", 0, k, t));
    let imp: Vec<f64> = rep.iter().map(|r| r["importance"].as_f64().unwrap_or(0.0)).collect();
    let pk = opt::pick(&imp);
    let dims: Vec<opt::Dim> = pk.iter().map(|i| cands[*i].clone()).collect();
    let keys: Vec<String> = dims.iter().map(|d| d.key()).collect();

    // tuned on `folds`: the best settings and its row (the current ones when nothing is tuned)
    let tune = |folds: &[(i64, i64)], step: usize| -> (Map<String, Value>, Value) {
        if dims.is_empty() {
            return (base.clone(), Value::Null);
        }
        let ev = |s: &Map<String, Value>| eval_over(span, folds, s);
        let r = opt::search(base, &dims, &opt::Opts { runs, workers }, &ev, flag, &|k, _| report("tuning", step, k, runs));
        match r["top"].get(0).cloned() {
            Some(t) => {
                let x: Vec<f64> = dims.iter().map(|d| t["values"][d.key().as_str()].as_f64().unwrap_or(f64::NAN)).collect();
                if x.iter().all(|v| v.is_finite()) { (opt::apply(base, &dims, &x), t) } else { (base.clone(), t) }
            }
            None => (base.clone(), Value::Null),
        }
    };
    let mut wf: Vec<f64> = vec![];
    let mut step_rows: Vec<Value> = vec![];
    for t in first..n {
        if stopped() {
            break;
        }
        let folds = folds_of(t);
        let (tuned, row) = tune(&folds, t - first + 1);
        let oos = span(&tuned, per[t].0, per[t].1).map(|x| x.0).unwrap_or(f64::NAN);
        wf.push(oos);
        step_rows.push(json!({"test": {"from": per[t].0, "to": per[t].1}, "train": {"from": folds[0].0, "to": folds[folds.len() - 1].1, "periods": folds.len()},
                              "values": row["values"], "train_score": row["score"], "oos": oos}));
    }
    let tested = wf.len();
    let tests: Vec<(i64, i64)> = (first..first + tested).map(|t| per[t]).collect();

    // the current settings and the references on the same periods
    let mut band = base.clone();
    if let Some(a) = band.get_mut("nodes").and_then(|n| n.as_array_mut()) {
        a.retain(|n| !["exit", "hold"].contains(&n["kind"].as_str().unwrap_or("")));
    }
    let has_band = band != *base && crate::graph::Graph::parse(&band, 10).is_ok();
    let subjects: Vec<&Map<String, Value>> = if has_band { vec![base, &band] } else { vec![base] };
    let cells: Vec<(usize, usize)> = (0..subjects.len()).flat_map(|i| (0..tests.len()).map(move |j| (i, j))).collect();
    let got: Mutex<Vec<Option<Value>>> = Mutex::new(vec![None; cells.len()]);
    let next = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..workers.min(cells.len().max(1)) {
            sc.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::SeqCst);
                if k >= cells.len() {
                    break;
                }
                let (i, j) = cells[k];
                let v = span(subjects[i], tests[j].0, tests[j].1).ok().map(|x| x.1);
                got.lock().unwrap()[k] = v;
            });
        }
    });
    let got = got.into_inner().unwrap();
    let cell = |i: usize, j: usize, k: &str| got[i * tests.len() + j].as_ref().and_then(|v| v[k].as_f64()).filter(|x| x.is_finite()).unwrap_or(f64::NAN);
    let cur: Vec<f64> = (0..tested).map(|j| cell(0, j, key)).collect();
    let bandv: Vec<f64> = if has_band { (0..tested).map(|j| cell(1, j, key)).collect() } else { vec![] };
    let hold: Vec<f64> = if vs_hold { vec![] } else { (0..tested).map(|j| cell(0, j, "apr") - cell(0, j, "vs_hold_apr")).collect() };
    let row = |name: &str, v: &[f64]| json!({"name": name, "values": v.iter().map(|x| if x.is_finite() { json!(x) } else { Value::Null }).collect::<Vec<_>>(), "score": opt::composite(v).1});
    let mut rows = vec![row("re-tuned every period", &wf), row("current settings", &cur)];
    if has_band {
        rows.push(row("without its exits", &bandv));
    }
    if !hold.is_empty() {
        rows.push(row("holding", &hold));
    }
    let sc = |v: &[f64]| if v.is_empty() { f64::NEG_INFINITY } else { opt::composite(v).0 };
    let (s_wf, s_cur, s_band, s_hold) = (sc(&wf), sc(&cur), sc(&bandv), sc(&hold));
    let wins = wf.iter().zip(&cur).filter(|(a, b)| a > b).count();
    let s_ref = s_band.max(s_hold);
    // the current settings picked on this history (the default; built-ins were): every period is
    // in-sample for them, so they are no fair bar, and re-tuning is judged against the references
    // nothing was tuned on, period by period against the better of them
    let cur_in = spec["current_in_sample"].as_bool().unwrap_or(true);
    let refv: Vec<f64> = (0..tested).map(|j| bandv.get(j).copied().unwrap_or(f64::NAN).max(hold.get(j).copied().unwrap_or(f64::NAN))).collect();
    let wins_ref = wf.iter().zip(&refv).filter(|(a, b)| a > b).count();
    // the edge itself, period by period: re-tuning less the bar, which must be reliably above zero
    // (its own mean − 1.64·sd/√n), not just higher on average: months swing by more than any edge
    let bar: &[f64] = if cur_in { &refv } else { &cur };
    let diffs: Vec<f64> = wf.iter().zip(bar).map(|(a, b)| a - b).collect();
    let (edge_lb, edge) = opt::composite(&diffs);
    let edge_mean = edge["mean"].as_f64().unwrap_or(f64::NAN);
    // how much the tuned settings moved from one period to the next: a large spread is tuning that
    // chases noise
    let stability: Vec<Value> = dims.iter().map(|d| {
        let us: Vec<f64> = step_rows.iter().filter_map(|r| r["values"][d.key().as_str()].as_f64()).map(|v| d.unit_of(v)).collect();
        let m = us.iter().sum::<f64>() / us.len().max(1) as f64;
        let sd = (us.iter().map(|u| (u - m).powi(2)).sum::<f64>() / us.len().max(1) as f64).sqrt();
        json!({"key": d.key(), "spread": sd, "values": step_rows.iter().map(|r| r["values"][d.key().as_str()].clone()).collect::<Vec<_>>()})
    }).collect();
    let unstable = stability.iter().filter(|x| x["spread"].as_f64().unwrap_or(0.0) > 0.3).count();
    let pdays = day(per[0].0, per[0].1);
    let train_txt = if train == 0 { "all the periods before".to_string() } else { format!("the {} periods before", train) };
    let mut top: Vec<Value> = vec![];
    let passed = tested >= 3 && !dims.is_empty() && s_wf > s_ref && edge_lb > 0.0
        && if cur_in { wins_ref * 2 > tested } else { s_wf > s_cur && wins * 2 > tested };
    let edge_txt = format!("the edge per period averaged {edge_mean:+.1} points, reliably at least {edge_lb:+.1}");
    let mut why = if dims.is_empty() {
        "keep the current settings: no setting moved the reliable return by a point or more on the first training span, so there is nothing to tune".to_string()
    } else if tested < 3 {
        format!("too few periods tested ({tested}) to judge: a longer span or shorter periods")
    } else if passed && cur_in {
        format!("re-tuning every {pdays:.0} days on {train_txt} beat the references nothing was tuned on, out of sample ({s_wf:+.1} vs {s_ref:+.1} %/yr reliable over {tested} periods, better in {wins_ref} of them; {edge_txt}): apply the settings tuned on the most recent periods, and re-tune every {pdays:.0} days. The current settings show {s_cur:+.1} here, but they were picked on this history, so that number is in-sample and no fair bar")
    } else if cur_in && s_wf > s_ref && wins_ref * 2 > tested {
        format!("no new settings: re-tuning did better than the references nothing was tuned on ({s_wf:+.1} vs {s_ref:+.1} %/yr reliable, better in {wins_ref} of {tested} periods), but not reliably: {edge_txt}, so it may be luck. The current settings' {s_cur:+.1} is in-sample (picked on this history): only time after they were picked can test them")
    } else if cur_in {
        format!("keep what you have, but not because it is proven: re-tuned on the past, the settings did not beat the references nothing was tuned on out of sample ({s_wf:+.1} vs {s_ref:+.1} %/yr reliable, better in {wins_ref} of {tested} periods). The current settings' {s_cur:+.1} is in-sample (picked on this history): only time after they were picked can test them")
    } else if passed {
        format!("re-tuning every {pdays:.0} days on {train_txt} beat the current settings out of sample ({s_wf:+.1} vs {s_cur:+.1} %/yr reliable over {tested} periods, better in {wins} of them; {edge_txt}) and the references ({s_ref:+.1}): apply the settings tuned on the most recent periods, and re-tune every {pdays:.0} days")
    } else if s_wf <= s_cur {
        format!("keep the current settings: re-tuned on the past, the settings did worse on the period after ({s_wf:+.1} vs {s_cur:+.1} %/yr reliable over {tested} periods, better in {wins}); tuning chased the past. If the current settings were picked on this history, their {s_cur:+.1} is in-sample and flatters them")
    } else if wins * 2 <= tested {
        format!("keep the current settings: re-tuning scored higher overall ({s_wf:+.1} vs {s_cur:+.1}) but did better in only {wins} of {tested} periods: a few large months, not a steady edge")
    } else if s_wf <= s_ref {
        format!("keep the current settings: re-tuning beat them ({s_wf:+.1} vs {s_cur:+.1}) but not the references nothing was tuned on ({s_ref:+.1}): tuning does not earn its keep here")
    } else {
        format!("keep the current settings: re-tuning did better ({s_wf:+.1} vs {s_cur:+.1} %/yr reliable, better in {wins} of {tested} periods), but not reliably: {edge_txt}, so it may be luck")
    };
    if s_ref > s_cur.max(s_wf) && tested >= 3 {
        let which = if s_band >= s_hold { "the strategy without its exits" } else { "holding" };
        why += &format!(". Note: {which} did best of all out here ({s_ref:+.1} %/yr reliable): neither the strategy's timing nor its tuning beat it on these periods");
    }
    if unstable > 0 && !dims.is_empty() {
        why += &format!(". {unstable} of {} tuned settings jumped around from period to period: tuning them chases noise", dims.len());
    }
    if passed && !stopped() {
        // the settings to use now: tuned on the most recent periods, as the walk would next
        let folds = folds_of(n);
        let (now, mut r) = tune(&folds, steps + 1);
        r["strategy"] = Value::Object(now);
        r["now"] = json!(true);
        top.push(r);
    }
    let suggestion = json!({"apply": if passed && !top.is_empty() { json!(0) } else { Value::Null }, "why": why});
    let mut current = json!({});
    current["wf"] = rows[1]["score"].clone();
    Ok(json!({"auto": true, "mode": "walk", "top": top, "current": current, "suggestion": suggestion, "stopped": stopped(), "picked": keys,
              "screen": rep, "screen_base": b0, "runs": 0, "rounds": 0,
              "walk": {"current_in_sample": cur_in, "edge": edge, "periods": n, "period_days": pdays, "train": train, "first": first, "tested": tested, "runs_per_period": runs,
                       "cols": tests.iter().map(|c| json!({"from": c.0, "to": c.1})).collect::<Vec<_>>(), "rows": rows, "steps": step_rows,
                       "stability": stability, "span": {"from": t_a, "to": t_b}}}))
}

#[cfg(test)]
mod walk_tests {
    use super::*;

    #[test]
    fn each_period_is_tuned_on_the_past_only() {
        let d = 86400;
        let (per, first, train_of) = walk_plan(0, 24 * 30 * d, 24, 6);
        assert_eq!(per.len(), 24);
        assert_eq!(first, 6);
        for t in first..24 {
            let tr: Vec<usize> = train_of(t).collect();
            assert_eq!(tr.len(), 6);
            assert!(tr.iter().all(|k| per[*k].1 <= per[t].0), "period {t} trained on {tr:?}: only periods that ended before it");
            assert_eq!(*tr.last().unwrap(), t - 1, "up to the one right before");
        }
        // anchored: everything before
        let (_, f2, all) = walk_plan(0, 24 * 30 * d, 24, 0);
        assert_eq!(f2, 6);
        assert_eq!(all(10).collect::<Vec<_>>(), (0..10).collect::<Vec<_>>());
    }
}

/// Over all an attempt's periods: its folds and its validation window, scored alike.
fn all_periods(t: &Value, key: &str) -> Value {
    let mut v: Vec<f64> = t["wf"]["folds"].as_array().map(|a| a.iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect()).unwrap_or_default();
    v.push(t["test"][key].as_f64().unwrap_or(f64::NAN));
    crate::optimize::composite(&v).1
}

#[cfg(test)]
mod window_tests {
    use super::*;

    #[test]
    fn every_attempt_validates_on_a_window_of_its_own_and_none_touches_the_exam() {
        let (t_a, t_b) = (0i64, 400 * 86400);
        let (exam, plan) = auto_windows(t_a, t_b, 2);
        assert_eq!(exam, (350 * 86400, 400 * 86400), "the last eighth");
        assert_eq!(plan.len(), WINDOWS);
        let vals: Vec<(i64, i64)> = plan.iter().map(|p| p.0).collect();
        // the most recent first, each a different one
        assert_eq!(vals[0], (280 * 86400, 350 * 86400));
        for i in 0..vals.len() {
            for j in 0..i {
                assert!(vals[i].1 <= vals[j].0 || vals[i].0 >= vals[j].1, "windows overlap: {:?} {:?}", vals[i], vals[j]);
            }
        }
        for (val, folds) in &plan {
            assert_eq!(folds.len(), (WINDOWS - 1) * 2);
            for f in folds {
                assert!(f.1 <= exam.0, "a fold in the exam");
                assert!(f.1 <= val.0 || f.0 >= val.1, "a fold in its own validation window");
            }
            // folds and window cover everything before the exam
            let covered: i64 = folds.iter().map(|f| f.1 - f.0).sum::<i64>() + (val.1 - val.0);
            assert_eq!(covered, exam.0 - t_a);
        }
    }
}

/// A replay and the bars of the period on it.
pub struct Rp<'a> {
    c: Ctx<'a>,
    b0: usize,
    b1: usize,
}

/// The replay a strategy runs on: the 1-minute one for a strategy that decides every minute.
fn pick<'r, 'a>(rp5: &'r Rp<'a>, rp60: &'r Option<Rp<'a>>, s: &Map<String, Value>, spacing: i64) -> &'r Rp<'a> {
    match rp60 {
        Some(r) if bar_of(s, spacing) == 60 => r,
        _ => rp5,
    }
}

/// How often a strategy decides, in seconds: a node strategy with a 1-minute block every minute,
/// everything else every 5-minute bar.
pub fn bar_of(s: &Map<String, Value>, spacing: i64) -> i64 {
    if s.get("type").and_then(|t| t.as_str()) == Some("graph") {
        if let Ok(g) = crate::graph::Graph::parse(s, spacing) {
            return g.bar_s();
        }
    }
    data::BAR_S
}

fn strategy(s: &Map<String, Value>, spacing: i64, max_ranges: usize, pegged: bool) -> Result<Strategy, String> {
    match s.get("type").and_then(|t| t.as_str()).unwrap_or("custom") {
        "custom" => Ok(Strategy::Custom(Rules::from(s, spacing)?)),
        "graph" => {
            let mut g = crate::graph::Graph::parse(s, spacing)?;
            g.max_ranges = max_ranges.clamp(1, 50);
            Ok(Strategy::Graph(Box::new(g)))
        }
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

/// For each token, its whitelisted pegged pool's day-by-day rate from `from` (`ParkRate`), its hourly
/// history downloaded back that far (a pool younger than that has history from its first hour only,
/// and nothing parks in it before). Not whitelisted: no rate. The live switch (Settings › Options)
/// does not matter here: a backtest shows what parking would earn.
fn park_rates(cid: u64, tokens: [&str; 2], from: i64, cost: &Costs, size_usd: f64) -> [ParkRate; 2] {
    let s = crate::settings::load_settings();
    let rate = |t: &str| -> ParkRate {
        let Some(w) = crate::parking::whitelisted(&s, cid, t) else { return ParkRate::default() };
        match apr_rows(cid, &w.pool, from - 3600) {
            Ok(rows) => ParkRate::from_rows(&w.pool, &rows, cost, size_usd),
            Err(e) => ParkRate { pool: w.pool.clone(), error: Some(e), ..Default::default() },
        }
    };
    [rate(tokens[0]), rate(tokens[1])]
}

/// One token's parking in the notes: its average over the period, when its pegged pool's history
/// begins inside the period, or why there is none.
fn park_note(sym: &str, r: &ParkRate, from: i64, to: i64) -> String {
    if r.pool.is_empty() {
        return format!("{sym}: not whitelisted");
    }
    if let Some(e) = &r.error {
        return format!("{sym}: no history of its pegged pool ({e}), so it earns nothing");
    }
    let begins = match r.first {
        None => " (its pegged pool has no history: nothing)".to_string(),
        Some(f) if f > from => format!(" (its pegged pool's history begins {}, nothing before)", crate::history::day_name(f)),
        _ => String::new(),
    };
    format!("{sym} {:.1}% on average{begins}", r.mean(from, to) * 100.0)
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
        Market { ts, tick: ticks.to_vec(), sp, fee: fees, l_others: vec![others; n], l_active: vec![others; n], bar_end, bar_idx, bar_tick, bar_hi, bar_lo, bar_fee, decimals: 6, px_scale: 1.0, liq_scale: 1e6, num1: true, bar_s: 300 }
    }

    fn ctx(m: &Market) -> Ctx<'_> {
        Ctx { m, spacing: 1, num1: false, num_per_usd: 1.0, emis_usd_h: HashMap::new(), sqrt_px: 1.0, beta: 1.0, park: Default::default(), cid: 8453, num_token: String::new() }
    }

    fn no_costs() -> Costs {
        Costs { swap_bps: 0.0, gas_usd: 0.0, fee_haircut: 1.0, emission_haircut: 1.0, stable_swap_bps: 0.0 }
    }

    fn custom(v: Value) -> Strategy {
        Strategy::Custom(Rules::from(v.as_object().unwrap(), 1).unwrap())
    }

    #[test]
    fn an_exit_to_the_stable_holds_dollars_and_buys_the_numeraire_back() {
        let m = market(&vec![0; 100], 0.0, 1e9);
        let c = ctx(&m);
        let mut sim = Sim { x: 100.0, upn: 2000.0, ..Default::default() };
        sim.exit(&c, &no_costs(), 0, 0, ExitTo::Stable, "test");
        assert_eq!((sim.x, sim.y, sim.usd), (0.0, 0.0, 200_000.0), "everything sold for dollars");
        // the numeraire halves against the dollar: the dollars buy twice as much of it
        sim.upn = 1000.0;
        assert!((sim.usd_value() - 200.0).abs() < 1e-9);
        sim.from_usd(&c, &Costs { stable_swap_bps: 10.0, ..no_costs() });
        assert!((sim.x - 199.8).abs() < 1e-9 && sim.usd == 0.0, "bought back, less the swap: {}", sim.x);
        assert!((sim.swap_costs - 0.2).abs() < 1e-9);
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
    fn parking_earns_while_waiting_and_pays_its_moves() {
        // flat price; an exit trigger above the price sends everything into token0 for the whole run
        let m = market(&vec![0; 6000], 0.0, 1e9);
        let mut c = ctx(&m);
        let rules = |park: bool| custom(json!({"width": 10, "exit_lo": 5, "exit_to": "token0", "park": park}));
        let cost = Costs { swap_bps: 4.0, gas_usd: 0.5, fee_haircut: 1.0, emission_haircut: 1.0, stable_swap_bps: 0.0 };
        let b1 = m.bar_end.len() - 1;
        c.park = [ParkRate::flat(0.10), ParkRate::flat(0.0)];
        let off = run(&c, &rules(false), &cost, 10_000.0, 0, b1);
        let on = run(&c, &rules(true), &cost, 10_000.0, 0, b1);
        let f = |r: &Value, k: &str| r[k].as_f64().unwrap();
        assert_eq!(f(&off, "park_yield"), 0.0, "parking is the strategy's choice");
        let days = (m.bar_end[b1] - m.bar_end[0]) as f64 / 86400.0;
        let want = 10_000.0 * 0.10 * days / 365.0;
        assert!((f(&on, "park_yield") / want - 1.0).abs() < 0.05, "{} vs {want}", f(&on, "park_yield"));
        // in: a swap of half the value and two transactions
        assert!((f(&on, "gas") - f(&off, "gas") - 1.0).abs() < 1e-9);
        assert!((f(&on, "swap_costs") - f(&off, "swap_costs") - 10_000.0 * 0.5 * 4e-4).abs() < 0.05);
        let lhs = f(&on, "value") - f(&on, "hodl_value");
        let rhs = f(&on, "fees") + f(&on, "emissions") + f(&on, "park_yield") - f(&on, "swap_costs") - f(&on, "gas") + f(&on, "il");
        assert!((lhs - rhs).abs() < 1e-4, "{lhs} vs {rhs}");
        // not whitelisted: nothing parked, nothing paid
        c.park = [ParkRate::flat(0.0), ParkRate::flat(0.10)];
        let none = run(&c, &rules(true), &cost, 10_000.0, 0, b1);
        assert_eq!(f(&none, "park_yield"), 0.0);
        assert_eq!(f(&none, "gas"), f(&off, "gas"));
    }

    #[test]
    fn parking_earns_nothing_before_its_pegged_pool_was_there() {
        // the pegged pool's history begins a third of the way in, paying 10% a year from then
        let m = market(&vec![0; 6000], 0.0, 1e9);
        let b1 = m.bar_end.len() - 1;
        let (t0, t1) = (m.bar_end[0], m.bar_end[b1]);
        let born = t0 + (t1 - t0) / 3;
        let rows: Vec<Value> = (born.div_euclid(3600)..=t1.div_euclid(3600) + 1)
            .map(|h| json!({"timestamp": h * 3600, "feesUSD": 0.10 * 1e6 / 8760.0, "emissionsUSD": 0, "activeTvlUSD": 1e6})).collect();
        let r = ParkRate::from_rows("0xpeg", &rows, &no_costs(), 0.0);
        assert_eq!(r.at(born - 3600), 0.0, "before its first hour");
        assert!((r.at(born + 86400) - 0.10).abs() < 1e-9, "{}", r.at(born + 86400));
        // a strategy that exits at once and waits in token0: it earns from the pool's birth on
        let rules = custom(json!({"width": 10, "exit_lo": 5, "exit_to": "token0", "park": true}));
        let mut c = ctx(&m);
        c.park = [r.clone(), ParkRate::default()];
        let late = run(&c, &rules, &no_costs(), 10_000.0, 0, b1);
        // a pool there from the start, same rate, earns over the whole period
        c.park = [ParkRate::flat(0.10), ParkRate::default()];
        let all = run(&c, &rules, &no_costs(), 10_000.0, 0, b1)["park_yield"].as_f64().unwrap();
        // the late one parks the day its pool appears and earns the last two thirds only
        let late = late["park_yield"].as_f64().unwrap();
        assert!((late / all - 2.0 / 3.0).abs() < 0.05, "{late} vs {all}");
        // a day with no rows inside the pool's life earns nothing, and the mean counts it as nothing
        let gap: Vec<Value> = rows.iter().filter(|x| x["timestamp"].as_i64().unwrap().div_euclid(86400) != (born + 86400).div_euclid(86400)).cloned().collect();
        let g = ParkRate::from_rows("0xpeg", &gap, &no_costs(), 0.0);
        assert_eq!(g.at(born + 86400), 0.0);
        assert!(g.mean(born, t1) < r.mean(born, t1));
        // a missing hour does not inflate a day: income over the average liquidity, over a year
        let thin: Vec<Value> = rows.iter().filter(|x| x["timestamp"].as_i64().unwrap() % 7200 == 0).cloned().collect();
        let th = ParkRate::from_rows("0xpeg", &thin, &no_costs(), 0.0);
        assert!((th.at(born + 86400) - 0.05).abs() < 0.01, "half the hours, half the income: {}", th.at(born + 86400));
    }

    #[test]
    fn an_exit_that_keeps_both_tokens_parks_each() {
        // in at tick 0, out when the price reaches 3, still inside [-5, 5): the exit keeps some of each
        let ticks: Vec<i64> = (0..6000).map(|k| if k < 600 { 0 } else { 3 }).collect();
        let m = market(&ticks, 0.0, 1e9);
        let rules = custom(json!({"width": 10, "exit_hi": 3, "exit_to": "keep", "park": true}));
        let b1 = m.bar_end.len() - 1;
        let y = |apr: [f64; 2]| {
            let mut c = ctx(&m);
            c.park = [ParkRate::flat(apr[0]), ParkRate::flat(apr[1])];
            run(&c, &rules, &no_costs(), 10_000.0, 0, b1)["park_yield"].as_f64().unwrap()
        };
        let (y0, y1, both) = (y([0.10, 0.0]), y([0.0, 0.04]), y([0.10, 0.04]));
        assert!(y0 > 0.0 && y1 > 0.0, "each token earns while kept: {y0} {y1}");
        assert!((both - y0 - y1).abs() < 1e-6 * both, "{both} vs {y0} + {y1}");
        // all of it parked: at most the higher APR on the whole value
        let days = (m.bar_end[b1] - m.bar_end[0]) as f64 / 86400.0;
        assert!(both < 10_000.0 * 0.10 * days / 365.0 * 1.01);
    }

    #[test]
    fn the_identity_holds_with_costs_and_moves() {
        let ticks: Vec<i64> = (0..3000).map(|k| ((k as f64 / 120.0).sin() * 40.0) as i64).collect();
        let m = market(&ticks, 0.5, 5e8);
        let c = ctx(&m);
        let cost = Costs { swap_bps: 5.0, gas_usd: 0.5, fee_haircut: 0.8, emission_haircut: 0.5, stable_swap_bps: 0.0 };
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
