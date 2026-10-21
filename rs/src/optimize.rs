//! Dynamic optimization for the Strategy lab: the best settings of the blocks a user picked, by an
//! adaptive search on the pool's real swaps.
//!
//! The settings to tune are chosen in the lab ({node, param, min, max} each; "tf" picks among the
//! candle lengths). The search is a cross-entropy method: a first round spreads trials evenly over
//! the ranges (Latin hypercube, plus the current settings), then every round draws new trials around
//! the best fifth so far, from their mean and spread per setting, narrowing as they agree, with some
//! trials that change one or two settings of the best one. Trials run in parallel on the replay
//! already in memory. It stops at the run budget, when the user stops it, or when the best fifth has
//! settled and two rounds found nothing better.
//!
//! The score is the return (P&L over the capital) or the return over holding. With a hold-out (the
//! default) the search sees only the first three quarters of the period, and the best settings are
//! then run on the last quarter they never saw: a large drop there means the settings fit noise.

use serde_json::{json, Map, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Settings one search may tune: each needs backtests of its own to be found (about ten), so the
/// budget, not the search, is what limits a useful run; this only keeps a run finite.
pub const MAX_DIMS: usize = 48;
pub const MAX_RUNS: usize = 2000;
const DEFAULT_RUNS: usize = 150;
const TOP: usize = 12;

/// One setting to tune.
#[derive(Clone, Debug)]
pub struct Dim {
    pub node: String,
    pub param: String,
    lo: f64,
    hi: f64,
    log: bool,
    int: bool,
    /// a choice among values (the candle length) instead of a range
    choices: Option<Vec<f64>>,
}

impl Dim {
    pub fn key(&self) -> String {
        format!("{}.{}", self.node, self.param)
    }
    /// The setting at position u in [0, 1] of its range.
    fn value(&self, u: f64) -> f64 {
        let u = u.clamp(0.0, 1.0);
        if let Some(c) = &self.choices {
            return c[((u * c.len() as f64) as usize).min(c.len() - 1)];
        }
        let v = if self.log { (self.lo.ln() + u * (self.hi.ln() - self.lo.ln())).exp() } else { self.lo + u * (self.hi - self.lo) };
        if self.int { v.round() } else { round_sig(v) }
    }
    /// `unit`, for callers outside the search (how far tuned values spread over the range).
    pub fn unit_of(&self, v: f64) -> f64 {
        self.unit(v)
    }
    /// Where a setting sits in the range (the current settings start the search).
    fn unit(&self, v: f64) -> f64 {
        if let Some(c) = &self.choices {
            let k = c.iter().position(|x| *x == v).unwrap_or(0);
            return (k as f64 + 0.5) / c.len() as f64;
        }
        let v = v.clamp(self.lo, self.hi);
        if self.log { (v.ln() - self.lo.ln()) / (self.hi.ln() - self.lo.ln()) } else { (v - self.lo) / (self.hi - self.lo) }
    }
}

/// Four significant digits: settings that read well and repeat when the search revisits them.
fn round_sig(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let p = 10f64.powi(3 - v.abs().log10().floor() as i32);
    (v * p).round() / p
}

fn node_of<'a>(s: &'a Map<String, Value>, id: &str) -> Option<&'a Value> {
    s.get("nodes")?.as_array()?.iter().find(|n| n["id"] == id)
}

/// The settings to tune, checked against the strategy and the block catalog (limits clamp the ranges).
pub fn dims(s: &Map<String, Value>, spec: &Value) -> Result<Vec<Dim>, String> {
    dims_upto(s, spec, MAX_DIMS)
}

/// `dims`, with room for `max` settings (screening looks at more than a search tunes).
pub fn dims_upto(s: &Map<String, Value>, spec: &Value, max: usize) -> Result<Vec<Dim>, String> {
    if s.get("type").and_then(|t| t.as_str()) != Some("graph") {
        return Err("optimization works on node strategies".into());
    }
    let list = spec.as_array().ok_or("pick at least one setting to optimize")?;
    if list.is_empty() {
        return Err("pick at least one setting to optimize".into());
    }
    if list.len() > max {
        return Err(format!("at most {max} settings at once"));
    }
    let cat = crate::graph::catalog();
    let mut out = vec![];
    for d in list {
        let id = d["node"].as_str().unwrap_or("");
        let param = d["param"].as_str().unwrap_or("");
        let n = node_of(s, id).ok_or_else(|| format!("no block {id:?}"))?;
        let kind = n["kind"].as_str().unwrap_or("");
        if param == "tf" {
            let c: Vec<f64> = d["choices"].as_array().map(|a| a.iter().filter_map(|x| x.as_f64()).filter(|x| crate::graph::TIMEFRAMES.contains(&(*x as i64))).collect())
                .unwrap_or_default();
            if c.len() < 2 {
                return Err(format!("{id}: pick at least two candle lengths"));
            }
            out.push(Dim { node: id.into(), param: "tf".into(), lo: 0.0, hi: 1.0, log: false, int: false, choices: Some(c) });
            continue;
        }
        let entry = cat.as_array().and_then(|a| a.iter().find(|k| k["kind"] == kind)).ok_or_else(|| format!("{id}: unknown block"))?;
        let p = entry["params"].as_array().and_then(|a| a.iter().find(|p| p["name"] == param)).ok_or_else(|| format!("{id}: {kind} has no setting {param:?}"))?;
        let (cmin, cmax) = (p["min"].as_f64().unwrap_or(f64::MIN), p["max"].as_f64().unwrap_or(f64::MAX));
        let lo = d["min"].as_f64().ok_or_else(|| format!("{id}.{param}: no minimum"))?.max(cmin);
        let hi = d["max"].as_f64().ok_or_else(|| format!("{id}.{param}: no maximum"))?.min(cmax);
        if !(lo < hi) {
            return Err(format!("{id}.{param}: the range is empty within the block's limits ({cmin} to {cmax})"));
        }
        let int = d["int"].as_bool().unwrap_or(false);
        out.push(Dim { node: id.into(), param: param.into(), lo, hi, log: lo > 0.0 && hi / lo >= 4.0, int, choices: None });
    }
    Ok(out)
}

/// The strategy with these settings.
pub fn apply(s: &Map<String, Value>, dims: &[Dim], x: &[f64]) -> Map<String, Value> {
    let mut s = s.clone();
    if let Some(nodes) = s.get_mut("nodes").and_then(|n| n.as_array_mut()) {
        for (d, v) in dims.iter().zip(x) {
            let Some(n) = nodes.iter_mut().find(|n| n["id"] == d.node.as_str()) else { continue };
            if d.param == "tf" {
                if *v == 5.0 {
                    n.as_object_mut().map(|o| o.remove("tf"));
                } else {
                    n["tf"] = json!(*v as i64);
                }
            } else {
                if !n["p"].is_object() {
                    n["p"] = json!({});
                }
                n["p"][d.param.as_str()] = json!(v);
            }
        }
    }
    s
}

