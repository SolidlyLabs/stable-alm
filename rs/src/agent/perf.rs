//! Per-pool performance (plan section 6), from what the app already records:
//! - logs/snapshots.jsonl, one line per cycle: each pool's capital (`pool_capital`, address → USD),
//!   its positions' token amounts and USD value, and its tick;
//! - logs/executor.jsonl, kind "capital": money added to a pool or withdrawn from it.
//!
//! For a window (24 h, 7 d, 30 d):
//! - **pnl** = capital at the end − at the start − net money added: transfers are not profit;
//! - **return** = pnl ÷ (start capital + time-weighted money added) (modified Dietz);
//! - **vs holding** = the end value against holding the token amounts the pool had at the start (and
//!   those it was given later), all in the pool's token1, so the pair's own price (ETH in USD, say)
//!   does not show as a loss. This is what the risk controller's loss limit watches (section 4).
//!
//! The token split of idle tokens is not in the snapshot: a pool's start amounts are its positions'
//! amounts scaled up to its whole capital.

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// One pool at one snapshot.
#[derive(Clone, Debug, Default)]
pub struct Sample {
    pub ts: i64,
    pub capital_usd: f64,
    /// token amounts in positions (whole tokens) and their USD value
    pub a0: f64,
    pub a1: f64,
    pub pos_usd: f64,
    /// token1 per token0 (whole tokens), from the tick
    pub price: f64,
    /// from the snapshot's `pool_stats` (0 in lines written before it): what the pool parked in another
    /// pool's strategy (a claim, part of its capital), and the USD price of one whole token0 / token1
    pub owed: f64,
    pub px0: f64,
    pub px1: f64,
}

impl Sample {
    /// USD price of token1, from the positions' value: usd = a0·p·u1 + a1·u1.
    pub fn u1(&self) -> Option<f64> {
        if self.px1 > 0.0 {
            return Some(self.px1);
        }
        let d = self.a0 * self.price + self.a1;
        (d > 0.0 && self.pos_usd > 0.0).then(|| self.pos_usd / d)
    }
    /// USD prices of a whole token0 and token1: recorded, or from the positions (none while it holds no
    /// positions, in a line written before the prices were recorded)
    pub fn px(&self) -> Option<(f64, f64)> {
        if self.px0 > 0.0 && self.px1 > 0.0 {
            return Some((self.px0, self.px1));
        }
        let u1 = self.u1()?;
        (self.price > 0.0).then(|| (u1 * self.price, u1))
    }
}

#[derive(Clone, Debug)]
pub struct Flow {
    pub ts: i64,
    /// USD into the pool (negative: out)
    pub usd: f64,
    /// deposit, withdrawal, transfer in, transfer out, sent out (tokens that left the wallet), reset
    pub kind: String,
}

fn flow(ts: i64, usd: f64, kind: &str) -> Flow {
    Flow { ts, usd, kind: kind.into() }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryMode {
    Live,
    DryRun,
}

impl HistoryMode {
    fn label(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::DryRun => "dry run",
        }
    }
}

type PoolSamples = HashMap<String, Vec<Sample>>;

#[derive(Default)]
struct Store {
    offset: u64,
    live: PoolSamples,
    dry: PoolSamples,
    /// Snapshot timestamps before thinning, for capital events written without a mode.
    cycles: BTreeMap<i64, HistoryMode>,
}

impl Store {
    fn mode(&self) -> HistoryMode {
        if self.live.is_empty() { HistoryMode::DryRun } else { HistoryMode::Live }
    }

    fn samples(&self) -> &PoolSamples {
        match self.mode() {
            HistoryMode::Live => &self.live,
            HistoryMode::DryRun => &self.dry,
        }
    }

    fn record(&mut self, v: &Value, names: &HashMap<String, String>, dec: &mut dyn FnMut(u64, &str) -> Option<(i32, i32)>) {
        let Some(ts) = v["ts"].as_i64() else { return };
        let mode = match v["mode"].as_str() {
            Some("dry run" | "paused" | "paused (STOP)") => HistoryMode::DryRun,
            _ => HistoryMode::Live,
        };
        self.cycles.insert(ts, mode);
        let by_pool = match mode {
            HistoryMode::Live => &mut self.live,
            HistoryMode::DryRun => &mut self.dry,
        };
        for (addr, sample) in parse_line(v, names, dec) {
            push_sample(by_pool.entry(addr).or_default(), sample);
        }
    }

    fn event_mode(&self, e: &Value, ts: i64) -> HistoryMode {
        match e["live"].as_bool() {
            Some(true) => HistoryMode::Live,
            Some(false) => HistoryMode::DryRun,
            None if e["dry"] == true => HistoryMode::DryRun,
            // Withdrawals and fee credits were only logged by live execution.
            None if matches!(e["what"].as_str(), Some("withdraw" | "fees")) => HistoryMode::Live,
            // The worker writes its snapshot at the end of the cycle. Limit recovery to nearby
            // snapshots so an unrelated cycle after a long shutdown cannot classify the event.
            None => self.cycles.range(ts..=ts.saturating_add(30 * 60)).next()
                .map(|(_, mode)| *mode).unwrap_or(HistoryMode::Live),
        }
    }
}

type WalletStores = HashMap<PathBuf, Arc<Mutex<Store>>>;
static STORES: LazyLock<Mutex<WalletStores>> = LazyLock::new(Mutex::default);
/// how far back samples are kept: the dashboard's "All" (a year); one sample per BUCKET_S per pool
const KEEP_S: i64 = 400 * 86_400;
const BUCKET_S: i64 = 300;
/// a window's start and end capital: the median over this long (see `window`)
const LEVEL_S: i64 = 3600;

