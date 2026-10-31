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
//! being the pool without the wallet's own positions. beta is measured on the week before the period
//! (elasticity.rs): at 1 the fee APR does not depend on the capital, at 0 volume is fixed and a
//! larger position dilutes itself. The flow never exceeds MAX_FLOW times the swap's real flow. Emissions are a fixed budget per pool and accrue by time in
//! ours / (others + ours): they do dilute. Fees and emissions are paid out (not compounded) and
//! reduced by the haircuts.
//!
//! Every figure is in the numeraire token. The result splits the change against holding:
//!   value − hodl = fees + emissions − swap costs − gas + IL,
//! where hodl holds the token amounts of the first deposit (before its swap cost) and IL is the
//! rest: what the position's inventory lost (or gained) against those amounts as the price moved.
//!
//! A node strategy with a Hedge block also holds a perp short on Hyperliquid against the pool's
//! price: the volatile token against the numeraire, so one perp when the
//! numeraire is a dollar and two (short the token's coin, long the numeraire's) when it is not. Its
//! margin comes out of the capital before the pool gets the rest, and is held in the numeraire next
//! to the hold baseline, so the identity becomes
//!   value − (hodl + margin) = fees + emissions − swap costs − gas + IL + hedge P&L + funding − hedge fees.
//! The perp's price is the pool's (basis is not modelled); funding is Hyperliquid's hourly history.

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::custom::{self, Decision, ExitTo, Rules};
use crate::data::{self, Market};
use crate::ladder::{self, Params};

/// Two years: the longest period the lab offers (a pool younger than that starts at its first swap).
const MAX_DAYS: i64 = 730;
/// History before the period, at least: 15 days of lookback plus a day.
const WARMUP_S: i64 = 16 * 86400;

/// History before the period a request needs: WARMUP_S, or its graphs' longest lookback plus a day.
/// An optimize request may try settings up to 4× its strategy's (optimize::candidates_by), so it
/// warms up for that (graph.rs MAX_LOOKBACK_HOURS at most).
pub fn warmup_s(req: &Value) -> i64 {
    let look = |s: &Value| {
        s.as_object()
            .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("graph"))
            .and_then(|o| crate::graph::Graph::parse(o, 1).ok())
            .map(|g| g.lookback)
            .unwrap_or(0.0)
    };
    let mut h = look(&req["strategy"]);
    for k in ["compare", "batch"] {
        for p in req[k].as_array().into_iter().flatten() {
            h = h.max(look(&p["strategy"]));
        }
    }
    if req["optimize"].is_object() {
        h *= 4.0;
    }
    let h = h.min(crate::graph::MAX_LOOKBACK_HOURS);
    WARMUP_S.max(((h + 24.0) * 3600.0).ceil() as i64)
}
const MAX_JOBS: usize = 2;
/// The most flow a position may draw, as a multiple of the swap's real flow (deeper liquidity
/// attracts volume, but not without limit).
const MAX_FLOW: f64 = 2.0;
const MAX_EVENTS: usize = 400;
/// The most events a run may be asked to keep
const MAX_EVENTS_ALL: usize = 2_000_000;
/// Margin transfers and shortfalls kept with their times (each run counts all of them)
const MAX_MARGIN_EVENTS: usize = 5000;

#[derive(Clone, Debug)]
pub struct Costs {
    pub swap_bps: f64,
    /// USD a swap of the pair's tokens takes to move the market 1% (the deepest pools together):
    /// a swap then pays swap_bps plus impact, 100 bp × size / depth. None: swap_bps alone
    pub swap_depth_usd: Option<f64>,
    pub gas_usd: f64,
    pub fee_haircut: f64,
    pub emission_haircut: f64,
    /// an exit to the dollar stable and its buy-back: a swap through the aggregator between the
    /// pair and USDC, dearer than a swap inside the pool (route fees and impact)
    pub stable_swap_bps: f64,
    /// the pool's live limit on a swap's cost (settings `max_swap_cost_bps`, 3 bp unless given): live
    /// refuses a swap that costs more; the backtest makes it and counts it (`swaps_over_limit`)
    pub max_swap_cost_bps: Option<f64>,
    /// live's daily rebalance cap (settings `max_actions_per_day`, 5 unless given): live blocks a
    /// rebalance past it on the UTC day (exits are exempt); None: not modelled
    pub max_actions_per_day: Option<usize>,
    /// the live engine's spot check (engine.rs `checks`): a move or deposit is blocked while the
    /// price is more than this many ticks from the median of the last hour's swaps (exits are
    /// not); None: not modelled
    pub max_spot_dev_ticks: Option<f64>,
    /// a hedge trade: Hyperliquid's taker fee and the slippage, per perp traded
    pub hedge_taker_bps: f64,
    pub hedge_slip_bps: f64,
    /// moving margin between Base and Hyperliquid (CCTP fee and gas), dollars a transfer
    pub bridge_usd: f64,
    /// how many events a run keeps (`max_events`, MAX_EVENTS unless given, up to MAX_EVENTS_ALL): a
    /// research harness asks for all of them; past it they are counted (`events_dropped`)
    pub max_events: usize,
    /// a graph's decisions (moves, exits, the perp's size) are taken on bar b and traded on bar
    /// b + this (`exec_delay_bars`, 0 unless given, up to 288): with candles, 1 trades a decision made
    /// on a candle's close at the next candle's open
    pub exec_delay_bars: usize,
}

impl Costs {
    pub fn from(v: &Value) -> Costs {
        let n = |k: &str, d: f64| v[k].as_f64().filter(|x| x.is_finite() && *x >= 0.0).unwrap_or(d);
        Costs {
            swap_bps: n("swap_bps", 0.3).min(500.0),
            swap_depth_usd: v["swap_depth_usd"].as_f64().filter(|x| x.is_finite() && *x > 0.0),
            max_swap_cost_bps: v["max_swap_cost_bps"].as_f64().filter(|x| x.is_finite() && *x >= 0.0),
            max_actions_per_day: match &v["max_actions_per_day"] {
                Value::Null => Some(5),
                x => x.as_f64().filter(|x| x.is_finite() && *x >= 0.0).map(|x| x as usize),
            },
            gas_usd: n("gas_usd", 0.02).min(100.0),
            fee_haircut: n("fee_haircut", 0.82).min(1.0),
            emission_haircut: n("emission_haircut", 0.5).min(1.0),
            stable_swap_bps: n("stable_swap_bps", 1.0).min(500.0),
            max_spot_dev_ticks: v["max_spot_dev_ticks"].as_f64().filter(|x| x.is_finite() && *x > 0.0),
            hedge_taker_bps: n("hedge_taker_bps", 4.5).min(100.0),
            hedge_slip_bps: n("hedge_slip_bps", 2.0).min(100.0),
            bridge_usd: n("bridge_usd", 0.5).min(100.0),
            max_events: (n("max_events", MAX_EVENTS as f64) as usize).clamp(1, MAX_EVENTS_ALL),
            exec_delay_bars: (n("exec_delay_bars", 0.0) as usize).min(288),
        }
    }
    fn json(&self) -> Value {
        json!({"swap_bps": self.swap_bps, "swap_depth_usd": self.swap_depth_usd, "max_swap_cost_bps": self.max_swap_cost_bps, "max_actions_per_day": self.max_actions_per_day, "gas_usd": self.gas_usd, "fee_haircut": self.fee_haircut, "emission_haircut": self.emission_haircut,
               "stable_swap_bps": self.stable_swap_bps, "max_spot_dev_ticks": self.max_spot_dev_ticks,
               "hedge_taker_bps": self.hedge_taker_bps, "hedge_slip_bps": self.hedge_slip_bps, "bridge_usd": self.bridge_usd, "max_events": self.max_events, "exec_delay_bars": self.exec_delay_bars})
    }
}