pub fn current(s: &Map<String, Value>, dims: &[Dim]) -> Vec<f64> {
    let cat = crate::graph::catalog();
    dims.iter()
        .map(|d| {
            let n = node_of(s, &d.node);
            if d.param == "tf" {
                return n.and_then(|n| n["tf"].as_f64()).unwrap_or(5.0);
            }
            n.and_then(|n| n["p"][d.param.as_str()].as_f64()).unwrap_or_else(|| {
                let kind = n.and_then(|n| n["kind"].as_str()).unwrap_or("");
                cat.as_array().and_then(|a| a.iter().find(|k| k["kind"] == kind))
                    .and_then(|k| k["params"].as_array()?.iter().find(|p| p["name"] == d.param.as_str())?["default"].as_f64())
                    .unwrap_or(d.lo)
            })
        })
        .collect()
}

/// The same xorshift as the overfit check: a run can be repeated.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
    fn normal(&mut self) -> f64 {
        let (a, b) = (self.next().max(1e-12), self.next());
        (-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()
    }
}

#[derive(Clone)]
struct Trial {
    x: Vec<f64>,
    u: Vec<f64>,
    score: f64,
    row: Value,
}

/// What a trial reports: the score the search maximizes and the row the lab shows.
pub type Eval<'a> = dyn Fn(&Map<String, Value>) -> Result<(f64, Value), String> + Sync + 'a;

pub struct Opts {
    pub runs: usize,
    pub workers: usize,
}

impl Opts {
    pub fn from(spec: &Value) -> Opts {
        let runs = spec["runs"].as_u64().map(|r| r as usize).unwrap_or(DEFAULT_RUNS).clamp(10, MAX_RUNS);
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        let workers = (cores / 2).clamp(1, 6);
        // a caller may ask for fewer (the agent's research runs beside the live app: 2)
        let workers = spec["workers"].as_u64().map(|w| (w as usize).clamp(1, workers)).unwrap_or(workers);
        Opts { runs, workers }
    }
}