fn median(it: impl Iterator<Item = f64>) -> Option<f64> {
    let mut v: Vec<f64> = it.filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

fn decimals(cid: u64, pool: &str) -> Option<(i32, i32)> {
    let (t0, t1) = crate::chain::pool_tokens(cid, pool).ok()?;
    Some((crate::prices::meta(cid, &t0).ok()?.decimals as i32, crate::prices::meta(cid, &t1).ok()?.decimals as i32))
}

/// Parse one snapshot line into per-pool samples.
fn parse_line(v: &Value, names: &HashMap<String, String>, dec: &mut dyn FnMut(u64, &str) -> Option<(i32, i32)>) -> Vec<(String, Sample)> {
    let ts = v["ts"].as_i64().unwrap_or(0);
    let mut out = vec![];
    for (name, info) in v["pools"].as_object().into_iter().flatten() {
        let Some(addr) = names.get(name) else { continue };
        let Some(cap) = v["pool_capital"][addr.as_str()].as_f64() else { continue };
        let cid = info["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
        let (mut a0, mut a1, mut usd) = (0.0, 0.0, 0.0);
        for p in info["positions"].as_array().into_iter().flatten() {
            a0 += p["amount0"].as_f64().unwrap_or(0.0);
            a1 += p["amount1"].as_f64().unwrap_or(0.0);
            usd += p["usd"].as_f64().unwrap_or(0.0);
        }
        let price = info["tick"].as_i64().and_then(|t| {
            let (d0, d1) = dec(cid, addr)?;
            Some(1.0001f64.powf(t as f64) * 10f64.powi(d0 - d1))
        }).unwrap_or(0.0);
        let st = &v["pool_stats"][addr.as_str()];
        let f = |k: &str| st[k].as_f64().unwrap_or(0.0);
        out.push((addr.clone(), Sample { ts, capital_usd: cap, a0, a1, pos_usd: usd, price, owed: f("owed_usd"), px0: f("px0"), px1: f("px1") }));
    }
    out
}

/// Adds a sample, one per BUCKET_S: a later one in the same bucket replaces it (the newest is kept).
fn push_sample(v: &mut Vec<Sample>, s: Sample) {
    let bucket = s.ts.div_euclid(BUCKET_S);
    if v.last().is_none_or(|last| last.ts.div_euclid(BUCKET_S) < bucket) {
        v.push(s);
        return;
    }
    match v.binary_search_by_key(&bucket, |old| old.ts.div_euclid(BUCKET_S)) {
        Ok(i) if s.ts >= v[i].ts => v[i] = s,
        Ok(_) => {}
        Err(i) => v.insert(i, s),
    }
}

fn with_history<T>(now: i64, read: impl FnOnce(&Store) -> T) -> T {
    let path = crate::paths::logs().join("snapshots.jsonl");
    let store = Arc::clone(STORES.lock().unwrap().entry(path.clone()).or_default());
    let mut store = store.lock().unwrap();
    refresh(&mut store, &path, now);
    read(&store)
}

/// Read what was appended to the snapshots since the last call.
fn refresh(st: &mut Store, path: &Path, now: i64) {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            *st = Store::default();
            return;
        }
        Err(_) => return,
    };
    let Ok(meta) = f.metadata() else { return };
    let len = meta.len();
    if len < st.offset {
        *st = Store::default();
    }
    if len == st.offset {
        return;
    }
    // Bound the first read for large logs. A partial line at this offset is ignored.
    let offset = if st.offset == 0 { len.saturating_sub(400 << 20) } else { st.offset };
    if f.seek(SeekFrom::Start(offset)).is_err() {
        return;
    }
    st.offset = offset;
    let cutoff = now - KEEP_S;
    let names: HashMap<String, String> = crate::settings::load_pools().iter()
        .filter_map(|p| Some((p["name"].as_str()?.to_string(), p["pool"].as_str()?.to_lowercase()))).collect();
    let mut known = HashMap::new();
    let mut dec = |cid: u64, a: &str| *known.entry((cid, a.to_string())).or_insert_with(|| decimals(cid, a));
    let mut reader = BufReader::new(f);
    let mut line = Vec::new();
    while let Ok(n) = reader.read_until(b'\n', &mut line) {
        if n == 0 || line.last() != Some(&b'\n') {
            break;
        }
        st.offset += n as u64;
        let snapshot = std::str::from_utf8(&line).ok()
            .filter(|s| s.contains("\"pool_capital\""))
            .and_then(|s| serde_json::from_str::<Value>(s).ok());
        line.clear();
        if let Some(v) = snapshot.filter(|v| v["ts"].as_i64().is_some_and(|ts| ts >= cutoff)) {
            st.record(&v, &names, &mut dec);
        }
    }
    st.cycles = st.cycles.split_off(&cutoff);
    for by_pool in [&mut st.live, &mut st.dry] {
        by_pool.retain(|_, samples| {
            let first = samples.partition_point(|s| s.ts < cutoff);
            samples.drain(..first);
            !samples.is_empty()
        });
    }
}

/// Historical flows recorded by hand describe live activity.
fn append_extra(events: &mut Vec<Value>, dir: &Path) {
    if let Ok(t) = std::fs::read_to_string(dir.join("flows_extra.jsonl")) {
        events.extend(t.lines().filter_map(|l| {
            let mut e = serde_json::from_str::<Value>(l).ok()?;
            e.as_object_mut()?.entry("live").or_insert(json!(true));
            Some(e)
        }));
    }
}

/// The wallet's executor and manually recorded flows, in timestamp order.
fn events() -> Vec<Value> {
    #[derive(Default)]
    struct AccountingEvents {
        entries: Vec<Value>,
        cutoff: i64,
    }
    static EVENTS: crate::jsonl::Index<AccountingEvents> = crate::jsonl::Index::new();
    let cutoff = super::now().div_euclid(86_400) * 86_400 - KEEP_S - 86_400;
    let mut ev = EVENTS.read(&crate::engine::log_path(), 64 << 20, |state, line| {
        let Some(text) = std::str::from_utf8(line).ok() else { return };
        if !text.contains("\"capital\"") && !text.contains("\"unpark\"") {
            return;
        }
        let Ok(e) = serde_json::from_str::<Value>(text) else { return };
        if e["ts"].as_i64().is_some_and(|ts| ts >= cutoff) && matches!(e["kind"].as_str(), Some("capital" | "unpark")) {
            state.entries.push(e);
        }
    }, |state| {
        if cutoff > state.cutoff {
            state.entries.retain(|e| e["ts"].as_i64().is_some_and(|ts| ts >= cutoff));
            state.cutoff = cutoff;
        }
        state.entries.clone()
    });
    append_extra(&mut ev, &crate::paths::logs());
    ev.sort_by_key(|e| e["ts"].as_i64().unwrap_or(0));
    ev
}

