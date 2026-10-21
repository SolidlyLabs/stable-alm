//! Custom mode: rebalance rules the user builds in the dashboard, executed automatically.
//!
//! A custom strategy keeps one range around the price and moves it by rules:
//!   - width and placement: centred (with a skew) or following the price from the crossed edge;
//!   - trigger: leaving the range, widened or narrowed by a buffer on each side;
//!   - cutoffs: fixed prices beyond which it does not rebalance (it waits);
//!   - delay and cooldown: how long the trigger must hold, and the minimum time between moves;
//!   - exit: fixed prices that withdraw everything (optionally into one token), with an optional
//!     re-entry once the price is back between them.
//! `decide` is pure: the live executor and the backtester call the same function, so a backtest
//! replays exactly what the app would have done. Ticks are absolute pool ticks; the dashboard shows
//! them as prices.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Placement {
    /// `skew` = share of the width below the price (0.5 = centred).
    Center { skew: f64 },
    /// Keep the width; after a move up the price sits `up` ticks below the new upper edge, after a
    /// move down `down` ticks above the new lower edge.
    Follow { up: i64, down: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExitTo {
    Keep,
    Token0,
    Token1,
    /// both tokens sold for the chain's dollar stable (USDC), which waits parked like any funding
    /// token and buys the pair back on re-entry (engine.rs `fund`)
    Stable,
}

#[derive(Clone, Debug)]
pub struct Rules {
    pub width: i64,
    pub placement: Placement,
    pub buffer_lo: i64,
    pub buffer_hi: i64,
    pub cutoff_lo: Option<i64>,
    pub cutoff_hi: Option<i64>,
    pub delay_s: i64,
    pub cooldown_s: i64,
    pub exit_lo: Option<i64>,
    pub exit_hi: Option<i64>,
    pub exit_delay_s: i64,
    pub exit_to: ExitTo,
    pub guard: Option<f64>,
    /// the token an exit waits in is parked in a pegged pool meanwhile (parking.rs)
    pub park: bool,
}

fn opt_i(s: &Map<String, Value>, k: &str) -> Option<i64> {
    s.get(k).and_then(|v| v.as_f64()).map(|x| x.round() as i64)
}

impl Rules {
    pub fn from(s: &Map<String, Value>, spacing: i64) -> Result<Rules, String> {
        let spacing = spacing.max(1);
        // widths, offsets and buffers in ticks, or in tick spacings ("unit": "spacings", used by the
        // built-in strategies so they fit any pool); cutoff and exit prices are always ticks
        let per = if s.get("unit").and_then(|v| v.as_str()) == Some("spacings") { spacing as f64 } else { 1.0 };
        let n = |k: &str, d: f64| s.get(k).and_then(|v| v.as_f64()).unwrap_or(d);
        let w = |k: &str| n(k, 0.0) * per;
        let width = ((n("width", 10.0) * per).round() as i64).max(spacing);
        let placement = match s.get("placement").and_then(|v| v.as_str()).unwrap_or("center") {
            "follow" => Placement::Follow { up: w("offset_up").round() as i64, down: w("offset_down").round() as i64 },
            _ => Placement::Center { skew: n("skew", 0.5).clamp(0.0, 1.0) },
        };
        let exit_to = match s.get("exit_to").and_then(|v| v.as_str()).unwrap_or("keep") {
            "token0" => ExitTo::Token0,
            "token1" => ExitTo::Token1,
            "stable" => ExitTo::Stable,
            _ => ExitTo::Keep,
        };
        let r = Rules {
            width,
            placement,
            buffer_lo: w("buffer_lo").round() as i64,
            buffer_hi: w("buffer_hi").round() as i64,
            cutoff_lo: opt_i(s, "cutoff_lo"),
            cutoff_hi: opt_i(s, "cutoff_hi"),
            delay_s: (n("delay_min", 0.0) * 60.0) as i64,
            cooldown_s: (n("cooldown_min", 0.0) * 60.0) as i64,
            exit_lo: opt_i(s, "exit_lo"),
            exit_hi: opt_i(s, "exit_hi"),
            exit_delay_s: (n("exit_delay_min", 0.0) * 60.0) as i64,
            exit_to,
            guard: s.get("guard").and_then(|v| v.as_f64()).filter(|g| *g > 0.0),
            park: s.get("park").and_then(|v| v.as_bool()).unwrap_or(false),
        };
        if let (Some(a), Some(b)) = (r.exit_lo, r.exit_hi) {
            if a >= b {
                return Err("custom mode: the lower exit price must be below the upper one".into());
            }
        }
        if let (Some(a), Some(b)) = (r.cutoff_lo, r.cutoff_hi) {
            if a >= b {
                return Err("custom mode: the lower cutoff must be below the upper one".into());
            }
        }
        if r.buffer_lo + r.buffer_hi + width <= 0 {
            return Err("custom mode: negative buffers leave no room inside the range".into());
        }
        Ok(r)
    }

    pub fn label(&self) -> String {
        let place = match &self.placement {
            Placement::Center { skew } if (*skew - 0.5).abs() < 1e-9 => "centred".to_string(),
            Placement::Center { skew } => format!("{:.0}% below price", skew * 100.0),
            Placement::Follow { up, down } => format!("follow (+{up}/-{down})"),
        };
        format!("custom(width={} ticks, {place})", self.width)
    }

    fn exit_hit(&self, tick: i64) -> bool {
        self.exit_lo.map(|x| tick < x).unwrap_or(false) || self.exit_hi.map(|x| tick >= x).unwrap_or(false)
    }

    fn beyond_cutoff(&self, tick: i64) -> bool {
        self.cutoff_lo.map(|x| tick < x).unwrap_or(false) || self.cutoff_hi.map(|x| tick >= x).unwrap_or(false)
    }

    /// The trigger band of a range: rebalance once the price is outside [lo, hi).
    pub fn band(&self, tl: i64, tu: i64) -> (i64, i64) {
        let (lo, hi) = (tl - self.buffer_lo, tu + self.buffer_hi);
        if lo < hi {
            (lo, hi)
        } else {
            let m = (tl + tu) / 2;
            (m, m + 1)
        }
    }

    /// A new range for the price at `tick`. `prev` is the range being replaced (for Follow).
    pub fn range_for(&self, tick: i64, prev: Option<(i64, i64)>, spacing: i64) -> (i64, i64) {
        let sp = spacing.max(1);
        let w = (self.width + sp - 1).div_euclid(sp) * sp;
        let (tl, tu) = match (&self.placement, prev) {
            (Placement::Follow { up, down }, Some((pl, pu))) => {
                let w = (pu - pl).max(sp);
                if tick >= (pl + pu) / 2 {
                    let tu = (tick + 1 + up).div_euclid(sp) * sp + if (tick + 1 + up).rem_euclid(sp) == 0 { 0 } else { sp };
                    (tu - w, tu)
                } else {
                    let tl = (tick - down).div_euclid(sp) * sp;
                    (tl, tl + w)
                }
            }
            (Placement::Center { skew }, _) => {
                let below = (w as f64 * skew).round() as i64;
                let tl = (tick - below).div_euclid(sp) * sp;
                (tl, tl + w)
            }
            (Placement::Follow { .. }, None) => {
                let tl = (tick - w / 2).div_euclid(sp) * sp;
                (tl, tl + w)
            }
        };
        // Centred placement keeps the price inside; a follow offset may deliberately leave it out.
        if matches!(self.placement, Placement::Center { .. }) && !(tl <= tick && tick < tu) {
            let tl = tick.div_euclid(sp) * sp - (w / sp / 2) * sp;
            return (tl, tl + w);
        }
        (tl, tu)
    }
}

/// Timers and flags kept between cycles (logs/custom_<chain>_<pool>.json, separate for dry runs).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct State {
    pub outside_since: Option<i64>,
    pub exit_since: Option<i64>,
    pub exited: bool,
    pub last_move: i64,
    /// the token an exit waits in (graph.rs side_code: 1 token0, -1 token1, 0 as it was)
    #[serde(default)]
    pub exit_side: i8,
    /// re-solve on the next decision whatever the turnover (a parked lot was handed over)
    #[serde(default)]
    pub force_solve: bool,
    /// when the strategy was first out after its exit (graph exits' min_out_hours)
    #[serde(default)]
    pub out_since: Option<i64>,
    /// since when the re-entry condition has held without a break (graph exits' confirm_hours)
    #[serde(default)]
    pub back_since: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Nothing to do (reason).
    Keep(String),
    /// A trigger holds but a delay, cooldown or cutoff says wait (reason).
    Wait(String),
    /// Move (or open) the position.
    Move { tl: i64, tu: i64, reason: String },
    /// Withdraw everything and stay out.
    Exit(String),
}