/// The search. `progress` gets (runs done, the best trials so far) after every trial.
pub fn search(base: &Map<String, Value>, dims: &[Dim], o: &Opts, eval: &Eval, stop: &AtomicBool, progress: &(dyn Fn(usize, Value) + Sync)) -> Value {
    let d = dims.len();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (d as u64) << 7 | 1);
    let trials: Mutex<Vec<Trial>> = Mutex::new(vec![]);
    let seen: Mutex<std::collections::HashSet<String>> = Mutex::new(Default::default());
    let batch = (o.workers * 2).max(4);
    let done = AtomicUsize::new(0);
    let mut best_hist: Vec<f64> = vec![];
    let mut stale_rounds = 0;
    let mut round = 0usize;
    let keyx = |x: &[f64]| x.iter().map(|v| format!("{v}")).collect::<Vec<_>>().join(",");
    let top_json = |ts: &[Trial]| -> Value {
        let mut v: Vec<&Trial> = ts.iter().filter(|t| t.score.is_finite()).collect();
        v.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        Value::Array(v.iter().take(TOP).map(|t| {
            let mut r = t.row.clone();
            r["values"] = Value::Object(dims.iter().zip(&t.x).map(|(d, x)| (d.key(), json!(x))).collect());
            r
        }).collect())
    };
    // one batch of trials, `workers` at a time; each is recorded and reported as it finishes
    let run_batch = |work: &[(Vec<f64>, Vec<f64>)]| {
        let next = AtomicUsize::new(0);
        std::thread::scope(|sc| {
            for _ in 0..o.workers.min(work.len()) {
                sc.spawn(|| loop {
                    let k = next.fetch_add(1, Ordering::SeqCst);
                    if k >= work.len() || stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let (u, x) = &work[k];
                    let s = apply(base, dims, x);
                    let (score, row) = match eval(&s) {
                        Ok((sc, r)) if sc.is_finite() => (sc, r),
                        Ok((_, r)) => (f64::NEG_INFINITY, r),
                        Err(e) => (f64::NEG_INFINITY, json!({"error": e})),
                    };
                    let snapshot = {
                        let mut g = trials.lock().unwrap();
                        g.push(Trial { x: x.clone(), u: u.clone(), score, row });
                        top_json(&g)
                    };
                    let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                    progress(n, snapshot);
                });
            }
        });
    };
    // the last part of the budget polishes the best trial one setting at a time (below)
    let polish = if o.runs >= 40 { (o.runs * 15 / 100).max(2 * d) } else { 0 };
    let main_runs = o.runs - polish.min(o.runs / 2);
    loop {
        let n_done = done.load(Ordering::SeqCst);
        if n_done >= main_runs || stop.load(Ordering::SeqCst) {
            break;
        }
        // the next round's candidates
        let mut cands: Vec<Vec<f64>> = vec![];
        if round == 0 {
            cands.push(dims.iter().zip(current(base, dims)).map(|(dm, v)| dm.unit(v)).collect());
            let n0 = (4 * d + 4).max(batch).min(o.runs / 3).max(2);
            // Latin hypercube: each setting's range in n0 strata, one trial per stratum, shuffled
            let mut cols: Vec<Vec<f64>> = (0..d).map(|_| {
                let mut c: Vec<f64> = (0..n0).map(|k| (k as f64 + rng.next()) / n0 as f64).collect();
                for i in (1..c.len()).rev() {
                    let j = (rng.next() * (i + 1) as f64) as usize;
                    c.swap(i, j.min(i));
                }
                c
            }).collect();
            for k in 0..n0 {
                cands.push((0..d).map(|j| cols[j][k]).collect());
            }
            cols.clear();
        } else {
            let ts = trials.lock().unwrap().clone();
            let mut ok: Vec<&Trial> = ts.iter().filter(|t| t.score.is_finite()).collect();
            if ok.is_empty() {
                // nothing valid yet: keep exploring at random
                for _ in 0..batch {
                    cands.push((0..d).map(|_| rng.next()).collect());
                }
            } else {
                ok.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
                let ne = (ok.len() / 5).max(3).min(ok.len());
                let elite = &ok[..ne];
                let frac = (n_done as f64 / o.runs as f64).min(1.0);
                let floor = 0.12 * (1.0 - frac) + 0.015;
                let mean: Vec<f64> = (0..d).map(|j| elite.iter().map(|t| t.u[j]).sum::<f64>() / ne as f64).collect();
                let sd: Vec<f64> = (0..d).map(|j| {
                    let v = elite.iter().map(|t| (t.u[j] - mean[j]).powi(2)).sum::<f64>() / ne as f64;
                    v.sqrt().max(floor)
                }).collect();
                if sd.iter().all(|s| *s <= 0.02) && stale_rounds >= 2 {
                    break; // settled
                }
                let best = &ok[0].u;
                for k in 0..batch {
                    if k % 3 == 2 {
                        // the best trial with one or two settings moved
                        let mut u = best.clone();
                        for _ in 0..(1 + (rng.next() * 2.0) as usize).min(d) {
                            let j = ((rng.next() * d as f64) as usize).min(d - 1);
                            u[j] = (u[j] + rng.normal() * sd[j].max(0.05)).clamp(0.0, 1.0);
                        }
                        cands.push(u);
                    } else {
                        cands.push((0..d).map(|j| (mean[j] + rng.normal() * sd[j]).clamp(0.0, 1.0)).collect());
                    }
                }
            }
        }
        // drop repeats (after rounding the settings) and keep within the budget
        let mut work: Vec<(Vec<f64>, Vec<f64>)> = vec![];
        {
            let mut s = seen.lock().unwrap();
            for u in cands {
                let x: Vec<f64> = dims.iter().zip(&u).map(|(dm, u)| dm.value(*u)).collect();
                if s.insert(keyx(&x)) {
                    work.push((u, x));
                }
                if n_done + work.len() >= main_runs {
                    break;
                }
            }
        }
        if work.is_empty() {
            stale_rounds += 1;
            round += 1;
            if stale_rounds > 4 {
                break; // every candidate already tried: the ranges are exhausted
            }
            continue;
        }
        run_batch(&work);
        let best = trials.lock().unwrap().iter().map(|t| t.score).fold(f64::NEG_INFINITY, f64::max);
        if best_hist.last().map(|b| best > *b + 1e-9).unwrap_or(true) {
            stale_rounds = 0;
        } else {
            stale_rounds += 1;
        }
        best_hist.push(best);
        round += 1;
    }
    // pattern search around the best trial: each setting a step up and down (in parallel), move to
    // the best improvement, else halve the step; backtest scores move in steps, where this does
    // better than the smooth sampling above
    let mut step = 0.08f64;
    let mut polished = 0usize;
    while done.load(Ordering::SeqCst) < o.runs && !stop.load(Ordering::SeqCst) && step >= 0.004 {
        let best = {
            let g = trials.lock().unwrap();
            g.iter().filter(|t| t.score.is_finite()).max_by(|a, b| a.score.partial_cmp(&b.score).unwrap()).cloned()
        };
        let Some(best) = best else { break };
        let mut work = vec![];
        {
            let mut seen_g = seen.lock().unwrap();
            for j in 0..d {
                for dir in [-1.0, 1.0] {
                    let mut u = best.u.clone();
                    // a choice moves to its neighbour
                    let st = if dims[j].choices.is_some() { 1.0 / dims[j].choices.as_ref().unwrap().len() as f64 } else { step };
                    u[j] = (u[j] + dir * st).clamp(0.0, 1.0);
                    let x: Vec<f64> = dims.iter().zip(&u).map(|(dm, u)| dm.value(*u)).collect();
                    if seen_g.insert(keyx(&x)) && done.load(Ordering::SeqCst) + work.len() < o.runs {
                        work.push((u, x));
                    }
                }
            }
        }
        if work.is_empty() {
            step /= 2.0;
            continue;
        }
        polished += work.len();
        run_batch(&work);
        let now = trials.lock().unwrap().iter().map(|t| t.score).fold(f64::NEG_INFINITY, f64::max);
        if now <= best.score + 1e-9 {
            step /= 2.0;
        }
        best_hist.push(now);
        round += 1;
    }
    let ts = trials.into_inner().unwrap();
    let invalid = ts.iter().filter(|t| !t.score.is_finite()).count();
    let first_err = ts.iter().find_map(|t| t.row["error"].as_str().map(String::from));
    json!({"top": top_json(&ts), "runs": ts.len(), "invalid": invalid, "first_error": first_err, "rounds": round,
           "best_by_round": best_hist, "stopped": stop.load(Ordering::SeqCst), "polish_runs": polished,
           "dims": dims.iter().map(|d| json!({"key": d.key(), "node": d.node, "param": d.param, "min": d.lo, "max": d.hi, "log": d.log,
                                                "choices": d.choices})).collect::<Vec<_>>()})
}

// -- auto: which settings are worth tuning, and whether to change anything -------------------------

/// Settings auto mode may screen at most (every setting of a large strategy).
pub const MAX_SCREEN: usize = 96;
/// Settings it tunes at most: few, so the search can find them with a modest budget and the result
/// is less likely to fit noise.
pub const AUTO_PICK: usize = 6;

/// Every setting of the blocks `only` (all blocks when empty) with the range the dialog offers: half
/// to twice the current value within the block's limits, the whole range when that is empty; and
/// the candle length of every block that has candles (5 min, 15 min, 1 hour, 4 hours and the current).
pub fn candidates(s: &Map<String, Value>, only: &[String]) -> Vec<Value> {
    candidates_by(s, only, 2.0)
}

/// `candidates` over the current value ÷ f to × f (a setting at zero: ±5·f around it).
pub fn candidates_by(s: &Map<String, Value>, only: &[String], f: f64) -> Vec<Value> {
    let cat = crate::graph::catalog();
    let mut out = vec![];
    for n in s.get("nodes").and_then(|n| n.as_array()).into_iter().flatten() {
        let id = n["id"].as_str().unwrap_or("");
        if !only.is_empty() && !only.iter().any(|o| o == id) {
            continue;
        }
        let kind = n["kind"].as_str().unwrap_or("");
        let Some(entry) = cat.as_array().and_then(|a| a.iter().find(|k| k["kind"] == kind)) else { continue };
        for p in entry["params"].as_array().into_iter().flatten() {
            let name = p["name"].as_str().unwrap_or("");
            let (lo, hi) = (p["min"].as_f64().unwrap_or(f64::MIN), p["max"].as_f64().unwrap_or(f64::MAX));
            if !(lo < hi) {
                continue;
            }
            let v = n["p"][name].as_f64().or_else(|| p["default"].as_f64()).unwrap_or(lo);
            let (mut a, mut b) = if v > 0.0 { (v / f, v * f) } else if v < 0.0 { (v * f, v / f) } else { (lo.max(-5.0 * f), hi.min(5.0 * f)) };
            a = a.max(lo);
            b = b.min(hi);
            if !(a < b) {
                (a, b) = (lo, hi);
            }
            out.push(json!({"node": id, "param": name, "min": round_sig(a), "max": round_sig(b)}));
        }
        let candles = entry["stateful"] != true && !["range", "rebalance", "exit", "hold", "hedge", "const"].contains(&kind);
        if candles {
            let cur = n["tf"].as_f64().unwrap_or(5.0);
            let mut c = vec![5.0, 15.0, 60.0, 240.0, cur];
            c.sort_by(|a, b| a.partial_cmp(b).unwrap());
            c.dedup();
            out.push(json!({"node": id, "param": "tf", "choices": c}));
        }
    }
    out.truncate(MAX_SCREEN);
    out
}