/// `flows` from executor log events (oldest first). What moves a pool's capital without being earned
/// or lost by it:
/// - capital "add" (money given to the pool), "allocate" (given from Unallocated to reach its %),
///   "withdraw" / "release" (taken out; to another pool when `to` is set), "shrink" (its tokens left
///   the wallet), "reset" (its book set again from the allocation %);
/// - "unpark": paying a parking claim back withdraws about 1% more of the host's positions than the
///   claim (and any token whose swap back failed): that surplus moves from the host to the pool that
///   parked, so it is a transfer, not the host's loss or the other pool's profit.
fn flows_from(st: &Store, mode: HistoryMode, events: &[Value], since: i64) -> HashMap<String, Vec<Flow>> {
    let mut out: HashMap<String, Vec<Flow>> = HashMap::new();
    let mut add = |pool: &str, ts: i64, usd: f64, kind: &str| {
        if usd.is_finite() && usd != 0.0 && !pool.is_empty() {
            out.entry(pool.to_lowercase()).or_default().push(flow(ts, usd, kind));
        }
    };
    for e in events {
        let ts = e["ts"].as_i64().unwrap_or(0);
        if ts < since {
            continue;
        }
        let usd = e["usd"].as_f64().unwrap_or(0.0);
        let pool = e["pool"].as_str().unwrap_or("");
        let kind = e["kind"].as_str().unwrap_or("");
        if kind == "capital" && st.event_mode(e, ts) != mode {
            continue;
        }
        match (kind, e["what"].as_str().unwrap_or("")) {
            ("capital", "allocate") => add(pool, ts, usd, "deposit"),
            ("capital", "add") => {
                add(pool, ts, e["from_unallocated_usd"].as_f64().unwrap_or(usd), "deposit");
                for (source, amount) in e["from_pools_usd"].as_object().into_iter().flatten() {
                    let amount = amount.as_f64().unwrap_or(0.0);
                    add(source, ts, -amount, "transfer out");
                    add(pool, ts, amount, "transfer in");
                }
            }
            ("capital", "withdraw" | "release") => match e["to"].as_str() {
                Some(to) => {
                    add(pool, ts, -usd, "transfer out");
                    add(to, ts, usd, "transfer in");
                }
                None => add(pool, ts, -usd, "withdrawal"),
            },
            ("capital", "shrink") => add(pool, ts, -usd, "sent out"),
            ("capital", "reset") => add(pool, ts, usd, "reset"),
            ("unpark", _) if mode == HistoryMode::Live && e["live"] == true && e["partial"] != true => {
                let lot = &e["lot"];
                let amount = lot["amount"].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                let back = e["back"].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                let lot_usd = lot["usd"].as_f64().unwrap_or(0.0);
                if amount > 0.0 && back > amount && lot["kind"] == "claim" {
                    let extra = lot_usd * (back / amount - 1.0);
                    add(lot["pool"].as_str().unwrap_or(""), ts, -extra, "transfer out");
                    add(pool, ts, extra, "transfer in");
                }
            }
            _ => {}
        }
    }
    out
}

/// Claimed fees credited to each pool (capital "fees" events), USD by pool, oldest first.
fn fees_from(st: &Store, mode: HistoryMode, events: &[Value], since: i64) -> HashMap<String, Vec<(i64, f64)>> {
    let mut out: HashMap<String, Vec<(i64, f64)>> = HashMap::new();
    for e in events.iter().filter(|e| e["kind"] == "capital" && e["what"] == "fees") {
        let ts = e["ts"].as_i64().unwrap_or(0);
        if ts >= since && st.event_mode(e, ts) == mode {
            out.entry(e["pool"].as_str().unwrap_or("").to_lowercase()).or_default().push((ts, e["usd"].as_f64().unwrap_or(0.0)));
        }
    }
    out
}

/// Samples without accounting glitches: a jump of more than 20% with no flow that comes back within 2%
/// of where it was inside a day, again with no flow (a parked claim counted by both pools for some
/// hours, 31 Oct 2026). A real move of that size does not undo itself so exactly.
fn despike<'a>(v: Vec<&'a Sample>, flows: &[Flow]) -> Vec<&'a Sample> {
    // a flow that could explain the jump: at least 5% of it (a $2 transfer beside a $4,600 jump cannot)
    let flow_in = |a: i64, b: i64, jump: f64| flows.iter().any(|f| f.ts > a && f.ts <= b && f.usd.abs() >= 0.05 * jump.abs());
    let mut drop = vec![false; v.len()];
    let mut i = 1;
    while i < v.len() {
        let base = v[i - 1].capital_usd;
        let jump = v[i].capital_usd - base;
        if base > 0.0 && (jump / base).abs() > 0.2 && !flow_in(v[i - 1].ts, v[i].ts, jump) {
            let back = (i + 1..v.len()).take_while(|&j| v[j].ts - v[i].ts <= 86_400)
                .find(|&j| ((v[j].capital_usd - base) / base).abs() < 0.02);
            if let Some(j) = back.filter(|&j| !flow_in(v[i - 1].ts, v[j].ts, jump)) {
                drop[i..j].iter_mut().for_each(|d| *d = true);
                i = j;
                continue;
            }
        }
        i += 1;
    }
    v.into_iter().zip(drop).filter(|(_, d)| !d).map(|(s, _)| s).collect()
}

