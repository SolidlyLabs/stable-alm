//! Compact overview points and position range transitions from the wallet's cycle log.

use serde::{Deserialize, Serialize};
use serde_json::{json, value::RawValue, Number, Value};
use std::collections::{BTreeMap, HashMap, VecDeque};

const CHART_ROWS: usize = 20_000;
const RANGE_DAYS: u64 = 91;
static HISTORY: crate::jsonl::Index<History> = crate::jsonl::Index::new();

#[derive(Deserialize)]
struct Snapshot<'a> {
    ts: Option<u64>,
    mode: Option<String>,
    total_usd: Option<Number>,
    value_usd: Option<Number>,
    rewards_usd: Option<Number>,
    solid_usd: Option<Number>,
    tokens: Option<BTreeMap<String, Balance>>,
    #[serde(borrow)]
    pools: Option<BTreeMap<String, Pool<'a>>>,
    #[serde(borrow)]
    chains: Option<BTreeMap<String, Chain<'a>>>,
}

#[derive(Deserialize)]
struct Balance {
    usd: Option<f64>,
}

#[derive(Deserialize)]
struct Pool<'a> {
    deployed_usd: Option<Number>,
    in_range: Option<bool>,
    tick: Option<i64>,
    #[serde(borrow)]
    positions: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct Chain<'a> {
    total_usd: Option<Number>,
    #[serde(borrow)]
    incomplete: Option<&'a RawValue>,
}

fn has_items(v: Option<&RawValue>) -> bool {
    v.and_then(|v| v.get().strip_prefix('[')).is_some_and(|rest| !rest.trim_start().starts_with(']'))
}

#[derive(Serialize)]
struct ChartPoint {
    ts: Option<u64>,
    mode: Option<String>,
    total_usd: Option<Number>,
    value_usd: Option<Number>,
    rewards_usd: Option<Number>,
    solid_usd: Option<Number>,
    idle_usd: f64,
    pools: BTreeMap<String, Option<Number>>,
    chains: BTreeMap<String, Option<Number>>,
}

impl ChartPoint {
    fn value(&self) -> f64 {
        self.value_usd.as_ref().or(self.total_usd.as_ref()).and_then(Number::as_f64).unwrap_or(0.0)
    }
}

struct RangeChange {
    name: String,
    ts: u64,
    tick: Option<i64>,
    in_range: bool,
}

#[derive(Default)]
struct History {
    // Count every line toward the chart window, including incomplete or unreadable records.
    chart: VecDeque<Option<ChartPoint>>,
    overview: Option<Vec<Value>>,
    last_in_range: HashMap<String, bool>,
    ranges: Vec<RangeChange>,
    range_cutoff: u64,
}

impl History {
    fn record(&mut self, line: &[u8], cutoff: u64) {
        if self.chart.len() == CHART_ROWS {
            self.chart.pop_front();
        }
        let point = serde_json::from_slice::<Snapshot<'_>>(line).ok().and_then(|r| self.project(r, cutoff));
        self.chart.push_back(point);
        self.overview = None;
    }

    fn project(&mut self, r: Snapshot<'_>, cutoff: u64) -> Option<ChartPoint> {
        for (name, pool) in r.pools.iter().flatten() {
            if !has_items(pool.positions) {
                continue;
            }
            let in_range = pool.in_range.unwrap_or(false);
            let ts = r.ts.unwrap_or(0);
            if self.last_in_range.insert(name.clone(), in_range).is_some_and(|was| was != in_range) && ts >= cutoff {
                self.ranges.push(RangeChange { name: name.clone(), ts, tick: pool.tick, in_range });
            }
        }
        let chains = r.chains.unwrap_or_default();
        if chains.values().any(|c| has_items(c.incomplete)) {
            return None;
        }
        Some(ChartPoint {
            ts: r.ts,
            mode: r.mode,
            total_usd: r.total_usd,
            value_usd: r.value_usd,
            rewards_usd: r.rewards_usd,
            solid_usd: r.solid_usd,
            idle_usd: r.tokens.iter().flatten().map(|(_, b)| b.usd.unwrap_or(0.0)).sum(),
            pools: r.pools.into_iter().flatten().map(|(name, p)| (name, p.deployed_usd)).collect(),
            chains: chains.into_iter().map(|(id, c)| (id, c.total_usd)).collect(),
        })
    }

