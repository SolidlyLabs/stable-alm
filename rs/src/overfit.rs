//! Overfit check for the Strategy lab: how much of a backtest's result is the strategy, and how much
//! is settings that happen to fit this stretch of history.
//!
//! The strategy is run again as a neighbourhood of variants (every setting scaled at random between
//! half and double; variant 0 is the strategy itself), the period is cut into equal time slices, and
//! each run's return over holding is measured per slice. From that matrix:
//! - walk-forward efficiency: pick the best variant on the slices so far, measure it on the next
//!   slice; out-of-sample return over in-sample return (1 = it held up, 0 or less = it did not);
//! - PBO, the probability of backtest overfitting (Bailey, Borwein, López de Prado and Zhu): over
//!   every split of the slices into two halves, how often the best variant of one half ranks in the
//!   bottom half of the other;
//! - stability: the share of neighbours that keep at least half of the strategy's edge;
//! - consistency: the share of slices in which the strategy beat holding.
//! The score (0–100) weighs them: 35% (1 − PBO), 25% efficiency, 25% stability, 15% consistency.

use serde_json::{json, Map, Value};

/// A small deterministic generator (xorshift64*): the same strategy gets the same variants.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
    /// A factor between 1/2 and 2, even on a log scale.
    fn factor(&mut self) -> f64 {
        (std::f64::consts::LN_2 * (2.0 * self.next() - 1.0)).exp()
    }
}