/// A window's figures from samples (oldest first) and flows.
pub fn window(samples: &[Sample], flows: &[Flow], from: i64, to: i64) -> Value {
    let inside = despike(samples.iter().filter(|s| s.ts >= from && s.ts <= to).collect(), flows);
    let (Some(a), Some(b)) = (inside.first(), inside.last()) else { return json!({"complete": false, "reason": "no samples in the window"}) };
    if b.ts <= a.ts {
        return json!({"complete": false, "reason": "one sample only"});
    }
    let fl: Vec<&Flow> = flows.iter().filter(|f| f.ts > a.ts && f.ts <= b.ts).collect();
    let net: f64 = fl.iter().map(|f| f.usd).sum();
    let span = (b.ts - a.ts) as f64;
    let weighted: f64 = fl.iter().map(|f| f.usd * (b.ts - f.ts) as f64 / span).sum();
    // the capital at each end as the median of its first / last hour (before the first flow, after the
    // last): a cycle's valuation moves with the token quotes (a USDC/USDT pool's by $10-70 on $6k with
    // nothing done), more than a day's fees
    let (first_flow, last_flow) = (fl.first().map(|f| f.ts).unwrap_or(i64::MAX), fl.last().map(|f| f.ts).unwrap_or(i64::MIN));
    let cap_a = median(inside.iter().filter(|s| s.ts <= a.ts + LEVEL_S && s.ts < first_flow).map(|s| s.capital_usd)).unwrap_or(a.capital_usd);
    let cap_b = median(inside.iter().filter(|s| s.ts >= b.ts - LEVEL_S && s.ts >= last_flow).map(|s| s.capital_usd)).unwrap_or(b.capital_usd);
    let pnl = cap_b - cap_a - net;
    let base = cap_a + weighted;
    let ret = if base > 0.0 { Some(pnl / base) } else { None };
    // against holding: the start mix (scaled to the capital), plus what was added, held
    let vs = (|| {
        let mut h = Hold::start(&Sample { capital_usd: cap_a, ..(*a).clone() })?;
        for f in &fl {
            let s = inside.iter().rev().find(|s| s.ts <= f.ts).copied().unwrap_or(a);
            h.flow(s, f.usd);
        }
        let excess = cap_b - h.usd(b)?;
        Some((excess, if base > 0.0 { excess / base } else { 0.0 }))
    })();
    json!({
        "complete": true, "from": a.ts, "to": b.ts, "start_usd": cap_a, "end_usd": cap_b, "net_flows_usd": net,
        "pnl_usd": pnl, "return": ret, "return_base_usd": (base > 0.0).then_some(base),
        "vs_hold_usd": vs.map(|x| x.0), "vs_hold": vs.map(|x| x.1), "samples": inside.len(),
    })
}

/// Holding instead of running the pool: the token amounts it started a window with (its positions' mix
/// scaled up to its whole capital; half and half by value when it held no positions, parked in USDC
/// say), and money added or taken later bought or sold at that mix at the prices of the moment.
pub struct Hold {
    h0: f64,
    h1: f64,
}

impl Hold {
    pub fn start(a: &Sample) -> Option<Hold> {
        let (p0, p1) = a.px()?;
        if a.pos_usd >= 1.0 && a.a0 * p0 + a.a1 * p1 > 0.0 {
            let k = a.capital_usd / (a.a0 * p0 + a.a1 * p1);
            Some(Hold { h0: a.a0 * k, h1: a.a1 * k })
        } else {
            Some(Hold { h0: a.capital_usd / 2.0 / p0, h1: a.capital_usd / 2.0 / p1 })
        }
    }
    /// `usd` added (negative: taken) at sample `s`'s prices, at the mix held
    pub fn flow(&mut self, s: &Sample, usd: f64) {
        let Some((p0, p1)) = s.px() else { return };
        let v = self.h0 * p0 + self.h1 * p1;
        if v > 0.0 {
            let k = (1.0 + usd / v).max(0.0);
            self.h0 *= k;
            self.h1 *= k;
        } else if usd > 0.0 {
            self.h0 = usd / 2.0 / p0;
            self.h1 = usd / 2.0 / p1;
        }
    }
    pub fn usd(&self, s: &Sample) -> Option<f64> {
        let (p0, p1) = s.px()?;
        Some(self.h0 * p0 + self.h1 * p1)
    }
}