pub enum Strategy {
    Custom(Rules),
    Graph(Box<crate::graph::Graph>),
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
    /// The rate a decision at `t` can know: the last full day's (a day's income is known once the
    /// day is over), 0 before the pool has a full day of history.
    pub fn known(&self, t: i64) -> f64 {
        self.at(t - 86400)
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

/// The perp market a Hedge block trades (hyperliquid.rs): what the short of the pool's price is.
#[derive(Clone, Debug, Default)]
pub struct HedgeMkt {
    /// perps per hedge trade: 1, or 2 when the numeraire is not a dollar; 0: no market (see `error`)
    pub legs: f64,
    /// what the short earns an hour, by hour start: the token's coin's funding less the numeraire's
    pub funding_h: HashMap<i64, f64>,
    /// maintenance margin over notional: half the initial margin at the coins' most leverage
    pub mm: f64,
    /// e.g. "short ETH" or "short AERO, long ETH"
    pub label: String,
    pub error: Option<String>,
    /// the first hour every leg traded (its first funding): no hedge before it, the perp did not
    /// exist yet (today's listing says nothing about last year)
    pub from_h: i64,
    /// a dollar-margined perp on the numeraire's own coin (both tokens hedge with it, as cbETH/WETH
    /// with ETH): it cannot hedge the pool's price, only take a position of its own (`size`) on the
    /// numeraire against the dollar. Its account (margin, P&L, funding, fees) is then in dollars,
    /// priced at the numeraire's USD price bar by bar, and worth that over it in the numeraire.
    pub usd: bool,
}

/// Where the margin stands against liquidation: equity left over maintenance, per unit of notional
/// (how far the price may rise before the short is liquidated). Set at the leverage, that is
/// 1/leverage − maintenance; a top-up brings it back there when it falls under MARGIN_LOW of that.
/// The excess goes back once the venue holds over MARGIN_HIGH × what it needs.
const MARGIN_LOW: f64 = 0.6;
const MARGIN_HIGH: f64 = 1.6;
/// Where a transfer sets the margin: the middle of the band, so the next small move does not send
/// another (set at the bare need, every tick against the position bridged again).
const MARGIN_MID: f64 = 0.5 * (1.0 + MARGIN_HIGH);

/// The leverage the short runs at: the block's, at most half the venue's maximum (D2's 50% buffer,
/// maintenance = 0.5 / maximum).
fn hedge_leverage(c: &Ctx, lev: f64) -> f64 {
    let cap = if c.hedge.mm > 0.0 { 1.0 / (4.0 * c.hedge.mm) } else { f64::INFINITY };
    lev.min(cap).max(1.0)
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
    /// the perps a Hedge block trades (legs 0: none)
    pub hedge: HedgeMkt,
    /// what a swap of the pair cost each day (day start: (fee bp, 1% depth USD)), from Liquidity
    /// Lens's swap-cost history (docs/LLENS_REQUESTS.md R17); empty: Costs' flat values
    pub swap_days: HashMap<i64, (f64, f64)>,
    /// per swap, the market's price as a tick (`depth_ref`): what the pool's price deviates from.
    /// Our liquidity damps the deviation (Sim::tick_at). Empty: the pool's price as it traded
    pub tref: std::sync::Arc<Vec<i64>>,
    /// a pegged pair: our liquidity damps each swap's move instead (Sim::advance), as there is no
    /// trend for arbitrage to carry the price along
    pub peg_damp: bool,
}

/// Per swap, the price a volatile pool's own deviates from: its cleaned price (data::spike_filter),
/// which follows the market's trend (arbitrage carries any pool along, however deep).
pub fn depth_ref(m: &Market) -> Vec<i64> {
    data::spike_filter(&m.tick)
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
    /// events past MAX_EVENTS, not kept: the list is then incomplete
    events_dropped: usize,
    /// the run's `max_events` (0: MAX_EVENTS)
    ev_cap: usize,
    /// margin transfers (+ to the venue, − back) and shortfalls, timed; and those past the cap
    margin_log: Vec<Value>,
    margin_log_dropped: usize,
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
    /// the hedge: the short in units of the pool's volatile token, its margin (numeraire), what it
    /// gained or lost since the margin was last set (price and funding, less fees), totals
    short: f64,
    margin: f64,
    margin0: f64,
    h_acc: f64,
    h_px: f64,
    hedge_pnl: f64,
    funding: f64,
    hedge_fees: f64,
    hedge_trades: usize,
    margin_moves: usize,
    liq_bars: usize,
    /// margin top-ups the pool's cash could not pay in full
    margin_short: usize,
    /// numeraire moved from the idle tokens to the venue's margin (set_margin): out of the
    /// inventory, but not a loss of it
    to_margin: f64,
    /// swaps dearer than the live limit, and the dearest (bp)
    swaps_over: usize,
    max_swap_bps: f64,
    /// income earned but not yet in hand: fees sit in the ranges until they are burned, emissions
    /// until the weekly claim (worker.rs, `claim.every_days`); counted in the value, but no
    /// collateral for the hedge before then. When emissions were last claimed
    unc_fees: f64,
    unc_emis: f64,
    last_claim: i64,
    /// a pegged pair's pool price with our liquidity in it: (the swap it is at, its tick)
    eff: Option<(usize, f64)>,
    /// rebalances on the current UTC day (day, count) and those live's daily cap blocked
    day_moves: (i64, usize),
    cap_blocked: usize,
    min_liq_dist: Option<f64>,
    max_naked: f64,
    /// the exposure averaged over the hedge's smooth_h, and when it was last updated
    exp_avg: Option<(f64, i64)>,
}

impl Sim {
    fn liq_at(&self, t: i64) -> f64 {
        self.ranges.iter().filter(|(a, b, l)| *a <= t && t < *b && *l > 0.0).map(|r| r.2).sum()
    }

    /// The pool's tick at swap k with our liquidity in it. The replay's price was made by others'
    /// liquidity alone: the same flow moves a deeper pool less, so its deviation from the market
    /// (Ctx::tref) shrinks by others / (others + ours). Where we would be most of the pool (thin
    /// pools, a large position) a few cents of swaps no longer drag the price off the market, and
    /// the replay stops trading at prices our own liquidity would have held off.
    ///
    /// On a pegged pair (Ctx::peg_damp) it is a state, `advance`d swap by swap.
    fn tick_at(&self, c: &Ctx, k: usize) -> i64 {
        let raw = c.m.tick[k];
        if c.peg_damp {
            return match self.eff {
                Some((j, e)) if j == k => e.round() as i64,
                _ => raw,
            };
        }
        let Some(r) = c.tref.get(k).copied() else { return raw };
        // what the move had to push through: our liquidity anywhere between the market, where the
        // price was and where the replay's went (a narrow range beside it still stands in its way)
        let prev = c.m.tick[k.saturating_sub(1)];
        let (lo_t, hi_t) = (r.min(raw).min(prev), r.max(raw).max(prev));
        let ours = self.in_way(lo_t, hi_t);
        if !(ours > 0.0) {
            return raw;
        }
        let lo = c.m.l_others[k].max(0.0);
        let w = lo / (lo + ours);
        (r as f64 + (raw - r) as f64 * w).round() as i64
    }

    /// Our deepest liquidity on any tick in [lo_t, hi_t].
    fn in_way(&self, lo_t: i64, hi_t: i64) -> f64 {
        self.ranges.iter().filter(|(a, b, l)| *a <= hi_t && lo_t < *b && *l > 0.0).map(|x| x.2).fold(0.0, f64::max)
    }

    /// A pegged pair's pool price through swap k with our liquidity in it: each swap pulls it toward
    /// the replay's price by others / (others + ours on the way), so where we are most of the pool a
    /// few cents of swaps barely move it, and without a position of ours it is the replay's price.
    fn advance(&mut self, c: &Ctx, k: usize) {
        if !c.peg_damp {
            return;
        }
        let m = c.m;
        let (j, mut e) = match self.eff {
            Some((j, e)) if j <= k => (j, e),
            _ => {
                self.eff = Some((k, m.tick[k] as f64));
                return;
            }
        };
        for q in (j + 1)..=k {
            let raw = m.tick[q] as f64;
            let ours = self.in_way(e.min(raw).floor() as i64, e.max(raw).ceil() as i64);
            let lo = m.l_others[q].max(0.0);
            let w = if ours > 0.0 { lo / (lo + ours) } else { 1.0 };
            e += w * (raw - e);
        }
        self.eff = Some((k, e));
    }

    /// The pool's sqrt price at swap k with our liquidity in it (`tick_at`).
    fn s_at(&self, c: &Ctx, k: usize) -> f64 {
        let t = self.tick_at(c, k);
        if t == c.m.tick[k] { c.s(k) } else { c.s_tick(t) }
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
                if self.waiting[sd] && !self.parked[sd] && c.park[sd].known(t0) > 0.0 {
                    let s = self.s_at(c, i0);
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
            // the waiting tokens earn their pegged pools' APRs over the interval, at the last full
            // day's rate (today's depends on fees not yet paid: later hours must not change earlier marks)
            let s = self.s_at(c, i1);
            let v = |side: usize| if side == 0 { c.value(self.x, 0.0, s) } else { c.value(0.0, self.y, s) };
            let y = (0..2).filter(|sd| self.parked[*sd]).map(|sd| v(sd) * c.park[sd].known(t0)).sum::<f64>() * (t1 - t0).max(0) as f64 / (365.0 * 86400.0);
            self.park_yield += y;
            self.cash += y;
        }
        if self.ranges.is_empty() || t1 <= t0 {
            self.t_tot += (t1 - t0).max(0) as f64;
            // nothing of ours in the pool: its price is the replay's
            self.eff = None;
            return;
        }
        let m = c.m;
        let mut fees = 0.0;
        // the pool's tick at each swap of the interval, with our liquidity in it
        self.advance(c, i0);
        let mut seg: Vec<i64> = Vec::with_capacity(i1 + 1 - i0);
        seg.push(self.tick_at(c, i0));
        for k in (i0 + 1)..=i1 {
            self.advance(c, k);
            seg.push(self.tick_at(c, k));
        }
        for k in (i0 + 1)..=i1 {
            let ours = 0.5 * (self.liq_at(seg[k - i0]) + self.liq_at(seg[k - 1 - i0]));
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
            let held = self.liq_at(seg[k - i0]);
            if held > 0.0 {
                self.t_in += dt;
                let rate = c.emis_usd_h.get(&(m.ts[k].max(t0).div_euclid(3600) * 3600)).copied().unwrap_or(0.0) / 3600.0;
                emis += rate * c.num_per_usd * dt * held / (m.l_others[k] + held);
            }
        }
        self.fees += fees * cost.fee_haircut;
        self.emissions += emis * cost.emission_haircut;
        self.cash += fees * cost.fee_haircut + emis * cost.emission_haircut;
        self.unc_fees += fees * cost.fee_haircut;
        self.unc_emis += emis * cost.emission_haircut;
        if self.last_claim == 0 {
            self.last_claim = t0;
        }
        if t1 - self.last_claim >= CLAIM_EVERY_S {
            self.unc_emis = 0.0;
            self.last_claim = t1;
        }
    }

    /// Cash in hand: what the pool could send to the hedge's venue now (not unclaimed income).
    fn spendable(&self) -> f64 {
        self.cash - self.unc_fees - self.unc_emis
    }

    /// Whether live's daily cap still lets a rebalance through at t.
    fn may_move(&self, cost: &Costs, t: i64) -> bool {
        match cost.max_actions_per_day {
            Some(cap) => !(self.day_moves.0 == t.div_euclid(86400) && self.day_moves.1 >= cap),
            None => true,
        }
    }

    fn count_move(&mut self, t: i64) {
        let d = t.div_euclid(86400);
        self.day_moves = if self.day_moves.0 == d { (d, self.day_moves.1 + 1) } else { (d, 1) };
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
        if value > 0.0 {
            self.note_swap(cost, cost.swap_bps);
        }
        self.pay_gas(c, cost, 2);
        fee
    }

    /// Value (numeraire) of the idle tokens parked now.
    fn parked_value(&self, c: &Ctx, s: f64) -> f64 {
        (if self.parked[0] { c.value(self.x, 0.0, s) } else { 0.0 }) + (if self.parked[1] { c.value(0.0, self.y, s) } else { 0.0 })
    }

    /// The hedged token's price in the numeraire at sqrt price s (the token that is not the numeraire).
    /// the numeraire one unit of the perp account is worth: 1, or for a dollar account the
    /// numeraire's dollar price's inverse at this bar
    fn hk(&self, c: &Ctx) -> f64 {
        if c.hedge.usd { 1.0 / self.upn.max(1e-300) } else { 1.0 }
    }

    /// a transfer's fixed cost in the perp account's unit
    fn bridge_h(c: &Ctx, cost: &Costs) -> f64 {
        if c.hedge.usd { cost.bridge_usd } else { cost.bridge_usd * c.num_per_usd }
    }

    fn hedge_px(c: &Ctx, s: f64) -> f64 {
        let p = s * s;
        if c.num1 { p } else { 1.0 / p }
    }

    /// The pool's exposure to the hedged token: what the ranges and the idle tokens hold of it.
    fn exposure(&self, c: &Ctx, s: f64) -> f64 {
        let (x, y) = self.inventory(c, s);
        if c.num1 { x } else { y }
    }

    /// Distance to liquidation (see MARGIN_LOW), None without a short.
    fn liq_dist(&self, c: &Ctx, px: f64) -> Option<f64> {
        let n = self.short.abs() * px;
        (n > 0.0).then(|| (self.margin + self.h_acc - n * c.hedge.mm) / n)
    }

    /// The position over [t0, t1) as the price went to `px`: its price P&L and the hour's funding
    /// (`short` < 0 is a long: it pays what a short earns). `hi`/`lo` are the hedged token's highest
    /// and lowest prices in between (every swap, not just the bar's close): when the position's
    /// equity fell under maintenance at the worse of them (the high for a short, the low for a
    /// long), the venue closed it at that price, with a taker's fees, and it stays closed until the
    /// strategy reopens it.
    #[allow(clippy::too_many_arguments)]
    fn hedge_mark(&mut self, c: &Ctx, cost: &Costs, t0: i64, t1: i64, px: f64, hi: f64, lo: f64, t: i64) {
        if self.short != 0.0 && self.h_px > 0.0 {
            // the rate paid at the end of the hour held (keyed by that payment), else, when it is
            // not paid yet (the last hour of the data), the hour before's
            let h = t0.div_euclid(3600) * 3600;
            let r = c.hedge.funding_h.get(&(h + 3600)).or_else(|| c.hedge.funding_h.get(&h)).copied().unwrap_or(0.0);
            let f = self.short * self.h_px * r * (t1 - t0).max(0) as f64 / 3600.0;
            let w = if self.short > 0.0 { hi.max(px) } else { lo.min(px) };
            let d_w = -self.short * (w - self.h_px);
            if self.margin + self.h_acc + f + d_w < self.short.abs() * w * c.hedge.mm {
                let fee = self.short.abs() * w * (cost.hedge_taker_bps + cost.hedge_slip_bps) / 1e4 * c.hedge.legs;
                // the venue closes it; its account never goes below zero (the loss past the
                // margin is the venue's, as on Hyperliquid)
                let past = (self.margin + self.h_acc + d_w + f - fee).min(0.0);
                let k = self.hk(c);
                self.hedge_pnl += (d_w - past) * k;
                self.funding += f * k;
                self.hedge_fees += fee * k;
                self.h_acc += d_w + f - fee - past;
                self.liq_bars += 1;
                self.min_liq_dist = Some(self.min_liq_dist.map_or(-1.0, |m: f64| m.min(-1.0)));
                if self.events.len() < self.ev_cap() {
                    self.events.push(json!({"ts": t, "kind": "liquidation", "short": round6(self.short), "px": round6(w)}));
                } else {
                    self.events_dropped += 1;
                }
                self.short = 0.0;
                self.h_px = px;
                return;
            }
            let d = -self.short * (px - self.h_px);
            let k = self.hk(c);
            self.hedge_pnl += d * k;
            self.funding += f * k;
            self.h_acc += d + f;
            if let Some(dist) = self.liq_dist(c, px) {
                self.min_liq_dist = Some(self.min_liq_dist.map_or(dist, |m| m.min(dist)));
            }
        }
        self.h_px = px;
    }

    /// What the venue should hold: notional / leverage, never less than the margin reserved at the
    /// start (it stays there, so a hedge that closes and reopens does not bridge each time). A
    /// dollar account keeps no reserve: dollars idle there are a bet against the numeraire.
    fn margin_need(&self, c: &Ctx, px: f64, lev: f64) -> f64 {
        (self.short.abs() * px / lev.max(1.0)).max(if c.hedge.usd { 0.0 } else { self.margin0 })
    }

    /// The numeraire the wallet holds idle (outside the pool's ranges, not parked): what can go to
    /// the venue as margin besides cash.
    fn idle_num(&self, c: &Ctx) -> f64 {
        let side = if c.num1 { 1 } else { 0 };
        if self.parked[side] { 0.0 } else if c.num1 { self.y } else { self.x }.max(0.0)
    }

    /// Set the margin to MARGIN_MID × the position's need (never under the reserve, margin_need): the
    /// difference moves between the pool's cash and the venue
    /// (one transfer, paying `bridge_usd`). A top-up comes out of cash the pool has, then out of the
    /// numeraire it holds idle: what it lacks is counted (`margin_short`) and the venue keeps less,
    /// nearer liquidation.
    fn set_margin(&mut self, c: &Ctx, cost: &Costs, px: f64, lev: f64, t: i64) {
        // a dollar account's numbers are dollars (`k` numeraire each): what moves in or out of the
        // pool is that many dollars' worth of the numeraire, sold or bought at the perp's taker
        // and slippage cost on the way
        let k = self.hk(c);
        let swap = if c.hedge.usd { (cost.hedge_taker_bps + cost.hedge_slip_bps) / 1e4 } else { 0.0 };
        let need = (self.short.abs() * px / lev.max(1.0) * MARGIN_MID).max(if c.hedge.usd { 0.0 } else { self.margin0 });
        let b = Sim::bridge_h(c, cost);
        let mut mv = self.margin + self.h_acc - need;
        if mv < 0.0 {
            let avail = ((self.spendable() + self.idle_num(c)) / k * (1.0 - swap) - b).max(0.0);
            if -mv > avail {
                self.margin_short += 1;
                self.log_margin(json!({"ts": t, "kind": "margin_short", "need": round6(need), "lacking": round6(-mv - avail)}));
                mv = -avail;
            }
        }
        if mv.abs() <= 1e-12 || mv.abs() <= b {
            return;
        }
        let sw = mv.abs() * swap;
        self.hedge_fees += (b + sw) * k;
        self.cash += (mv - b - sw) * k;
        // what cash could not cover comes out of the idle numeraire
        let lack = (-self.spendable()).min(self.idle_num(c)).max(0.0);
        if lack > 0.0 {
            self.cash += lack;
            self.to_margin += lack;
            if c.num1 { self.y -= lack } else { self.x -= lack }
        }
        self.margin = self.margin + self.h_acc - mv;
        self.h_acc = 0.0;
        self.margin_moves += 1;
        self.log_margin(json!({"ts": t, "kind": "margin", "to_venue": round6(-mv), "margin": round6(self.margin), "from_idle": round6(lack)}));
    }

    fn ev_cap(&self) -> usize {
        if self.ev_cap == 0 { MAX_EVENTS } else { self.ev_cap }
    }

    fn log_margin(&mut self, e: Value) {
        if self.margin_log.len() < MAX_MARGIN_EVENTS.max(self.ev_cap()) {
            self.margin_log.push(e);
        } else {
            self.margin_log_dropped += 1;
        }
    }

    /// Trade the short toward `ratio` of the exposure, less a long of `size` × the strategy's value
    /// (a short when negative), when it drifted out of the band, then keep the margin between
    /// MARGIN_LOW and MARGIN_HIGH.
    /// `s` is the pool's price (what the ranges hold), `px` the perp's: the hedged token's price from
    /// the bar's cleaned close, as the perp's mark follows the market, not a spike in this pool.
    #[allow(clippy::too_many_arguments)]
    fn hedge_retarget(&mut self, c: &Ctx, cost: &Costs, h: &crate::graph::HedgeCfg, ratio: f64, size: f64, s: f64, px: f64, t: i64) {
        // before the perp existed there is nothing to trade
        let (ratio, size) = if t < c.hedge.from_h { (0.0, 0.0) } else { (ratio, size) };
        // a dollar account on the numeraire's own coin cannot hedge the pool's price
        let ratio = if c.hedge.usd { 0.0 } else { ratio };
        let k = self.hk(c);
        let raw = self.exposure(c, s).max(0.0);
        // averaged over smooth_h (time-weighted), so a ladder's re-solves do not all trade; nothing
        // held means nothing to hedge at once
        let e = if h.smooth_h > 0.0 && raw > 0.0 {
            let (avg, t0) = self.exp_avg.unwrap_or((raw, t));
            let a = 1.0 - (-((t - t0).max(0) as f64) / (h.smooth_h * 3600.0)).exp();
            avg + a * (raw - avg)
        } else {
            raw
        };
        self.exp_avg = (raw > 0.0).then_some((e, t));
        // a short needs margin: never more notional than the venue's equity, with what the pool's
        // cash could send it, carries at the leverage
        // what the pool could send counts only past the transfer's cost and only from cash in hand;
        // the trade's own fee comes out of the same equity
        let lev = hedge_leverage(c, h.leverage);
        let b = Sim::bridge_h(c, cost);
        let rate = (cost.hedge_taker_bps + cost.hedge_slip_bps) / 1e4 * c.hedge.legs;
        let equity = (self.margin + self.h_acc).max(0.0) + ((self.spendable() + self.idle_num(c)) / k - b).max(0.0);
        let carry = equity * lev / px.max(1e-300) / (1.0 + lev * rate);
        // a reversal pays the fee on closing the old side too: what is left carries the new one
        let carry_rev = ((equity / px.max(1e-300) - self.short.abs() * rate) / (1.0 / lev + rate)).max(0.0);
        // the position of its own: size × everything the strategy is worth, in the token
        let own = if size != 0.0 {
            let (x, y) = self.inventory(c, s);
            size * ((c.value(x, y, s) + self.usd_value() + self.cash) / k + self.margin + self.h_acc).max(0.0) / px.max(1e-300)
        } else {
            0.0
        };
        let want = ratio * e - own;
        let cap = if want * self.short < 0.0 { carry_rev } else { carry };
        let target = want.clamp(-cap, cap);
        // the most the position ever drifted from its target (numeraire): what the band leaves open
        self.max_naked = self.max_naked.max((ratio * raw - own - self.short).abs() * px);
        let d = target - self.short;
        let band = h.band * target.abs().max(self.short.abs());
        let worth = d.abs() * px >= h.min_usd * if c.hedge.usd { 1.0 } else { c.num_per_usd } || (target == 0.0 && self.short != 0.0);
        if d.abs() > band && worth {
            let fee = d.abs() * px * (cost.hedge_taker_bps + cost.hedge_slip_bps) / 1e4 * c.hedge.legs;
            self.hedge_fees += fee * k;
            self.h_acc -= fee;
            self.short = target;
            self.hedge_trades += 1;
            if self.events.len() < self.ev_cap() {
                self.events.push(json!({"ts": t, "kind": "hedge", "short": round6(self.short), "px": round6(px)}));
            } else {
                self.events_dropped += 1;
            }
        }
        // a top-up when the short came too close to liquidation, the excess back when the venue holds
        // well over what it needs
        let low = self.liq_dist(c, px).map(|d| d < MARGIN_LOW * (1.0 / lev - c.hedge.mm)).unwrap_or(false)
            || self.margin + self.h_acc < self.short.abs() * px / lev.max(1.0);
        let high = self.margin + self.h_acc > MARGIN_HIGH * self.margin_need(c, px, lev);
        if low || high {
            self.set_margin(c, cost, px, lev, t);
        }
        // the venue never holds a negative account: a short it could not carry is closed, and what
        // was past zero is not ours to lose
        if self.margin + self.h_acc < 0.0 {
            let past = self.margin + self.h_acc;
            self.h_acc -= past;
            self.hedge_pnl -= past * k;
            if self.short != 0.0 {
                self.short = 0.0;
                self.liq_bars += 1;
                if self.events.len() < self.ev_cap() {
                    self.events.push(json!({"ts": t, "kind": "liquidation", "short": 0.0, "px": round6(px)}));
                } else {
                    self.events_dropped += 1;
                }
            }
        }
    }

    /// The cost of swapping `amount1` (token1 units) at time t: the day's fee and depth when the
    /// swap-cost history has the day, else Costs' (fee, and impact when it gives a depth).
    fn swap_fee(c: &Ctx, cost: &Costs, t: i64, amount1: f64, p: f64) -> f64 {
        let usd = (if c.num1 { amount1 } else { amount1 / p }) / c.num_per_usd.max(1e-300);
        let (bps, depth) = match c.swap_days.get(&t.div_euclid(86400)) {
            Some((f, d)) => (*f, Some(*d)),
            None => (cost.swap_bps, cost.swap_depth_usd),
        };
        let impact = depth.filter(|d| *d > 0.0).map(|d| 100.0 * usd / d).unwrap_or(0.0);
        amount1 * (bps + impact).min(1e4) / 1e4
    }

    /// A swap's cost against the live limit: live would refuse one that costs more.
    fn note_swap(&mut self, cost: &Costs, bps: f64) {
        if bps.is_finite() {
            self.max_swap_bps = self.max_swap_bps.max(bps);
            if bps > cost.max_swap_cost_bps.unwrap_or(3.0) + 1e-9 {
                self.swaps_over += 1;
            }
        }
    }

    fn pay_gas(&mut self, c: &Ctx, cost: &Costs, n_tx: usize) {
        let g = n_tx as f64 * cost.gas_usd * c.num_per_usd;
        self.gas += g;
        self.cash -= g;
    }

    /// Burn everything, swap to the ratio `target` (ranges with value weights) needs, mint it.
    fn rebalance(&mut self, c: &Ctx, cost: &Costs, i: usize, t: i64, target: &[(i64, i64, f64)], why: &str) {
        // no range to go into (or no weight): keep what is held rather than empty the balances
        if target.is_empty() || !(target.iter().map(|r| r.2).sum::<f64>() > 0.0) {
            return;
        }
        self.from_usd(c, cost);
        let s = self.s_at(c, i);
        let p = s * s;
        let (x, y) = self.inventory(c, s);
        let mut n_tx = self.ranges.len() + target.len();
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
        // the balances after the parked tokens come back and the swap: the swap is sized on the
        // mix before its cost and delivers less than it takes, so the mint takes what fits the
        // target's mix and the rest stays idle, as a real mint leaves it
        let (mut bx, mut by) = (x, y);
        if self.parked.iter().any(|p| *p) {
            let fee = self.park_move(c, cost, self.parked_value(c, s));
            self.parked = [false; 2];
            let fee1 = if c.num1 { fee } else { fee * p };
            if v1 > 0.0 {
                let k = (1.0 - fee1 / v1).max(0.0);
                bx *= k;
                by *= k;
            }
        }
        self.waiting = [false; 2];
        let need0 = f0 * (bx * p + by) / p;
        let swapped1 = (need0 - bx).abs() * p;
        if swapped1 > v1 * 1e-9 {
            let c1 = Self::swap_fee(c, cost, t, swapped1, p);
            self.note_swap(cost, c1 / swapped1 * 1e4);
            self.swap_costs += if c.num1 { c1 } else { c1 / p };
            n_tx += 1;
            if need0 > bx {
                // token1 sold for token0: the cost comes out of what arrives
                by -= swapped1;
                bx += (swapped1 - c1).max(0.0) / p;
            } else {
                bx -= swapped1 / p;
                by += (swapped1 - c1).max(0.0);
            }
        }
        let (bx, by) = (bx.max(0.0), by.max(0.0));
        // the most value (token1) the target's mix can take from these balances
        let mint1 = if f0 <= 1e-12 { by } else if f0 >= 1.0 - 1e-12 { bx * p } else { (bx * p / f0).min(by / (1.0 - f0)) };
        self.close_segment(t);
        self.ranges = target
            .iter()
            .zip(&unit)
            .map(|((a, b, w), (ux, uy))| (*a, *b, w / wsum * mint1 / (ux * p + uy).max(1e-300)))
            .collect();
        self.x = (bx - f0 * mint1 / p).max(0.0);
        self.y = (by - (1.0 - f0) * mint1).max(0.0);
        self.unc_fees = 0.0;
        self.count_move(t);
        self.seg_start = t;
        self.pay_gas(c, cost, n_tx);
        self.actions += 1;
        if self.events.len() < self.ev_cap() {
            let (a, b) = self.span().unwrap_or((0, 0));
            self.events.push(json!({"ts": t, "kind": "move", "tl": a, "tu": b, "ranges": target.len(), "reason": why}));
        } else {
            self.events_dropped += 1;
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
        self.note_swap(cost, cost.stable_swap_bps);
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
        let s = self.s_at(c, i);
        let (x, y) = self.inventory(c, s);
        let mut n_tx = self.ranges.len() + (x > 0.0) as usize + (y > 0.0) as usize;
        let mut v = c.value(x, y, s);
        if self.parked.iter().any(|p| *p) {
            v -= self.park_move(c, cost, self.parked_value(c, s));
            n_tx += 1;
        }
        let fee = v * cost.stable_swap_bps / 1e4;
        self.swap_costs += fee;
        if v > 0.0 {
            self.note_swap(cost, cost.stable_swap_bps);
        }
        self.usd += (v - fee).max(0.0) * self.upn;
        // withdrawn: the ranges' fees are in hand
        self.unc_fees = 0.0;
        self.close_segment(t);
        self.ranges.clear();
        self.x = 0.0;
        self.y = 0.0;
        self.waiting = [false; 2];
        self.parked = [false; 2];
        self.pay_gas(c, cost, n_tx);
        self.actions += 1;
        if self.events.len() < self.ev_cap() {
            self.events.push(json!({"ts": t, "kind": "exit", "reason": why, "to": "stable"}));
        } else {
            self.events_dropped += 1;
        }
    }

    fn exit(&mut self, c: &Ctx, cost: &Costs, i: usize, t: i64, to: ExitTo, why: &str) {
        if to == ExitTo::Stable {
            return self.exit_stable(c, cost, i, t, why);
        }
        // from the stable into a token: the dollars buy the numeraire back first
        self.from_usd(c, cost);
        let s = self.s_at(c, i);
        let p = s * s;
        let (mut x, mut y) = self.inventory(c, s);
        let mut n_tx = self.ranges.len();
        let swapped1 = match to {
            ExitTo::Token0 => y,
            ExitTo::Token1 => x * p,
            ExitTo::Keep | ExitTo::Stable => 0.0,
        };
        if swapped1 > 0.0 {
            let c1 = Self::swap_fee(c, cost, t, swapped1, p);
            self.note_swap(cost, c1 / swapped1 * 1e4);
            self.swap_costs += if c.num1 { c1 } else { c1 / p };
            n_tx += 1;
            let v1 = x * p + y - c1;
            (x, y) = if matches!(to, ExitTo::Token0) { (v1 / p, 0.0) } else { (0.0, v1) };
        }
        self.close_segment(t);
        self.ranges.clear();
        self.unc_fees = 0.0;
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
            self.parked[sd] &= self.park_on && c.park[sd].known(t) > 0.0;
        }
        if self.parked.iter().any(|p| *p) {
            fee += self.park_move(c, cost, self.parked_value(c, s));
        }
        if fee > 0.0 && v > 0.0 {
            let k = (1.0 - fee / v).max(0.0);
            self.x *= k;
            self.y *= k;
        }
        if self.events.len() < self.ev_cap() {
            self.events.push(json!({"ts": t, "kind": "exit", "reason": why}));
        } else {
            self.events_dropped += 1;
        }
    }
}

/// Emissions are claimed weekly, as the worker's default `claim.every_days`.
const CLAIM_EVERY_S: i64 = 7 * 86400;

/// A fingerprint of the daily swap-cost data (day, fee, depth), for result caches.
fn days_key(d: &HashMap<i64, (f64, f64)>) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut v: Vec<(i64, u64, u64)> = d.iter().map(|(k, (f, x))| (*k, f.to_bits(), x.to_bits())).collect();
    v.sort_unstable();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// Replay one strategy over bars [b0, b1] with `capital` (numeraire units, all in the numeraire).
pub fn run(c: &Ctx, strat: &Strategy, cost: &Costs, capital: f64, b0: usize, b1: usize) -> Value {
    run_carry(c, strat, cost, capital, b0, b1, None).0
}

/// What a run hands the next one: the portfolio (ranges, idle tokens, cash, the hedge and its
/// margin, the hold baseline), the strategy's timers, and where accrual stopped.
pub struct Carry {
    sim: Sim,
    cst: custom::State,
    last: (usize, i64),
}

/// `run`, optionally from where an earlier run left off (`init`, its Carry, with b0 the bar after
/// its last): the portfolio carries over and only the strategy changes, as when re-tuned settings
/// are applied to a running pool. The capital stays the first run's, the base of every return.
pub fn run_carry(c: &Ctx, strat: &Strategy, cost: &Costs, capital: f64, b0: usize, b1: usize, init: Option<Carry>) -> (Value, Carry) {
    let m = c.m;
    let fresh = init.is_none();
    let (mut sim, mut cst, carried_last) = match init {
        Some(k) => (k.sim, k.cst, Some(k.last)),
        None => (Sim::default(), custom::State::default(), None),
    };
    let park = match strat {
        Strategy::Graph(g) => g.park,
        Strategy::Custom(r) => r.park,
        _ => false,
    };
    sim.park_on = park;
    sim.ev_cap = cost.max_events;
    let i_start = m.bar_idx[b0];
    let mut watch_errors: Vec<String> = vec![];
    // the hedge: its margin comes out of the capital first (a ratio of the exposure, about half the
    // value of a range around the price, at the block's leverage)
    let hedge = match strat {
        Strategy::Graph(g) => g.hedge(),
        _ => None,
    };
    let hedge = match hedge {
        Some(_) if c.hedge.legs == 0.0 => {
            watch_errors.push(format!("hedge: {}; run without it", c.hedge.error.clone().unwrap_or_else(|| "no perp market".into())));
            None
        }
        h => h,
    };
    if let (Some(h), true) = (&hedge, fresh) {
        // the margin for a short of about half the capital (the pool's exposure) at the ratio, and
        // for the position of its own at its size (at least 1× the capital when a signal sets it)
        let own = if h.sized { h.size.abs().max(1.0) } else { h.size.abs() };
        // never more than the capital: what a position cannot be margined for, the carry cap trims
        sim.margin0 = (capital * (h.ratio * 0.5 + own) / hedge_leverage(c, h.leverage)).min(capital);
        sim.margin = sim.margin0;
        if sim.margin0 > 0.0 {
            sim.hedge_fees += cost.bridge_usd * c.num_per_usd;
            sim.cash -= cost.bridge_usd * c.num_per_usd;
            sim.margin_moves = 1;
            // in the numeraire (a dollar account's dollars are set once the price is known)
            sim.log_margin(json!({"ts": m.bar_end[b0], "kind": "margin", "to_venue": round6(sim.margin0), "margin": round6(sim.margin0), "from_idle": 0.0, "initial": true}));
        }
    }
    let lp_capital = capital - sim.margin0;
    if fresh {
        if c.num1 {
            sim.y = lp_capital;
        } else {
            sim.x = lp_capital;
        }
    }
    let med = data::rolling_median_trailing(&m.bar_tick, data::week_bars(m), 12);
    let guard_dev = |b: usize| (m.bar_tick[b] as f64 - med[b]).abs();
    let (mut last_i, mut last_t) = match carried_last {
        Some((_, t)) => (m.ts.partition_point(|x| *x <= t).saturating_sub(1).min(i_start), t),
        None => (i_start, m.bar_end[b0]),
    };
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
    }
    let mut guard_frozen = 0usize;
    // the live spot check, asked only when the strategy wants to move (as the engine does)
    let mut spot_blocked = 0usize;
    let spot_ok = |i: usize| -> bool {
        let Some(lim) = cost.max_spot_dev_ticks else { return true };
        spot_dev(m, i) <= lim
    };
    let glad = match strat {
        Strategy::Graph(g) => g.ladder(),
        _ => None,
    };
    let gev = match strat {
        Strategy::Graph(g) => {
            let (mut ext, errs) = crate::graph::load_externals(g, c.cid, m);
            watch_errors.extend(errs);
            for (i, n) in g.nodes.iter().enumerate().filter(|(_, n)| n.kind == "funding") {
                if c.hedge.funding_h.is_empty() {
                    watch_errors.push(format!("{}: no funding history for this pool's tokens", n.id));
                }
                ext.insert(i, funding_series(c, m));
            }
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
    // a carried portfolio may hold dollars even when these settings cannot exit to them
    let usd_perp = hedge.is_some() && c.hedge.usd;
    let upn: Vec<f64> = if !(stable_exit || sim.usd > 0.0 || usd_perp) || c.num_token.is_empty() {
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
                watch_errors.push(format!("the numeraire's USD price ({}): {e}; today's price is used", if usd_perp { "the dollar perp" } else { "exit to stable" }));
                vec![]
            }
        }
    };
    // a carried run keeps the price its dollars were last valued at, so its first value is the
    // last one of the run before (today's price there made a gain or loss out of nothing)
    if fresh || !(sim.upn > 0.0) {
        sim.upn = flat_upn;
    }
    // a dollar perp account starts with the margin's worth in dollars at the first bar's price
    if fresh && usd_perp {
        if let Some(u) = upn.get(b0) {
            sim.upn = *u;
        }
        sim.margin = sim.margin0 * sim.upn;
    }
    // the value before anything was traded, at the first bar's time ahead of its trades: the first
    // period's return then includes what going in cost
    {
        // handed on: valued at the price the run before ended at, so its last value is this first one
        // and no bar's change falls between the two
        let s0 = sim.s_at(c, if fresh { i_start } else { last_i });
        let (x0, y0) = sim.inventory(c, s0);
        let v0 = if fresh { capital } else { c.value(x0, y0, s0) + sim.usd_value() + sim.cash + (sim.margin + sim.h_acc) * sim.hk(c) };
        let h0 = if fresh { capital } else { sim.hodl.map(|(hx, hy)| c.value(hx, hy, s0)).unwrap_or(lp_capital) + sim.margin0 };
        series.push(json!([m.bar_end[b0], round6(v0), round6(h0)]));
    }
    for b in b0..=b1 {
        let i = m.bar_idx[b];
        let t = m.bar_end[b];
        if let Some(u) = upn.get(b) {
            sim.upn = *u;
        }
        sim.accrue(c, cost, last_i, i, last_t, t);
        // the pool's price with our liquidity in it: what we trade, earn and are valued at
        let tick = sim.tick_at(c, i);
        let s = sim.s_at(c, i);
        if hedge.is_some() {
            // the short's worst moment in the bar: the hedged token's highest price in it, from the
            // bar's cleaned high and low (the perp's mark price follows the market, not a single
            // spike in this pool, such as a sandwich)
            // a dollar account: the numeraire's own coin in dollars, the bar's close (no range)
            let px = if c.hedge.usd { sim.upn } else { Sim::hedge_px(c, c.s_tick(m.bar_tick[b])) };
            let ends = if c.hedge.usd { [px, px] } else { [m.bar_hi[b], m.bar_lo[b]].map(|t| Sim::hedge_px(c, c.s_tick(t))) };
            let (hi, lo) = (ends.iter().copied().fold(px, f64::max), ends.iter().copied().fold(px, f64::min));
            sim.hedge_mark(c, cost, last_t, t, px, hi, lo, t);
        }
        (last_i, last_t) = (i, t);
        match strat {
            Strategy::Hodl => {
                if sim.hodl.is_none() {
                    // the mix a range of ±10% around the price would take: half and half by value
                    let p = s * s;
                    let v1 = if c.num1 { lp_capital } else { lp_capital * p };
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
                        Decision::Move { .. } if !spot_ok(i) => spot_blocked += 1,
                        Decision::Move { .. } if !sim.may_move(cost, t) => sim.cap_blocked += 1,
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
            // with an execution delay, what was decided on an earlier bar (its time, price and
            // signals) is traded now, at this bar's price; nothing is decided before that bar exists
            Strategy::Graph(_) if b < b0 + cost.exec_delay_bars => {}
            Strategy::Graph(g) => {
                let range = sim.span();
                let bd = b - cost.exec_delay_bars;
                let (td, tick_d, i_d) = if bd == b { (t, tick, i) } else { (m.bar_end[bd], sim.tick_at(c, m.bar_idx[bd]), m.bar_idx[bd]) };
                match crate::graph::decide(g, gev.as_ref().unwrap(), &mut cst, td, bd, tick_d, range, c.spacing) {
                    Decision::Move { .. } if !spot_ok(i) => spot_blocked += 1,
                    Decision::Move { .. } if !sim.may_move(cost, t) => sim.cap_blocked += 1,
                    Decision::Move { tl, tu, reason } => {
                        // the ladder's settings at the decision (its inputs may set them)
                        let lad = glad.as_ref().and_then(|_| crate::graph::ladder_at(g, gev.as_ref().unwrap(), bd, tick_d, range));
                        match &lad {
                            // a ladder block: the ladder's own solve, taken when it moves enough of the value
                            Some(p) => {
                                sim.from_usd(c, cost);
                                let (x, y) = sim.inventory(c, s);
                                let target = crate::engine::cap_ranges(
                                    crate::engine::snap_ranges(ladder::decide_at(m, p, x + y / (s * s), s / c.sqrt_px, tick_d, i_d), c.spacing),
                                    g.max_ranges,
                                );
                                if sim.ranges.is_empty() || turnover(c, &sim, &target, s) >= p.turnover {
                                    sim.rebalance(c, cost, i, t, &target, &reason);
                                    cst.moved(t);
                                } else {
                                    // a solve that kept the ranges, as live (engine.rs): no move
                                    cst.solved(t);
                                }
                            }
                            None => {
                                sim.rebalance(c, cost, i, t, &[(tl, tu, 1.0)], &reason);
                                cst.moved(t);
                            }
                        }
                    }
                    Decision::Exit(reason) => {
                        let to = crate::graph::exit_to(g, gev.as_ref().unwrap(), bd, tick_d, range);
                        sim.exit(c, cost, i, t, to, &reason);
                        cst.exited_to(to);
                        // the minimum time out counts from the exit itself
                        cst.out_since = Some(t);
                    }
                    Decision::Wait(r) => {
                        if r.starts_with("hold") {
                            guard_frozen += 1;
                        }
                    }
                    Decision::Keep(_) => {}
                }
            }
        }
        if let (Some(h), Strategy::Graph(g), true) = (&hedge, strat, b >= b0 + cost.exec_delay_bars) {
            let bd = b - cost.exec_delay_bars;
            let tick_d = if bd == b { tick } else { sim.tick_at(c, m.bar_idx[bd]) };
            let (ratio, size) = crate::graph::hedge_ratio(g, gev.as_ref().unwrap(), bd, tick_d, sim.span()).unwrap_or((0.0, 0.0));
            let px = if c.hedge.usd { sim.upn } else { Sim::hedge_px(c, c.s_tick(m.bar_tick[b])) };
            sim.hedge_retarget(c, cost, h, ratio, size, s, px, t);
        }
        if t >= next_mark || b == b1 {
            let (x, y) = sim.inventory(c, s);
            let hodl = sim.hodl.map(|(hx, hy)| c.value(hx, hy, s)).unwrap_or(lp_capital) + sim.margin0;
            series.push(json!([t, round6(c.value(x, y, s) + sim.usd_value() + sim.cash + (sim.margin + sim.h_acc) * sim.hk(c)), round6(hodl)]));
            next_mark = (t.div_euclid(3600) + 1) * 3600;
        }
    }
    let t_end = m.bar_end[b1];
    sim.close_segment(t_end);
    let s_end = sim.s_at(c, m.bar_idx[b1]);
    let (x, y) = sim.inventory(c, s_end);
    let inventory = c.value(x, y, s_end) + sim.usd_value();
    let value = inventory + sim.cash + (sim.margin + sim.h_acc) * sim.hk(c);
    let hodl = sim.hodl.map(|(hx, hy)| c.value(hx, hy, s_end)).unwrap_or(lp_capital);
    let il = inventory + sim.to_margin - hodl + sim.swap_costs;
    // the hold baseline holds the margin too (in the numeraire)
    let hodl = hodl + sim.margin0;
    if sim.cap_blocked > 0 {
        watch_errors.push(format!("live's daily cap of {} rebalances blocked {} moves the strategy wanted (they were not made here either)",
                                  cost.max_actions_per_day.unwrap_or(0), sim.cap_blocked));
    }
    if hedge.is_some() && c.hedge.from_h > m.bar_end[b0] {
        watch_errors.push("the perp listed after this run starts: until then its margin sat idle as cash, outside the pool".into());
    }
    if sim.swaps_over > 0 {
        let lim = cost.max_swap_cost_bps.unwrap_or(3.0);
        watch_errors.push(format!("{} swaps cost more than the live limit of {lim} bp (the dearest {:.1} bp): live refuses those swaps (it swaps the part within the limit, deposits the rest on its own side of the range or leaves it idle, and keeps in-range positions it would have withdrawn), so it cannot trade this way unless the pool's max swap cost is raised to about {:.0} bp",
                                  sim.swaps_over, sim.max_swap_bps, sim.max_swap_bps.ceil()));
    }
    let hedge_json = hedge.map(|h| {
        json!({"perps": c.hedge.label, "ratio": h.ratio, "band_pct": h.band * 100.0, "leverage": hedge_leverage(c, h.leverage), "margin": round6(sim.margin0),
               "trades": sim.hedge_trades, "margin_moves": sim.margin_moves, "available_from": c.hedge.from_h, "liquidation_bars": sim.liq_bars, "liquidations": sim.liq_bars, "margin_short": sim.margin_short,
               "min_liquidation_distance": sim.min_liq_dist.map(round6), "max_naked": round6(sim.max_naked),
               "short_end": round6(sim.short), "margin_events": sim.margin_log, "margin_events_dropped": sim.margin_log_dropped})
    });
    // the worst fall of the hourly value from its high, % (an hourly series: a fall inside an hour
    // is not seen)
    let max_dd = {
        let mut hi = f64::MIN;
        let mut dd = 0.0f64;
        for p in &series {
            let q = p[1].as_f64().unwrap_or(0.0);
            hi = hi.max(q);
            if hi > 0.0 {
                dd = dd.max(1.0 - q / hi);
            }
        }
        round6(dd * 100.0)
    };
    let out = json!({
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
        "swaps_over_limit": sim.swaps_over,
        "cap_blocked": sim.cap_blocked,
        "max_swap_bps": round6(sim.max_swap_bps),
        "gas": round6(sim.gas),
        "costs": round6(sim.swap_costs + sim.gas),
        "il": round6(il),
        "hedge_pnl": round6(sim.hedge_pnl),
        "funding": round6(sim.funding),
        "hedge_fees": round6(sim.hedge_fees),
        "hedge_net": round6(sim.hedge_pnl + sim.funding - sim.hedge_fees),
        "hedge": hedge_json,
        "actions": sim.actions,
        "time_in_range": if sim.t_tot > 0.0 { sim.t_in / sim.t_tot } else { 0.0 },
        "guard_bars": guard_frozen,
        "spot_blocked_bars": spot_blocked,
        "series": series,
        "segments": sim.segments,
        "events": sim.events,
        "max_drawdown": max_dd,
        "events_dropped": sim.events_dropped,
        "events_truncated": sim.events_dropped > 0,
    });
    (out, Carry { sim, cst, last: (last_i, last_t) })
}

/// How far the tick after swap `i` is from the median tick of the swaps in the hour before it:
/// the live engine's spot check, on the same swaps.
fn spot_dev(m: &Market, i: usize) -> f64 {
    let t0 = m.ts[i] - 3600;
    let mut j = i;
    while j > 0 && m.ts[j - 1] >= t0 {
        j -= 1;
    }
    let mut w: Vec<f64> = m.tick[j..=i].iter().map(|k| *k as f64).collect();
    (m.tick[i] as f64 - data::median(&mut w)).abs()
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

/// BATCH_WORKERS, or fewer when ALM_BT_WORKERS says so (to keep a machine's other work going).
fn batch_workers() -> usize {
    std::env::var("ALM_BT_WORKERS").ok().and_then(|v| v.parse().ok()).unwrap_or(BATCH_WORKERS).clamp(1, BATCH_WORKERS)
}
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
    let warm = warmup_s(req);
    let t0 = std::time::Instant::now();
    // where the time went, ms by phase (in the result as timings_ms)
    let mut timings = Map::new();
    let mut lap = std::time::Instant::now();
    let mut mark = |k: &str, timings: &mut Map<String, Value>| {
        timings.insert(k.into(), json!(lap.elapsed().as_millis() as u64));
        lap = std::time::Instant::now();
    };
    // topped up at most every two minutes: rule edits in a row replay the same events
    static ENSURED: Mutex<Option<HashMap<String, (std::time::Instant, i64)>>> = Mutex::new(None);
    let ekey = format!("{cid}:{pool}");
    let recent = ENSURED.lock().unwrap().get_or_insert_with(HashMap::new).get(&ekey)
        .map(|(t, f)| t.elapsed().as_secs() < 120 && *f <= from - warm).unwrap_or(false);
    // the fee history downloads while the events do
    let apr_job = {
        let pool = pool.to_string();
        std::thread::spawn(move || apr_rows(cid, &pool, from - warm - 3600))
    };
    let rep = if recent {
        crate::history::Report::default()
    } else {
        let r = crate::history::ensure(cid, &[pool.to_string()], from - warm)?;
        ENSURED.lock().unwrap().as_mut().unwrap().insert(ekey, (std::time::Instant::now(), from - warm));
        r
    };
    mark("history", &mut timings);
    step(id, "loading fee history");
    let (tok0, tok1, spacing) = crate::chain::pool_info(cid, pool)?;
    let (m0, m1) = (crate::prices::meta(cid, &tok0)?, crate::prices::meta(cid, &tok1)?);
    let num1 = req["numeraire"].as_i64().map(|n| n == 1).unwrap_or_else(|| {
        ["WETH", "ETH"].contains(&m1.symbol.to_uppercase().as_str()) && !["USDC", "USDT"].contains(&m0.symbol.to_uppercase().as_str())
    });
    let apr_rows = apr_job.join().map_err(|_| "the fee history download failed".to_string())??;
    mark("fee_history", &mut timings);
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
    let mkey = format!("{cid}:{pool}:{days}:{}:{num1}:{wallet}:{cov_to}:{}:{warm}", from.div_euclid(3600), apr_rows.len());
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
        let mut events = crate::history::read(cid, pool, from.div_euclid(3600) * 3600 - warm, now + 3600);
        // the wallet's own mints and burns from before the window too, as live loads them: a
        // position opened before it and still open is the wallet's, not others' liquidity (the
        // replayed strategy stands in for it); duplicates are dropped by the builder
        if !wallet.is_empty() {
            events.extend(data::read_jsonl(&data::wallet_path(cid, pool, wallet)));
        }
        let as_of = crate::history::coverage(cid, pool).map(|c| c.to_ts);
        let m = std::sync::Arc::new(data::build_market_asof(events, &wallet.to_lowercase(), &apr, m0.decimals as i32, m1.decimals as i32, if num1 { 1 } else { 0 }, bar_s, as_of)?);
        let mut g = LAST.lock().unwrap();
        g.push((key, m.clone()));
        while g.len() > 2 {
            g.remove(0);
        }
        Ok(m)
    };
    let m_arc = market_at(data::BAR_S)?;
    let m: &Market = &m_arc;
    mark("replay", &mut timings);
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
    // how much the pool's flow follows its liquidity, from the week before the period (what was
    // known at its start: the period's own swaps would fit it to the result), or as the request says
    let mut elasticity = crate::elasticity::estimate(m, m.bar_end[b0], 7);
    if let Some(b) = req["flow_beta"].as_f64().filter(|b| (0.0..=1.0).contains(b)) {
        elasticity["beta"] = json!(b);
        elasticity["method"] = json!("set");
    }
    let beta = elasticity["beta"].as_f64().unwrap_or(0.5);
    // the perps a Hedge block trades, loaded only when a strategy in the request has one
    let hedge = if serde_json::to_string(req).map(|s| s.contains("\"hedge\"") || s.contains("\"funding\"")).unwrap_or(false) {
        hedge_market(if num1 { &m0.symbol } else { &m1.symbol }, if num1 { &m1.symbol } else { &m0.symbol }, from - warm)
    } else {
        HedgeMkt::default()
    };
    let c = Ctx {
        m,
        spacing,
        num1,
        num_per_usd,
        emis_usd_h: apr.iter().map(|(h, (_, e))| (*h, *e)).collect(),
        sqrt_px: m.px_scale.sqrt(),
        beta,
        // `live_parking` (the agent's strategy gate): parking earns only when the live switch is on
        park: if req["live_parking"] == true && !crate::parking::enabled(&crate::settings::load_settings()) {
            Default::default()
        } else {
            park_rates(cid, [tok0.as_str(), tok1.as_str()], from - warm, &Costs::from(&req["costs"]), req["capital_usd"].as_f64().unwrap_or(0.0))
        },
        cid,
        num_token: num_tok.to_lowercase(),
        hedge,
        // measured swap costs by day (Liquidity Lens, LLENS_REQUESTS R17): {"<day start>": [fee bp, 1% depth USD]};
        // the days outside it use costs.swap_bps and costs.swap_depth_usd (set to the measured average)
        swap_days: req["costs"]["swap_days"]
            .as_object()
            .map(|o| {
                o.iter()
                    .filter_map(|(d, v)| Some((d.parse::<i64>().ok()?.div_euclid(86400), (v[0].as_f64()?, v[1].as_f64()?))))
                    .filter(|(_, (f, d))| f.is_finite() && *f >= 0.0 && *d > 0.0)
                    .collect()
            })
            .unwrap_or_default(),
        tref: Default::default(),
        peg_damp: false,
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
    // our liquidity's weight on the pool's price (Sim::tick_at); off with "depth_damping": false
    let damp = req["depth_damping"].as_bool().unwrap_or(true);
    let peg_damp = damp && pegged;
    let tref = std::sync::Arc::new(if damp && !pegged { depth_ref(m) } else { vec![] });
    let rp5 = Rp { c: Ctx { m, tref: tref.clone(), peg_damp, ..c }, b0, b1 };
    let rp60 = match &m60_arc {
        Some(mm) => {
            let mm: &Market = mm;
            let a = mm.bar_end.partition_point(|t| *t < from);
            if mm.bar_end.is_empty() || a >= mm.bar_end.len() {
                return Err("not enough swaps in the period".into());
            }
            Some(Rp { c: Ctx { m: mm, spacing, num1, num_per_usd, emis_usd_h: rp5.c.emis_usd_h.clone(), sqrt_px: mm.px_scale.sqrt(), beta, park: rp5.c.park.clone(), cid, num_token: rp5.c.num_token.clone(), hedge: rp5.c.hedge.clone(), swap_days: rp5.c.swap_days.clone(), tref: tref.clone(), peg_damp }, b0: a, b1: mm.bar_end.partition_point(|t| *t <= end).saturating_sub(1).max(a) })
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
            for _ in 0..batch_workers().min(items.len().max(1)) {
                sc.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    if k >= items.len() {
                        break;
                    }
                    let it = &items[k];
                    let sv = it["strategy"].as_object().cloned().unwrap_or_default();
                    let key = format!("{mkey}:{capital}:{beta}:{max_ranges}:{}:{}:{}:{}", rp5.c.hedge.legs, days_key(&rp5.c.swap_days), cost.json(), Value::Object(sv.clone()));
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
                                   "park_apr": ann("park_yield"), "hedge_apr": ann("hedge_net"), "funding_apr": ann("funding"),
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
        let base = if strat_v.get("type").and_then(|t| t.as_str()) == Some("ladder") {
            crate::ladder::as_graph(&Params::from(&ladder_params(&strat_v, spacing, pegged))?).as_object().unwrap().clone()
        } else {
            strat_v.clone()
        };
        let vs = crate::overfit::variants(&base, WF_VARIANTS);
        let rp = rp_for(&strat_v);
        let (c, b0, b1, m) = (&rp.c, rp.b0, rp.b1, rp.c.m);
        let (f0, f1) = (m.bar_end[b0], m.bar_end[b1]);
        // the reference each variant's slices are measured over: "own" (default), holding the
        // variant's own first deposit; "hold", one 50/50 hold from the start shared by every variant
        let shared_bench = req["benchmark"].as_str() == Some("hold");
        let bench: Vec<(i64, f64)> = if shared_bench {
            run(c, &Strategy::Hodl, &cost, capital, b0, b1)["series"].as_array().map(|a| a.iter().filter_map(|p| Some((p[0].as_i64()?, p[1].as_f64()?))).collect()).unwrap_or_default()
        } else {
            vec![]
        };
        let rebase = |series: &[Value]| -> Vec<Value> { if shared_bench { with_benchmark(series, &bench) } else { series.to_vec() } };
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let rows: Mutex<Vec<Option<Result<Vec<f64>, String>>>> = Mutex::new(vec![None; vs.len()]);
        std::thread::scope(|sc| {
            for _ in 0..batch_workers().min(vs.len()) {
                sc.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    if k >= vs.len() {
                        break;
                    }
                    let r = strategy(&vs[k], spacing, max_ranges, pegged).map(|st| {
                        let v = run(c, &st, &cost, capital, b0, b1);
                        crate::overfit::slice_excess(&rebase(v["series"].as_array().map(|a| a.as_slice()).unwrap_or(&[])), f0, f1, WF_SLICES, capital)
                    });
                    rows.lock().unwrap()[k] = Some(r);
                    let d = done.fetch_add(1, Ordering::SeqCst) + 1;
                    step(id, &format!("overfit check: {d} of {} runs", vs.len()));
                });
            }
        });
        let rows = rows.into_inner().unwrap();
        let mut matrix = vec![];
        let mut kept = vec![];
        for (k, r) in rows.into_iter().enumerate() {
            match r {
                Some(Ok(x)) => {
                    matrix.push(x);
                    kept.push(k);
                }
                Some(Err(e)) if k == 0 => return Err(e),
                _ => {} // a neighbour the checks refuse (a chain too long, say) is left out
            }
        }
        let mut st = crate::overfit::stats(&matrix);
        if let Some(o) = st.as_object_mut() {
            for (k, v) in [("chain_id", json!(cid)), ("pool", json!(pool)), ("days", json!(days)), ("span_days", json!(span_days)), ("from", json!(f0)), ("to", json!(f1)),
                           ("capital", json!(capital)), ("elapsed_ms", json!(t0.elapsed().as_millis() as u64)),
                           ("benchmark", json!(if shared_bench { "hold 50/50 from the start, shared" } else { "each variant's own first deposit, held" })),
                           // per variant (0: the strategy) its return over the benchmark per slice, %/yr, and its settings
                           ("matrix", json!(kept.iter().zip(&matrix).map(|(k, x)| json!({"variant": k, "slices": x, "strategy": vs[*k]})).collect::<Vec<_>>()))] {
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
    mark("strategy", &mut timings);
    // what to compare with: strategies from the library (default: the ladder), and holding
    let picks: Vec<Value> = match &req["compare"] {
        Value::Array(a) => a.iter().take(MAX_COMPARE).cloned().collect(),
        Value::Bool(false) => vec![],
        _ => vec![json!({"id": "ladder", "name": "Ladder", "strategy": {"type": "ladder"}})],
    };
    let mut compare = vec![];
    for (k, pick) in picks.iter().enumerate() {
        let sv = pick["strategy"].as_object().cloned().unwrap_or_default();
        let key = format!("{mkey}:{capital}:{beta}:{max_ranges}:{}:{}:{}:{}", rp5.c.hedge.legs, days_key(&rp5.c.swap_days), cost.json(), Value::Object(sv.clone()));
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
    mark("comparisons", &mut timings);
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
    mark("charts", &mut timings);
    Ok(json!({
        "chain_id": cid, "pool": pool, "from": m.bar_end[b0], "to": m.bar_end[b1], "days": days, "span_days": span_days,
        "symbols": [m0.symbol, m1.symbol], "decimals": [m0.decimals, m1.decimals], "tick_spacing": spacing,
        "numeraire": if num1 { 1 } else { 0 }, "numeraire_symbol": if num1 { &m1.symbol } else { &m0.symbol },
        "numeraire_usd": usd, "capital": capital, "capital_usd": capital_usd, "entry_cost": entry_cost, "costs": cost.json(),
        "swaps": m.ts.len(), "history_requests": rep.requests,
        "elapsed_ms": t0.elapsed().as_millis() as u64, "timings_ms": timings,
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
    // walk-forward: the training part in consecutive folds, slices of one continuous run (at least a day each)
    let folds = (spec["folds"].as_u64().unwrap_or(4) as usize).clamp(1, 24).min(days(t_a, t_split).floor().max(1.0) as usize);
    let cuts: Vec<i64> = (0..=folds).map(|k| t_a + (t_split - t_a) * k as i64 / folds as i64).collect();
    // a run handed on from the one before it (the bars after its last), for the walk-forward's
    // re-tuned settings: one portfolio, the settings changed at each period's start
    // an error hands the portfolio back, so a period that cannot run does not reset it
    let run_chain = |s: &Map<String, Value>, from: i64, to: i64, init: Option<Carry>| -> Result<(Vec<(i64, f64, f64)>, Carry), (String, Option<Carry>)> {
        let st = match strategy(s, spacing, max_ranges, pegged) {
            Ok(st) => st,
            Err(e) => return Err((e, init)),
        };
        let r = rp_for(s);
        // handed on: from the first bar after the last one the run before traded (its last bar may
        // end past `from`), so no bar is traded twice
        let a = match init.as_ref() {
            Some(k) => r.c.m.bar_end.partition_point(|t| *t <= k.last.1.max(from - 1)),
            None => r.c.m.bar_end.partition_point(|t| *t < from),
        }
        .clamp(r.b0, r.b1);
        // through the last bar that ends by `to`, as `slices` ends a period: the next run starts
        // after it, and no bar past the period counts in it
        let b = r.c.m.bar_end.partition_point(|t| *t <= to).saturating_sub(1).clamp(r.b0, r.b1);
        if b < a {
            return Err(("a period with no bars".into(), init));
        }
        let (v, k) = run_carry(&r.c, &st, cost, capital, a, b, init);
        Ok((v["series"].as_array().map(|a| a.iter().filter_map(|p| Some((p[0].as_i64()?, p[1].as_f64()?, p[2].as_f64()?))).collect()).unwrap_or_default(), k))
    };
    let run_series = |s: &Map<String, Value>, from: i64, to: i64| -> Result<Vec<(i64, f64, f64)>, String> {
        let st = strategy(s, spacing, max_ranges, pegged)?;
        let r = rp_for(s);
        let a = r.c.m.bar_end.partition_point(|t| *t < from).clamp(r.b0, r.b1);
        let b = r.c.m.bar_end.partition_point(|t| *t < to).clamp(r.b0, r.b1);
        if b <= a {
            return Err("a period with no bars".into());
        }
        let v = run(&r.c, &st, cost, capital, a, b);
        Ok(v["series"].as_array().map(|a| a.iter().filter_map(|p| Some((p[0].as_i64()?, p[1].as_f64()?, p[2].as_f64()?))).collect()).unwrap_or_default())
    };
    // the search's score: the reliable return over the folds (optimize.rs `composite`)
    // its folds are slices of one continuous run over the training part, as the backtest runs it
    let fold_spans: Vec<(i64, i64)> = (0..folds).map(|k| (cuts[k], cuts[k + 1])).collect();
    let eval = |s: &Map<String, Value>| -> Result<(f64, Value), String> { eval_over(&run_series, capital, vs_hold, t_a, &fold_spans, s) };
    let flag = opt::flag(id);
    if auto {
        let r = if spec["mode"].as_str() == Some("walk") {
            walk_forward(id, spec, base, o.workers, &run_series, &run_chain, capital, (t_a, t_b), vs_hold, &flag)
        } else {
            // every window, exam half and period a slice of one continuous run from the span's start,
            // as the backtest runs a strategy (a window run afresh flattered settings that pay off
            // right after a fresh start)
            let span_cont = |s: &Map<String, Value>, a: i64, b: i64| -> Result<(f64, Value), String> {
                let pts = run_series(s, t_a, b)?;
                let (apr, vh) = (slices(&pts, &[(a, b)], capital, false)[0].0, slices(&pts, &[(a, b)], capital, true)[0].0);
                if !apr.is_finite() {
                    return Err("no value in this window".into());
                }
                Ok((if vs_hold { vh } else { apr }, json!({"apr": apr, "vs_hold_apr": vh})))
            };
            auto_attempts(id, spec, base, o.workers, &span_cont, &run_series, capital, (t_a, t_b), vs_hold, &flag)
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
        // the held-out quarter as a slice of one continuous run from the start: the strategy comes
        // into it as it would, holding what the training part left it
        let test_cont = |s: &Map<String, Value>| -> Value {
            match run_series(s, t_a, t_b) {
                Ok(pts) => {
                    let (apr, vh) = (slices(&pts, &[(t_split, t_b)], capital, false)[0].0, slices(&pts, &[(t_split, t_b)], capital, true)[0].0);
                    json!({"apr": if apr.is_finite() { json!(apr) } else { Value::Null }, "vs_hold_apr": if vh.is_finite() { json!(vh) } else { Value::Null }})
                }
                Err(e) => json!({"error": e}),
            }
        };
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
                    let v = test_cont(&tops[k]);
                    tests.lock().unwrap()[k] = Some(v);
                });
            }
        });
        // over all periods: the training folds and the held-out quarter, scored alike, shown for
        // each finalist. The ranking stays the search's, on the training folds only: re-ranking on a
        // score that includes the held-out quarter picked the finalist that was lucky there, and its
        // held-out number was then no independent test (Oct 2026 review)
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
        }
        current["test"] = test_cont(base);
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

/// A run's series [[ts, value, hold value], …] with its hold column replaced by a benchmark's value
/// (the last at or before each time), so every run is measured over the same reference.
fn with_benchmark(series: &[Value], bench: &[(i64, f64)]) -> Vec<Value> {
    series.iter().filter_map(|p| {
        let t = p[0].as_i64()?;
        let k = bench.partition_point(|q| q.0 <= t);
        let h = if k == 0 { bench.first()? } else { &bench[k - 1] };
        Some(json!([t, p[1], h.1]))
    }).collect()
}

/// A run of a strategy over [from, to): the objective (annualised, %) and its row.
type Span<'a> = dyn Fn(&Map<String, Value>, i64, i64) -> Result<(f64, Value), String> + Sync + 'a;
/// One continuous run over [from, to): its hourly (time, value, holding's value), in the numeraire.
type Series<'a> = dyn Fn(&Map<String, Value>, i64, i64) -> Result<Vec<(i64, f64, f64)>, String> + Sync + 'a;
/// A run over [from, to) from where another left off (None: afresh): its series and what it hands on.
type Chain<'a> = dyn Fn(&Map<String, Value>, i64, i64, Option<Carry>) -> Result<(Vec<(i64, f64, f64)>, Carry), (String, Option<Carry>)> + Sync + 'a;

/// How auto mode cuts the period: the last eighth is the exam, which no search, screening or
/// validation ever runs on; the rest is cut into WINDOWS windows, and each attempt validates on one
/// of them (the most recent first, a fresh one every attempt) and searches on the others, its folds.
const WINDOWS: usize = 5;
/// Candidates that may take the exam: each look makes a lucky pass likelier, so few, and the second
/// must pass each half of it.
const EXAMS: usize = 2;

/// The reliable return over `spans` (optimize.rs `composite`), and the row the lab shows. The spans
/// are slices of one continuous run from `origin` (the span's start) to the last, as the backtest
/// runs a strategy (positions, ranges and latches carry over), not runs that each start afresh, nor
/// one that starts afresh at the first span scored (a window left out made a later fresh start).
fn eval_over(series: &Series, capital: f64, vs_hold: bool, origin: i64, spans: &[(i64, i64)], s: &Map<String, Value>) -> Result<(f64, Value), String> {
    let (from, to) = (spans.iter().map(|p| p.0).min().ok_or("no fold")?.min(origin), spans.iter().map(|p| p.1).max().ok_or("no fold")?);
    let pts = series(s, from, to)?;
    let apr: Vec<f64> = slices(&pts, spans, capital, false).into_iter().map(|x| x.0).collect();
    let vh: Vec<f64> = slices(&pts, spans, capital, true).into_iter().map(|x| x.0).collect();
    let (score, parts) = crate::optimize::composite(if vs_hold { &vh } else { &apr });
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len().max(1) as f64;
    Ok((score, json!({"score": score, "wf": parts, "apr": mean(&apr), "vs_hold_apr": mean(&vh)})))
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
fn auto_attempts(id: u64, spec: &Value, base: &Map<String, Value>, workers: usize, span: &Span, series: &Series, capital: f64, period: (i64, i64),
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
        let eval: Box<opt::Eval> = Box::new(move |s: &Map<String, Value>| eval_over(series, capital, vs_hold, t_a, &f2, s));
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
fn walk_forward(id: u64, spec: &Value, base: &Map<String, Value>, workers: usize, series: &Series, chain: &Chain, capital: f64, period: (i64, i64),
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
    // every result is a slice of one continuous run, as the Lab runs a strategy: positions, ranges
    // and latches carry over from one period into the next. Periods run each on its own, starting
    // afresh, favoured settings that only pay off right after a fresh start, and the Lab's
    // continuous backtest then fell (Oct 2026: a range setting that won 7 of 9 fresh 30-day runs lost
    // in every continuous year). Per period: (the objective, holding), %/yr of the capital.
    let cont = |s: &Map<String, Value>, from: i64, periods: &[(i64, i64)]| -> Result<Vec<(f64, f64)>, String> {
        let end = periods.iter().map(|p| p.1).max().ok_or("no period")?;
        Ok(slices(&series(s, from, end)?, periods, capital, vs_hold))
    };
    let eval_cont = |s: &Map<String, Value>, folds: &[(i64, i64)]| -> Result<(f64, Value), String> {
        // from the span's start, as every other score here: a training window run afresh is not
        // what these settings would hold coming into it
        let v: Vec<f64> = cont(s, t_a, folds)?.into_iter().map(|x| x.0).collect();
        let (score, parts) = opt::composite(&v);
        let mean = v.iter().copied().filter(|x| x.is_finite()).sum::<f64>() / v.len().max(1) as f64;
        let mut row = json!({"score": score, "wf": parts});
        row[key] = json!(mean);
        Ok((score, row))
    };

    // which settings to tune: screened on the first training span only
    let f0 = folds_of(first);
    let ev0 = |s: &Map<String, Value>| eval_cont(s, &f0);
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
        let ev = |s: &Map<String, Value>| eval_cont(s, folds);
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
    // re-tuning as it would really run: one portfolio, from the start of the span as the current
    // settings run, each period traded with the settings tuned just before it on what the period
    // before handed it (its ranges, tokens and hedge), never a portfolio another setting built.
    // The return of a period: its value change less holding's when vs_hold, %/yr of the capital
    // (the objective, the plain return)
    let seg = |pts: &[(i64, f64, f64)], a: i64, b: i64| -> (f64, f64) {
        match (pts.first(), pts.last()) {
            (Some(p), Some(q)) if b > a && capital > 0.0 => {
                let ann = 365.0 * 86400.0 / (b - a) as f64 / capital * 100.0;
                (((q.1 - p.1) - if vs_hold { q.2 - p.2 } else { 0.0 }) * ann, (q.1 - p.1) * ann)
            }
            _ => (f64::NAN, f64::NAN),
        }
    };
    let mut wf_apr: Vec<f64> = vec![];
    let mut carry: Option<Carry> = None;
    for t in first..n {
        if stopped() {
            break;
        }
        let folds = folds_of(t);
        let (tuned, row) = tune(&folds, t - first + 1);
        // before the first tested period the current settings trade from the span's start (the
        // ones known then: settings tuned on that time must not build the portfolio they are then
        // tested with), so the portfolio comes into it already built
        if t == first && per[t].0 > t_a {
            carry = chain(base, t_a, per[t].0, None).ok().map(|x| x.1);
        }
        let (oos, oos_apr) = match chain(&tuned, per[t].0, per[t].1, carry.take()) {
            Ok((pts, k)) => {
                carry = Some(k);
                seg(&pts, per[t].0, per[t].1)
            }
            Err((_, back)) => {
                carry = back;
                (f64::NAN, f64::NAN)
            }
        };
        wf.push(oos);
        wf_apr.push(oos_apr);
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
    // the tested periods and, last, the whole span (what the Lab's backtest shows), each from one
    // continuous run from the start of the span
    let mut tests_whole = tests.clone();
    tests_whole.push((t_a, t_b));
    let run_all = |xs: &[&Map<String, Value>]| -> Vec<Vec<(f64, f64)>> {
        let (cont, tw) = (&cont, &tests_whole);
        let nan = vec![(f64::NAN, f64::NAN); tw.len()];
        std::thread::scope(|sc| {
            let hs: Vec<_> = xs.chunks(xs.len().div_ceil(workers.max(1)).max(1)).map(|c| c.to_vec()).map(|c| sc.spawn(move || c.iter().map(|s| cont(s, t_a, tw).ok()).collect::<Vec<_>>())).collect();
            hs.into_iter().flat_map(|h| h.join().unwrap()).map(|r| r.unwrap_or_else(|| nan.clone())).collect()
        })
    };
    let subj = run_all(&subjects);
    // the benchmark nothing is tuned on and every candidate shares: holding 50/50 from the span's
    // start, its plain return per tested period and over the whole span, and each row's return over it
    let hodl: Map<String, Value> = json!({"type": "hodl"}).as_object().unwrap().clone();
    let bench_all = cont(&hodl, t_a, &tests_whole).unwrap_or_else(|_| vec![(f64::NAN, f64::NAN); tests_whole.len()]);
    let plain = |x: &(f64, f64)| if vs_hold { x.0 + x.1 } else { x.0 };
    let bench: Vec<f64> = bench_all[..tested].iter().map(plain).collect();
    let over = |v: &[f64]| -> Vec<f64> { v.iter().zip(&bench).map(|(a, b)| a - b).collect() };
    let cur: Vec<f64> = subj[0][..tested].iter().map(|x| x.0).collect();
    let cur_whole = subj[0][tested].0;
    let bandv: Vec<f64> = if has_band { subj[1][..tested].iter().map(|x| x.0).collect() } else { vec![] };
    let hold: Vec<f64> = if vs_hold { vec![] } else { subj[0][..tested].iter().map(|x| x.1).collect() };
    let cur_apr: Vec<f64> = subj[0][..tested].iter().map(plain).collect();
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
    let passed = retune_passes(tested, dims.is_empty(), s_wf, s_ref, s_cur, edge_lb, wins, wins_ref, cur_in);
    let avg = |v: &[f64]| {
        let f: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
        f.iter().sum::<f64>() / f.len().max(1) as f64
    };
    let (m_wf, m_cur) = (avg(&wf), avg(&cur));
    let edge_txt = format!("the edge per period averaged {edge_mean:+.1} points, reliably at least {edge_lb:+.1}");
    let mut why = if dims.is_empty() {
        "keep the current settings: no setting moved the reliable return by a point or more on the first training span, so there is nothing to tune".to_string()
    } else if tested < 3 {
        format!("too few periods tested ({tested}) to judge: a longer span or shorter periods")
    } else if passed && cur_in {
        format!("re-tuning every {pdays:.0} days on {train_txt} beat the references nothing was tuned on, out of sample ({s_wf:+.1} vs {s_ref:+.1} %/yr reliable over {tested} periods, better in {wins_ref} of them; {edge_txt}), and the current settings in {wins} of the {tested} periods ({s_wf:+.1} vs {s_cur:+.1}, though theirs is in-sample, picked on this history): apply the settings tuned on the most recent periods, and re-tune every {pdays:.0} days")
    } else if cur_in && s_wf > s_ref && wins_ref * 2 > tested && edge_lb > 0.0 {
        format!("no new settings: re-tuning beat the references nothing was tuned on ({s_wf:+.1} vs {s_ref:+.1} %/yr reliable, better in {wins_ref} of {tested} periods), but not the current settings: better than them in only {wins} of {tested} periods (mean {m_wf:+.1} vs {m_cur:+.1} %/yr). The current settings are in-sample here, but tuned settings that lose to them in most periods are no improvement to apply")
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
    let mut effects: Vec<Value> = vec![];
    if tested >= 3 && !dims.is_empty() && !stopped() {
        // the settings to use now: tuned on the most recent periods, as the walk would next
        let folds = folds_of(n);
        let (_, mut r) = tune(&folds, steps + 1);
        // then each tuned change on its own, the others at their current values, on the tested
        // periods: only a change that helps there (better on average and in most periods) is kept;
        // one that alters nothing (a setting dead at that value), hurts or is mixed stays at its
        // current value, so a good change cannot carry a bad one along
        let x0 = opt::current(base, &dims);
        let xt: Vec<f64> = dims.iter().map(|d| r["values"][d.key().as_str()].as_f64().unwrap_or(f64::NAN)).collect();
        let changed: Vec<usize> = (0..dims.len()).filter(|i| xt[*i].is_finite() && (xt[*i] - x0[*i]).abs() > 1e-12 * x0[*i].abs().max(1.0)).collect();
        let alone: Vec<Map<String, Value>> = changed.iter().map(|i| {
            let mut x = x0.clone();
            x[*i] = xt[*i];
            opt::apply(base, &dims, &x)
        }).collect();
        report("effects", steps + 2, 0, alone.len() + 1);
        let refs: Vec<&Map<String, Value>> = alone.iter().collect();
        let res = run_all(&refs);
        report("effects", steps + 2, alone.len(), alone.len() + 1);
        let mut keep = x0.clone();
        let mut wholes: Vec<(usize, f64, f64)> = vec![];
        for (a, i) in changed.iter().enumerate() {
            let vals: Vec<f64> = res[a][..tested].iter().map(|x| x.0).collect();
            let whole = res[a][tested].0;
            let diffs: Vec<f64> = vals.iter().zip(&cur).map(|(v, c)| v - c).collect();
            let (mut effect, mean_diff, better, worse) = effect_of(&diffs);
            // and over the whole span, as the Lab's backtest will show it
            if effect == "helps" && !(whole >= cur_whole) {
                effect = "mixed";
            }
            if effect == "helps" {
                keep[*i] = xt[*i];
                wholes.push((*i, mean_diff, whole));
            }
            effects.push(json!({"key": dims[*i].key(), "current": x0[*i], "tuned": xt[*i], "effect": effect,
                                "mean_diff": if mean_diff.is_finite() { json!(mean_diff) } else { Value::Null }, "better": better, "worse": worse, "whole": if whole.is_finite() { json!(whole) } else { Value::Null },
                                "values": vals.iter().map(|x| if x.is_finite() { json!(x) } else { Value::Null }).collect::<Vec<_>>()}));
        }
        let dropped = effects.iter().filter(|e| e["effect"] != "helps").count();
        if dropped > 0 {
            let (it, its) = if dropped == 1 { ("it stays", "its current value") } else { ("they stay", "their current values") };
            why += &format!(". On its own, {dropped} of the {} tuned changes changed nothing or did not do better than the current settings in most periods: {it} at {its}", changed.len());
        }
        // the kept changes together must hold up as each did alone: changes that help apart can
        // still clash. If they do not, only the one that helped most on its own is kept
        let mut whole_now = wholes.first().map(|w| w.2).unwrap_or(f64::NAN);
        if wholes.len() > 1 && !stopped() {
            let both = opt::apply(base, &dims, &keep);
            let r2 = run_all(&[&both]);
            let vals: Vec<f64> = r2[0][..tested].iter().map(|x| x.0).collect();
            let diffs: Vec<f64> = vals.iter().zip(&cur).map(|(v, c)| v - c).collect();
            let together = r2[0][tested].0;
            if effect_of(&diffs).0 == "helps" && together >= cur_whole {
                whole_now = together;
            } else {
                let best = wholes.iter().copied().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)).unwrap();
                keep = x0.clone();
                keep[best.0] = xt[best.0];
                whole_now = best.2;
                why += &format!(". The changes that helped one at a time did not hold up together: only the one that helped most, {}, is kept", dims[best.0].key());
            }
        }
        r["whole"] = json!({"current": cur_whole, "tuned": whole_now});
        let kept = (0..dims.len()).any(|i| keep[i] != x0[i]);
        // re-tuning as a whole did not hold up, but some changes helped on their own in most of the
        // tested periods: offered as a lead to test, not as the suggestion (most of those periods
        // came before the values were tuned, the last few are where they were)
        let lead = !passed && kept;
        if lead {
            let names: Vec<String> = effects.iter().filter(|e| e["effect"] == "helps").map(|e| e["key"].as_str().unwrap_or("").to_string()).collect();
            why += &format!(". A lead: {} on its own did better than the current settings in most periods; try it alone (button below) and judge it on time after today", names.join(", "));
        }
        if (passed || lead) && kept {
            let mut vals = Map::new();
            for (i, d) in dims.iter().enumerate() {
                if keep[i] != x0[i] {
                    vals.insert(d.key(), json!(keep[i]));
                }
            }
            r["values"] = Value::Object(vals);
            r["strategy"] = Value::Object(opt::apply(base, &dims, &keep));
            r["now"] = json!(true);
            r["lead"] = json!(lead);
            top.push(r);
        } else if passed {
            why += ". No tuned change helps on its own, so there is nothing to apply";
        }
    }
    let suggestion = json!({"apply": if passed && !top.is_empty() { json!(0) } else { Value::Null }, "why": why});
    let mut current = json!({});
    current["wf"] = rows[1]["score"].clone();
    Ok(json!({"auto": true, "mode": "walk", "top": top, "current": current, "suggestion": suggestion, "stopped": stopped(), "picked": keys,
              "screen": rep, "screen_base": b0, "runs": 0, "rounds": 0,
              "walk": {"current_in_sample": cur_in, "edge": edge, "periods": n, "period_days": pdays, "train": train, "first": first, "tested": tested, "runs_per_period": runs,
                       "cols": tests.iter().map(|c| json!({"from": c.0, "to": c.1})).collect::<Vec<_>>(), "rows": rows, "steps": step_rows,
                       "stability": stability, "effects": effects, "whole_current": if cur_whole.is_finite() { json!(cur_whole) } else { Value::Null },
                       "span": {"from": t_a, "to": t_b},
                       "benchmark": {"name": "hold 50/50 from the span's start", "values": row("", &bench)["values"], "whole": if bench_all[tested].0.is_finite() { json!(plain(&bench_all[tested])) } else { Value::Null },
                                     "rows": [row("re-tuned every period", &over(&wf_apr)), row("current settings", &over(&cur_apr))]}}}))
}

/// Per period, from a continuous run's hourly series (time, value, holding's value): the return,
/// or the return over holding when `vs_hold`, and holding's own, both %/yr of the capital.
fn slices(pts: &[(i64, f64, f64)], periods: &[(i64, i64)], capital: f64, vs_hold: bool) -> Vec<(f64, f64)> {
    // a period ends with the last value at or before its end and starts from where the one before
    // ended; at the very start of a run that is the first value at that instant, before any trade
    let at = |t: i64, end: bool| -> Option<(f64, f64)> {
        let p = if end {
            let k = pts.partition_point(|p| p.0 <= t);
            if k == 0 { pts.first() } else { pts.get(k - 1) }
        } else {
            let k = pts.partition_point(|p| p.0 < t);
            match pts.get(k) {
                Some(q) if q.0 == t => Some(q),
                _ => if k == 0 { pts.first() } else { pts.get(k - 1) },
            }
        };
        p.map(|p| (p.1, p.2))
    };
    periods.iter().map(|(a, b)| match (at(*a, false), at(*b, true)) {
        (Some((va, ha)), Some((vb, hb))) if capital > 0.0 && b > a => {
            let ann = 365.0 * 86400.0 / (b - a) as f64 / capital * 100.0;
            let (r, h) = ((vb - va) * ann, (hb - ha) * ann);
            (if vs_hold { r - h } else { r }, h)
        }
        _ => (f64::NAN, f64::NAN),
    }).collect()
}

/// Whether re-tuning earns its keep: over at least 3 tested periods its reliable return beats the
/// references nothing was tuned on, its edge over the bar is reliably above zero (`edge_lb`), and it
/// beats the current settings on the reliable return and in most periods. That last holds even when
/// the current settings are in-sample (picked on this history), which flatters them: tuned settings
/// that lose to the ones in use period by period are no improvement to apply.
#[allow(clippy::too_many_arguments)]
fn retune_passes(tested: usize, no_dims: bool, s_wf: f64, s_ref: f64, s_cur: f64, edge_lb: f64, wins: usize, wins_ref: usize, cur_in: bool) -> bool {
    tested >= 3 && !no_dims && s_wf > s_ref && edge_lb > 0.0 && s_wf > s_cur && wins * 2 > tested && (!cur_in || wins_ref * 2 > tested)
}

/// What one tuned change does on its own, from its result less the current settings', period by
/// period: "none" (identical in every period: the setting is dead at that value), "helps" (better on
/// average and in most periods), "hurts" (worse on average and in most periods), else "mixed".
/// Also the mean difference and the periods better and worse.
fn effect_of(diffs: &[f64]) -> (&'static str, f64, usize, usize) {
    let d: Vec<f64> = diffs.iter().copied().filter(|x| x.is_finite()).collect();
    if d.is_empty() {
        return ("mixed", f64::NAN, 0, 0);
    }
    let better = d.iter().filter(|x| **x > 1e-9).count();
    let worse = d.iter().filter(|x| **x < -1e-9).count();
    let mean = d.iter().sum::<f64>() / d.len() as f64;
    let e = if better == 0 && worse == 0 {
        "none"
    } else if mean > 0.0 && better * 2 > d.len() {
        "helps"
    } else if mean < 0.0 && worse * 2 > d.len() {
        "hurts"
    } else {
        "mixed"
    };
    (e, mean, better, worse)
}

#[cfg(test)]
mod walk_tests {
    use super::*;

    #[test]
    fn retuning_must_beat_the_current_settings_in_most_periods() {
        // the Lab run of 2026-10-20: beat the references, 3 of 9 periods against the current settings
        assert!(!retune_passes(9, false, 78.2, 9.4, 65.6, 53.4, 3, 8, true));
        assert!(retune_passes(9, false, 78.2, 9.4, 65.6, 53.4, 6, 8, true));
        // a higher reliable return alone is not enough, nor most periods with a lower one
        assert!(!retune_passes(9, false, 60.0, 9.4, 65.6, 20.0, 6, 8, true));
        // nothing to tune, too few periods, no reliable edge
        assert!(!retune_passes(9, true, 78.2, 9.4, 65.6, 53.4, 6, 8, true));
        assert!(!retune_passes(2, false, 78.2, 9.4, 65.6, 53.4, 2, 2, true));
        assert!(!retune_passes(9, false, 78.2, 9.4, 65.6, -1.0, 6, 8, true));
        // out-of-sample current settings: the references' period count does not matter
        assert!(retune_passes(9, false, 78.2, 9.4, 65.6, 5.0, 6, 3, false));
    }

    #[test]
    fn periods_are_slices_of_one_continuous_run() {
        // hourly: value climbs 10 an hour, holding 4 an hour; capital 1000
        let pts: Vec<(i64, f64, f64)> = (0..=48).map(|h| (h * 3600, 1000.0 + 10.0 * h as f64, 1000.0 + 4.0 * h as f64)).collect();
        let day = 365.0 * 100.0 / 1000.0;
        let v = slices(&pts, &[(0, 86400), (86400, 2 * 86400)], 1000.0, false);
        assert!((v[0].0 - 240.0 * day).abs() < 1e-6 && (v[1].0 - 240.0 * day).abs() < 1e-6);
        assert!((v[0].1 - 96.0 * day).abs() < 1e-6);
        let vh = slices(&pts, &[(0, 86400)], 1000.0, true);
        assert!((vh[0].0 - 144.0 * day).abs() < 1e-6);
        // a period outside the run has no value
        assert!(slices(&[], &[(0, 86400)], 1000.0, false)[0].0.is_nan());
    }

    #[test]
    fn a_change_alone_is_dead_helps_hurts_or_mixed() {
        assert_eq!(effect_of(&[0.0, 0.0, 0.0, 0.0]).0, "none");
        assert_eq!(effect_of(&[5.0, -1.0, 3.0, 2.0]).0, "helps");
        assert_eq!(effect_of(&[-39.0, -38.0, 2.0, -47.0]).0, "hurts");
        assert_eq!(effect_of(&[30.0, -5.0, -5.0, -5.0]).0, "mixed");
        assert_eq!(effect_of(&[f64::NAN, f64::NAN]).0, "mixed");
        let (_, mean, b, w) = effect_of(&[2.0, f64::NAN, -1.0]);
        assert_eq!((mean, b, w), (0.5, 1, 1));
    }

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
        // an old plain ladder (a saved strategy, a request): the same ladder as a node strategy
        "ladder" => {
            let g = crate::ladder::as_graph(&Params::from(&ladder_params(s, spacing, pegged))?);
            strategy(g.as_object().unwrap(), spacing, max_ranges, pegged)
        }
        // holding 50/50 from the start, unmanaged: the benchmark every candidate shares
        "hodl" => Ok(Strategy::Hodl),
        t => Err(format!("unknown strategy type {t}")),
    }
}

/// The perps that short the pool's price: the volatile token's coin, less a long on the numeraire's
/// coin when the numeraire is not a dollar, with their hourly funding from `from`.
/// The hedge's net funding at each bar of `m`, as % a year a short is paid (NaN where unknown).
/// A rate is keyed by the hour it is paid at (Hyperliquid pays each hour's funding at its end), and
/// a bar sees only payments made before it ends: the bar ending on the hour sees the hour before's,
/// not the one paid at that same instant.
fn funding_series(c: &Ctx, m: &Market) -> crate::external::Series {
    let close: Vec<f64> = m.bar_end.iter().map(|t| c.hedge.funding_h.get(&((t - 1).div_euclid(3600) * 3600)).map(|r| r * 8760.0 * 100.0).unwrap_or(f64::NAN)).collect();
    crate::external::Series { hi: close.clone(), lo: close.clone(), at: vec![0; close.len()], close }
}

/// A coin against the dollar replayed from price candles (candles.rs), for a strategy's direction
/// on the coin's own long history: token0 the coin, token1 the dollar (the numeraire), price in
/// dollars. Each candle trades four times: its open (a second after the candle starts, so the bar
/// ending at a candle's boundary holds the completed close: a decision there sees it and trades at
/// it), then its high and low (the low first in a
/// candle that closed up, so the path is the likelier one), then its close; the 5-minute bars in
/// between hold the price. No fees and deep liquidity: what the pool earns is not modeled here,
/// only what holding, exiting and the perp position make.
pub fn candle_market(cs: &[crate::candles::Candle], step: i64) -> Market {
    candle_market_ordered(cs, step, "close")
}

/// `candle_market` with the order of each candle's high and low: "close" (the low first in a
/// candle that closed up, the likelier path), "high_first" or "low_first" (to test what the
/// unobserved intraday path changes, as a liquidation at the high or low does)
pub fn candle_market_ordered(cs: &[crate::candles::Candle], step: i64, order: &str) -> Market {
    let tick = |p: f64| (p.max(1e-300).ln() / 1.0001f64.ln()).round() as i64;
    let (mut ts, mut ticks) = (Vec::with_capacity(cs.len() * 4), Vec::with_capacity(cs.len() * 4));
    for c in cs {
        let (a, b) = match order {
            "high_first" => (c.h, c.l),
            "low_first" => (c.l, c.h),
            _ => if c.c >= c.o { (c.l, c.h) } else { (c.h, c.l) },
        };
        for (dt, p) in [(1, c.o), (step / 4, a), (step / 2, b), (step - 1, c.c)] {
            ts.push(c.t + dt);
            ticks.push(tick(p));
        }
    }
    let n = ts.len();
    let sp: Vec<f64> = ticks.iter().map(|t| 1.0001f64.powf(*t as f64 / 2.0)).collect();
    let bar_s = data::BAR_S;
    // up to the bar that holds the last candle's close (its end is past the close by a second)
    let bar_end: Vec<i64> = if n == 0 { vec![] } else { (0..).map(|i| (ts[0].div_euclid(bar_s) + 1) * bar_s + i * bar_s).take_while(|t| *t < ts[n - 1] + bar_s).collect() };
    let bar_idx: Vec<usize> = bar_end.iter().map(|b| ts.partition_point(|t| t <= b).max(1) - 1).collect();
    let bar_tick = bar_idx.iter().map(|i| ticks[*i]).collect();
    let fees = vec![0.0; n];
    let (bar_hi, bar_lo, bar_fee) = data::bar_ranges(&ticks, &fees, &bar_idx);
    Market { ts, tick: ticks, sp, fee: fees, fee_sig: vec![], l_others: vec![1e30; n], l_active: vec![1e30; n], bar_end, bar_idx, bar_tick, bar_hi, bar_lo, bar_fee,
             decimals: 18, px_scale: 1.0, liq_scale: 1.0, num1: true, bar_s }
}

fn hedge_market(token: &str, numeraire: &str, from: i64) -> HedgeMkt {
    let fail = |e: String| HedgeMkt { error: Some(e), ..Default::default() };
    let listed = match crate::hyperliquid::listed() {
        Ok(l) => l,
        Err(e) => return fail(format!("Hyperliquid unavailable ({e})")),
    };
    let leg = |sym: &str| -> Result<Option<(String, f64)>, String> {
        match crate::hyperliquid::coin_for(sym) {
            None => Ok(None),
            Some(coin) => match listed.get(&coin) {
                Some(lev) => Ok(Some((coin, *lev))),
                None => Err(format!("Hyperliquid lists no perp for {sym}")),
            },
        }
    };
    let (short, long) = match (leg(token), leg(numeraire)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return fail(e),
    };
    if short.as_ref().map(|s| s.0.clone()) == long.as_ref().map(|l| l.0.clone()) {
        // the same perp: no hedge of the pool's price, but a dollar-margined position of its own
        let Some((coin, lev)) = &long else { return fail(format!("{token} and {numeraire}: no perp")) };
        return match crate::hyperliquid::funding(coin, from) {
            Ok(f) => match f.keys().min().copied() {
                Some(first) => HedgeMkt { legs: 1.0, funding_h: f, mm: 0.5 / lev.max(1.0), error: None, from_h: first, usd: true,
                                          label: format!("{coin} perp against the dollar (its size only: {token} and {numeraire} hedge with the same perp)") },
                None => fail(format!("Hyperliquid has no funding history for {coin} in this period: it was not trading yet")),
            },
            Err(e) => fail(e),
        };
    }
    let mut funding_h: HashMap<i64, f64> = HashMap::new();
    let mut label = vec![];
    let mut mm: f64 = 0.0;
    let mut from_h = i64::MIN;
    for (leg, sign) in [(&short, 1.0), (&long, -1.0)] {
        if let Some((coin, lev)) = leg {
            match crate::hyperliquid::funding(coin, from) {
                Ok(f) => {
                    match f.iter().map(|(h, _)| *h).min() {
                        Some(first) => from_h = from_h.max(first),
                        None => return fail(format!("Hyperliquid has no funding history for {coin} in this period: it was not trading yet")),
                    }
                    for (h, r) in f {
                        *funding_h.entry(h).or_default() += sign * r;
                    }
                }
                Err(e) => return fail(e),
            }
            label.push(format!("{} {coin}", if sign > 0.0 { "short" } else { "long" }));
            mm = mm.max(0.5 / lev.max(1.0));
        }
    }
    HedgeMkt { legs: label.len() as f64, funding_h, mm, label: label.join(", "), error: None, from_h, usd: false }
}

/// An old plain ladder's settings: the app's defaults for this pool (as adding it set them), then what was given.
fn ladder_params(s: &Map<String, Value>, spacing: i64, pegged: bool) -> Map<String, Value> {
    let mut lp = crate::ladder::plain_defaults();
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
        Market { ts, tick: ticks.to_vec(), sp, fee: fees, fee_sig: vec![], l_others: vec![others; n], l_active: vec![others; n], bar_end, bar_idx, bar_tick, bar_hi, bar_lo, bar_fee, decimals: 6, px_scale: 1.0, liq_scale: 1e6, num1: true, bar_s: 300 }
    }

    fn ctx(m: &Market) -> Ctx<'_> {
        Ctx { m, spacing: 1, num1: false, num_per_usd: 1.0, emis_usd_h: HashMap::new(), sqrt_px: 1.0, beta: 1.0, park: Default::default(), cid: 8453, num_token: String::new(), hedge: HedgeMkt::default(), swap_days: HashMap::new(), tref: Default::default(), peg_damp: false }
    }

    fn no_costs() -> Costs {
        Costs { swap_bps: 0.0, gas_usd: 0.0, fee_haircut: 1.0, emission_haircut: 1.0, stable_swap_bps: 0.0, max_spot_dev_ticks: None, swap_depth_usd: None, max_swap_cost_bps: None, max_actions_per_day: None, hedge_taker_bps: 0.0, hedge_slip_bps: 0.0, bridge_usd: 0.0, max_events: MAX_EVENTS, exec_delay_bars: 0 }
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
        let cost = Costs { swap_bps: 4.0, gas_usd: 0.5, fee_haircut: 1.0, emission_haircut: 1.0, stable_swap_bps: 0.0, max_spot_dev_ticks: None, swap_depth_usd: None, max_swap_cost_bps: None, max_actions_per_day: None, hedge_taker_bps: 0.0, hedge_slip_bps: 0.0, bridge_usd: 0.0, max_events: MAX_EVENTS, exec_delay_bars: 0 };
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
        // the late one parks once a full day of its pool's income is known (a decision cannot know
        // today's), and earns from then on only
        let late = late["park_yield"].as_f64().unwrap();
        let share = (t1 - (born + 86400)) as f64 / (t1 - t0) as f64;
        assert!((late / all - share).abs() < 0.05, "{late} vs {all}, {share}");
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

    /// A range of ±`w` ticks around the price, re-centred when out of range, with a Hedge block.
    fn hedged(w: i64, hedge: Value) -> Strategy {
        let mut nodes = vec![json!({"id": "p", "kind": "price"}), json!({"id": "w", "kind": "const", "p": {"value": w}}),
            json!({"id": "a", "kind": "around", "in": {"center": ["p", "tick"], "width": ["w", "value"]}}),
            json!({"id": "r", "kind": "range", "in": {"lower": ["a", "lower"], "upper": ["a", "upper"]}}),
            json!({"id": "o", "kind": "out_of_range"}), json!({"id": "rb", "kind": "rebalance", "p": {"cooldown_min": 0}, "in": {"when": ["o", "out"]}})];
        if !hedge.is_null() {
            nodes.push(json!({"id": "h", "kind": "hedge", "p": hedge}));
        }
        Strategy::Graph(Box::new(crate::graph::Graph::parse(json!({"type": "graph", "nodes": nodes}).as_object().unwrap(), 1).unwrap()))
    }

    /// One perp, funding `rate` an hour throughout, ETH-like margin.
    fn with_hedge<'a>(m: &'a Market, rate: f64) -> Ctx<'a> {
        let mut c = ctx(m);
        let h0 = m.ts[0].div_euclid(3600) * 3600;
        c.hedge = HedgeMkt { legs: 1.0, funding_h: (0..400).map(|k| (h0 + k * 3600, rate)).collect(), mm: 0.02, label: "short ETH".into(), error: None, from_h: 0, usd: false };
        c
    }

    /// The H0 report: each hedged library strategy against itself without the hedge, on real pools.
    /// Run by hand with ALM_HOME set to a copy of the data (never the live folder) and OUT to a file:
    /// cargo test --release h0_report -- --ignored
    /// Where a Lab backtest's time goes: POOL (Base), DAYS, the strategy in STRAT (JSON), the default
    /// comparison and hold, run twice. ALM_HOME as for h0_report.
    #[test]
    #[ignore]
    fn lab_timing() {
        let home = std::env::var("ALM_HOME").expect("set ALM_HOME to a copy of the data");
        let live = std::env::var("LOCALAPPDATA").map(|l| format!("{l}\\StableALM").to_lowercase()).unwrap_or_default();
        assert!(!live.is_empty() && !home.to_lowercase().replace('/', "\\").starts_with(&live), "ALM_HOME is the live folder");
        let pool = std::env::var("POOL").expect("POOL");
        let days: i64 = std::env::var("DAYS").ok().and_then(|d| d.parse().ok()).unwrap_or(365);
        let st: Value = serde_json::from_str(&std::env::var("STRAT").expect("STRAT")).unwrap();
        let req = json!({"chain_id": 8453, "pool": pool, "days": days, "capital_usd": 5100, "strategy": st});
        for k in 0..2 {
            let t = std::time::Instant::now();
            let v = execute(0, 8453, &pool, days, &req, "").unwrap();
            eprintln!("run {k}: {:?}, {} swaps, timings {}", t.elapsed(), v["swaps"], v["timings_ms"]);
        }
    }

    /// Hedge variants from a file: SPEC holds [{pair, chain_id (default Base), pool, numeraire (optional), name, strategy}],
    /// DAYS the periods (default 45,180), OUT where the rows go. ALM_HOME as for h0_report.
    /// An optimization as the Lab runs it, on a copy of the data (ALM_HOME, never the live folder):
    /// SPEC is a backtest request with its `optimize` spec; the result goes to OUT.
    #[test]
    #[ignore]
    fn optimize_check() {
        let home = std::env::var("ALM_HOME").expect("set ALM_HOME to a copy of the data");
        let live = std::env::var("LOCALAPPDATA").map(|l| format!("{l}\\StableALM").to_lowercase()).unwrap_or_default();
        assert!(!live.is_empty() && !home.to_lowercase().replace('/', "\\").starts_with(&live), "ALM_HOME is the live folder");
        let req: Value = serde_json::from_str(&std::fs::read_to_string(std::env::var("SPEC").expect("SPEC")).unwrap()).unwrap();
        let t0 = std::time::Instant::now();
        let r = execute(1, req["chain_id"].as_u64().unwrap_or(8453), req["pool"].as_str().unwrap(), req["days"].as_i64().unwrap_or(365), &req, "");
        let mut v = r.unwrap_or_else(|e| json!({"error": e}));
        v["seconds"] = json!(t0.elapsed().as_secs_f64());
        std::fs::write(std::env::var("OUT").expect("OUT"), serde_json::to_string_pretty(&v).unwrap()).unwrap();
    }

    /// DUMP set instead writes the built-in library there, to build the variants from.
    /// A strategy's direction on a coin's long candle history (candles.rs: Binance from 2017, or
    /// Hyperliquid), fetched as needed. SPEC: a list of rows {"name", "strategy", "coin" ("ETH"),
    /// "source" ("binance"), "interval" ("1h"), "from", "until" (unix seconds), "capital" (dollars,
    /// 10,000), "costs" (swap_bps for spot moves; hedge_taker_bps, hedge_slip_bps, bridge_usd for the
    /// perp), "pre_funding_apr" (what a short earns a year, %, before Hyperliquid's funding history
    /// starts: 0)}. The strategy holds the coin or the dollar (exit `to`) and may take a perp position
    /// (hedge `size`); the pool itself earns nothing here. OUT: results; SERIES=1 adds the series.
    #[test]
    #[ignore]
    fn direction_study() {
        let home = std::env::var("ALM_HOME").expect("set ALM_HOME to a copy of the data");
        let live = std::env::var("LOCALAPPDATA").map(|l| format!("{l}\\StableALM").to_lowercase()).unwrap_or_default();
        assert!(!live.is_empty() && !home.to_lowercase().replace('/', "\\").starts_with(&live), "ALM_HOME is the live folder");
        let spec: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(std::env::var("SPEC").expect("SPEC")).unwrap()).unwrap();
        let mut out = vec![];
        for x in &spec {
            let mut row = json!({"name": x["name"]});
            let r = (|| -> Result<Value, String> {
                let coin = x["coin"].as_str().unwrap_or("ETH");
                let source = x["source"].as_str().unwrap_or("binance");
                let iv = x["interval"].as_str().unwrap_or("1h");
                let step = match iv { "1h" => 3600, "4h" => 14400, "1d" => 86400, _ => return Err(format!("interval {iv}")) };
                let until = x["until"].as_i64().unwrap_or(crate::engine::now() as i64);
                let from = x["from"].as_i64().ok_or("from")?;
                let strat_v = x["strategy"].as_object().cloned().ok_or("strategy")?;
                let warm = warmup_s(&json!({"strategy": strat_v}));
                // candles completed by `until` only: the period ends at its last close
                let cs: Vec<_> = crate::candles::get(source, coin, iv, from - warm, until)?.into_iter().filter(|c| c.t + step <= until).collect();
                let order = x["ohlc_order"].as_str().unwrap_or("close");
                if !["close", "high_first", "low_first"].contains(&order) {
                    return Err(format!("ohlc_order {order:?}: close, high_first or low_first"));
                }
                let m = candle_market_ordered(&cs, step, order);
                let b0 = m.bar_end.partition_point(|t| *t < from);
                let b1 = m.bar_end.partition_point(|t| *t <= until).saturating_sub(1);
                if b0 >= b1 {
                    return Err("not enough candles in the period".into());
                }
                // the perp: Hyperliquid's funding where it has it, the stated rate before
                let pre = x["pre_funding_apr"].as_f64().unwrap_or(0.0) / 100.0 / 8760.0;
                let hl = crate::hyperliquid::funding(coin, from - warm).unwrap_or_default();
                let first = hl.keys().min().copied().unwrap_or(i64::MAX);
                let mut funding_h: HashMap<i64, f64> = hl;
                let mut h = (from - warm).div_euclid(3600) * 3600;
                while h < first.min(until) {
                    funding_h.insert(h, pre);
                    h += 3600;
                }
                let lev = crate::hyperliquid::listed().ok().and_then(|l| l.get(coin).copied()).unwrap_or(25.0);
                let hedge = HedgeMkt { legs: 1.0, funding_h, mm: 0.5 / lev.max(1.0), label: format!("{coin} perp ({source} candles)"), error: None, from_h: i64::MIN, usd: false };
                let c = Ctx { m: &m, spacing: 1, num1: true, num_per_usd: 1.0, emis_usd_h: HashMap::new(), sqrt_px: 1.0, beta: 1.0, park: Default::default(), cid: 8453,
                              num_token: "USD".into(), hedge, swap_days: HashMap::new(), tref: Default::default(), peg_damp: false };
                let st = strategy(&strat_v, 1, 5, false)?;
                let capital = x["capital"].as_f64().unwrap_or(10_000.0);
                // with SERIES, every event (unless the row caps them)
                let mut cv = x["costs"].clone();
                if !cv.is_object() {
                    cv = json!({});
                }
                // "next_open": a decision on a candle's close trades at the next candle's open (its
                // open is the first trade in the bar after the close's); "close" (default) trades at it
                match x["execution"].as_str().unwrap_or("close") {
                    "close" => {}
                    "next_open" => cv["exec_delay_bars"] = json!(1),
                    e => return Err(format!("execution {e:?}: close or next_open")),
                }
                if std::env::var("SERIES").is_ok() && cv["max_events"].is_null() {
                    if !cv.is_object() {
                        cv = json!({});
                    }
                    cv["max_events"] = json!(MAX_EVENTS_ALL);
                }
                let v = run(&c, &st, &Costs::from(&cv), capital, b0, b1);
                let mut r = json!({"coin": coin, "source": source, "interval": iv, "ohlc_order": order, "execution": x["execution"].as_str().unwrap_or("close"), "from": m.bar_end[b0], "until": m.bar_end[b1], "capital": capital,
                                   "hl_funding_from": if first == i64::MAX { Value::Null } else { json!(first) }, "first_px": cs.first().map(|c| c.c), "last_px": cs.last().map(|c| c.c)});
                for k in ["value", "hodl_value", "pnl", "swap_costs", "hedge_pnl", "funding", "hedge_fees", "hedge_net", "actions", "hedge", "watch_errors", "max_drawdown",
                          "events_dropped", "events_truncated"] {
                    r[k] = v[k].clone();
                }
                // the coin held throughout, and the worst fall from a high (from the series)
                let px = |b: usize| 1.0001f64.powf(m.bar_tick[b] as f64);
                r["coin_return_pct"] = json!(100.0 * (px(b1) / px(b0) - 1.0));
                let mut hi = f64::MIN;
                let mut dd = 0.0f64;
                for p in v["series"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
                    let q = p[1].as_f64().unwrap_or(0.0);
                    hi = hi.max(q);
                    if hi > 0.0 {
                        dd = dd.max(1.0 - q / hi);
                    }
                }
                r["drawdown_pct"] = json!(dd * 100.0);
                r["return_pct"] = json!(100.0 * (v["value"].as_f64().unwrap_or(0.0) / capital - 1.0));
                if std::env::var("SERIES").is_ok() {
                    r["series"] = v["series"].clone();
                    r["events"] = v["events"].clone();
                }
                Ok(r)
            })();
            match r {
                Ok(v) => {
                    if let (Some(a), Some(b)) = (row.as_object_mut(), v.as_object()) {
                        a.extend(b.clone());
                    }
                }
                Err(e) => row["error"] = json!(e),
            }
            eprintln!("{row}");
            out.push(row);
            if let Ok(p) = std::env::var("OUT") {
                std::fs::write(p, serde_json::to_string_pretty(&out).unwrap()).unwrap();
            }
        }
    }

    #[test]
    #[ignore]
    fn hedge_variants() {
        if let Ok(p) = std::env::var("DUMP") {
            std::fs::write(p, serde_json::to_string_pretty(&crate::library::builtin()).unwrap()).unwrap();
            return;
        }
        let home = std::env::var("ALM_HOME").expect("set ALM_HOME to a copy of the data");
        let live = std::env::var("LOCALAPPDATA").map(|l| format!("{l}\\StableALM").to_lowercase()).unwrap_or_default();
        assert!(!live.is_empty() && !home.to_lowercase().replace('/', "\\").starts_with(&live), "ALM_HOME is the live folder");
        let spec: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(std::env::var("SPEC").expect("SPEC")).unwrap()).unwrap();
        let days: Vec<i64> = std::env::var("DAYS").unwrap_or("45,180".into()).split(',').filter_map(|d| d.parse().ok()).collect();
        let mut out = vec![];
        for d in &days {
            for x in &spec {
                let pool = x["pool"].as_str().unwrap();
                let cid = x["chain_id"].as_u64().unwrap_or(8453);
                // a run may name its own period: days, and until (unix seconds) for one in the past
                let d = &x["days"].as_i64().unwrap_or(*d);
                let mut req = json!({"chain_id": cid, "pool": pool, "days": d, "capital_usd": x["capital_usd"].as_f64().unwrap_or(10000.0), "strategy": x["strategy"], "compare": false, "hold": false, "until": x["until"], "costs": x["costs"]});
                if !x["numeraire"].is_null() {
                    req["numeraire"] = x["numeraire"].clone();
                }
                req["walkforward"] = x["walkforward"].clone();
                req["flow_beta"] = x["flow_beta"].clone();
                // the neighbourhood's reference ("hold": one 50/50 hold shared by every variant)
                req["benchmark"] = x["benchmark"].clone();
                // with SERIES, every event (unless the row caps them)
                if std::env::var("SERIES").is_ok() && req["costs"]["max_events"].is_null() {
                    if !req["costs"].is_object() {
                        req["costs"] = json!({});
                    }
                    req["costs"]["max_events"] = json!(MAX_EVENTS_ALL);
                }
                let mut row = json!({"pair": x["pair"], "days": d, "name": x["name"]});
                match execute(0, cid, pool, *d, &req, "") {
                    Ok(v) if !v["walkforward"].is_null() => row["walkforward"] = v["walkforward"].clone(),
                    Ok(v) => {
                        let m = &v["runs"]["main"];
                        for k in ["value", "hodl_value", "pnl", "vs_hodl", "fees", "emissions", "il", "swap_costs", "gas", "hedge_pnl", "funding", "hedge_fees", "hedge_net",
                                  "actions", "time_in_range", "hedge", "watch_errors", "park_yield", "max_drawdown", "spot_blocked_bars", "guard_bars", "events_dropped", "events_truncated"] {
                            row[k] = m[k].clone();
                        }
                        // the worst fall of the value from its high, % (from the series)
                        let mut hi = f64::MIN;
                        let mut dd = 0.0f64;
                        for p in m["series"].as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
                            let v = p[1].as_f64().unwrap_or(0.0);
                            hi = hi.max(v);
                            if hi > 0.0 {
                                dd = dd.max(1.0 - v / hi);
                            }
                        }
                        row["drawdown_pct"] = json!(dd * 100.0);
                        if std::env::var("SERIES").is_ok() {
                            row["series"] = m["series"].clone();
                            row["events"] = m["events"].clone();
                        }
                        row["capital_num"] = v["capital"].clone();
                        row["span_days"] = v["span_days"].clone();
                    }
                    Err(e) => row["error"] = json!(e),
                }
                eprintln!("{}", row);
                out.push(row);
                if let Ok(p) = std::env::var("OUT") {
                    std::fs::write(p, serde_json::to_string_pretty(&out).unwrap()).unwrap();
                }
            }
        }
    }

    /// Strategies (SPEC: [{name, strategy}]) on pools (POOLS: "name=address,…" on Base) over periods
    /// (PERIODS: "days@days_ago,…": 90@0 is the last 90 days, 90@90 the 90 before), each run's
    /// result a line on stderr and all of them in OUT.
    #[test]
    #[ignore]
    fn strategy_study() {
        let home = std::env::var("ALM_HOME").expect("set ALM_HOME to a copy of the data");
        let live = std::env::var("LOCALAPPDATA").map(|l| format!("{l}\\StableALM").to_lowercase()).unwrap_or_default();
        assert!(!live.is_empty() && !home.to_lowercase().replace('/', "\\").starts_with(&live), "ALM_HOME is the live folder");
        let spec: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(std::env::var("SPEC").expect("SPEC")).unwrap()).unwrap();
        let pools: Vec<(String, String)> = std::env::var("POOLS").expect("POOLS").split(',').filter_map(|x| x.split_once('=')).map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let periods: Vec<(i64, i64)> = std::env::var("PERIODS").unwrap_or("90@0".into()).split(',')
            .filter_map(|x| x.split_once('@')).filter_map(|(d, a)| Some((d.parse().ok()?, a.parse().ok()?))).collect();
        let now = crate::engine::now() as i64;
        let mut out = vec![];
        for (name, pool) in &pools {
            for (days, ago) in &periods {
                for x in &spec {
                    let mut req = json!({"chain_id": 8453, "pool": pool, "days": days, "capital_usd": 10000, "strategy": x["strategy"], "compare": false, "hold": false});
                    if *ago > 0 {
                        req["until"] = json!(now - ago * 86400);
                    }
                    if let Some(c) = x.get("costs") {
                        req["costs"] = c.clone();
                    }
                    let t = std::time::Instant::now();
                    let mut row = json!({"pool": name, "days": days, "ago": ago, "name": x["name"]});
                    match execute(0, 8453, pool, *days, &req, "") {
                        Ok(v) => {
                            let m = &v["runs"]["main"];
                            for k in ["value", "hodl_value", "pnl", "vs_hodl", "fees", "emissions", "il", "swap_costs", "gas", "actions", "time_in_range", "watch_errors", "capital"] {
                                row[k] = m[k].clone();
                            }
                            row["span_days"] = v["span_days"].clone();
                        }
                        Err(e) => row["error"] = json!(e),
                    }
                    row["secs"] = json!(t.elapsed().as_secs_f64());
                    eprintln!("{row}");
                    out.push(row);
                    if let Ok(p) = std::env::var("OUT") {
                        std::fs::write(p, serde_json::to_string_pretty(&out).unwrap()).unwrap();
                    }
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn h0_report() {
        let home = std::env::var("ALM_HOME").expect("set ALM_HOME to a copy of the data");
        let live = std::env::var("LOCALAPPDATA").map(|l| format!("{l}\\StableALM").to_lowercase()).unwrap_or_default();
        assert!(!live.is_empty() && !home.to_lowercase().replace('/', "\\").starts_with(&live), "ALM_HOME is the live folder");
        let pools = [("WETH/USDC", "0x551a0e3d267bea87048f08cc94cc6035ad99221b", Some(1)), ("AERO/WETH", "0xf0deaf7ab32fddfd3bd1d47187cc09683bb709f6", None)];
        let days: Vec<i64> = std::env::var("DAYS").unwrap_or("45,180".into()).split(',').filter_map(|d| d.parse().ok()).collect();
        let lib: Vec<Value> = crate::library::builtin().into_iter().filter(|x| x["category"] == "Hedged").collect();
        let mut out = vec![];
        for (name, pool, num) in pools {
            for d in &days {
                for x in &lib {
                    let hedged = x["strategy"].clone();
                    let mut open = hedged.clone();
                    open["nodes"].as_array_mut().unwrap().retain(|n| n["kind"] != "hedge" && n["id"] != "calm");
                    for (label, st) in [("open", open), ("hedged", hedged)] {
                        let mut req = json!({"chain_id": 8453, "pool": pool, "days": d, "capital_usd": 10000, "strategy": st, "compare": false, "hold": false});
                        if let Some(n) = num {
                            req["numeraire"] = json!(n);
                        }
                        let t = std::time::Instant::now();
                        let r = execute(0, 8453, pool, *d, &req, "");
                        let row = match r {
                            Ok(v) => {
                                let m = &v["runs"]["main"];
                                let mut row = json!({"pair": name, "days": d, "strategy": x["id"], "label": label, "secs": t.elapsed().as_secs()});
                                for k in ["value", "hodl_value", "pnl", "vs_hodl", "fees", "emissions", "il", "swap_costs", "gas", "hedge_pnl", "funding", "hedge_fees", "hedge_net",
                                          "actions", "time_in_range", "hedge", "watch_errors", "capital", "park_yield"] {
                                    row[k] = m[k].clone();
                                }
                                row["capital_num"] = v["capital"].clone();
                                row["numeraire"] = v["numeraire_symbol"].clone();
                                row["span_days"] = v["span_days"].clone();
                                row["numeraire_usd"] = v["numeraire_usd"].clone();
                                row
                            }
                            Err(e) => json!({"pair": name, "days": d, "strategy": x["id"], "label": label, "error": e}),
                        };
                        eprintln!("{}", row);
                        out.push(row);
                        if let Ok(p) = std::env::var("OUT") {
                            std::fs::write(p, serde_json::to_string_pretty(&out).unwrap()).unwrap();
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_funding_block_hedges_only_while_the_short_is_paid() {
        let ticks: Vec<i64> = (0..3000).map(|k| ((k as f64 / 120.0).sin() * 40.0) as i64).collect();
        let m = market(&ticks, 0.5, 5e8);
        // hedge while funding pays more than 5% a year
        let st = || {
            let nodes = vec![json!({"id": "p", "kind": "price"}), json!({"id": "w", "kind": "const", "p": {"value": 20}}),
                json!({"id": "a", "kind": "around", "in": {"center": ["p", "tick"], "width": ["w", "value"]}}),
                json!({"id": "r", "kind": "range", "in": {"lower": ["a", "lower"], "upper": ["a", "upper"]}}),
                json!({"id": "f", "kind": "funding"}), json!({"id": "five", "kind": "const", "p": {"value": 5}}),
                json!({"id": "paid", "kind": "gt", "in": {"a": ["f", "apr"], "b": ["five", "value"]}}),
                json!({"id": "h", "kind": "hedge", "p": {"ratio": 1, "min_usd": 0}, "in": {"when": ["paid", "out"]}})];
            Strategy::Graph(Box::new(crate::graph::Graph::parse(json!({"type": "graph", "nodes": nodes}).as_object().unwrap(), 1).unwrap()))
        };
        // 1e-5 an hour is 8.76% a year: hedged; 2e-6 (1.75%) is not
        let paid = run(&with_hedge(&m, 1e-5), &st(), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        assert!(paid["hedge"]["trades"].as_u64().unwrap() >= 1 && paid["funding"].as_f64().unwrap() > 0.0);
        let poor = run(&with_hedge(&m, 2e-6), &st(), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        assert_eq!(poor["hedge"]["trades"], 0);
        // without a funding history the block is empty, the comparison false, and it is reported
        let mut c = ctx(&m);
        c.hedge = HedgeMkt { legs: 1.0, funding_h: HashMap::new(), mm: 0.02, label: "short ETH".into(), error: None, from_h: 0, usd: false };
        let none = run(&c, &st(), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        assert_eq!(none["hedge"]["trades"], 0);
        assert!(none["watch_errors"].to_string().contains("no funding history"), "{}", none["watch_errors"]);
    }

    #[test]
    fn smoothing_the_exposure_trades_the_hedge_less() {
        let ticks: Vec<i64> = (0..6000).map(|k| ((k as f64 / 30.0).sin() * 60.0) as i64).collect();
        let m = market(&ticks, 0.5, 5e8);
        let c = with_hedge(&m, 0.0);
        let raw = run(&c, &hedged(80, json!({"ratio": 1, "band_pct": 5, "min_usd": 0})), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        let smooth = run(&c, &hedged(80, json!({"ratio": 1, "band_pct": 5, "min_usd": 0, "smooth_h": 24})), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        let (a, b) = (raw["hedge"]["trades"].as_u64().unwrap(), smooth["hedge"]["trades"].as_u64().unwrap());
        assert!(a > 10 && b * 3 < a, "raw {a} trades, smoothed {b}");
    }

    #[test]
    fn a_perp_with_high_maintenance_is_margined_at_half_its_maximum() {
        // maximum leverage 3 (maintenance 1/6): 3x asked runs at 1.5x and is not re-margined every bar
        let ticks: Vec<i64> = (0..3000).map(|k| ((k as f64 / 120.0).sin() * 40.0) as i64).collect();
        let m = market(&ticks, 0.5, 5e8);
        let mut c = with_hedge(&m, 0.0);
        c.hedge.mm = 0.5 / 3.0;
        let r = run(&c, &hedged(20, json!({"ratio": 1, "min_usd": 0, "leverage": 3})), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        assert_eq!(r["hedge"]["leverage"], 1.5);
        assert!(r["hedge"]["margin_moves"].as_u64().unwrap() < 10, "{}", r["hedge"]);
        assert!(r["hedge"]["min_liquidation_distance"].as_f64().unwrap() > 0.2);
    }

    #[test]
    fn a_swap_pays_its_fee_plus_impact_and_the_day_s_measured_cost_wins() {
        let m = market(&vec![0i64; 600], 0.0, 5e8);
        let mut c = ctx(&m);
        c.num1 = true;
        let flat = Costs { swap_bps: 5.0, ..no_costs() };
        // $10k at 5 bp, no depth given: 5 bp
        assert!((Sim::swap_fee(&c, &flat, 0, 10_000.0, 1.0) - 5.0).abs() < 1e-9);
        // with $1M moving the market 1%: 5 bp + 1 bp of impact for $10k, 10 bp for $100k
        let deep = Costs { swap_bps: 5.0, swap_depth_usd: Some(1e6), ..no_costs() };
        assert!((Sim::swap_fee(&c, &deep, 0, 10_000.0, 1.0) - 6.0).abs() < 1e-9);
        assert!((Sim::swap_fee(&c, &deep, 0, 100_000.0, 1.0) - 150.0).abs() < 1e-9);
        // a day the swap-cost history measured: its own fee and depth
        c.swap_days.insert(2, (20.0, 2e5));
        assert!((Sim::swap_fee(&c, &deep, 2 * 86400 + 5, 10_000.0, 1.0) - 25.0).abs() < 1e-9);
        assert!((Sim::swap_fee(&c, &deep, 3 * 86400, 10_000.0, 1.0) - 6.0).abs() < 1e-9);
    }

    #[test]
    fn a_hedge_at_ratio_zero_changes_nothing() {
        let ticks: Vec<i64> = (0..3000).map(|k| ((k as f64 / 120.0).sin() * 40.0) as i64).collect();
        let m = market(&ticks, 0.5, 5e8);
        let c = with_hedge(&m, 1e-5);
        let plain = run(&c, &hedged(20, Value::Null), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        let zero = run(&c, &hedged(20, json!({"ratio": 0})), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        for k in ["value", "fees", "actions", "il"] {
            assert_eq!(plain[k], zero[k], "{k}");
        }
        assert_eq!(zero["hedge"]["trades"], 0);
    }

    #[test]
    fn the_identity_holds_with_a_hedge() {
        let ticks: Vec<i64> = (0..3000).map(|k| ((k as f64 / 120.0).sin() * 400.0) as i64 + k / 3).collect();
        let m = market(&ticks, 0.5, 5e8);
        let c = with_hedge(&m, 1e-5);
        let cost = Costs { swap_bps: 5.0, gas_usd: 0.5, fee_haircut: 0.8, emission_haircut: 0.5, stable_swap_bps: 0.0, max_spot_dev_ticks: None, swap_depth_usd: None, max_swap_cost_bps: None, max_actions_per_day: None,
                           hedge_taker_bps: 4.5, hedge_slip_bps: 2.0, bridge_usd: 0.5, max_events: MAX_EVENTS, exec_delay_bars: 0 };
        let r = run(&c, &hedged(200, json!({"ratio": 1, "band_pct": 5, "min_usd": 0})), &cost, 10_000.0, 0, m.bar_end.len() - 1);
        let f = |k: &str| r[k].as_f64().unwrap();
        assert!(r["hedge"]["trades"].as_u64().unwrap() > 2 && f("funding") > 0.0 && f("hedge_fees") > 0.0);
        let lhs = f("value") - f("hodl_value");
        let rhs = f("fees") + f("emissions") - f("swap_costs") - f("gas") + f("il") + f("hedge_pnl") + f("funding") - f("hedge_fees");
        assert!((lhs - rhs).abs() < 1e-4, "{lhs} vs {rhs}");
        assert_eq!(r["hedge"]["liquidation_bars"], 0);
    }

    #[test]
    fn a_run_handed_on_ends_where_one_run_ends() {
        // the walk-forward's chain: a run split in two, the second from what the first handed on
        let ticks: Vec<i64> = (0..3000).map(|k| ((k as f64 / 120.0).sin() * 400.0) as i64 + k / 3).collect();
        let m = market(&ticks, 0.5, 5e8);
        let c = with_hedge(&m, 1e-5);
        let cost = Costs { swap_bps: 5.0, gas_usd: 0.5, fee_haircut: 0.8, emission_haircut: 0.5, stable_swap_bps: 0.0, max_spot_dev_ticks: None, swap_depth_usd: None, max_swap_cost_bps: None, max_actions_per_day: None,
                           hedge_taker_bps: 4.5, hedge_slip_bps: 2.0, bridge_usd: 0.5, max_events: MAX_EVENTS, exec_delay_bars: 0 };
        let st = hedged(200, json!({"ratio": 1, "band_pct": 5, "min_usd": 0}));
        let n = m.bar_end.len() - 1;
        let whole = run(&c, &st, &cost, 10_000.0, 0, n);
        let (_, k) = run_carry(&c, &st, &cost, 10_000.0, 0, n / 2, None);
        let (second, _) = run_carry(&c, &st, &cost, 10_000.0, n / 2 + 1, n, Some(k));
        for key in ["value", "fees", "swap_costs", "gas", "hedge_pnl", "funding", "hedge_fees", "hodl_value"] {
            let (a, b) = (whole[key].as_f64().unwrap(), second[key].as_f64().unwrap());
            assert!((a - b).abs() < 1e-6 * a.abs().max(1.0), "{key}: {a} vs {b}");
        }
        assert_eq!(whole["actions"], second["actions"]);
    }

    #[test]
    fn a_short_that_runs_out_of_margin_is_liquidated_and_the_books_still_balance() {
        // flat, then the hedged token jumps 35% inside one bar: more than a 5x short's margin holds
        let ticks: Vec<i64> = (0..4000).map(|k| if k < 2000 { 0 } else { -3000 }).collect();
        let m = market(&ticks, 0.0, 5e8);
        let c = with_hedge(&m, 0.0);
        let cost = Costs { swap_bps: 5.0, gas_usd: 0.0, fee_haircut: 1.0, emission_haircut: 1.0, stable_swap_bps: 0.0, max_spot_dev_ticks: None, swap_depth_usd: None, max_swap_cost_bps: None, max_actions_per_day: None,
                           hedge_taker_bps: 4.5, hedge_slip_bps: 2.0, bridge_usd: 0.5, max_events: MAX_EVENTS, exec_delay_bars: 0 };
        let r = run(&c, &hedged(400, json!({"ratio": 1, "band_pct": 5, "min_usd": 0, "leverage": 5})), &cost, 10_000.0, 0, m.bar_end.len() - 1);
        assert!(r["hedge"]["liquidations"].as_u64().unwrap() > 0, "{}", r["hedge"]);
        // the venue never takes more than the margin it held: the account ends at zero or above
        assert!(r["value"].as_f64().unwrap() > 0.0);
        let f = |k: &str| r[k].as_f64().unwrap();
        let lhs = f("value") - f("hodl_value");
        let rhs = f("fees") + f("emissions") - f("swap_costs") - f("gas") + f("il") + f("hedge_pnl") + f("funding") - f("hedge_fees");
        assert!((lhs - rhs).abs() < 1e-4, "{lhs} vs {rhs}");
    }

    #[test]
    fn a_rebalance_mints_only_what_its_swap_delivers() {
        // 100 token1 at price 1 into a range around the price (half token0): a 50% swap cost takes 25
        // of the 50 swapped, so 25 token0 arrive; the mint takes 25 + 25 and 25 token1 stay idle
        let m = market(&vec![0i64; 100], 0.0, 5e8);
        let c = with_hedge(&m, 0.0);
        let mut cost = no_costs();
        cost.swap_bps = 5000.0;
        let mut sim = Sim::default();
        sim.y = 100.0;
        sim.rebalance(&c, &cost, m.bar_idx[1], m.bar_end[1], &[(-100, 100, 1.0)], "in");
        let s = c.s(m.bar_idx[1]);
        let (x, y) = sim.inventory(&c, s);
        assert!((c.value(x, y, s) - 75.0).abs() < 1e-6, "value {}", c.value(x, y, s));
        assert!(sim.x.abs() < 1e-9 && (sim.y - 25.0).abs() < 1e-6, "idle {} {}", sim.x, sim.y);
        assert!((x - 25.0).abs() < 1e-6, "token0 {x}: no more than the swap delivered");
    }

    #[test]
    fn live_daily_cap_blocks_rebalances_past_it() {
        let mut cost = no_costs();
        cost.max_actions_per_day = Some(2);
        let mut sim = Sim::default();
        let d = 20_000 * 86400;
        assert!(sim.may_move(&cost, d + 10));
        sim.count_move(d + 10);
        sim.count_move(d + 20);
        assert!(!sim.may_move(&cost, d + 30), "the third on the day");
        assert!(sim.may_move(&cost, d + 86400), "the next UTC day");
        cost.max_actions_per_day = None;
        assert!(sim.may_move(&cost, d + 30));
    }

    #[test]
    fn dollars_handed_on_keep_their_value() {
        // a carried portfolio of 1000 dollars at 2000 dollars a numeraire unit (0.5): its first value
        // in the next run is the last one, not today's price (flat 1 here, which made it 1000)
        let m = market(&vec![0i64; 200], 0.0, 5e8);
        let c = ctx(&m);
        let mut sim = Sim::default();
        sim.usd = 1000.0;
        sim.upn = 2000.0;
        sim.hodl = Some((0.0, 0.0));
        let k = Carry { sim, cst: custom::State::default(), last: (m.bar_idx[10], m.bar_end[10]) };
        let (r, _) = run_carry(&c, &Strategy::Hodl, &no_costs(), 10_000.0, 11, 30, Some(k));
        let v0 = r["series"][0][1].as_f64().unwrap();
        assert!((v0 - 0.5).abs() < 1e-9, "first value {v0}");
    }

    #[test]
    fn our_liquidity_holds_a_thin_pool_near_the_market() {
        // others hold 1, we hold 99 around the market (tick 0): a swap the replay saw at tick 500
        // moves the pool 1% of that with us in it; with no position of ours it is as it traded
        let mut m = market(&[0, 0, 500, 0], 0.0, 1.0);
        m.l_others = vec![1.0; 4];
        let mut c = ctx(&m);
        c.tref = std::sync::Arc::new(vec![0; 4]);
        let mut sim = Sim::default();
        assert_eq!(sim.tick_at(&c, 2), 500);
        sim.ranges = vec![(-100, 100, 99.0)];
        assert_eq!(sim.tick_at(&c, 2), 5);
        // a narrow range beside the market, in the move's way, holds it as well
        sim.ranges = vec![(2, 7, 99.0)];
        assert_eq!(sim.tick_at(&c, 2), 5);
        c.tref = Default::default();
        assert_eq!(sim.tick_at(&c, 2), 500, "off: the replay's price");
        // pegged: swap by swap from where the pool was (tick 0), 1% of the way to 500, then back
        c.peg_damp = true;
        let mut sim = Sim::default();
        sim.ranges = vec![(2, 7, 99.0)];
        sim.advance(&c, 1);
        sim.advance(&c, 2);
        assert_eq!(sim.tick_at(&c, 2), 5);
        sim.advance(&c, 3);
        assert_eq!(sim.tick_at(&c, 3), 5, "back at 0 in the replay, one swap pulls 1% of the way");
        sim.ranges.clear();
        sim.eff = None;
        sim.advance(&c, 3);
        assert_eq!(sim.tick_at(&c, 3), 0, "no position: the replay's price");
    }

    #[test]
    fn an_empty_target_keeps_the_principal() {
        let m = market(&vec![0i64; 100], 0.0, 5e8);
        let c = with_hedge(&m, 0.0);
        let mut sim = Sim::default();
        sim.y = 10_000.0;
        sim.rebalance(&c, &no_costs(), m.bar_idx[1], m.bar_end[1], &[], "nothing");
        assert_eq!(sim.y, 10_000.0);
        sim.rebalance(&c, &no_costs(), m.bar_idx[1], m.bar_end[1], &[(0, 10, 0.0)], "no weight");
        assert_eq!(sim.y, 10_000.0);
        assert!(sim.ranges.is_empty());
    }

    #[test]
    fn a_still_price_earns_exactly_its_funding() {
        // the price never moves: the short opens once on the first deposit and earns rate × notional an hour
        let m = market(&vec![0i64; 3000], 0.0, 5e8);
        let rate = 2e-5;
        let c = with_hedge(&m, rate);
        let r = run(&c, &hedged(100, json!({"ratio": 1, "min_usd": 0})), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        assert_eq!(r["hedge"]["trades"], 1);
        let short = r["hedge"]["short_end"].as_f64().unwrap();
        // funding accrues from the bar after the opening one
        let hours = (m.bar_end[m.bar_end.len() - 1] - m.bar_end[0]) as f64 / 3600.0;
        let want = short * 1.0 * rate * hours;
        assert!((r["funding"].as_f64().unwrap() - want).abs() < want * 1e-6, "{} vs {want}", r["funding"]);
        assert!(r["hedge_pnl"].as_f64().unwrap().abs() < 1e-9);
    }

    #[test]
    fn a_size_of_its_own_bets_on_the_direction() {
        // the volatile token (token1 here) falls steadily; shorts are paid funding
        let ticks: Vec<i64> = (0..6000).map(|k| k / 2).collect();
        let m = market(&ticks, 0.0, 5e8);
        let c = with_hedge(&m, 1e-5);
        let end = m.bar_end.len() - 1;
        let open = run(&c, &hedged(4000, Value::Null), &no_costs(), 10_000.0, 0, end);
        let short = run(&c, &hedged(4000, json!({"ratio": 0, "size": -1, "band_pct": 2, "min_usd": 0})), &no_costs(), 10_000.0, 0, end);
        let long = run(&c, &hedged(4000, json!({"ratio": 0, "size": 1, "band_pct": 2, "min_usd": 0})), &no_costs(), 10_000.0, 0, end);
        let pnl = |r: &Value| r["pnl"].as_f64().unwrap();
        assert!(short["hedge_pnl"].as_f64().unwrap() > 0.0 && pnl(&short) > pnl(&open) + 500.0, "short {} vs open {}", pnl(&short), pnl(&open));
        assert!(short["funding"].as_f64().unwrap() > 0.0, "a short is paid");
        assert!(short["hedge"]["short_end"].as_f64().unwrap() > 0.0);
        assert!(long["hedge_pnl"].as_f64().unwrap() < 0.0 && pnl(&long) < pnl(&open), "long {} vs open {}", pnl(&long), pnl(&open));
        assert!(long["funding"].as_f64().unwrap() < 0.0, "a long pays what a short earns");
        assert!(long["hedge"]["short_end"].as_f64().unwrap() < 0.0, "a long is a negative short");
        // size 0 is the plain hedge, unchanged
        let a = run(&c, &hedged(4000, json!({"ratio": 1, "band_pct": 2, "min_usd": 0})), &no_costs(), 10_000.0, 0, end);
        let b = run(&c, &hedged(4000, json!({"ratio": 1, "size": 0, "band_pct": 2, "min_usd": 0})), &no_costs(), 10_000.0, 0, end);
        assert_eq!(a["value"], b["value"]);
    }

    #[test]
    fn margin_transfers_are_logged_with_their_times_and_a_full_event_list_says_so() {
        // a short on a rising price: the venue keeps asking for margin
        let ticks: Vec<i64> = (0..6000).map(|k| k * 2).collect();
        let m = market(&ticks, 0.0, 5e8);
        let c = with_hedge(&m, 1e-5);
        let end = m.bar_end.len() - 1;
        let r = run(&c, &hedged(4000, json!({"ratio": 0, "size": -1, "band_pct": 2, "min_usd": 0})), &no_costs(), 10_000.0, 0, end);
        let h = &r["hedge"];
        let log = h["margin_events"].as_array().unwrap();
        let moves = log.iter().filter(|e| e["kind"] == "margin").count();
        assert!(moves > 0);
        // the opening deposit too
        assert_eq!(moves as u64, h["margin_moves"].as_u64().unwrap());
        assert_eq!(log[0]["initial"], true);
        assert_eq!(log.iter().filter(|e| e["kind"] == "margin_short").count() as u64, h["margin_short"].as_u64().unwrap());
        let ts: Vec<i64> = log.iter().map(|e| e["ts"].as_i64().unwrap()).collect();
        assert!(ts.windows(2).all(|w| w[0] <= w[1]) && ts.iter().all(|t| *t >= m.bar_end[0] && *t <= m.bar_end[end]));
        assert_eq!(r["events_truncated"], false);
        // a price swinging 4% back and forth trades the short again and again: more than the list keeps
        let zig: Vec<i64> = (0..60000).map(|k| if (k / 12) % 2 == 0 { 0 } else { 400 }).collect();
        let mz = market(&zig, 0.0, 5e8);
        let cz = with_hedge(&mz, 1e-5);
        let busy = run(&cz, &hedged(4000, json!({"ratio": 0, "size": -1, "band_pct": 1, "min_usd": 0})), &no_costs(), 10_000.0, 0, mz.bar_end.len() - 1);
        assert_eq!(busy["events"].as_array().unwrap().len(), MAX_EVENTS);
        assert_eq!(busy["events_truncated"], true);
        let dropped = busy["events_dropped"].as_u64().unwrap();
        assert!(dropped > 0);
        // a harness that asks for all of them gets every one
        let all = run(&cz, &hedged(4000, json!({"ratio": 0, "size": -1, "band_pct": 1, "min_usd": 0})), &Costs { max_events: MAX_EVENTS_ALL, ..no_costs() }, 10_000.0, 0, mz.bar_end.len() - 1);
        assert_eq!(all["events_truncated"], false);
        assert_eq!(all["events"].as_array().unwrap().len() as u64, MAX_EVENTS as u64 + dropped);
        assert!(all["max_drawdown"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn funding_is_seen_only_once_paid_and_each_hour_pays_its_own_rate() {
        let ticks: Vec<i64> = vec![0; 3000];
        let m = market(&ticks, 0.0, 5e8);
        let mut c = with_hedge(&m, 0.0);
        let h0 = m.ts[0].div_euclid(3600) * 3600;
        // the rate paid at h0 + k hours is k millionths
        c.hedge.funding_h = (0..400).map(|k| (h0 + k * 3600, k as f64 * 1e-6)).collect();
        let f = funding_series(&c, &m);
        let k_of = |r: f64| (r / (8760.0 * 100.0) / 1e-6).round() as i64;
        for (b, t) in m.bar_end.iter().enumerate() {
            // a bar sees the last payment made before it ended
            assert_eq!(h0 + k_of(f.close[b]) * 3600, (t - 1).div_euclid(3600) * 3600, "bar ending {t}");
        }
        // a short held over the hour starting at h0 + 5h earns the rate paid at its end, h0 + 6h
        let mut sim = Sim { short: 1.0, h_px: 1000.0, margin: 1e6, ..Default::default() };
        let t0 = h0 + 5 * 3600;
        sim.hedge_mark(&c, &no_costs(), t0, t0 + 3600, 1000.0, 1000.0, 1000.0, t0 + 3600);
        assert!((sim.funding - 1000.0 * 6e-6).abs() < 1e-12, "{}", sim.funding);
    }

    #[test]
    fn a_dollar_perp_on_the_numeraire_bets_on_its_price_against_the_dollar() {
        // cbETH/WETH-like: the pool's price stands still while WETH goes from $2,000 to $3,000
        let ticks: Vec<i64> = vec![0; 8000];
        let m = market(&ticks, 0.0, 5e8);
        let mut c = ctx(&m);
        c.num_token = "testweth".into();
        c.num_per_usd = 1.0 / 3000.0;
        let h0 = m.ts[0].div_euclid(3600) * 3600;
        c.hedge = HedgeMkt { legs: 1.0, funding_h: (0..400).map(|k| (h0 + k * 3600, 0.0)).collect(), mm: 0.02, label: "ETH perp against the dollar".into(), error: None, from_h: 0, usd: true };
        let n = m.bar_end.len();
        let usd = |b: usize| 2000.0 + 1000.0 * b as f64 / (n - 1) as f64;
        let close: Vec<f64> = (0..n).map(|b| usd(b).ln() / 1.0001f64.ln()).collect();
        crate::external::preload(c.cid, "testweth", &m.bar_end, crate::external::Series { hi: close.clone(), lo: close.clone(), at: vec![0; n], close });
        let end = n - 1;
        let pnl = |r: &Value| r["pnl"].as_f64().unwrap();
        let open = run(&c, &hedged(4000, Value::Null), &no_costs(), 10_000.0, 0, end);
        let long = run(&c, &hedged(4000, json!({"ratio": 0, "size": 1, "band_pct": 2, "min_usd": 0})), &no_costs(), 10_000.0, 0, end);
        let short = run(&c, &hedged(4000, json!({"ratio": 0, "size": -1, "band_pct": 2, "min_usd": 0})), &no_costs(), 10_000.0, 0, end);
        // a long of the whole value, its dollar margin a share of it that loses against WETH: never
        // re-sized it makes ETH's +50% a third of the long's WETH less a third of the margin; re-sized
        // all the time (margin share m) the value grows as 1.5^(1 − m)
        let m0 = long["hedge"]["margin"].as_f64().unwrap();
        let want = 10_000.0 / 3.0 - m0 / 3.0;
        let most = 10_000.0 * (1.5f64.powf(1.0 - m0 / 10_000.0) - 1.0);
        let gain = pnl(&long) - pnl(&open);
        assert!(gain > 0.95 * want && gain < 1.05 * most, "long {gain}: between {want} and {most}");
        assert!(pnl(&short) < pnl(&open) - 0.5 * want, "short {}", pnl(&short));
        // a ratio means nothing here (the pool's price cannot be hedged with it): it is ignored
        let ratio = run(&c, &hedged(4000, json!({"ratio": 1, "size": 0, "band_pct": 2, "min_usd": 0})), &no_costs(), 10_000.0, 0, end);
        assert_eq!(ratio["hedge"]["trades"].as_u64().unwrap(), 0);
    }

    #[test]
    fn every_variant_is_measured_over_one_shared_hold() {
        let ticks: Vec<i64> = (0..6000).map(|k| k / 2).collect();
        let m = market(&ticks, 0.0, 5e8);
        let c = with_hedge(&m, 1e-5);
        let end = m.bar_end.len() - 1;
        let (t0, t1) = (m.bar_end[0], m.bar_end[end]);
        assert!(matches!(strategy(json!({"type": "hodl"}).as_object().unwrap(), 1, 4, false), Ok(Strategy::Hodl)));
        let h = run(&c, &Strategy::Hodl, &no_costs(), 10_000.0, 0, end);
        let bench: Vec<(i64, f64)> = h["series"].as_array().unwrap().iter().map(|p| (p[0].as_i64().unwrap(), p[1].as_f64().unwrap())).collect();
        // holding over itself earns nothing in any slice
        let own = crate::overfit::slice_excess(&with_benchmark(h["series"].as_array().unwrap(), &bench), t0, t1, 8, 10_000.0);
        assert!(own.iter().all(|x| x.abs() < 1e-9), "{own:?}");
        // two strategies: over the shared hold, their excesses differ by exactly their returns' difference
        let a = run(&c, &hedged(4000, Value::Null), &no_costs(), 10_000.0, 0, end);
        let s = run(&c, &hedged(4000, json!({"ratio": 0, "size": -1, "band_pct": 2, "min_usd": 0})), &no_costs(), 10_000.0, 0, end);
        let ex = |r: &Value| crate::overfit::slice_excess(&with_benchmark(r["series"].as_array().unwrap(), &bench), t0, t1, 8, 10_000.0);
        let plain = |r: &Value| {
            let z: Vec<(i64, f64)> = r["series"].as_array().unwrap().iter().map(|p| (p[0].as_i64().unwrap(), 0.0)).collect();
            crate::overfit::slice_excess(&with_benchmark(r["series"].as_array().unwrap(), &z), t0, t1, 8, 10_000.0)
        };
        let (ea, es, pa, ps) = (ex(&a), ex(&s), plain(&a), plain(&s));
        for k in 0..8 {
            assert!(((es[k] - ea[k]) - (ps[k] - pa[k])).abs() < 1e-6, "slice {k}");
        }
    }

    #[test]
    fn a_decision_on_a_close_trades_at_the_next_open_when_asked() {
        use crate::candles::Candle;
        let day = 86400;
        // flat at $1,000; day 10 closes at $2,000; day 11 opens at $2,100 and stays there
        let cs: Vec<Candle> = (0..20).map(|k| {
            let (o, c) = match k { 0..=9 => (1000.0, 1000.0), 10 => (1000.0, 2000.0), _ => (2100.0, 2100.0) };
            Candle { t: 1_700_006_400 + k * day, o, h: o.max(if k == 10 { 1000.0 } else { c }), l: o.min(c), c, v: 1.0 }
        }).collect();
        let m = candle_market(&cs, day);
        let mut c = ctx(&m);
        c.num1 = true;
        c.hedge = HedgeMkt { legs: 1.0, funding_h: HashMap::new(), mm: 0.02, label: "ETH perp".into(), error: None, from_h: i64::MIN, usd: false };
        let th = 1500f64.ln() / 1.0001f64.ln();
        let g = crate::graph::Graph::parse(json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"}, {"id": "w", "kind": "const", "p": {"value": 4000}},
            {"id": "a", "kind": "around", "in": {"center": ["p", "tick"], "width": ["w", "value"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["a", "lower"], "upper": ["a", "upper"]}},
            {"id": "th", "kind": "const", "p": {"value": th}}, {"id": "up", "kind": "gt", "in": {"a": ["p", "tick"], "b": ["th", "value"]}},
            {"id": "h", "kind": "hedge", "p": {"ratio": 0, "band_pct": 1, "min_usd": 0}, "in": {"size": ["up", "out"]}}]}).as_object().unwrap(), 1).unwrap();
        let st = Strategy::Graph(Box::new(g));
        let first_px = |delay: usize| {
            let r = run(&c, &st, &Costs { exec_delay_bars: delay, ..no_costs() }, 10_000.0, 0, m.bar_end.len() - 1);
            let e = r["events"].as_array().unwrap().iter().find(|e| e["kind"] == "hedge").cloned().unwrap();
            (e["px"].as_f64().unwrap(), e["ts"].as_i64().unwrap())
        };
        let (at_close, t_close) = first_px(0);
        let (at_open, t_open) = first_px(1);
        let close_t = 1_700_006_400 + 11 * day;
        assert!((at_close - 2000.0).abs() < 1.0 && t_close == close_t, "{at_close} at {t_close}");
        assert!((at_open - 2100.0).abs() < 1.0 && t_open == close_t + 300, "{at_open} at {t_open}");
        // and the candles' high and low in the order asked
        let up = [Candle { t: 0, o: 100.0, h: 120.0, l: 90.0, c: 110.0, v: 1.0 }];
        let px = |mk: &Market, k: usize| 1.0001f64.powf(mk.tick[k] as f64);
        for (order, first) in [("close", 90.0), ("high_first", 120.0), ("low_first", 90.0)] {
            let mk = candle_market_ordered(&up, day, order);
            assert!((px(&mk, 1) / first - 1.0).abs() < 1e-3, "{order}");
        }
    }

    #[test]
    fn a_candle_market_ends_on_its_last_close_and_sees_only_closed_daily_candles() {
        use crate::candles::Candle;
        let day = 86400;
        let cs: Vec<Candle> = (0..40).map(|k| {
            let o = 1000.0 + k as f64;
            Candle { t: 1_700_006_400 + k * day, o, h: o + 50.0, l: o - 50.0, c: o + 10.0, v: 1.0 }
        }).collect();
        let m = candle_market(&cs, day);
        let px = |b: usize| 1.0001f64.powf(m.bar_tick[b] as f64);
        let last = cs.last().unwrap();
        assert!((px(m.bar_end.len() - 1) / last.c - 1.0).abs() < 1e-3, "ends at {} not the close {}", px(m.bar_end.len() - 1), last.c);
        // at each day's boundary the price is that day's completed close (no later candle seen)
        for (k, c) in cs.iter().enumerate().take(39) {
            let b = m.bar_end.partition_point(|t| *t < c.t + day);
            assert_eq!(m.bar_end[b], c.t + day);
            assert!((px(b) / c.c - 1.0).abs() < 1e-3, "day {k}: {} vs close {}", px(b), c.c);
        }
    }

    #[test]
    fn the_margin_reserve_never_exceeds_the_capital() {
        let ticks: Vec<i64> = (0..6000).map(|k| k / 2).collect();
        let m = market(&ticks, 0.0, 5e8);
        let c = with_hedge(&m, 0.0);
        let r = run(&c, &hedged(4000, json!({"ratio": 1.5, "size": -2, "leverage": 1, "band_pct": 2, "min_usd": 0})), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        assert!(r["hedge"]["margin"].as_f64().unwrap() <= 10_000.0 + 1e-9, "{}", r["hedge"]);
        assert!(r["value"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn a_leveraged_long_is_liquidated_at_the_bars_low() {
        // flat, then the volatile token (token1) gaps a third down in one swap
        let ticks: Vec<i64> = (0..6000).map(|k| if k < 3000 { 0 } else { 4000 }).collect();
        let m = market(&ticks, 0.0, 5e8);
        let c = with_hedge(&m, 0.0);
        let r = run(&c, &hedged(40000, json!({"ratio": 0, "size": 2, "leverage": 5, "band_pct": 50, "min_usd": 0})), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        assert!(r["hedge"]["liquidations"].as_u64().unwrap() > 0, "{}", r["hedge"]);
        assert!(r["value"].as_f64().unwrap() > 0.0, "the venue's loss stops at the margin");
    }

    #[test]
    fn a_hedge_takes_out_most_of_a_trend() {
        // a steady fall of the volatile token (token1 per token0 rises: token1 is the one hedged here)
        let ticks: Vec<i64> = (0..6000).map(|k| k / 2).collect();
        let m = market(&ticks, 0.0, 5e8);
        let c = with_hedge(&m, 0.0);
        let open = run(&c, &hedged(4000, Value::Null), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        let hedged_r = run(&c, &hedged(4000, json!({"ratio": 1, "band_pct": 2, "min_usd": 0})), &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        let (a, b) = (open["pnl"].as_f64().unwrap(), hedged_r["pnl"].as_f64().unwrap());
        assert!(a < -500.0, "the open pool loses with the token: {a}");
        assert!(b.abs() < a.abs() * 0.1, "hedged {b} vs open {a}");
        assert!(hedged_r["hedge_pnl"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn the_identity_holds_with_costs_and_moves() {
        let ticks: Vec<i64> = (0..3000).map(|k| ((k as f64 / 120.0).sin() * 40.0) as i64).collect();
        let m = market(&ticks, 0.5, 5e8);
        let c = ctx(&m);
        let cost = Costs { swap_bps: 5.0, gas_usd: 0.5, fee_haircut: 0.8, emission_haircut: 0.5, stable_swap_bps: 0.0, max_spot_dev_ticks: None, swap_depth_usd: None, max_swap_cost_bps: None, max_actions_per_day: None, hedge_taker_bps: 0.0, hedge_slip_bps: 0.0, bridge_usd: 0.0, max_events: MAX_EVENTS, exec_delay_bars: 0 };
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
    fn the_spot_check_delays_a_move_after_a_jump_as_the_worker_would() {
        // flat, then a 100-tick jump that stays: the 1h median catches up after about half an hour
        let mut ticks = vec![0i64; 600];
        ticks.extend(vec![100i64; 600]);
        let m = market(&ticks, 1.0, 1e9);
        let c = ctx(&m);
        let strat = custom(json!({"width": 10}));
        let free = run(&c, &strat, &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        let checked = run(&c, &strat, &Costs { max_spot_dev_ticks: Some(20.0), ..no_costs() }, 10_000.0, 0, m.bar_end.len() - 1);
        assert_eq!(free["spot_blocked_bars"], json!(0));
        let blocked = checked["spot_blocked_bars"].as_u64().unwrap();
        // 60 swaps an hour: the median flips once more than half the hour's swaps are at the new price
        assert!((4..=8).contains(&blocked), "{blocked}");
        // it still moves once the median has caught up
        assert_eq!(checked["actions"], free["actions"]);
        assert!(spot_dev(&m, 600) == 100.0 && spot_dev(&m, 640) == 0.0);
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
    fn the_migrated_ladder_block_trades_as_the_plain_ladder() {
        // a calm peg with a depeg of 40 ticks for two hours: the guard holds through it. The figures
        // are what the plain ladder (removed in v0.5.1) returned on this market, after a day of warm-up.
        let ticks: Vec<i64> = (0..6000).map(|k| if (2500..2620).contains(&k) { 40 } else { ((k as f64 / 300.0).sin() * 3.0) as i64 }).collect();
        let m = market(&ticks, 0.5, 5e8);
        let c = ctx(&m);
        for (guard, plain) in [(None, json!({"value": 10022.543486, "fees": 25.032618, "actions": 7, "guard_bars": 0})),
                               (Some(15.0), json!({"value": 10030.41445, "fees": 31.708518, "actions": 6, "guard_bars": 24}))] {
            let p = Params { lookback: 86400, span: 3, every: 72, turnover: 0.4, guard, width: 1 };
            let g = crate::ladder::as_graph(&p);
            let mut gr = crate::graph::Graph::parse(g.as_object().unwrap(), 1).unwrap();
            gr.max_ranges = 5;
            let node = run(&c, &Strategy::Graph(Box::new(gr)), &no_costs(), 10_000.0, 300, m.bar_end.len() - 1);
            for k in ["value", "fees", "actions", "guard_bars"] {
                assert_eq!(plain[k], node[k], "{guard:?} {k}");
            }
        }
    }

    #[test]
    fn ladder_and_hold_run() {
        let ticks: Vec<i64> = (0..3000).map(|k| ((k as f64 / 300.0).sin() * 3.0) as i64).collect();
        let m = market(&ticks, 0.5, 5e8);
        let c = ctx(&m);
        let mut g = crate::graph::Graph::parse(crate::ladder::as_graph(&Params { lookback: 86400, span: 3, every: 72, turnover: 0.4, guard: None, width: 1 }).as_object().unwrap(), 1).unwrap();
        g.max_ranges = 5;
        let l = run(&c, &Strategy::Graph(Box::new(g)), &no_costs(), 10_000.0, 300, m.bar_end.len() - 1);
        assert!(l["actions"].as_u64().unwrap() >= 1);
        assert!(l["fees"].as_f64().unwrap() > 0.0);
        let h = run(&c, &Strategy::Hodl, &no_costs(), 10_000.0, 0, m.bar_end.len() - 1);
        assert_eq!(h["actions"], 0);
        assert!((h["value"].as_f64().unwrap() - h["hodl_value"].as_f64().unwrap()).abs() < 1e-6);
    }
}