/// The values a setting is screened at: each candle length, or four points spread over its range
/// (its ends and quarter points), none the current value.
fn probes(d: &Dim, cur: f64) -> Vec<f64> {
    let mut v: Vec<f64> = match &d.choices {
        Some(c) => c.clone(),
        None => [0.0, 0.25, 0.75, 1.0].iter().map(|u| d.value(*u)).collect(),
    };
    v.dedup();
    v.retain(|x| (x - cur).abs() > 1e-12 * cur.abs().max(1.0));
    v
}

/// Screening: each setting moved on its own over its range, the others at their current values,
/// every run scored like the search (`eval`). How far a setting moves the score is how much it
/// matters here. Returns the current settings' score and, per setting, what was found.
pub fn screen(base: &Map<String, Value>, dims: &[Dim], workers: usize, eval: &Eval, stop: &AtomicBool,
              progress: &(dyn Fn(usize, usize) + Sync)) -> (f64, Vec<Value>) {
    let x0 = current(base, dims);
    // (setting, value); the current settings first, as setting usize::MAX
    let mut work: Vec<(usize, f64)> = vec![(usize::MAX, 0.0)];
    for (i, d) in dims.iter().enumerate() {
        for v in probes(d, x0[i]) {
            work.push((i, v));
        }
    }
    let scores: Mutex<Vec<Option<f64>>> = Mutex::new(vec![None; work.len()]);
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..workers.max(1).min(work.len()) {
            sc.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::SeqCst);
                if k >= work.len() || stop.load(Ordering::SeqCst) {
                    break;
                }
                let (i, v) = work[k];
                let mut x = x0.clone();
                if i != usize::MAX {
                    x[i] = v;
                }
                let sc = eval(&apply(base, dims, &x)).ok().map(|r| r.0).filter(|s| s.is_finite());
                scores.lock().unwrap()[k] = sc;
                progress(done.fetch_add(1, Ordering::SeqCst) + 1, work.len());
            });
        }
    });
    let scores = scores.into_inner().unwrap();
    let base_score = scores[0].unwrap_or(f64::NAN);
    let mut out = vec![];
    for (i, d) in dims.iter().enumerate() {
        let mut pts: Vec<(f64, f64)> = vec![(x0[i], base_score)];
        for (k, (j, v)) in work.iter().enumerate() {
            if *j == i {
                if let Some(s) = scores[k] {
                    pts.push((*v, s));
                }
            }
        }
        pts.retain(|p| p.1.is_finite());
        pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let (mn, mx) = pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, p| (a.0.min(p.1), a.1.max(p.1)));
        let best = pts.iter().copied().fold((f64::NAN, f64::NEG_INFINITY), |a, p| if p.1 > a.1 { p } else { a });
        out.push(json!({"key": d.key(), "node": d.node, "param": d.param, "min": d.lo, "max": d.hi, "choices": d.choices, "current": x0[i],
                        "importance": if pts.len() > 1 { mx - mn } else { 0.0 }, "gain": if best.1.is_finite() { best.1 - base_score } else { 0.0 },
                        "best": best.0, "points": pts.iter().map(|p| json!([p.0, p.1])).collect::<Vec<_>>()}));
    }
    (base_score, out)
}

/// The settings worth tuning, by how much they moved the score in screening (`importance`, points
/// of %/yr): at most AUTO_PICK, each moving it by at least a point and by a quarter of the most any
/// did. A setting that hardly moves it only adds room for the search to fit noise.
pub fn pick(importance: &[f64]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..importance.len()).filter(|i| importance[*i].is_finite()).collect();
    idx.sort_by(|a, b| importance[*b].partial_cmp(&importance[*a]).unwrap());
    let top = idx.first().map(|i| importance[*i]).unwrap_or(0.0);
    let floor = (0.25 * top).max(1.0);
    idx.into_iter().take_while(|i| importance[*i] >= floor).take(AUTO_PICK).collect()
}

/// Whether to suggest the best row over the current settings, and why. It must be better where the
/// search could not have tuned it (the held-out quarter) and over all periods together, and most of
/// the top rows must agree: a lone winner among losers is luck. `obj` is "apr" or "vs_hold_apr".
pub fn suggest(current: &Value, top: &[Value], obj: &str) -> Value {
    suggest_strict(current, top, obj, false)
}

/// `suggest`; `strict` (a later attempt: the held-out quarter has been looked at before) also wants
/// the best row to beat the current settings in each half of the held-out quarter on its own.
pub fn suggest_strict(current: &Value, top: &[Value], obj: &str, strict: bool) -> Value {
    let f = |v: &Value| v.as_f64().filter(|x| x.is_finite());
    let (Some(best), Some(cur_all), Some(cur_ho)) = (top.first(), f(&current["all"]["score"]), f(&current["test"][obj])) else {
        return json!({"apply": null, "why": "nothing to compare: the run has no held-out check or no valid result"});
    };
    let (b_all, b_ho) = (f(&best["all"]["score"]).unwrap_or(f64::NEG_INFINITY), f(&best["test"][obj]).unwrap_or(f64::NEG_INFINITY));
    let n = top.len().min(5);
    let agree = top[..n].iter().filter(|t| f(&t["test"][obj]).unwrap_or(f64::NEG_INFINITY) > cur_ho).count();
    let (ok_all, ok_ho, ok_agree) = (b_all > cur_all, b_ho > cur_ho, agree * 2 > n);
    let halves = |t: &Value| -> Option<(f64, f64)> { Some((f(&t["halves"][0])?, f(&t["halves"][1])?)) };
    let ok_halves = !strict || matches!((halves(best), halves(current)), (Some(b), Some(c)) if b.0 > c.0 && b.1 > c.1);
    let why = if ok_all && ok_ho && ok_agree && ok_halves {
        format!("better over all periods ({b_all:+.1} vs {cur_all:+.1} %/yr reliable) and on the held-out quarter ({b_ho:+.1} vs {cur_ho:+.1}{}), and {agree} of the top {n} beat the current settings there too",
                if strict { ", in each half of it" } else { "" })
    } else if !ok_ho {
        format!("keep the current settings: the best ones did worse on the held-out quarter ({b_ho:+.1} vs {cur_ho:+.1} %/yr), so they fit the searched period, not what came after")
    } else if !ok_all {
        format!("keep the current settings: over all periods the best ones are no more reliable ({b_all:+.1} vs {cur_all:+.1} %/yr)")
    } else if !ok_agree {
        format!("keep the current settings: only {agree} of the top {n} beat them on the held-out quarter; the best one is likely luck")
    } else {
        let (b, c) = (halves(best).unwrap_or((f64::NAN, f64::NAN)), halves(current).unwrap_or((f64::NAN, f64::NAN)));
        format!("keep the current settings: better on the held-out quarter as a whole, not in each half of it ({:+.1} and {:+.1} vs {:+.1} and {:+.1} %/yr); after earlier attempts looked at that quarter, winning one half is not enough", b.0, b.1, c.0, c.1)
    };
    let ok = ok_all && ok_ho && ok_agree && ok_halves;
    json!({"apply": if ok { json!(0) } else { Value::Null }, "why": why, "agree": agree, "of": n})
}