/// One pool's performance over [from, to] for the dashboard: the window's figures, the flows in it,
/// fees claimed, gas and swap cost (`costs`: day start → (gas, swap)), and a series of at most `points`
/// points: capital, money put in (the start capital plus net flows), profit, the hold benchmark and
/// what was parked.
pub fn pool_report(samples: &[Sample], flows: &[Flow], fees: &[(i64, f64)], costs: &[(i64, f64, f64)], from: i64, to: i64, points: usize) -> Value {
    let inside = despike(samples.iter().filter(|s| s.ts >= from && s.ts <= to).collect(), flows);
    let mut w = window(samples, flows, from, to);
    let (Some(a), Some(b)) = (inside.first().copied(), inside.last().copied()) else { return json!({"window": w, "series": [], "flows": []}) };
    let fl: Vec<&Flow> = flows.iter().filter(|f| f.ts > a.ts && f.ts <= b.ts).collect();
    let sum = |k: &dyn Fn(&Flow) -> bool| fl.iter().filter(|f| k(f)).map(|f| f.usd).sum::<f64>();
    let fees_usd: f64 = fees.iter().filter(|x| x.0 > a.ts && x.0 <= b.ts).map(|x| x.1).sum();
    let day0 = a.ts.div_euclid(86_400) * 86_400;
    let (gas, swap) = costs.iter().filter(|c| c.0 >= day0 && c.0 <= b.ts).fold((0.0, 0.0), |s, c| (s.0 + c.1, s.1 + c.2));
    let days = (b.ts - a.ts) as f64 / 86_400.0;
    if let Some(o) = w.as_object_mut() {
        o.insert("deposits_usd".into(), json!(sum(&|f| f.usd > 0.0 && !f.kind.starts_with("transfer"))));
        o.insert("withdrawals_usd".into(), json!(-sum(&|f| f.usd < 0.0 && !f.kind.starts_with("transfer"))));
        o.insert("transfers_usd".into(), json!(sum(&|f| f.kind.starts_with("transfer"))));
        o.insert("fees_claimed_usd".into(), json!(fees_usd));
        o.insert("gas_usd".into(), json!(gas));
        o.insert("swap_cost_usd".into(), json!(swap));
        o.insert("days".into(), json!(days));
        let parked = inside.windows(2).filter(|p| p[0].owed > 1.0).map(|p| (p[1].ts - p[0].ts) as f64).sum::<f64>();
        o.insert("parked_share".into(), json!(if b.ts > a.ts { parked / (b.ts - a.ts) as f64 } else { 0.0 }));
        if let Some(r) = o.get("return").and_then(|r| r.as_f64()) {
            if days >= 1.0 {
                o.insert("return_yearly".into(), json!(r * 365.0 / days));
            }
        }
    }
    // the series: every sample, thinned to `points`; flows and the hold benchmark applied as they come
    // capital smoothed as the window's ends are: the median of the last hour's samples since the last flow
    let step = (inside.len() / points.max(2)).max(1);
    let start = w["start_usd"].as_f64().unwrap_or(a.capital_usd);
    let mut hold = Hold::start(&Sample { capital_usd: start, ..a.clone() });
    let (mut put_in, mut fi) = (start, 0);
    let mut recent: std::collections::VecDeque<(i64, f64)> = Default::default();
    let mut series = vec![];
    for (i, s) in inside.iter().enumerate() {
        while fi < fl.len() && fl[fi].ts <= s.ts {
            put_in += fl[fi].usd;
            let at = inside[..=i].iter().rev().find(|x| x.ts <= fl[fi].ts).copied().unwrap_or(s);
            if let Some(h) = hold.as_mut() {
                h.flow(at, fl[fi].usd);
            }
            recent.clear();
            fi += 1;
        }
        recent.push_back((s.ts, s.capital_usd));
        while recent.front().map(|x| x.0 < s.ts - LEVEL_S).unwrap_or(false) {
            recent.pop_front();
        }
        if i % step == 0 || i + 1 == inside.len() {
            let r = |x: f64| (x * 100.0).round() / 100.0;
            let cap = if i + 1 == inside.len() { w["end_usd"].as_f64().unwrap_or(s.capital_usd) } else { median(recent.iter().map(|x| x.1)).unwrap_or(s.capital_usd) };
            series.push(json!([s.ts, r(cap), r(put_in), r(cap - put_in), hold.as_ref().and_then(|h| h.usd(s)).map(r), r(s.owed)]));
        }
    }
    let flows_json: Vec<Value> = fl.iter().map(|f| json!({"ts": f.ts, "usd": f.usd, "kind": f.kind})).collect();
    json!({"window": w, "series": series, "series_cols": ["ts", "capital", "put_in", "profit", "hold", "parked"], "flows": flows_json})
}

/// The Performance tab: every configured pool over the last `days` (0: all that is recorded), with
/// 24 h / 7 d / 30 d figures. `costs`: pool → per day (day start, gas USD, swap cost USD), the
/// ledger's.
pub fn report(days: i64, costs: &HashMap<String, Vec<(i64, f64, f64)>>) -> Value {
    let now = super::now();
    let pools = crate::settings::load_pools();
    with_history(now, |st| report_at(st, &events(), &pools, costs, days, now))
}

fn report_at(st: &Store, events: &[Value], configured: &[Value], costs: &HashMap<String, Vec<(i64, f64, f64)>>, days: i64, now: i64) -> Value {
    let from = if days > 0 { now - days * 86_400 } else { now - KEEP_S };
    let since = from.min(now - 31 * 86_400) - 3600;
    let mode = st.mode();
    let fl = flows_from(st, mode, events, since);
    let fe = fees_from(st, mode, events, since);
    let samples = st.samples();
    let mut pools = vec![];
    for p in configured {
        let addr = p["pool"].as_str().unwrap_or("").to_lowercase();
        let s = samples.get(&addr).map(Vec::as_slice).unwrap_or_default();
        // a pool that is off and held nothing in the period is left out
        if p["enabled"] != true && !s.iter().any(|x| x.ts >= from && x.capital_usd >= 1.0) {
            continue;
        }
        let f = fl.get(&addr).map(Vec::as_slice).unwrap_or_default();
        let fz = fe.get(&addr).map(Vec::as_slice).unwrap_or_default();
        let c = if mode == HistoryMode::Live { costs.get(&addr).map(Vec::as_slice).unwrap_or_default() } else { &[] };
        let mut r = pool_report(s, f, fz, c, from, now, 360);
        let short = |secs: i64| pool_report(s, f, fz, c, now - secs, now, 2)["window"].clone();
        r["pool"] = json!(addr);
        r["name"] = p["name"].clone();
        r["chain_id"] = json!(crate::worker::pool_chain(p));
        r["enabled"] = p["enabled"].clone();
        r["capital_usd"] = json!(s.last().map(|x| x.capital_usd));
        r["first_ts"] = json!(s.first().map(|x| x.ts));
        r["last_ts"] = json!(s.last().map(|x| x.ts));
        r["h24"] = short(86_400);
        r["d7"] = short(7 * 86_400);
        r["d30"] = short(30 * 86_400);
        pools.push(r);
    }
    json!({"now": now, "days": days, "from": from, "history_mode": (!samples.is_empty()).then(|| mode.label()), "pools": pools})
}