fn seed_of(v: &Value) -> u64 {
    // FNV-1a of the strategy's JSON
    serde_json::to_string(v).unwrap_or_default().bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

/// `n` variants of the strategy (variant 0 is the strategy itself), or just it when it has no
/// settings to vary.
pub fn variants(s: &Map<String, Value>, n: usize) -> Vec<Map<String, Value>> {
    let mut rng = Rng::new(seed_of(&Value::Object(s.clone())));
    let mut out = vec![s.clone()];
    let ty = s.get("type").and_then(|t| t.as_str()).unwrap_or("custom");
    for _ in 1..n {
        let mut v = s.clone();
        let mut changed = false;
        // a setting scaled by 1/2 to 2 (zero stays zero: it switches something off)
        fn scale(o: &mut Map<String, Value>, k: &str, lo: f64, hi: f64, round: bool, rng: &mut Rng) -> bool {
            let Some(x) = o.get(k).and_then(|x| x.as_f64()).filter(|x| *x != 0.0) else { return false };
            let mut y = (x * rng.factor()).clamp(lo, hi);
            if round {
                y = y.round().max(lo.max(1.0));
            }
            o.insert(k.into(), json!(y));
            true
        }
        match ty {
            "custom" => {
                for k in ["width", "buffer_lo", "buffer_hi", "delay_min", "cooldown_min", "offset_up", "offset_down", "exit_delay_min", "guard"] {
                    changed |= scale(&mut v, k, 0.0, 1e6, false, &mut rng);
                }
                if let Some(sk) = v.get("skew").and_then(|x| x.as_f64()) {
                    v.insert("skew".into(), json!((sk + 0.5 * (rng.next() - 0.5)).clamp(0.0, 1.0)));
                    changed = true;
                }
            }
            "graph" => {
                if let Some(nodes) = v.get_mut("nodes").and_then(|n| n.as_array_mut()) {
                    for node in nodes.iter_mut() {
                        let kind = node["kind"].as_str().unwrap_or("").to_string();
                        let Some(spec) = crate::graph::KINDS.iter().find(|k| k.0 == kind) else { continue };
                        let Some(p) = node.get_mut("p").and_then(|p| p.as_object_mut()) else { continue };
                        for (name, _, lo, hi) in spec.3 {
                            if *name == "q" || *name == "below" && kind == "around" {
                                // shares: moved, not scaled
                                if let Some(x) = p.get(*name).and_then(|x| x.as_f64()) {
                                    p.insert(name.to_string(), json!((x + 0.3 * (rng.next() - 0.5)).clamp(*lo, *hi)));
                                    changed = true;
                                }
                                continue;
                            }
                            changed |= scale(p, name, *lo, *hi, false, &mut rng);
                        }
                    }
                }
            }
            _ => {}
        }
        if !changed {
            break; // nothing to vary
        }
        out.push(v);
    }
    out
}

/// The run's return over holding in each of `n` equal time slices of [t0, t1], as % a year of
/// `capital`. `series` is [[ts, value, hold value], …] in time order.
pub fn slice_excess(series: &[Value], t0: i64, t1: i64, n: usize, capital: f64) -> Vec<f64> {
    let pts: Vec<(i64, f64, f64)> = series.iter().filter_map(|p| Some((p[0].as_i64()?, p[1].as_f64()?, p[2].as_f64()?))).collect();
    // a slice ends with the last value at or before its end and starts from where the one before
    // ended; at the very start of the run, the first value at that instant, before any trade, so
    // the first slice carries the entry costs
    let at = |t: i64, end: bool| -> (f64, f64) {
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
        p.map(|p| (p.1, p.2)).unwrap_or((capital, capital))
    };
    let len = (t1 - t0) as f64 / n as f64;
    (0..n)
        .map(|k| {
            let (a, b) = (t0 + (len * k as f64) as i64, t0 + (len * (k + 1) as f64) as i64);
            let ((va, ha), (vb, hb)) = (at(a, false), at(b, true));
            ((vb - va) - (hb - ha)) / capital * (365.0 * 86400.0 / (b - a).max(1) as f64) * 100.0
        })
        .collect()
}

fn mean(x: &[f64]) -> f64 {
    if x.is_empty() { 0.0 } else { x.iter().sum::<f64>() / x.len() as f64 }
}

fn combos(n: usize, k: usize) -> Vec<Vec<usize>> {
    let mut out = vec![];
    let mut cur = vec![];
    fn go(start: usize, n: usize, k: usize, cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if cur.len() == k {
            out.push(cur.clone());
            return;
        }
        for i in start..n {
            cur.push(i);
            go(i + 1, n, k, cur, out);
            cur.pop();
        }
    }
    go(0, n, k, &mut cur, &mut out);
    out
}

/// Statistics of a variants × slices matrix of returns (row 0 is the strategy itself).
pub fn stats(r: &[Vec<f64>]) -> Value {
    let m = r.len();
    let s = r.first().map(|x| x.len()).unwrap_or(0);
    if m == 0 || s < 2 {
        return json!({"error": "not enough data"});
    }
    let full: Vec<f64> = r.iter().map(|x| mean(x)).collect();
    let cur = full[0];
    let consistency = r[0].iter().filter(|x| **x > 0.0).count() as f64 / s as f64;
    let rank = if m > 1 { full[1..].iter().filter(|x| **x < cur).count() as f64 / (m - 1) as f64 } else { f64::NAN };
    let stability = if m < 2 {
        f64::NAN
    } else if cur > 0.0 {
        full[1..].iter().filter(|x| **x >= 0.5 * cur).count() as f64 / (m - 1) as f64
    } else {
        0.0
    };
    // anchored walk-forward: in-sample = slices before k (at least two), out-of-sample = slice k
    let mut steps = vec![];
    let (mut is_sum, mut oos_sum) = (0.0, 0.0);
    for k in 2..s {
        let is_of = |row: &Vec<f64>| mean(&row[..k]);
        let sel = (0..m).max_by(|a, b| is_of(&r[*a]).partial_cmp(&is_of(&r[*b])).unwrap_or(std::cmp::Ordering::Equal)).unwrap();
        let (is, oos) = (is_of(&r[sel]), r[sel][k]);
        is_sum += is;
        oos_sum += oos;
        steps.push(json!({"slice": k, "picked": sel, "in_sample": is, "out_of_sample": oos, "strategy_out_of_sample": r[0][k]}));
    }
    let wfe = if is_sum > 0.0 { Some(oos_sum / is_sum) } else { None };
    // PBO by combinatorially symmetric cross-validation over halves of the slices
    let pbo = if m >= 3 && s >= 4 {
        let half = s / 2;
        let cs = combos(s, half);
        let mut below = 0usize;
        for is in &cs {
            let oos: Vec<usize> = (0..s).filter(|i| !is.contains(i)).collect();
            let perf = |row: &Vec<f64>, idx: &[usize]| mean(&idx.iter().map(|i| row[*i]).collect::<Vec<_>>());
            let best = (0..m).max_by(|a, b| perf(&r[*a], is).partial_cmp(&perf(&r[*b], is)).unwrap_or(std::cmp::Ordering::Equal)).unwrap();
            let b_oos = perf(&r[best], &oos);
            // relative rank of the in-sample winner out of sample, in (0, 1); logit < 0 is the bottom
            // half. Runs within a quarter point a year (or 5%) of it tie and share the middle rank:
            // neighbours that all earn the same are not a sign of overfitting.
            let tol = (0.05 * b_oos.abs()).max(0.25);
            let o: Vec<f64> = (0..m).map(|j| perf(&r[j], &oos)).collect();
            let less = o.iter().filter(|x| **x < b_oos - tol).count() as f64;
            let tied = o.iter().filter(|x| (**x - b_oos).abs() <= tol).count() as f64; // includes the winner
            let w = (less + 1.0 + (tied - 1.0) / 2.0) / (m as f64 + 1.0);
            if (w / (1.0 - w)).ln() < -1e-9 {
                below += 1;
            }
        }
        Some(below as f64 / cs.len() as f64)
    } else {
        None
    };
    let part = |x: Option<f64>| x.filter(|v| v.is_finite()).unwrap_or(0.0);
    let score = if m < 2 {
        None
    } else {
        Some(
            (100.0 * (0.35 * (1.0 - part(pbo)) + 0.25 * part(wfe).clamp(0.0, 1.0) + 0.25 * part(Some(stability)) + 0.15 * consistency))
                .round()
                .clamp(0.0, 100.0),
        )
    };
    let verdict = match score {
        None => "no settings to vary",
        Some(x) if x >= 70.0 => "robust",
        Some(x) if x >= 45.0 => "some overfit risk",
        _ => "likely overfit",
    };
    let mut f = full[1..].to_vec();
    f.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if f.is_empty() { Value::Null } else { json!(f[f.len() / 2]) };
    json!({
        "score": score, "verdict": verdict, "variants": m, "slices": s,
        "pbo": pbo, "wfe": wfe, "stability": if stability.is_finite() { json!(stability) } else { Value::Null },
        "consistency": consistency, "rank": if rank.is_finite() { json!(rank) } else { Value::Null },
        "strategy_slices": r[0], "neighbours_median": median,
        "strategy": cur, "walk_forward": steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_vary_every_setting_within_bounds() {
        let s = json!({"type": "custom", "width": 10, "buffer_lo": 2, "buffer_hi": 2, "delay_min": 15, "cooldown_min": 60, "skew": 0.5});
        let v = variants(s.as_object().unwrap(), 12);
        assert_eq!(v.len(), 12);
        assert_eq!(v[0], *s.as_object().unwrap());
        for x in &v[1..] {
            let w = x["width"].as_f64().unwrap();
            assert!((5.0..=20.0).contains(&w), "{w}");
            assert!((0.0..=1.0).contains(&x["skew"].as_f64().unwrap()));
        }
        // deterministic
        assert_eq!(variants(s.as_object().unwrap(), 12), v);
        let g = json!({"type": "graph", "nodes": [{"id": "bb", "kind": "bollinger", "p": {"hours": 24, "k": 2}}]});
        for x in &variants(g.as_object().unwrap(), 8)[1..] {
            let h = x["nodes"][0]["p"]["hours"].as_f64().unwrap();
            assert!((12.0..=48.0).contains(&h), "{h}");
        }
        let none = json!({"type": "graph", "nodes": [{"id": "p", "kind": "price"}]});
        assert_eq!(variants(none.as_object().unwrap(), 8).len(), 1);
    }

    #[test]
    fn slices_measure_return_over_holding() {
        // value grows 1/hour, holding stays: 24 a day on 10000 capital
        let series: Vec<Value> = (0..=48).map(|h| json!([h * 3600, 10000.0 + h as f64, 10000.0])).collect();
        let x = slice_excess(&series, 0, 48 * 3600, 2, 10000.0);
        assert_eq!(x.len(), 2);
        for v in x {
            assert!((v - 24.0 / 10000.0 * 365.0 * 100.0).abs() < 1e-6, "{v}");
        }
    }

    #[test]
    fn noise_is_overfit_and_a_real_edge_is_robust() {
        // pure noise: the in-sample winner is a coin toss out of sample
        let mut rng = Rng::new(7);
        let noise: Vec<Vec<f64>> = (0..20).map(|_| (0..8).map(|_| 20.0 * (rng.next() - 0.5)).collect()).collect();
        let a = stats(&noise);
        let pbo = a["pbo"].as_f64().unwrap();
        assert!((0.2..=0.8).contains(&pbo), "{pbo}");
        // every variant earns about the same steady edge: robust
        let edge: Vec<Vec<f64>> = (0..20).map(|i| (0..8).map(|k| 5.0 + 0.01 * ((i * 7 + k * 3) % 5) as f64).collect()).collect();
        let b = stats(&edge);
        assert!(b["score"].as_f64().unwrap() >= 70.0, "{b}");
        assert_eq!(b["verdict"], "robust");
        // one lucky setting, its neighbours lose: likely overfit
        let mut lucky: Vec<Vec<f64>> = (0..20).map(|_| (0..8).map(|k| if k % 2 == 0 { -2.0 } else { -1.0 }).collect()).collect();
        lucky[0] = vec![30.0, -5.0, 25.0, -4.0, 20.0, -6.0, 1.0, -3.0];
        let c = stats(&lucky);
        assert!(c["score"].as_f64().unwrap() < 70.0 && c["stability"] == 0.0, "{c}");
        assert_eq!(combos(8, 4).len(), 70);
    }
}