// -- auto, further attempts: when the first finds nothing that holds up --------------------------

/// Settings the search could move without changing anything: among the top rows whose folds came
/// out exactly alike (at least three), the ones whose values still spread over a sixth of their
/// range or differ in choice. Tuning them only spent runs.
pub fn flat_keys(top: &[Value], dims: &[Dim]) -> Vec<String> {
    let folds = |t: &Value| -> Vec<f64> { t["wf"]["folds"].as_array().map(|a| a.iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect()).unwrap_or_default() };
    let Some(f0) = top.first().map(folds) else { return vec![] };
    let alike: Vec<&Value> = top.iter().filter(|t| {
        let f = folds(t);
        !f.is_empty() && f.len() == f0.len() && f.iter().zip(&f0).all(|(a, b)| (a - b).abs() < 1e-9)
    }).collect();
    if alike.len() < 3 {
        return vec![];
    }
    dims.iter().filter(|d| {
        let us: Vec<f64> = alike.iter().filter_map(|t| t["values"][d.key().as_str()].as_f64()).map(|v| d.unit(v)).collect();
        let (mn, mx) = us.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, u| (a.0.min(*u), a.1.max(*u)));
        us.len() >= 2 && mx - mn >= 0.16
    }).map(|d| d.key()).collect()
}

/// Settings whose best value sat at an end of the range they were tried over, with a gain: the
/// optimum may lie beyond it. From a screening report (`screen`) or the best row of a search.
pub fn edge_keys(rep: &[Value], best: Option<&Value>, dims: &[Dim]) -> Vec<String> {
    let mut out: Vec<String> = rep.iter().filter(|r| {
        let (b, lo, hi) = (r["best"].as_f64(), r["min"].as_f64(), r["max"].as_f64());
        r["choices"].is_null() && r["gain"].as_f64().unwrap_or(0.0) > 0.0
            && matches!((b, lo, hi), (Some(b), Some(lo), Some(hi)) if (b - lo).abs() <= 1e-9 * lo.abs().max(1.0) || (b - hi).abs() <= 1e-9 * hi.abs().max(1.0))
    }).filter_map(|r| r["key"].as_str().map(String::from)).collect();
    if let Some(t) = best {
        for d in dims.iter().filter(|d| d.choices.is_none()) {
            if let Some(v) = t["values"][d.key().as_str()].as_f64() {
                let u = d.unit(v);
                if (u <= 0.02 || u >= 0.98) && !out.contains(&d.key()) {
                    out.push(d.key());
                }
            }
        }
    }
    out
}

/// Kinds that can stand in for each other: same inputs and outputs, the same settings.
const SWAPS: &[&[&str]] = &[&["sma", "ema", "median"], &["bollinger", "keltner"], &["and", "or"], &["highest", "percentile"], &["lowest", "percentile"]];

/// Changes to the strategy itself, one at a time, each a description and the changed strategy:
/// without an exit or a hold (never exit, never freeze), a sink's optional input unwired (its
/// default: rebalance when the price leaves the range, re-enter when the exit condition clears,
/// wait in whatever the exit leaves), and a block as a kind that can stand in for it (SMA, EMA or
/// median; Bollinger or Keltner bands; and or or). Only blocks of `only` (all when empty). A
/// change the graph refuses is simply not offered.
pub fn variants(s: &Map<String, Value>, only: &[String]) -> Vec<(Value, Map<String, Value>)> {
    let Some(nodes) = s.get("nodes").and_then(|n| n.as_array()) else { return vec![] };
    let mut out = vec![];
    let with = |f: &dyn Fn(&mut Vec<Value>)| -> Map<String, Value> {
        let mut t = s.clone();
        if let Some(a) = t.get_mut("nodes").and_then(|n| n.as_array_mut()) {
            f(a);
        }
        t
    };
    for n in nodes {
        let id = n["id"].as_str().unwrap_or("").to_string();
        if !only.is_empty() && !only.contains(&id) {
            continue;
        }
        let kind = n["kind"].as_str().unwrap_or("");
        if ["exit", "hold"].contains(&kind) {
            let id2 = id.clone();
            out.push((json!({"op": "remove", "node": id, "kind": kind}), with(&move |a| a.retain(|x| x["id"] != id2.as_str()))));
        }
        for (k, port) in [("rebalance", "when"), ("exit", "reenter"), ("exit", "to")] {
            if kind == k && !n["in"][port].is_null() {
                let id2 = id.clone();
                out.push((json!({"op": "unwire", "node": id, "kind": kind, "port": port}), with(&move |a| {
                    if let Some(x) = a.iter_mut().find(|x| x["id"] == id2.as_str()) {
                        if let Some(i) = x["in"].as_object_mut() {
                            i.remove(port);
                        }
                    }
                })));
            }
        }
        for group in SWAPS.iter().filter(|g| g.contains(&kind)) {
            for to in group.iter().filter(|k| **k != kind) {
                // percentile stands in for highest (q 1) and lowest (q 0), not the other way round
                if kind == "percentile" {
                    continue;
                }
                let (id2, to2, from) = (id.clone(), to.to_string(), kind.to_string());
                out.push((json!({"op": "kind", "node": id, "kind": kind, "to": to}), with(&move |a| {
                    if let Some(x) = a.iter_mut().find(|x| x["id"] == id2.as_str()) {
                        x["kind"] = json!(to2);
                        if to2 == "keltner" {
                            // Keltner bands read the bars' highs and lows: no input
                            x.as_object_mut().map(|o| o.remove("in"));
                        }
                        if from == "keltner" {
                            x["in"] = json!({"in": ["__price", "tick"]});
                        }
                        if to2 == "percentile" {
                            x["p"]["q"] = json!(if from == "highest" { 0.95 } else { 0.05 });
                        }
                    }
                    if from == "keltner" && !a.iter().any(|x| x["id"] == "__price") {
                        a.push(json!({"id": "__price", "kind": "price"}));
                    }
                })));
            }
        }
    }
    // the graph must still build
    out.retain(|(_, t)| crate::graph::Graph::parse(t, 10).is_ok());
    out
}