    fn overview(&mut self) -> Vec<Value> {
        self.overview.get_or_insert_with(|| {
            let rows = clean_history(self.chart.iter().flatten().collect());
            let step = (rows.len() / 600).max(1);
            let mut keep: Vec<_> = rows.iter().step_by(step).map(|r| json!(r)).collect();
            if let Some(last) = rows.last().filter(|_| (rows.len() - 1) % step != 0) {
                keep.push(json!(last));
            }
            keep
        }).clone()
    }
}

fn path() -> std::path::PathBuf {
    crate::paths::logs().join("snapshots.jsonl")
}

fn read<R>(get: impl FnOnce(&mut History) -> R) -> R {
    let cutoff = (crate::engine::now() / 86_400).saturating_sub(RANGE_DAYS) * 86_400;
    HISTORY.read(&path(), u64::MAX, |h, line| h.record(line, cutoff), |h| {
        if cutoff > h.range_cutoff {
            h.ranges.retain(|r| r.ts >= cutoff);
            h.range_cutoff = cutoff;
        }
        get(h)
    })
}

pub fn overview() -> Vec<Value> {
    read(History::overview)
}

pub fn latest() -> Value {
    crate::jsonl::last(&path()).unwrap_or_default()
}

/// Find range changes in file order so callers can merge them with the other timeline events.
pub fn range_changes(since: u64, by_name: &HashMap<&str, (&str, u64)>) -> Vec<Value> {
    read(|h| h.ranges.iter().filter(|r| r.ts >= since).map(|r| {
        let (pool, cid) = by_name.get(r.name.as_str()).copied().unwrap_or_default();
        json!({"ts": r.ts, "chain_id": cid, "kind": if r.in_range { "range_enter" } else { "range_exit" },
               "pool": pool, "name": r.name, "tick": r.tick})
    }).collect())
}

/// Drop one to three bad valuations whose neighbours agree, without rounding away real moves.
fn clean_history(rows: Vec<&ChartPoint>) -> Vec<&ChartPoint> {
    const AGREE: f64 = 0.01;
    const OFF: f64 = 0.03;
    let mut drop = vec![false; rows.len()];
    let mut i = 1;
    while i + 1 < rows.len() {
        let mut hit = 0;
        for k in 1..=3 {
            if i + k >= rows.len() {
                break;
            }
            let (a, b) = (rows[i - 1].value(), rows[i + k].value());
            let mid = (a + b) / 2.0;
            if mid <= 0.0 || (a - b).abs() > AGREE * a.max(b) {
                continue;
            }
            let run = &rows[i..i + k];
            if run.iter().all(|r| r.value() < mid * (1.0 - OFF)) || run.iter().all(|r| r.value() > mid * (1.0 + OFF)) {
                hit = k;
                break;
            }
        }
        if hit > 0 {
            drop[i..i + hit].fill(true);
            i += hit;
        } else {
            i += 1;
        }
    }
    rows.into_iter().zip(drop).filter_map(|(r, d)| (!d).then_some(r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_value_history_drops_incomplete_cycles_and_spikes_and_keeps_steps() {
        let row = |ts: u64, v: f64| json!({"ts": ts, "total_usd": v, "value_usd": v, "chains": {"8453": {"incomplete": []}}});
        let times = |rows: &[Value]| {
            let mut history = History::default();
            for row in rows {
                history.record(row.to_string().as_bytes(), 0);
            }
            history.overview().iter().map(|r| r["ts"].as_u64().unwrap()).collect::<Vec<_>>()
        };
        let mut bad = row(3, 6000.0);
        bad["chains"]["8453"]["incomplete"] = json!(["usdc-usdt: rate limited"]);
        assert_eq!(times(&[row(1, 15000.0), row(2, 15010.0), bad, row(4, 15005.0)]), [1, 2, 4]);
        assert_eq!(times(&[row(1, 15000.0), row(2, 15010.0), row(3, 6000.0), row(4, 15005.0), row(5, 15002.0)]), [1, 2, 4, 5]);
        assert_eq!(times(&[row(1, 100.0), row(2, 60.0), row(3, 61.0), row(4, 100.5), row(5, 160.0), row(6, 100.2)]), [1, 4, 6]);
        assert_eq!(times(&[row(1, 15000.0), row(2, 9000.0), row(3, 9001.0), row(4, 9002.0), row(5, 8900.0), row(6, 9003.0)]), [1, 2, 3, 4, 5, 6]);
        assert_eq!(times(&[row(1, 100.0), row(2, 100.0), row(3, 40.0)]), [1, 2, 3]);
    }
}