/// Every configured pool's figures over 24 h, 7 d and 30 d.
pub fn all() -> Value {
    let now = super::now();
    let configured = crate::settings::load_pools();
    with_history(now, |st| {
        let fl = flows_from(st, st.mode(), &events(), now - 31 * 86_400);
        let mut out = serde_json::Map::new();
        for p in configured {
            let addr = p["pool"].as_str().unwrap_or("").to_lowercase();
            let s = st.samples().get(&addr).map(Vec::as_slice).unwrap_or_default();
            let f = fl.get(&addr).map(Vec::as_slice).unwrap_or_default();
            out.insert(addr, json!({
                "name": p["name"],
                "h24": window(s, f, now - 86_400, now),
                "d7": window(s, f, now - 7 * 86_400, now),
                "d30": window(s, f, now - 30 * 86_400, now),
                "capital_usd": s.last().map(|x| x.capital_usd),
            }));
        }
        Value::Object(out)
    })
}

/// One pool's figures for a window ending now.
pub fn pool(addr: &str, seconds: i64) -> Value {
    let now = super::now();
    pool_since(addr, now - seconds, now)
}

/// One pool's figures for the given window.
pub fn pool_since(addr: &str, from: i64, to: i64) -> Value {
    let addr = addr.to_lowercase();
    with_history(to, |st| {
        let Some(s) = st.samples().get(&addr) else { return window(&[], &[], from, to) };
        let fl = flows_from(st, st.mode(), &events(), from - 3600);
        window(s, fl.get(&addr).map(Vec::as_slice).unwrap_or_default(), from, to)
    })
}