fn mins(s: i64) -> String {
    if s >= 5400 { format!("{:.1} h", s as f64 / 3600.0) } else { format!("{} min", (s + 59) / 60) }
}

/// The rules' decision at `now` for the price at `tick` and the current range (None = no position).
/// Updates the timers in `st`; the caller records a completed move or exit with `moved` / `exited`.
pub fn decide(r: &Rules, st: &mut State, now: i64, tick: i64, range: Option<(i64, i64)>, spacing: i64) -> Decision {
    // 1. exit triggers
    if r.exit_hit(tick) {
        if st.exited {
            return Keep(format!("exited; price still beyond the exit trigger"));
        }
        let since = *st.exit_since.get_or_insert(now);
        if now - since < r.exit_delay_s {
            return Decision::Wait(format!("exit trigger crossed; exiting in {}", mins(r.exit_delay_s - (now - since))));
        }
        return Decision::Exit(format!("price crossed the exit trigger (tick {tick})"));
    }
    st.exit_since = None;
    if st.exited {
        // the strategy deposits again by its own rule, never by hand: back between the exit triggers
        if r.beyond_cutoff(tick) {
            return Decision::Wait("exited; price is back inside the exit triggers but beyond a cutoff".into());
        }
        let (tl, tu) = r.range_for(tick, None, spacing);
        return Decision::Move { tl, tu, reason: "price is back between the exit triggers: re-entering".into() };
    }
    // 2. first deposit
    let Some((tl, tu)) = range else {
        if r.beyond_cutoff(tick) {
            return Decision::Wait("no position yet, and the price is beyond a cutoff".into());
        }
        let (a, b) = r.range_for(tick, None, spacing);
        return Decision::Move { tl: a, tu: b, reason: "no position yet: opening the first range".into() };
    };
    // 3. trigger band
    let (lo, hi) = r.band(tl, tu);
    if lo <= tick && tick < hi {
        st.outside_since = None;
        return Keep(if tl <= tick && tick < tu { "in range".into() } else { format!("out of range but inside the buffer [{lo},{hi})") });
    }
    let since = *st.outside_since.get_or_insert(now);
    if r.beyond_cutoff(tick) {
        return Decision::Wait(format!("price is beyond a cutoff (tick {tick}): not rebalancing"));
    }
    if now - since < r.delay_s {
        return Decision::Wait(format!("trigger met; rebalancing in {}", mins(r.delay_s - (now - since))));
    }
    if st.last_move > 0 && now - st.last_move < r.cooldown_s {
        return Decision::Wait(format!("cooldown: next move allowed in {}", mins(r.cooldown_s - (now - st.last_move))));
    }
    let (a, b) = r.range_for(tick, Some((tl, tu)), spacing);
    if (a, b) == (tl, tu) {
        return Keep("the new range equals the current one".into());
    }
    let side = if tick >= hi { "above" } else { "below" };
    Decision::Move { tl: a, tu: b, reason: format!("price {side} the trigger band [{lo},{hi}): moving to [{a},{b})") }
}