/// Rows whose folds and held-out results are exactly alike are one strategy as far as the period
/// can tell: the first is kept, with how many it stands for (`alike`).
pub fn dedupe(rows: Vec<Value>) -> Vec<Value> {
    let sig = |t: &Value| format!("{}|{}|{}", t["wf"]["folds"], t["test"]["apr"], t["actions"]);
    let mut out: Vec<Value> = vec![];
    for r in rows {
        if let Some(k) = out.iter_mut().find(|o| sig(o) == sig(&r)) {
            k["alike"] = json!(k["alike"].as_u64().unwrap_or(1) + 1);
        } else {
            out.push(r);
        }
    }
    out
}

// -- scoring across periods -----------------------------------------------------------------------

/// How a setting scores over walk-forward folds (consecutive slices of the period, each run on its
/// own): a return it can be relied on for, not its best stretch. The score is the one-sided 95%
/// lower bound of the mean annualised return across folds, mean − 1.64·sd/√n: steady beats lucky,
/// and one bad fold costs more than a good one earns. Any fold that failed makes it invalid (NaN).
/// Returns the score and its parts (mean, sd, worst, positive folds, each fold).
pub fn composite(v: &[f64]) -> (f64, Value) {
    let n = v.len();
    if n == 0 || v.iter().any(|x| !x.is_finite()) {
        return (f64::NAN, json!({"folds": v.iter().map(|x| if x.is_finite() { json!(x) } else { Value::Null }).collect::<Vec<_>>()}));
    }
    let mean = v.iter().sum::<f64>() / n as f64;
    let sd = if n > 1 { (v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt() } else { 0.0 };
    let score = mean - 1.64 * sd / (n as f64).sqrt();
    let worst = v.iter().copied().fold(f64::INFINITY, f64::min);
    let positive = v.iter().filter(|x| **x > 0.0).count();
    (score, json!({"score": score, "mean": mean, "sd": sd, "worst": worst, "positive": positive, "n": n, "folds": v}))
}

// -- stopping a running optimization ---------------------------------------------------------------

static STOPS: Mutex<Option<std::collections::HashMap<u64, std::sync::Arc<AtomicBool>>>> = Mutex::new(None);

/// The stop flag of job `id` (made on first use).
pub fn flag(id: u64) -> std::sync::Arc<AtomicBool> {
    STOPS.lock().unwrap().get_or_insert_with(Default::default).entry(id).or_default().clone()
}

pub fn stop(id: u64) -> bool {
    let g = STOPS.lock().unwrap();
    match g.as_ref().and_then(|m| m.get(&id)) {
        Some(f) => {
            f.store(true, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

pub fn forget(id: u64) {
    if let Some(m) = STOPS.lock().unwrap().as_mut() {
        m.remove(&id);
    }
}

#[cfg(test)]
mod auto_tests {
    use super::*;

    fn strat() -> Map<String, Value> {
        json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"},
            {"id": "bb", "kind": "bollinger", "p": {"hours": 24, "k": 2}, "in": {"in": ["p", "tick"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["bb", "lower"], "upper": ["bb", "upper"]}},
            {"id": "m", "kind": "rebalance", "p": {"delay_min": 0, "cooldown_min": 30}}
        ]}).as_object().unwrap().clone()
    }

    #[test]
    fn candidates_cover_every_setting_with_sane_ranges() {
        let c = candidates(&strat(), &[]);
        let keys: Vec<String> = c.iter().map(|x| format!("{}.{}", x["node"].as_str().unwrap(), x["param"].as_str().unwrap())).collect();
        for k in ["bb.hours", "bb.k", "bb.tf", "m.cooldown_min", "m.delay_min"] {
            assert!(keys.contains(&k.to_string()), "{k} in {keys:?}");
        }
        assert!(!keys.iter().any(|k| k == "r.tf" || k == "m.tf"), "no candles on decisions: {keys:?}");
        let h = c.iter().find(|x| x["node"] == "bb" && x["param"] == "hours").unwrap();
        assert_eq!((h["min"].as_f64().unwrap(), h["max"].as_f64().unwrap()), (12.0, 48.0));
        // a setting at zero gets a range around zero within its limits
        let d = c.iter().find(|x| x["param"] == "delay_min").unwrap();
        assert!(d["min"].as_f64().unwrap() < d["max"].as_f64().unwrap());
        // only the blocks asked for; and every candidate is a valid dimension
        assert!(candidates(&strat(), &["m".into()]).iter().all(|x| x["node"] == "m"));
        assert!(dims_upto(&strat(), &Value::Array(c.clone()), MAX_SCREEN).is_ok());
    }

    #[test]
    fn screening_finds_the_settings_that_move_the_score() {
        let s = strat();
        let ds = dims_upto(&s, &Value::Array(candidates(&s, &[])), MAX_SCREEN).unwrap();
        // a made-up score: hours matter a lot (best near 40), k a little, the rest not at all
        let eval = |x: &Map<String, Value>| -> Result<(f64, Value), String> {
            let n = |id: &str, p: &str| x["nodes"].as_array().unwrap().iter().find(|n| n["id"] == id).unwrap()["p"][p].as_f64().unwrap_or(0.0);
            Ok((10.0 - (n("bb", "hours") - 40.0).abs() + 0.5 * n("bb", "k"), json!({})))
        };
        let (base, rep) = screen(&s, &ds, 3, &eval, &AtomicBool::new(false), &|_, _| {});
        assert_eq!(base, 10.0 - 16.0 + 1.0);
        let imp = |k: &str| rep.iter().find(|r| r["key"] == k).unwrap()["importance"].as_f64().unwrap();
        assert!(imp("bb.hours") > 15.0, "{}", imp("bb.hours"));
        assert_eq!(imp("m.cooldown_min"), 0.0);
        let best = rep.iter().find(|r| r["key"] == "bb.hours").unwrap()["best"].as_f64().unwrap();
        assert!((best - 12.0 * 4f64.powf(0.75)).abs() < 0.01, "the probe nearest the peak at 40: {best}");
        let picked: Vec<String> = pick(&rep.iter().map(|r| r["importance"].as_f64().unwrap()).collect::<Vec<_>>()).iter().map(|i| rep[*i]["key"].as_str().unwrap().to_string()).collect();
        assert_eq!(picked, vec!["bb.hours".to_string()], "k moves it by 0.5-2 points, under a quarter of hours'");
    }

    #[test]
    fn pick_keeps_few_that_matter() {
        assert!(pick(&[0.2, 0.5, 0.9]).is_empty(), "nothing moves it by a point");
        assert_eq!(pick(&[10.0, 3.0, 2.0, f64::NAN]), vec![0, 1]);
        assert_eq!(pick(&[5.0; 10]).len(), AUTO_PICK);
    }

    fn row(all: f64, ho: f64) -> Value {
        json!({"all": {"score": all}, "test": {"apr": ho}})
    }

    #[test]
    fn settings_that_changed_nothing_are_found() {
        let s = strat();
        let ds = dims(&s, &json!([{"node": "bb", "param": "hours", "min": 12, "max": 48}, {"node": "bb", "param": "k", "min": 1, "max": 4}])).unwrap();
        // three rows alike in every fold, k spread over its range: k changed nothing; hours did not move
        let r = |k: f64, f: [f64; 2]| json!({"values": {"bb.hours": 24.0, "bb.k": k}, "wf": {"folds": f}});
        let top = [r(1.2, [5.0, 6.0]), r(2.5, [5.0, 6.0]), r(3.8, [5.0, 6.0]), r(2.0, [4.0, 6.0])];
        assert_eq!(flat_keys(&top, &ds), vec!["bb.k".to_string()]);
        // two alike are not enough to tell
        assert!(flat_keys(&top[1..], &ds).is_empty());
        // and identical rows collapse into one
        let d = dedupe(top.to_vec());
        assert_eq!(d.len(), 2);
        assert_eq!(d[0]["alike"], 3);
    }

    #[test]
    fn a_best_value_on_the_edge_is_noticed() {
        let s = strat();
        let ds = dims(&s, &json!([{"node": "bb", "param": "hours", "min": 12, "max": 48}, {"node": "bb", "param": "k", "min": 1, "max": 4}])).unwrap();
        let rep = [json!({"key": "bb.hours", "min": 12.0, "max": 48.0, "best": 12.0, "gain": 3.0, "choices": null}),
                   json!({"key": "bb.k", "min": 1.0, "max": 4.0, "best": 2.5, "gain": 3.0, "choices": null}),
                   json!({"key": "m.cooldown_min", "min": 15.0, "max": 60.0, "best": 60.0, "gain": 0.0, "choices": null})];
        assert_eq!(edge_keys(&rep, None, &ds), vec!["bb.hours".to_string()], "on the edge with a gain only");
        let best = json!({"values": {"bb.hours": 30.0, "bb.k": 3.99}});
        assert_eq!(edge_keys(&rep, Some(&best), &ds), vec!["bb.hours".to_string(), "bb.k".to_string()]);
        // wider ranges reach further
        let w = candidates_by(&s, &["bb".into()], 4.0);
        let h = w.iter().find(|x| x["param"] == "hours").unwrap();
        assert_eq!((h["min"].as_f64().unwrap(), h["max"].as_f64().unwrap()), (6.0, 96.0));
    }

    #[test]
    fn variants_change_one_thing_and_still_build() {
        let s = json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"},
            {"id": "s", "kind": "sma", "p": {"hours": 24}, "in": {"in": ["p", "tick"]}},
            {"id": "bb", "kind": "bollinger", "p": {"hours": 24, "k": 2}, "in": {"in": ["s", "out"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["bb", "lower"], "upper": ["bb", "upper"]}},
            {"id": "o", "kind": "out_of_range"},
            {"id": "m", "kind": "rebalance", "p": {"delay_min": 0, "cooldown_min": 30}, "in": {"when": ["o", "out"]}},
            {"id": "f", "kind": "fee_vs_arb", "p": {"hours": 168}},
            {"id": "one", "kind": "const", "p": {"value": 1}},
            {"id": "lo", "kind": "lt", "in": {"a": ["f", "out"], "b": ["one", "value"]}},
            {"id": "x", "kind": "exit", "in": {"when": ["lo", "out"]}}]}).as_object().unwrap().clone();
        let v = variants(&s, &[]);
        let ops: Vec<String> = v.iter().map(|(d, _)| format!("{} {} {}", d["op"].as_str().unwrap(), d["node"].as_str().unwrap(), d["to"].as_str().or(d["port"].as_str()).unwrap_or(""))).collect();
        for want in ["remove x ", "unwire m when", "kind s ema", "kind s median", "kind bb keltner"] {
            assert!(ops.iter().any(|o| o == want), "{want} in {ops:?}");
        }
        // each changes one thing: the exit gone, the rest as it was
        let (_, no_exit) = v.iter().find(|(d, _)| d["op"] == "remove").unwrap();
        assert_eq!(no_exit["nodes"].as_array().unwrap().len(), 9);
        // Keltner reads the bars: its input is gone
        let (_, kel) = v.iter().find(|(d, _)| d["to"] == "keltner").unwrap();
        assert!(kel["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "bb").unwrap().get("in").is_none());
        // only the blocks asked for
        assert!(variants(&s, &["s".into()]).iter().all(|(d, _)| d["node"] == "s"));
    }

    #[test]
    fn a_later_attempt_must_win_both_halves() {
        let h = |all: f64, ho: f64, a: f64, b: f64| json!({"all": {"score": all}, "test": {"apr": ho}, "halves": [a, b]});
        let cur = h(-10.0, 20.0, 10.0, 30.0);
        // better as a whole, carried by one half
        let top = [h(5.0, 40.0, 75.0, 5.0), h(4.0, 35.0, 60.0, 10.0), h(3.0, 30.0, 50.0, 10.0)];
        assert_eq!(suggest_strict(&cur, &top, "apr", false)["apply"], 0, "the first attempt does not ask for halves");
        let s = suggest_strict(&cur, &top, "apr", true);
        assert!(s["apply"].is_null() && s["why"].as_str().unwrap().contains("each half"), "{s}");
        let even = [h(5.0, 40.0, 35.0, 45.0), h(4.0, 35.0, 30.0, 40.0), h(3.0, 30.0, 20.0, 40.0)];
        assert_eq!(suggest_strict(&cur, &even, "apr", true)["apply"], 0);
    }

    #[test]
    fn suggest_only_what_holds_up_where_it_was_not_tuned() {
        let cur = row(-11.7, 50.5);
        // the run you saw: better in the search, worse held out
        let s = suggest(&cur, &[row(2.6, -43.1)], "apr");
        assert!(s["apply"].is_null() && s["why"].as_str().unwrap().contains("held-out"), "{s}");
        // better everywhere, and the runners-up agree
        let top = [row(8.0, 60.0), row(7.0, 55.0), row(6.0, 52.0), row(5.0, 40.0)];
        assert_eq!(suggest(&cur, &top, "apr")["apply"], 0);
        // better, but alone
        let lone = [row(8.0, 60.0), row(7.0, 10.0), row(6.0, 12.0), row(5.0, 40.0)];
        assert!(suggest(&cur, &lone, "apr")["apply"].is_null());
        // better held out, not over all periods
        assert!(suggest(&cur, &[row(-20.0, 70.0)], "apr")["apply"].is_null());
        // no held-out check: nothing to go on
        assert!(suggest(&json!({}), &top, "apr")["apply"].is_null());
    }
}

#[cfg(test)]
mod composite_tests {
    use super::*;

    #[test]
    fn steady_beats_lucky() {
        let (steady, _) = composite(&[10.0, 11.0, 9.0, 10.0]);
        let (lucky, s) = composite(&[70.0, -10.0, -12.0, 2.0]);
        assert!(s["mean"].as_f64().unwrap() > 10.0, "the lucky one earns more on average");
        assert!(steady > lucky, "{steady} vs {lucky}");
        assert_eq!(s["positive"], 2);
        assert_eq!(s["worst"], -12.0);
    }

    #[test]
    fn the_score_is_the_mean_less_its_uncertainty() {
        let (sc, s) = composite(&[4.0, 8.0]);
        let sd = (8.0f64).sqrt(); // sample sd of 4 and 8
        assert!((s["sd"].as_f64().unwrap() - sd).abs() < 1e-9);
        assert!((sc - (6.0 - 1.64 * sd / 2f64.sqrt())).abs() < 1e-9);
        // one fold: no spread to measure, the score is its return
        assert_eq!(composite(&[7.0]).0, 7.0);
        // more folds that agree: closer to the mean
        assert!(composite(&[4.0, 8.0, 4.0, 8.0, 4.0, 8.0]).0 > sc);
    }

    #[test]
    fn a_failed_fold_makes_it_invalid() {
        assert!(composite(&[5.0, f64::NAN, 6.0]).0.is_nan());
        assert!(composite(&[]).0.is_nan());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strat() -> Map<String, Value> {
        json!({"type": "graph", "nodes": [
            {"id": "p", "kind": "price"},
            {"id": "s", "kind": "sma", "p": {"hours": 24}, "in": {"in": ["p", "tick"]}},
            {"id": "w", "kind": "const", "p": {"value": 100}},
            {"id": "ar", "kind": "around", "in": {"center": ["s", "out"], "width": ["w", "value"]}},
            {"id": "r", "kind": "range", "in": {"lower": ["ar", "lower"], "upper": ["ar", "upper"]}}]}).as_object().unwrap().clone()
    }

    #[test]
    fn finds_the_peak_of_a_smooth_score() {
        let s = strat();
        let spec = json!([{"node": "s", "param": "hours", "min": 1, "max": 168}, {"node": "w", "param": "value", "min": 10, "max": 1000},
                          {"node": "s", "param": "tf", "choices": [5, 60, 240]}]);
        let dims = dims(&s, &spec).unwrap();
        assert!(dims[0].log && dims[1].log && dims[2].choices.is_some());
        // the score peaks at hours 40, width 300, hourly candles
        let eval = |st: &Map<String, Value>| -> Result<(f64, Value), String> {
            let n = |id: &str| st["nodes"].as_array().unwrap().iter().find(|n| n["id"] == id).unwrap().clone();
            let (h, w, tf) = (n("s")["p"]["hours"].as_f64().unwrap(), n("w")["p"]["value"].as_f64().unwrap(), n("s")["tf"].as_f64().unwrap_or(5.0));
            let sc = -(h / 40.0).ln().powi(2) - (w / 300.0).ln().powi(2) - if tf == 60.0 { 0.0 } else { 1.0 };
            Ok((sc, json!({"score": sc})))
        };
        let stop = AtomicBool::new(false);
        let r = search(&s, &dims, &Opts { runs: 150, workers: 2 }, &eval, &stop, &|_, _| {});
        let best = &r["top"][0]["values"];
        assert!((best["s.hours"].as_f64().unwrap() / 40.0).ln().abs() < 0.25, "{best}");
        assert!((best["w.value"].as_f64().unwrap() / 300.0).ln().abs() < 0.25, "{best}");
        assert_eq!(best["s.tf"].as_f64().unwrap(), 60.0);
        assert!(r["runs"].as_u64().unwrap() <= 150);
        // applying the best writes the block settings (5-minute candles leave no "tf")
        let a = apply(&s, &dims, &[40.0, 300.0, 5.0]);
        let sn = a["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "s").unwrap();
        assert_eq!(sn["p"]["hours"], json!(40.0));
        assert!(sn.get("tf").is_none());
    }

    #[test]
    fn refuses_ranges_outside_the_block_and_unknown_settings() {
        let s = strat();
        assert!(dims(&s, &json!([{"node": "s", "param": "hours", "min": 9000, "max": 10000}])).unwrap_err().contains("range is empty"));
        assert!(dims(&s, &json!([{"node": "s", "param": "nope", "min": 1, "max": 2}])).unwrap_err().contains("no setting"));
        assert!(dims(&s, &json!([{"node": "x", "param": "hours", "min": 1, "max": 2}])).unwrap_err().contains("no block"));
        assert!(dims(&s, &json!([])).is_err());
    }

    #[test]
    fn a_stop_ends_the_search_early() {
        let s = strat();
        let dims = dims(&s, &json!([{"node": "s", "param": "hours", "min": 1, "max": 168}])).unwrap();
        let stop = AtomicBool::new(false);
        let eval = |_: &Map<String, Value>| -> Result<(f64, Value), String> { Ok((1.0, json!({}))) };
        let r = search(&s, &dims, &Opts { runs: 300, workers: 2 }, &eval, &stop, &|n, _| if n >= 20 { stop.store(true, Ordering::SeqCst) });
        assert!(r["runs"].as_u64().unwrap() < 40 && r["stopped"] == true);
    }
}