/// The pool's samples (for the shadow books and the snapshot).
pub fn samples(addr: &str) -> Vec<Sample> {
    with_history(super::now(), |st| st.samples().get(&addr.to_lowercase()).cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(ts: i64, cap: f64, a0: f64, a1: f64, price: f64, u1: f64) -> Sample {
        Sample { ts, capital_usd: cap, a0, a1, pos_usd: (a0 * price + a1) * u1, price, ..Default::default() }
    }

    #[test]
    fn money_added_is_not_profit() {
        // 1000 → 1600 with 500 added halfway: 100 earned
        let v = vec![s(0, 1000.0, 500.0, 500.0, 1.0, 1.0), s(100, 1600.0, 800.0, 800.0, 1.0, 1.0)];
        let w = window(&v, &[flow(50, 500.0, "deposit")], 0, 100);
        assert!((w["pnl_usd"].as_f64().unwrap() - 100.0).abs() < 1e-9);
        // modified Dietz: 100 / (1000 + 500 × 0.5)
        assert!((w["return"].as_f64().unwrap() - 0.08).abs() < 1e-9);
    }

    #[test]
    fn the_pairs_own_price_is_not_a_loss_against_holding() {
        // ETH pair: both tokens fall 10% in USD, the pool ratio is unchanged, nothing earned
        let a = s(0, 2000.0, 0.5, 0.5, 1.0, 2000.0);
        let b = s(10_000, 1800.0, 0.5, 0.5, 1.0, 1800.0);
        let w = window(&[a, b], &[], 0, 10_000);
        assert!((w["pnl_usd"].as_f64().unwrap() + 200.0).abs() < 1e-6);
        assert!(w["vs_hold_usd"].as_f64().unwrap().abs() < 1e-6, "{w}");
    }

    #[test]
    fn a_strategy_loss_shows_against_holding() {
        // the ratio moved 10% and the position sold the riser: worse than holding the start mix
        let a = s(0, 1000.0, 500.0, 500.0, 1.0, 1.0);
        let b = s(10_000, 1000.0, 0.0, 1000.0, 1.1, 1.0);
        let w = window(&[a, b], &[], 0, 10_000);
        // holding: 500 × 1.1 + 500 = 1050 → 50 behind
        assert!((w["vs_hold_usd"].as_f64().unwrap() + 50.0).abs() < 1e-6, "{w}");
    }

    #[test]
    fn withdrawals_transfers_and_the_unpark_surplus_are_flows() {
        let ev: Vec<Value> = vec![
            json!({"ts": 10, "kind": "capital", "what": "add", "pool": "0xA", "usd": 100.0}),
            json!({"ts": 20, "kind": "capital", "what": "withdraw", "pool": "0xa", "usd": 40.0, "to": null}),
            json!({"ts": 30, "kind": "capital", "what": "withdraw", "pool": "0xa", "usd": 10.0, "to": "0xb"}),
            json!({"ts": 40, "kind": "capital", "what": "shrink", "pool": "0xb", "usd": 5.0}),
            // a claim of 1000 USDC paid back with 1010: the host gives 10 to the pool that parked
            json!({"ts": 50, "kind": "unpark", "live": true, "pool": "0xa", "back": "1010000000",
                   "lot": {"kind": "claim", "pool": "0xb", "amount": "1000000000", "usd": 1000.0}}),
            json!({"ts": 60, "kind": "capital", "what": "fees", "pool": "0xa", "usd": 3.0}),
            json!({"ts": 70, "kind": "capital", "what": "add", "live": true, "pool": "0xb", "usd": 50.0,
                   "from_unallocated_usd": 20.0, "from_pools_usd": {"0xa": 30.0}}),
        ];
        let st = Store::default();
        let f = flows_from(&st, HistoryMode::Live, &ev, 0);
        let a: Vec<(f64, &str)> = f["0xa"].iter().map(|x| ((x.usd * 1e6).round() / 1e6, x.kind.as_str())).collect();
        assert_eq!(a, vec![(100.0, "deposit"), (-40.0, "withdrawal"), (-10.0, "transfer out"), (10.0, "transfer in"), (-30.0, "transfer out")]);
        let b: Vec<(f64, &str)> = f["0xb"].iter().map(|x| ((x.usd * 1e6).round() / 1e6, x.kind.as_str())).collect();
        assert_eq!(b, vec![(10.0, "transfer in"), (-5.0, "sent out"), (-10.0, "transfer out"), (20.0, "deposit"), (30.0, "transfer in")]);
        assert_eq!(fees_from(&st, HistoryMode::Live, &ev, 0)["0xa"], vec![(60, 3.0)]);
    }

    fn snapshot(ts: i64, mode: &str, capital: f64) -> Value {
        json!({"ts": ts, "mode": mode, "pool_capital": {"0xa": capital}, "pools": {"Pool A": {}}})
    }

    #[test]
    fn report_uses_live_capital_and_excludes_legacy_dry_run_flows() {
        let pools = [json!({"pool": "0xa", "name": "Pool A", "enabled": true})];
        let names = HashMap::from([("Pool A".to_string(), "0xa".to_string())]);
        let costs = HashMap::from([("0xa".to_string(), vec![(0, 2.0, 1.0)])]);
        let mut st = Store::default();
        let mut dec = |_: u64, _: &str| None;
        let ev = [
            json!({"ts": 330, "kind": "capital", "what": "shrink", "pool": "0xa", "usd": 10.0}),
            json!({"ts": 3900, "kind": "capital", "what": "release", "pool": "0xa", "usd": 20.0, "dry": true}),
            json!({"ts": 3900, "kind": "capital", "what": "withdraw", "pool": "0xa", "usd": 200.0}),
            json!({"ts": 4050, "kind": "capital", "what": "fees", "pool": "0xa", "usd": 5.0}),
            json!({"ts": 4060, "kind": "capital", "what": "release", "pool": "0xa", "usd": 4.0, "live": false}),
        ];
        for (ts, mode, capital) in [(10, "dry run", 700.0), (420, "dry run", 690.0), (4000, "paused", 670.0)] {
            st.record(&snapshot(ts, mode, capital), &names, &mut dec);
        }
        let dry = report_at(&st, &ev, &pools, &costs, 1, 9000);
        assert_eq!(dry["history_mode"], "dry run");
        let w = &dry["pools"][0]["h24"];
        assert_eq!(w["pnl_usd"], 0.0);
        assert_eq!(w["withdrawals_usd"], 30.0);
        assert_eq!(w["fees_claimed_usd"], 0.0);
        assert_eq!(w["gas_usd"], 0.0);
        for (ts, capital) in [(100, 1000.0), (310, 1000.0), (4010, 800.0), (8000, 805.0)] {
            st.record(&snapshot(ts, "LIVE", capital), &names, &mut dec);
        }
        let live = report_at(&st, &ev, &pools, &costs, 1, 9000);
        assert_eq!(live["history_mode"], "live");
        let p = &live["pools"][0];
        assert_eq!(p["last_ts"], 8000);
        let w = &p["h24"];
        assert_eq!(w["samples"], 4);
        assert_eq!(w["pnl_usd"], 5.0);
        assert_eq!(w["withdrawals_usd"], 200.0);
        assert_eq!(w["fees_claimed_usd"], 5.0);
        assert_eq!(w["gas_usd"], 2.0);
    }

    #[test]
    fn switching_wallets_keeps_separate_snapshot_offsets_and_samples() {
        let home = crate::wallets::tests::temp_home("performance-wallets");
        let ts = super::super::now() - 5;
        let write = |mode, capital| {
            std::fs::create_dir_all(crate::paths::config()).unwrap();
            std::fs::create_dir_all(crate::paths::logs()).unwrap();
            let p = json!([{"name": "Pool A", "pool": "0xa", "enabled": true}]);
            std::fs::write(crate::paths::config().join("pools.json"), p.to_string()).unwrap();
            std::fs::write(crate::paths::logs().join("snapshots.jsonl"), format!("{}\n", snapshot(ts, mode, capital))).unwrap();
        };
        write("LIVE", 111.0);
        crate::paths::scope("watch", || write("dry run", 876.0));
        let latest = || samples("0xa").last().unwrap().capital_usd;
        assert_eq!(latest(), 111.0);
        assert_eq!(crate::paths::scope("watch", latest), 876.0);
        assert!(crate::paths::scope("empty", || samples("0xa")).is_empty());
        assert_eq!(latest(), 111.0);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_pool_parked_in_usdc_is_held_half_and_half() {
        // no positions (all parked): prices recorded in the snapshot; ETH-like tokens fall 10%
        let a = Sample { ts: 0, capital_usd: 2000.0, owed: 2000.0, px0: 2200.0, px1: 2000.0, ..Default::default() };
        let b = Sample { ts: 10_000, capital_usd: 2000.0, owed: 2000.0, px0: 1980.0, px1: 1800.0, ..Default::default() };
        let w = window(&[a, b], &[], 0, 10_000);
        // holding would be worth 1800: the pool, flat in USDC, is 200 ahead
        assert!((w["vs_hold_usd"].as_f64().unwrap() - 200.0).abs() < 1e-6, "{w}");
    }

    #[test]
    fn the_report_separates_money_put_in_from_profit() {
        let v: Vec<Sample> = (0..=4).map(|i| s(i * 10_000, [1000.0, 1010.0, 610.0, 615.0, 620.0][i as usize], 500.0, 500.0, 1.0, 1.0)).collect();
        let fl = vec![flow(15_000, -400.0, "withdrawal")];
        let r = pool_report(&v, &fl, &[(25_000, 4.0)], &[(0, 0.5, 0.25)], 0, 40_000, 100);
        let w = &r["window"];
        assert!((w["pnl_usd"].as_f64().unwrap() - 20.0).abs() < 1e-9, "{w}");
        assert_eq!(w["withdrawals_usd"].as_f64(), Some(400.0));
        assert_eq!(w["fees_claimed_usd"].as_f64(), Some(4.0));
        assert_eq!(w["gas_usd"].as_f64(), Some(0.5));
        let last = r["series"].as_array().unwrap().last().unwrap().clone();
        // capital 620, put in 1000 − 400, profit 20
        assert_eq!((last[1].as_f64(), last[2].as_f64(), last[3].as_f64()), (Some(620.0), Some(600.0), Some(20.0)));
    }

    #[test]
    fn a_glitch_that_undoes_itself_is_dropped_but_a_withdrawal_is_not() {
        let mk = |c: &[f64]| -> Vec<Sample> { c.iter().enumerate().map(|(i, c)| Sample { ts: i as i64 * 3600, capital_usd: *c, ..Default::default() }).collect() };
        let v = mk(&[8000.0, 8000.0, 12600.0, 12600.0, 8010.0, 8010.0]);
        let kept: Vec<f64> = despike(v.iter().collect(), &[]).iter().map(|s| s.capital_usd).collect();
        assert_eq!(kept, vec![8000.0, 8000.0, 8010.0, 8010.0]);
        // a real withdrawal (a flow) and a move that does not come back stay
        let v = mk(&[8000.0, 6000.0, 6000.0, 8000.0]);
        assert_eq!(despike(v.iter().collect(), &[flow(3000, -2000.0, "withdrawal"), flow(10_000, 2000.0, "deposit")]).len(), 4);
        let v = mk(&[8000.0, 5000.0, 5000.0, 5100.0]);
        assert_eq!(despike(v.iter().collect(), &[]).len(), 4);
    }

    #[test]
    fn a_noisy_valuation_at_the_ends_is_smoothed() {
        // a flat pool whose valuation jumps by ±40 cycle to cycle: the ends are each hour's median
        let caps = [1000.0, 1040.0, 960.0, 1000.0, 1000.0, 1000.0, 1040.0, 1000.0, 960.0, 1040.0];
        let v: Vec<Sample> = caps.iter().enumerate().map(|(i, c)| {
            let ts = if i < 5 { i as i64 * 600 } else { 100_000 + i as i64 * 600 };
            Sample { ts, capital_usd: *c, ..Default::default() }
        }).collect();
        let w = window(&v, &[], 0, 200_000);
        assert!(w["pnl_usd"].as_f64().unwrap().abs() < 1e-9, "{w}");
        assert_eq!(w["return_base_usd"], 1000.0);
    }

    /// The report on real logs: PERF_LOGS=<a logs folder> PERF_POOLS=<pools.json>, read only.
    /// PERF_AT can pin the end of the report to a Unix timestamp.
    #[test]
    #[ignore]
    fn report_on_real_logs() {
        let (Ok(dir), Ok(pools)) = (std::env::var("PERF_LOGS"), std::env::var("PERF_POOLS")) else { return };
        let pools: Value = serde_json::from_str(&std::fs::read_to_string(pools).unwrap()).unwrap();
        let pools = pools.as_array().cloned().or_else(|| pools["pools"].as_array().cloned()).unwrap();
        let names: HashMap<String, String> = pools.iter().filter_map(|p| Some((p["name"].as_str()?.to_string(), p["pool"].as_str()?.to_lowercase()))).collect();
        let mut st = Store::default();
        let as_of: Option<i64> = std::env::var("PERF_AT").ok().and_then(|v| v.parse().ok());
        let text = std::fs::read_to_string(format!("{dir}/snapshots.jsonl")).unwrap();
        let mut dec = |_: u64, _: &str| Some((18, 18));
        for line in text.lines().filter(|l| l.contains("\"pool_capital\"")) {
            if let Some(v) = serde_json::from_str::<Value>(line).ok().filter(|v| v["ts"].as_i64().is_some_and(|ts| as_of.is_none_or(|to| ts <= to))) {
                st.record(&v, &names, &mut dec);
            }
        }
        let mut events: Vec<Value> = std::fs::read_to_string(format!("{dir}/executor.jsonl")).unwrap().lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        append_extra(&mut events, Path::new(&dir));
        events.sort_by_key(|e| e["ts"].as_i64().unwrap_or(0));
        let to = as_of.unwrap_or_else(|| st.samples().values().filter_map(|v| v.last()).map(|s| s.ts).max().unwrap());
        let costs = HashMap::new();
        for days in [1i64, 7, 14, 30, 400] {
            let results = report_at(&st, &events, &pools, &costs, days, to);
            for r in results["pools"].as_array().unwrap() {
                let w = &r["window"];
                let name = r["name"].as_str().unwrap_or("");
                println!("{days:>3}d {name:<11} start {:>9.2} end {:>9.2} flows {:>9.2} (dep {:.2} wd {:.2} tr {:.2}) pnl {:>8.2} ret {:>6.2}% vs_hold {:>8.2} parked {:.0}% days {:.1}",
                    w["start_usd"].as_f64().unwrap_or(0.0), w["end_usd"].as_f64().unwrap_or(0.0), w["net_flows_usd"].as_f64().unwrap_or(0.0),
                    w["deposits_usd"].as_f64().unwrap_or(0.0), w["withdrawals_usd"].as_f64().unwrap_or(0.0), w["transfers_usd"].as_f64().unwrap_or(0.0),
                    w["pnl_usd"].as_f64().unwrap_or(0.0), w["return"].as_f64().unwrap_or(0.0) * 100.0, w["vs_hold_usd"].as_f64().unwrap_or(f64::NAN),
                    w["parked_share"].as_f64().unwrap_or(0.0) * 100.0, w["days"].as_f64().unwrap_or(0.0));
            }
        }
        for (addr, v) in flows_from(&st, st.mode(), &events, 0) {
            for f in v {
                println!("flow {} {} {:>9.2} {}", &addr[..addr.len().min(8)], f.ts, f.usd, f.kind);
            }
        }
        // the /api/pool_stats/<days> answer, for a page preview
        if let Ok(out) = std::env::var("PERF_OUT") {
            let mut all = serde_json::Map::new();
            for days in [7i64, 30, 90, 0] {
                all.insert(days.to_string(), report_at(&st, &events, &pools, &costs, days, to));
            }
            std::fs::write(out, Value::Object(all).to_string()).unwrap();
        }
    }

    #[test]
    fn one_sample_per_bucket_keeps_the_newest() {
        let mut v = vec![];
        for t in [0, 299, 100, 650, 300, 325] {
            push_sample(&mut v, Sample { ts: t, capital_usd: t as f64, ..Default::default() });
        }
        assert_eq!(v.iter().map(|x| x.ts).collect::<Vec<_>>(), vec![299, 325, 650]);
    }
}