use Decision::Keep;

impl State {
    pub fn moved(&mut self, now: i64) {
        self.last_move = now;
        self.force_solve = false;
        self.outside_since = None;
        self.exited = false;
        self.out_since = None;
        self.back_since = None;
    }
    /// An exit that waits in `to` is done.
    pub fn exited_to(&mut self, to: ExitTo) {
        self.exited();
        self.exit_side = crate::graph::side_code(to);
    }
    pub fn exited(&mut self) {
        self.exited = true;
        self.exit_since = None;
        self.outside_since = None;
        self.back_since = None;
    }
}

fn state_path(cid: u64, pool: &str, live: bool) -> std::path::PathBuf {
    crate::paths::logs().join(format!("custom_{}{cid}_{}.json", if live { "" } else { "dry_" }, pool.to_lowercase()))
}

pub fn load_state(cid: u64, pool: &str, live: bool) -> State {
    std::fs::read_to_string(state_path(cid, pool, live)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_state(cid: u64, pool: &str, live: bool, st: &State) {
    let p = state_path(cid, pool, live);
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let _ = std::fs::write(p, serde_json::to_string(st).unwrap());
}


#[cfg(test)]
mod tests {
    use super::*;

    fn rules(v: Value) -> Rules {
        Rules::from(v.as_object().unwrap(), 1).unwrap()
    }

    #[test]
    fn centred_range_contains_price() {
        let r = rules(json!({"width": 10}));
        let (a, b) = r.range_for(103, None, 1);
        assert_eq!((a, b), (98, 108));
        let r = rules(json!({"width": 10, "skew": 0.2}));
        assert_eq!(r.range_for(103, None, 1), (101, 111));
    }

    #[test]
    fn spacing_units() {
        let r = Rules::from(json!({"width": 4, "buffer_hi": 1, "unit": "spacings"}).as_object().unwrap(), 10).unwrap();
        assert_eq!((r.width, r.buffer_hi), (40, 10));
    }

    #[test]
    fn spacing_rounds_width_up() {
        let r = rules(json!({"width": 15}));
        let (a, b) = r.range_for(1003, None, 10);
        assert_eq!(b - a, 20);
        assert!(a <= 1003 && 1003 < b && a % 10 == 0);
    }

    #[test]
    fn follow_moves_from_crossed_edge() {
        let r = rules(json!({"width": 10, "placement": "follow", "offset_up": 2, "offset_down": 3}));
        assert_eq!(r.range_for(112, Some((100, 110)), 1), (105, 115));
        assert_eq!(r.range_for(95, Some((100, 110)), 1), (92, 102));
    }

    #[test]
    fn buffer_delay_cooldown() {
        let r = rules(json!({"width": 10, "buffer_hi": 3, "delay_min": 10, "cooldown_min": 60}));
        let mut st = State::default();
        assert!(matches!(decide(&r, &mut st, 0, 111, Some((100, 110)), 1), Keep(_)));
        assert!(matches!(decide(&r, &mut st, 0, 113, Some((100, 110)), 1), Decision::Wait(_)));
        assert!(matches!(decide(&r, &mut st, 599, 113, Some((100, 110)), 1), Decision::Wait(_)));
        assert!(matches!(decide(&r, &mut st, 600, 113, Some((100, 110)), 1), Decision::Move { .. }));
        st.moved(600);
        assert!(matches!(decide(&r, &mut st, 700, 140, Some((108, 118)), 1), Decision::Wait(_)));
        assert!(matches!(decide(&r, &mut st, 4300, 140, Some((108, 118)), 1), Decision::Move { .. }));
    }

    #[test]
    fn negative_buffer_triggers_inside() {
        let r = rules(json!({"width": 10, "buffer_lo": -2}));
        let mut st = State::default();
        assert!(matches!(decide(&r, &mut st, 0, 101, Some((100, 110)), 1), Decision::Move { .. }));
    }

    #[test]
    fn cutoff_and_exit() {
        let r = rules(json!({"width": 10, "cutoff_hi": 130, "exit_lo": 50}));
        let mut st = State::default();
        assert!(matches!(decide(&r, &mut st, 0, 135, Some((100, 110)), 1), Decision::Wait(_)));
        assert!(matches!(decide(&r, &mut st, 0, 40, Some((100, 110)), 1), Decision::Exit(_)));
        st.exited();
        assert!(matches!(decide(&r, &mut st, 10, 45, None, 1), Keep(_)));
        assert!(matches!(decide(&r, &mut st, 20, 60, None, 1), Decision::Move { .. }));
    }
}
