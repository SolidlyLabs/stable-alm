//! Strategy decision -> exact Solidly V3 transactions.
//! Modes (exit / rebalance / topup / keep), checks, partial resize, swaps through the
//! aggregator, mints with shrink-on-failure, reward sales and cross-pool funding.

use crate::chain::{self, TxError};
use crate::data::{self, Market};
use crate::eth::{self, addr, calldata, u, Tok};
use crate::ladder;
use crate::prices;
use crate::settings::PoolConfig;
use crate::tickmath::{amounts_at, liquidity_for_amounts, q96, sqrt_ratio_at_tick, to_f64, MAX_TICK, MIN_TICK};
use num_bigint::BigUint;
use num_traits::ToPrimitive;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEADLINE_S: u64 = 600;

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn big(v: i128) -> BigUint {
    BigUint::from(v.max(0) as u128)
}
fn small(v: &BigUint) -> i128 {
    v.to_i128().unwrap_or(i128::MAX)
}

// -- executor log ----------------------------------------------------------------------------------
pub fn log_path() -> std::path::PathBuf {
    crate::paths::logs().join("executor.jsonl")
}

pub fn log(mut event: Value) {
    let p = log_path();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    if let Some(o) = event.as_object_mut() {
        let mut full = serde_json::Map::new();
        full.insert("ts".into(), json!(now()));
        full.extend(std::mem::take(o));
        event = Value::Object(full);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{event}");
    }
}

/// Last `max_bytes` of the executor log as parsed events.
pub fn log_tail(max_bytes: u64) -> Vec<Value> {
    let Ok(mut f) = std::fs::File::open(log_path()) else { return vec![] };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(max_bytes)));
    let mut s = String::new();
    let _ = f.read_to_string(&mut s);
    s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn event_chain(e: &Value) -> u64 {
    e["chain_id"].as_u64().unwrap_or(crate::chains::BASE)
}

pub fn actions_today(cid: u64, pool: &str) -> i64 {
    let day = now() / 86400;
    log_tail(8 << 20)
        .iter()
        .filter(|e| {
            e["pool"].as_str() == Some(pool) && event_chain(e) == cid && e["kind"] == "rebalance" && e["live"] == true
                && e["ts"].as_u64().unwrap_or(0) / 86400 == day
        })
        .count() as i64
}

pub fn minted_ranges(cid: u64, pool: &str) -> HashSet<(i64, i64)> {
    let mut out = HashSet::new();
    for e in log_tail(3 << 20) {
        if e["kind"] != "sent" || event_chain(&e) != cid || !e["pool"].as_str().map(|p| p.eq_ignore_ascii_case(pool)).unwrap_or(false) {
            continue;
        }
        if let Some(r) = e["what"].as_str().and_then(|w| w.strip_prefix("mint ")) {
            if let Some((a, b)) = r.split_once(':') {
                if let (Ok(a), Ok(b)) = (a.parse(), b.parse()) {
                    out.insert((a, b));
                }
            }
        }
    }
    out
}

/// Open ranges (tl, tu, liquidity): history + executor-log mints, confirmed on-chain.
pub fn wallet_positions(cid: u64, pool: &str, wallet: &str) -> Result<Vec<(i64, i64, BigUint)>, String> {
    let mut keys: Vec<(i64, i64)> = data::history_ranges(cid, pool, wallet).union(&minted_ranges(cid, pool)).cloned().collect();
    keys.sort();
    // one multicall for every known range
    let calls: Vec<(String, String)> = keys.iter().map(|(tl, tu)| (pool.to_lowercase(), chain::position_calldata(wallet, *tl, *tu))).collect();
    // liquidity parked here by another pool's strategy (parking.rs) is that strategy's, not this pool's
    let parked = crate::parking::parked_in(cid, pool);
    let mut out = vec![];
    for ((tl, tu), r) in keys.into_iter().zip(chain::eth_calls(cid, &calls)?) {
        let mut liq = eth::word_u(&r?, 0);
        if let Some(p) = parked.get(&(tl, tu)) {
            liq = match u128::try_from(&liq) {
                Ok(l) => BigUint::from(crate::parking::own_share(l, *p)),
                Err(_) => liq - BigUint::from(*p), // more than u128 holds: larger than any lot
            };
        }
        if liq > BigUint::default() {
            out.push((tl, tu, liq));
        }
    }
    Ok(out)
}

pub fn cap_ranges(mut ranges: Vec<(i64, i64, f64)>, n: usize) -> Vec<(i64, i64, f64)> {
    ranges.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if n == 0 || ranges.len() <= n {
        return ranges;
    }
    let total: f64 = ranges.iter().map(|r| r.2).sum::<f64>();
    let total = if total == 0.0 { 1.0 } else { total };
    let mut groups: Vec<Vec<(i64, i64, f64)>> = vec![];
    let mut cur = vec![];
    let mut acc = 0.0;
    for r in ranges {
        cur.push(r);
        acc += r.2;
        if acc >= total * (groups.len() + 1) as f64 / n as f64 && groups.len() < n - 1 {
            groups.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    groups.iter().map(|g| (g[0].0, g[g.len() - 1].1, g.iter().map(|x| x.2).sum())).collect()
}

pub fn snap_ranges(ranges: Vec<(i64, i64, f64)>, spacing: i64) -> Vec<(i64, i64, f64)> {
    if spacing <= 1 {
        let mut r = ranges;
        r.sort_by(|a, b| a.partial_cmp(b).unwrap());
        return r;
    }
    let lo_b = -((-MIN_TICK).div_euclid(spacing)) * spacing;
    let hi_b = MAX_TICK.div_euclid(spacing) * spacing;
    let mut merged: Vec<((i64, i64), f64)> = vec![];
    for (tl, tu, w) in ranges {
        let a = (tl.div_euclid(spacing) * spacing).max(lo_b);
        let mut b = (-((-tu).div_euclid(spacing)) * spacing).min(hi_b);
        if b <= a {
            b = a + spacing;
        }
        match merged.iter_mut().find(|(k, _)| *k == (a, b)) {
            Some(x) => x.1 += w,
            None => merged.push(((a, b), w)),
        }
    }
    merged.sort_by(|a, b| a.0.cmp(&b.0));
    merged.into_iter().map(|((a, b), w)| (a, b, w)).collect()
}

/// Turn current positions into the target with the fewest movements (checked against the original
/// Python version in parity.rs).
pub fn resize_plan(
    current: &[(i64, i64, i128, f64)],
    target: &[(i64, i64, f64)],
    budget0: f64,
    tol: f64,
    dust: f64,
) -> (Vec<(i64, i64, i128)>, Vec<(i64, i64, f64)>) {
    let mut goal: Vec<((i64, i64), f64)> = vec![];
    for (tl, tu, w) in target {
        match goal.iter_mut().find(|(k, _)| *k == (*tl, *tu)) {
            Some(g) => g.1 += w * budget0,
            None => goal.push(((*tl, *tu), w * budget0)),
        }
    }
    let (mut burns, mut adds, mut held) = (vec![], vec![], HashSet::new());
    for (tl, tu, liq, val) in current {
        let key = (*tl, *tu);
        let g = goal.iter().find(|(k, _)| *k == key).map(|x| x.1);
        let Some(g) = g.filter(|_| !held.contains(&key)) else {
            burns.push((*tl, *tu, *liq));
            continue;
        };
        held.insert(key);
        let delta = g - val;
        if delta < -tol {
            let cut = if g < dust || *val <= 0.0 { *liq } else { (*liq).min((*liq as f64 * (1.0 - g / val)) as i128) };
            if cut > 0 {
                burns.push((*tl, *tu, cut));
            }
        } else if delta > tol {
            adds.push((*tl, *tu, delta));
        }
    }
    for ((a, b), g) in &goal {
        if !held.contains(&(*a, *b)) && *g > dust {
            adds.push((*a, *b, *g));
        }
    }
    adds.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (burns, adds)
}

// -- executor --------------------------------------------------------------------------------------
pub enum StepErr {
    Abort(String),
    Fail(String),
}
impl From<String> for StepErr {
    fn from(s: String) -> Self {
        StepErr::Fail(s)
    }
}
impl From<TxError> for StepErr {
    fn from(e: TxError) -> Self {
        // Paused, or an earlier transaction still unresolved: stop the pass instead of retrying.
        if e.reason == "paused" || e.reason == "pending" {
            StepErr::Abort(e.message)
        } else {
            StepErr::Fail(e.message)
        }
    }
}
impl std::fmt::Display for StepErr {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            StepErr::Abort(s) | StepErr::Fail(s) => f.write_str(s),
        }
    }
}
type Res<T> = Result<T, StepErr>;

pub struct Target {
    pub ranges: Vec<(i64, i64, f64)>,
    pub swap: bool,
}

pub struct Executor {
    pub cfg: PoolConfig,
    pub cid: u64,
    pub wallet: String,
    pub settings: Value,
    pub execute: bool,
    account: Option<eth::Account>,
    pub t0: String,
    pub t1: String,
    pub spacing: i64,
    pub dec0: i32,
    pub dec1: i32,
    unit: f64,
    unit1: f64,
    budget_usd: Option<f64>,
    /// What this run may not touch (capital.rs): per token, the wallet's balance beyond the pool's own
    /// tokens. None: the whole wallet (the backtester's and parking's own executors).
    reserve: Option<crate::capital::Tokens>,
    attempts: usize,
    backoff: f64,
    virt: HashMap<String, i128>,
    /// Allowances as this run knows them: set by its own approvals and lowered by its own spends, so
    /// a read from an RPC node a block behind cannot hide an allowance a mint just used up. Unknown
    /// until this run approves or spends: then the chain's figure is read.
    virt_allow: HashMap<(String, String), i128>,
    v_deployed: f64,
    v_removed: f64,
    withdrawn: HashSet<(i64, i64)>,
    pub pos_val0: f64,
    pub report: Vec<String>,
    pub is_solid_lp: bool,
    pub solid_pair: Option<(String, String)>, // (solid, pair) for the SOLID LP usable() rule
    /// Transactions handed to the network in this pass (counted before broadcasting). A step that
    /// failed after broadcasting is never retried: its transaction may have gone through.
    broadcasts: usize,
    /// the pass's rebalance entry, logged with its first transaction: a pass that sends nothing
    /// is not an action (it neither shows in the activity nor counts against the daily cap)
    begun: Option<Value>,
}

impl Executor {
    pub fn new(cfg: PoolConfig, cid: u64, wallet: &str, settings: &Value, execute: bool, budget_usd: Option<f64>) -> Result<Executor, String> {
        let account = if execute { Some(crate::signer::load_account()?) } else { None };
        let wallet = wallet.to_lowercase();
        if let Some(a) = &account {
            if a.address.to_lowercase() != wallet {
                return Err(format!("signer {} does not match the configured wallet {wallet}", a.address));
            }
        }
        let (t0, t1) = chain::pool_tokens(cid, &cfg.pool)?;
        let spacing = chain::tick_spacing(cid, &cfg.pool)?;
        let dec0 = prices::meta(cid, &t0)?.decimals as i32;
        let dec1 = prices::meta(cid, &t1)?.decimals as i32;
        Ok(Executor {
            cid,
            wallet,
            settings: settings.clone(),
            execute,
            account,
            t0,
            t1,
            spacing,
            dec0,
            dec1,
            unit: 10f64.powi(dec0),
            unit1: 10f64.powi(dec1),
            budget_usd,
            reserve: None,
            attempts: settings["retries"]["attempts"].as_f64().unwrap_or(3.0).max(1.0) as usize,
            backoff: settings["retries"]["backoff_s"].as_f64().unwrap_or(10.0),
            virt: HashMap::new(),
            virt_allow: HashMap::new(),
            v_deployed: 0.0,
            v_removed: 0.0,
            withdrawn: HashSet::new(),
            pos_val0: 0.0,
            report: vec![],
            is_solid_lp: false,
            solid_pair: None,
            broadcasts: 0,
            begun: None,
            cfg,
        })
    }

    pub fn say(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        crate::info!(&format!("pool.{}", self.cfg.name), "{m}");
        self.report.push(m);
    }
    pub fn say_warn(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        crate::warn!(&format!("pool.{}", self.cfg.name), "{m}");
        self.report.push(m);
    }
    pub fn say_err(&mut self, msg: impl Into<String>) {
        let m = msg.into();
        crate::error!(&format!("pool.{}", self.cfg.name), "{m}");
        self.report.push(m);
    }

    /// The pool's own balance of `token`: the wallet's, less what is reserved for other pools and
    /// Unallocated (capital.rs).
    pub fn bal(&self, token: &str) -> Res<i128> {
        let t = token.to_lowercase();
        let raw = small(&chain::erc20_balance(self.cid, token, &self.wallet)?) + self.virt.get(&t).copied().unwrap_or(0);
        Ok(match self.reserve.as_ref().and_then(|r| r.get(&t)) {
            Some(r) => (raw - r).max(0),
            None => raw,
        })
    }

    /// USD value of the funding tokens (other than the pool's own two) that the pool holds.
    fn funding_idle_usd(&self) -> f64 {
        crate::settings::funding_tokens(&self.settings, self.cid).into_iter()
            .filter(|t| *t != self.t0 && *t != self.t1)
            .filter_map(|t| {
                let b = self.bal(&t).ok()?.max(0);
                if b == 0 || self.reserve.is_none() {
                    return None; // without a capital book the whole wallet would count
                }
                Some(b as f64 / 10f64.powi(prices::meta(self.cid, &t).ok()?.decimals as i32) * prices::usd(self.cid, &t).ok()?)
            })
            .sum()
    }

    /// The pool's capital for this run: its budget (USD) and what it may not touch.
    pub fn set_capital(&mut self, budget_usd: f64, reserve: crate::capital::Tokens) {
        self.budget_usd = Some(budget_usd);
        self.reserve = Some(reserve);
    }

    fn mv(&mut self, token: &str, delta: i128) {
        if !self.execute {
            *self.virt.entry(token.to_lowercase()).or_default() += delta;
        }
    }

    pub fn price(&self) -> Res<(BigUint, i64, u64, f64)> {
        let (sp, tick, fee) = chain::slot0(self.cid, &self.cfg.pool)?;
        let p = (to_f64(&sp) / 2f64.powi(96)).powi(2);
        Ok((sp, tick, fee, p))
    }

    pub fn value0(&self, x: f64, y: f64, p: f64) -> f64 {
        (x + y / p) / self.unit
    }

    fn deployed_value(&self) -> Res<f64> {
        if !self.execute {
            return Ok(self.pos_val0 - self.v_removed + self.v_deployed);
        }
        let (sp, _, _, p) = self.price()?;
        let mut v = 0.0;
        for (tl, tu, liq) in wallet_positions(self.cid, &self.cfg.pool, &self.wallet)? {
            let (a0, a1) = amounts_at(&sp, tl, tu, &liq);
            v += self.value0(to_f64(&a0), to_f64(&a1), p);
        }
        Ok(v)
    }

    pub fn usable(&self, budget0: f64, p: f64) -> Res<(i128, i128)> {
        if self.is_solid_lp {
            return self.solid_usable();
        }
        let (b0, b1) = (self.bal(&self.t0)?.max(0), self.bal(&self.t1)?.max(0));
        let have = self.value0(b0 as f64, b1 as f64, p);
        let room = budget0 - self.deployed_value()?;
        if have <= 0.0 || room <= 0.0 {
            return Ok((0, 0));
        }
        let k = (room / have).min(1.0);
        Ok(((b0 as f64 * k) as i128, (b1 as f64 * k) as i128))
    }

    fn solid_usable(&self) -> Res<(i128, i128)> {
        let (solid, pair) = self.solid_pair.clone().unwrap();
        let (sp, _, _, _) = self.price()?;
        let (tl, tu) = crate::solid_lp::full_range(self.spacing);
        let (sa, sb) = (sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu));
        let (s_amt, p_amt) = (self.bal(&solid)?.max(0), self.bal(&pair)?.max(0));
        let huge = BigUint::from(10u8).pow(60);
        if self.t0 == solid {
            let l = liquidity_for_amounts(&sp, &sa, &sb, &big(s_amt), &huge);
            let m1 = small(&crate::tickmath::amounts_for_liquidity(&sp, &sa, &sb, &l).1);
            Ok((s_amt, p_amt.min(m1 * 1005 / 1000)))
        } else {
            let l = liquidity_for_amounts(&sp, &sa, &sb, &huge, &big(s_amt));
            let m0 = small(&crate::tickmath::amounts_for_liquidity(&sp, &sa, &sb, &l).0);
            Ok((p_amt.min(m0 * 1005 / 1000), s_amt))
        }
    }

    pub fn retry<T>(&mut self, what: &str, mut f: impl FnMut(&mut Self) -> Res<T>) -> Res<T> {
        let mut last = String::new();
        for attempt in 1..=self.attempts {
            let sent_before = self.broadcasts;
            match f(self) {
                Ok(v) => return Ok(v),
                Err(StepErr::Abort(m)) => return Err(StepErr::Abort(m)),
                Err(StepErr::Fail(e)) if self.broadcasts > sent_before => {
                    // A transaction of this step was broadcast: repeating the step could do it twice.
                    let msg = format!("{what}: failed after its transaction was sent ({e}); not retrying");
                    self.say_err(format!("  {msg}"));
                    log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "error", "what": what, "attempt": attempt, "error": e,
                               "after_broadcast": true}));
                    return Err(StepErr::Abort(msg));
                }
                Err(StepErr::Fail(e)) => {
                    let fin = attempt == self.attempts;
                    let msg = format!("  {what}: attempt {attempt}/{} failed: {e}", self.attempts);
                    if fin {
                        self.say_err(msg)
                    } else {
                        self.say_warn(msg)
                    }
                    log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": if fin {"error"} else {"retry"}, "what": what,
                               "attempt": attempt, "error": e}));
                    last = e;
                    if !fin {
                        std::thread::sleep(Duration::from_secs_f64(self.backoff * attempt as f64));
                    }
                }
            }
        }
        Err(StepErr::Abort(format!("{what} failed after {} attempts: {last}", self.attempts)))
    }

    /// Simulate, then send if executing. `live` = this step can be simulated on the current chain.
    pub fn tx(&mut self, to: &str, data: &str, what: &str, live: bool) -> Result<Value, TxError> {
        let mut entry = json!({"chain_id": self.cid, "pool": self.cfg.pool, "what": what, "to": to, "data": data});
        let mut gas = 0;
        if live || self.execute {
            gas = chain::simulate(self.cid, &self.wallet, to, data)?;
            entry["gas"] = json!(gas);
            self.say(format!("    simulated OK, gas {}", thousands(gas)));
        } else {
            self.say("    (projected: rebuilt and simulated live after the previous step)");
        }
        if self.execute {
            if let Some(why) = crate::paths::execution_blocked() {
                return Err(chain::err(format!("not sent: {why}"), "paused"));
            }
            if let Err(why) = crate::agent::gate::pre_sign(self.cid, Some(&self.cfg.pool), what) {
                return Err(chain::err(format!("not sent: {why}"), "gate"));
            }
            if let Some(b) = self.begun.take() {
                log(b);
            }
            self.broadcasts += 1;
            let res = chain::send(self.cid, self.account.as_ref().unwrap(), to, data, gas)?;
            entry["kind"] = json!("sent");
            entry["hash"] = json!(res.hash);
            entry["block"] = json!(res.block);
            entry["gas_used"] = json!(res.gas_used);
            self.say(format!("    sent {} (block {}, gas {})", res.hash, res.block, thousands(res.gas_used)));
            log(entry.clone());
            entry["receipt"] = res.receipt;
            return Ok(entry);
        }
        entry["kind"] = json!("dry_run");
        if let Some(b) = self.begun.take() {
            log(b);
        }
        log(entry.clone());
        Ok(entry)
    }

    // -- checks ------------------------------------------------------------------------------------
    fn checks(&self, m: &Market, tick: i64) -> Res<Vec<String>> {
        let mut problems = vec![];
        if crate::paths::stop_file().exists() || std::env::var("ALM_DISABLED").as_deref() == Ok("1") {
            problems.push("kill switch is on (STOP file or ALM_DISABLED=1)".to_string());
        }
        let last = *m.ts.last().unwrap();
        let mut recent: Vec<f64> = m.ts.iter().zip(&m.tick).filter(|(t, _)| **t >= last - 3600).map(|(_, k)| *k as f64).collect();
        let med = if recent.is_empty() { tick as f64 } else { data::median(&mut recent) };
        if (tick as f64 - med).abs() > self.cfg.max_spot_dev_ticks as f64 {
            problems.push(format!("spot tick {tick} is {:.0} ticks from the 1h median {med:.0} (possible manipulation or fast move)", (tick as f64 - med).abs()));
        }
        let gas = to_f64(&chain::eth_balance(self.cid, &self.wallet)?);
        let c = crate::chains::get(self.cid);
        if gas < c.min_gas_wei {
            problems.push(format!("wallet has < {} {} for gas", c.min_gas_wei / 1e18, c.gas_symbol));
        }
        if actions_today(self.cid, &self.cfg.pool) >= self.cfg.max_actions_per_day {
            problems.push("daily rebalance cap reached".into());
        }
        // experimental features: safety locks, emergency exits, engine caps, the risk controller's heartbeat
        problems.extend(crate::agent::gate::engine_problems(self.cid, &self.cfg.pool, self.budget_usd.unwrap_or(0.0), self.cfg.max_actions_per_day));
        Ok(problems)
    }

    fn blocked(&mut self, m: &Market, tick: i64, exit_only: bool) -> Res<bool> {
        let mut problems = self.checks(m, tick)?;
        if exit_only {
            problems.retain(|x| x.contains("kill switch") || x.contains("for gas"));
        }
        for x in &problems {
            self.say_warn(format!("  BLOCKED: {x}"));
        }
        if !problems.is_empty() {
            log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "blocked", "problems": problems}));
        }
        Ok(!problems.is_empty())
    }

    fn begin(&mut self, mode: &str, target: Option<&Target>) {
        let ranges: Vec<Value> = target.map(|t| t.ranges.iter().map(|(a, b, w)| json!([a, b, w])).collect()).unwrap_or_default();
        self.begun = Some(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "rebalance", "mode": mode, "live": self.execute, "ranges": ranges}));
    }

    fn fmt0(&self, v: f64) -> String {
        format!("{:.4}", v / self.unit)
    }
    fn fmt1(&self, v: f64) -> String {
        format!("{:.4}", v / self.unit1)
    }

    // -- main --------------------------------------------------------------------------------------
    pub fn run(&mut self, m: &Market) -> Res<Value> {
        let (sp, tick, fee, p) = self.price()?;
        let positions = wallet_positions(self.cid, &self.cfg.pool, &self.wallet)?;
        let (b0, b1) = (self.bal(&self.t0)?, self.bal(&self.t1)?);
        self.say(format!("[{}] tick {tick}  fee {fee} pips  wallet {} t0 + {} t1", self.cfg.name, commas(b0 as f64 / self.unit), commas(b1 as f64 / self.unit1)));
        for (tl, tu, liq) in &positions {
            let (a0, a1) = amounts_at(&sp, *tl, *tu, liq);
            let (a0, a1) = (to_f64(&a0), to_f64(&a1));
            self.pos_val0 += self.value0(a0, a1, p);
            let state = if *tl <= tick && tick < *tu { "IN RANGE" } else { "out of range" };
            self.say(format!("  position [{tl},{tu}) {} t0 + {} t1 {state}", commas(a0 / self.unit), commas(a1 / self.unit1)));
        }
        let pos_val = self.pos_val0;
        let usd0 = prices::usd(self.cid, &self.t0)?;
        // funding tokens the pool holds (USDC moved in, say) count as idle: deploying swaps them in
        let idle0 = self.value0(b0 as f64, b1 as f64, p) + self.funding_idle_usd() / usd0;
        let budget0 = self.budget_usd.map(|b| b / usd0).unwrap_or(pos_val + idle0);
        self.say(format!("  deployed {}  idle {}  budget {} (token0 units: the pool's capital)", commas(pos_val), commas(idle0), commas(budget0)));
        let pos_json: Vec<Value> = positions.iter().map(|(a, b, l)| json!([a, b, l.to_string()])).collect();
        let mut summary = json!({"chain_id": self.cid, "pool": self.cfg.pool, "name": self.cfg.name, "tick": tick, "fee_pips": fee,
                                 "deployed0": pos_val, "idle0": idle0, "budget0": budget0, "usd0": usd0, "positions": pos_json, "mode": "keep"});
        if (budget0 <= 0.0 || !self.cfg.enabled) && !positions.is_empty() {
            self.say("  pool switched off: withdrawing all positions");
            summary["mode"] = json!("exit");
            if !self.blocked(m, tick, true)? {
                self.begin("exit", None);
                let all: Vec<(i64, i64, i128)> = positions.iter().map(|(a, b, l)| (*a, *b, small(l))).collect();
                self.burn_all(&all)?;
            }
            return Ok(summary);
        }
        if budget0 <= 0.0 || !self.cfg.enabled {
            return Ok(summary);
        }
        // custom rules or a node strategy: an old plain ladder became a ladder block on load (ladder.rs)
        self.run_custom(m, positions, summary, budget0, idle0, pos_val, &sp, p, tick)
    }

    // -- custom mode (custom.rs) -------------------------------------------------------------------
    #[allow(clippy::too_many_arguments)]
    fn run_custom(&mut self, m: &Market, positions: Vec<(i64, i64, BigUint)>, mut summary: Value, budget0: f64, idle0: f64, pos_val: f64,
                  sp: &BigUint, p: f64, tick: i64) -> Res<Value> {
        // custom rules or a node strategy (graph.rs): the same decisions, timers and actions
        let is_graph = self.cfg.strategy.get("type").and_then(|t| t.as_str()) == Some("graph");
        let rules = if is_graph { None } else { Some(crate::custom::Rules::from(&self.cfg.strategy, self.spacing)?) };
        let graph = if is_graph { Some(crate::graph::Graph::parse(&self.cfg.strategy, self.spacing)?) } else { None };
        let label = rules.as_ref().map(|r| r.label()).or_else(|| graph.as_ref().map(|g| g.label())).unwrap_or_default();
        let mut exit_to = rules.as_ref().map(|r| r.exit_to).or_else(|| graph.as_ref().map(|g| g.exit_to)).unwrap_or(crate::custom::ExitTo::Keep);
        if let Some(g) = rules.as_ref().and_then(|r| r.guard) {
            let med = data::rolling_median_trailing(&m.bar_tick, data::week_bars(m), 12);
            if let (Some(t), Some(md)) = (m.bar_tick.last(), med.last()) {
                if (*t as f64 - md).abs() > g {
                    self.say(format!("  DEPEG GUARD: holding, no action (tick {tick} is {:.0} ticks from the 7-day median)", (*t as f64 - md).abs()));
                    log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "guard_freeze", "tick": tick}));
                    summary["mode"] = json!("guard");
                    return Ok(summary);
                }
            }
        }
        let now = now() as i64;
        let mut st = crate::custom::load_state(self.cid, &self.cfg.pool, self.execute);
        let range = (!positions.is_empty()).then(|| (positions.iter().map(|x| x.0).min().unwrap(), positions.iter().map(|x| x.1).max().unwrap()));
        let d = match (&rules, &graph) {
            (Some(r), _) => {
                let d = crate::custom::decide(r, &mut st, now, tick, range, self.spacing);
                summary["custom"] = json!({"decision": format!("{d:?}"), "range": range, "band": range.map(|(a, b)| r.band(a, b))});
                d
            }
            (None, Some(g)) => {
                let (ext, errs) = crate::graph::load_externals(g, self.cid, m);
                for e in errs {
                    self.say_warn(format!("  watched token unavailable: {e}"));
                }
                let ev = crate::graph::evaluate_ext(g, m, &ext);
                let bar = m.bar_tick.len().checked_sub(1).ok_or_else(|| "no bars in the replay market".to_string())?;
                let d = crate::graph::decide(g, &ev, &mut st, now, bar, tick, range, self.spacing);
                exit_to = crate::graph::exit_to(g, &ev, bar, tick, range);
                summary["custom"] = json!({"decision": format!("{d:?}"), "range": range, "wanted": crate::graph::wanted(g, &ev, bar, self.spacing)});
                d
            }
            _ => unreachable!(),
        };
        let save = |s: &Self, st: &crate::custom::State| crate::custom::save_state(s.cid, &s.cfg.pool, s.execute, st);
        summary["parked"] = json!(crate::parking::of_origin(self.execute, self.cid, &self.cfg.pool));
        match d {
            crate::custom::Decision::Move { tl, tu, reason } => {
                summary["mode"] = json!("rebalance");
                self.say(format!("  {label}: {reason}"));
                save(self, &st);
                if self.blocked(m, tick, false)? {
                    return Ok(summary);
                }
                let ranges = match graph.as_ref().and_then(|g| g.ladder()) {
                    // a ladder block: the ladder's own solve, taken when it moves enough of the value
                    Some(lp) => {
                        let spf = to_f64(sp) / 2f64.powi(96);
                        let r = cap_ranges(snap_ranges(ladder::decide(m, &lp, budget0, spf, tick), self.spacing), self.cfg.max_ranges);
                        let moved = if positions.is_empty() || st.exited || st.force_solve { 1.0 } else { self.moved_share(&positions, sp, p, &r) };
                        if moved < lp.turnover {
                            self.say(format!("  ladder target differs by {:.0}% < turnover {:.0}%: keep", moved * 100.0, lp.turnover * 100.0));
                            summary["mode"] = json!("keep");
                            st.moved(now);
                            save(self, &st);
                            return Ok(summary);
                        }
                        r
                    }
                    None => vec![(tl, tu, 1.0)],
                };
                let t = Target { ranges, swap: true };
                self.begin("rebalance", Some(&t));
                // what waited in a parking pool comes back first (parking.rs); a claim's dollars were
                // out of this pool's budget while another pool held them (worker): they count again
                let owed = crate::parking::claims_usd(self.execute, self.cid, &self.cfg.pool).1;
                self.unpark_all()?;
                let back = owed - crate::parking::claims_usd(self.execute, self.cid, &self.cfg.pool).1;
                let budget0 = if back > 0.0 { budget0 + back / prices::usd(self.cid, &self.t0)? } else { budget0 };
                self.resize(&positions, &t, budget0, sp, p)?;
                st.moved(now);
                save(self, &st);
            }
            crate::custom::Decision::Exit(reason) => {
                summary["mode"] = json!("exit");
                self.say(format!("  {label}: EXIT: {reason}"));
                save(self, &st);
                if self.blocked(m, tick, true)? {
                    return Ok(summary);
                }
                self.begin("exit", None);
                // switching sides: a parked lot comes back before the swap
                self.unpark_all()?;
                let all: Vec<(i64, i64, i128)> = positions.iter().map(|(a, b, l)| (*a, *b, small(l))).collect();
                self.burn_all(&all)?;
                if exit_to == crate::custom::ExitTo::Stable {
                    // both tokens sold for the chain's dollar stable, which waits as a funding token
                    // (parked below) and buys the pair back on re-entry
                    self.exit_to_stable(budget0)?;
                }
                let into = match exit_to {
                    crate::custom::ExitTo::Token0 => Some((self.t1.clone(), self.t0.clone(), true)),
                    crate::custom::ExitTo::Token1 => Some((self.t0.clone(), self.t1.clone(), false)),
                    crate::custom::ExitTo::Keep | crate::custom::ExitTo::Stable => None,
                };
                if let Some((tin, tout, to0)) = into {
                    let (_, _, _, p) = self.price()?;
                    // at most this pool's budget: the wallet's other tokens belong to other pools
                    let amt = self.bal(&tin)?.max(0).min(self.budget_raw(&tin, budget0, p));
                    let fair = if to0 { amt as f64 / p } else { amt as f64 * p };
                    if amt > 0 {
                        let max_cost = self.cfg.max_swap_cost_bps;
                        if let Err(e) = self.retry("exit swap", |s| s.swap(&tin, &tout, amt, fair, max_cost, "exit swap")) {
                            self.say_warn(format!("  exit swap skipped: {e}; the tokens stay as they are"));
                        }
                    }
                }
                st.exited_to(exit_to);
                // the minimum time out counts from the exit itself
                st.out_since = Some(crate::engine::now() as i64);
                save(self, &st);
                let side = match exit_to {
                    crate::custom::ExitTo::Token0 => crate::parking::Side::Token0,
                    crate::custom::ExitTo::Token1 => crate::parking::Side::Token1,
                    crate::custom::ExitTo::Keep | crate::custom::ExitTo::Stable => crate::parking::Side::Both,
                };
                if let Err(e) = self.park_pass(side, budget0, 0.0) {
                    self.say_warn(format!("  parking failed: {e}; the tokens wait in the wallet (retried next cycle)"));
                }
            }
            crate::custom::Decision::Wait(reason) => {
                summary["mode"] = json!("wait");
                self.say(format!("  {label}: waiting: {reason}"));
                log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "keep", "tick": tick, "reason": reason}));
                save(self, &st);
            }
            crate::custom::Decision::Keep(reason) => {
                save(self, &st);
                let idle_usable = idle0.min((budget0 - pos_val).max(0.0));
                let thr = budget0 * self.settings["idle_redeploy_pct"].as_f64().unwrap_or(2.0) / 100.0;
                if !st.exited && !positions.is_empty() && idle_usable > thr.max(self.cfg.min_swap_value) {
                    summary["mode"] = json!("topup");
                    let t = self.current_as_target(&positions, sp, p).unwrap();
                    self.say(format!("  {} idle (> {}): deploying into the current range", commas(idle_usable), commas(thr)));
                    if self.blocked(m, tick, false)? {
                        return Ok(summary);
                    }
                    self.begin("topup", Some(&t));
                    self.deploy(&t, budget0)?;
                } else if !st.exited && pos_val > budget0 * 1.10 && !positions.is_empty() {
                    summary["mode"] = json!("rebalance");
                    self.say(format!("  pool holds {} > 110% of its budget {}: rebalance down to budget", commas(pos_val), commas(budget0)));
                    if self.blocked(m, tick, false)? {
                        return Ok(summary);
                    }
                    let t = self.current_as_target(&positions, sp, p).unwrap();
                    self.begin("rebalance", Some(&t));
                    self.resize(&positions, &t, budget0, sp, p)?;
                } else {
                    summary["mode"] = json!(if st.exited { "exited" } else { "keep" });
                    self.say(format!("  {label}: {reason}"));
                    log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "keep", "tick": tick}));
                    // what it holds and does not use waits parked until a move needs it
                    let side = if !st.exited || st.exit_side == 0 || st.exit_side == 2 {
                        crate::parking::Side::Both
                    } else if st.exit_side > 0 {
                        crate::parking::Side::Token0
                    } else {
                        crate::parking::Side::Token1
                    };
                    if let Err(e) = self.park_pass(side, budget0, if positions.is_empty() { 0.0 } else { pos_val }) {
                        self.say_warn(format!("  parking failed: {e}; retried next cycle"));
                    }
                }
            }
        }
        Ok(summary)
    }

    // -- targets -----------------------------------------------------------------------------------
    /// Share of value that moves between the positions and `ranges` (0 = same, 1 = all).
    fn moved_share(&self, positions: &[(i64, i64, BigUint)], sp: &BigUint, p: f64, ranges: &[(i64, i64, f64)]) -> f64 {
        let mut cur_w: HashMap<i64, f64> = HashMap::new();
        for (tl, tu, liq) in positions {
            let (a0, a1) = amounts_at(sp, *tl, *tu, liq);
            let v = self.value0(to_f64(&a0), to_f64(&a1), p);
            for t in *tl..*tu {
                *cur_w.entry(t).or_default() += v / (tu - tl) as f64;
            }
        }
        let s: f64 = cur_w.values().sum::<f64>();
        let s = if s == 0.0 { 1.0 } else { s };
        let mut new_w: HashMap<i64, f64> = HashMap::new();
        for (a, c, w) in ranges {
            for t in *a..*c {
                *new_w.entry(t).or_default() += w / (c - a) as f64;
            }
        }
        let keys: HashSet<i64> = cur_w.keys().chain(new_w.keys()).cloned().collect();
        keys.iter().map(|t| (new_w.get(t).unwrap_or(&0.0) - cur_w.get(t).unwrap_or(&0.0) / s).abs()).sum::<f64>() / 2.0
    }

    fn current_as_target(&self, positions: &[(i64, i64, BigUint)], sp: &BigUint, p: f64) -> Option<Target> {
        if positions.is_empty() {
            return None;
        }
        let vals: Vec<(i64, i64, f64)> = positions
            .iter()
            .map(|(tl, tu, liq)| {
                let (a0, a1) = amounts_at(sp, *tl, *tu, liq);
                (*tl, *tu, self.value0(to_f64(&a0), to_f64(&a1), p))
            })
            .collect();
        let s: f64 = vals.iter().map(|v| v.2).sum::<f64>();
        let s = if s == 0.0 { 1.0 } else { s };
        Some(Target { ranges: vals.into_iter().map(|(a, b, v)| (a, b, v / s)).collect(), swap: true })
    }

    // -- a. resize / withdraw ------------------------------------------------------------------------
    fn resize(&mut self, positions: &[(i64, i64, BigUint)], target: &Target, budget0: f64, sp: &BigUint, p: f64) -> Res<()> {
        let ws: f64 = target.ranges.iter().map(|r| r.2).sum();
        let ranges: Vec<(i64, i64, f64)> = target.ranges.iter().map(|(a, b, w)| (*a, *b, w / ws)).collect();
        let current: Vec<(i64, i64, i128, f64)> = positions
            .iter()
            .map(|(tl, tu, liq)| {
                let (a0, a1) = amounts_at(sp, *tl, *tu, liq);
                (*tl, *tu, small(liq), self.value0(to_f64(&a0), to_f64(&a1), p))
            })
            .collect();
        let tol = self.cfg.min_swap_value.max(budget0 * self.settings["resize_tolerance_pct"].as_f64().unwrap_or(1.0) / 100.0);
        let (burns, adds) = resize_plan(&current, &ranges, budget0, tol, self.cfg.min_swap_value);
        let liq_of: HashMap<(i64, i64), i128> = current.iter().map(|(a, b, l, _)| ((*a, *b), *l)).collect();
        let partial = burns.iter().filter(|(a, b, c)| *c < liq_of[&(*a, *b)]).count();
        let into = adds.iter().filter(|(a, b, _)| liq_of.contains_key(&(*a, *b))).count();
        self.say(format!("  plan: withdraw {} position(s), trim {partial}, add to {into} existing, open {} new, leave {} as they are (tolerance {:.4})",
                         burns.len() - partial, adds.len() - into, positions.len() - burns.len() - into, tol));
        let bj: Vec<Value> = burns.iter().map(|(a, b, c)| json!([a, b, c.to_string()])).collect();
        let aj: Vec<Value> = adds.iter().map(|(a, b, v)| json!([a, b, v])).collect();
        // the share of the pool's capital this move redeploys (the engine caps count it per day)
        let turnover = if budget0 > 0.0 { adds.iter().map(|x| x.2).sum::<f64>() / budget0 } else { 0.0 };
        log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "plan", "burns": bj, "adds": aj, "tolerance": tol, "turnover": turnover, "live": self.execute}));
        self.burn_all(&burns)?;
        if adds.is_empty() {
            self.say("  nothing to deposit; withdrawn tokens stay in the wallet");
            return Ok(());
        }
        self.deploy(&Target { ranges: adds, swap: target.swap }, budget0)
    }

    pub fn burn_all(&mut self, positions: &[(i64, i64, i128)]) -> Res<()> {
        let mut last_block = 0;
        for (tl, tu, liq) in positions.to_vec() {
            let res = self.retry(&format!("withdraw [{tl},{tu})"), |s| s.burn_one(tl, tu, liq))?;
            if let Some(b) = res.as_ref().and_then(|r| r["block"].as_u64()) {
                last_block = last_block.max(b);
            }
        }
        if self.execute && last_block > 0 {
            let t0 = std::time::Instant::now();
            while crate::rpc::block_number(self.cid).unwrap_or(u64::MAX) <= last_block && t0.elapsed() < Duration::from_secs(60) {
                std::thread::sleep(Duration::from_secs(1));
            }
            std::thread::sleep(Duration::from_secs(4));
        }
        Ok(())
    }

    fn burn_one(&mut self, tl: i64, tu: i64, amount: i128) -> Res<Option<Value>> {
        if self.withdrawn.contains(&(tl, tu)) {
            self.say(format!("  a. [{tl},{tu}) already withdrawn this pass; skip"));
            return Ok(None);
        }
        let have = small(&chain::position_liquidity(self.cid, &self.cfg.pool, &self.wallet, tl, tu)?);
        let liq = amount.min(have);
        if liq == 0 {
            self.say(format!("  a. [{tl},{tu}) already empty; skip"));
            return Ok(None);
        }
        let (sp, _, _, p) = self.price()?;
        let slip = self.cfg.slippage_bps / 1e4;
        let (a0, a1) = amounts_at(&sp, tl, tu, &big(liq));
        let (a0, a1) = (small(&a0), small(&a1));
        let data = calldata(
            "burnAndCollect(address,int24,int24,uint128,uint256,uint256,uint128,uint128,uint256)",
            &[addr(&self.wallet), chain::int(tl), chain::int(tu), u(big(liq)), u(big((a0 as f64 * (1.0 - slip)) as i128)),
              u(big((a1 as f64 * (1.0 - slip)) as i128)), Tok::Uint(chain::max_u128()), Tok::Uint(chain::max_u128()), u(now() + DEADLINE_S)],
        );
        let part = if liq < have { format!(" ({:.0}% of the position)", liq as f64 / have as f64 * 100.0) } else { String::new() };
        self.say(format!("  a. burnAndCollect [{tl},{tu}) liquidity {liq}{part} -> ~{} t0 + {} t1", commas(a0 as f64 / self.unit), commas(a1 as f64 / self.unit1)));
        let pool = self.cfg.pool.clone();
        let res = self.tx(&pool, &data, &format!("burn {tl}:{tu}"), true)?;
        if self.execute {
            self.withdrawn.insert((tl, tu));
        }
        let (t0, t1) = (self.t0.clone(), self.t1.clone());
        self.mv(&t0, a0);
        self.mv(&t1, a1);
        self.v_removed += self.value0(a0 as f64, a1 as f64, p);
        Ok(Some(res))
    }

    // -- c-g. deploy -------------------------------------------------------------------------------
    pub fn deploy(&mut self, target: &Target, budget0: f64) -> Res<()> {
        let ws: f64 = target.ranges.iter().map(|r| r.2).sum();
        let mut ranges: Vec<(i64, i64, f64)> = target.ranges.iter().map(|(a, b, w)| (*a, *b, w / ws)).collect();
        ranges.sort_by(|a, b| a.partial_cmp(b).unwrap());
        self.sell_rewards()?;
        self.fund(budget0, &ranges)?;
        let thr = budget0 * self.settings["idle_redeploy_pct"].as_f64().unwrap_or(2.0) / 100.0;
        let mut minted_any = false;
        for rnd in 0..2 {
            if target.swap {
                let r = ranges.clone();
                if let Err(e) = self.retry("ratio swap", |s| s.ratio_swap(&r, budget0)) {
                    self.say_warn(format!("  swap skipped: {e}; minting with the balances on hand"));
                }
            }
            for i in 0..ranges.len() {
                let (tl, tu, wi) = ranges[i];
                let rest: f64 = ranges[i..].iter().map(|x| x.2).sum();
                let share = wi / rest;
                match self.retry(&format!("mint [{tl},{tu})"), |s| s.mint_one(tl, tu, share, budget0)) {
                    Ok(true) => {
                        minted_any = true;
                        UNMINTED_SWAP.lock().unwrap().get_or_insert_with(HashMap::new).remove(&unminted_key(self.cid, &self.cfg.pool));
                    }
                    Ok(false) => {}
                    Err(e) => self.say_err(format!("  mint [{tl},{tu}) gave up: {e}; tokens stay idle for the next pass")),
                }
            }
            if !self.execute {
                break;
            }
            let (_, _, _, p) = self.price()?;
            let (x, y) = self.usable(budget0, p)?;
            let left = self.value0(x as f64, y as f64, p);
            if left <= thr.max(self.cfg.min_swap_value) {
                break;
            }
            self.say(format!("  {} usable left over (> {}): top-up round {}", commas(left), commas(thr), rnd + 2));
        }
        if !minted_any {
            self.say_warn("  nothing minted this pass; tokens are idle and the next pass will redeploy them");
        }
        let (t0, t1) = (self.t0.clone(), self.t1.clone());
        let (l0, l1) = (self.bal(&t0)? as f64, self.bal(&t1)? as f64);
        self.say(format!("  left idle: {} t0 + {} t1", commas(l0 / self.unit), commas(l1 / self.unit1)));
        Ok(())
    }

    /// The share of token0 (by value) the ranges' mints want: what pays for them anywhere in the
    /// sizing band (mint_mix), so a ratio swap leaves headroom on both sides.
    pub fn need0_fraction(&self, ranges: &[(i64, i64, f64)], sp: &BigUint, p: f64) -> f64 {
        ranges.iter().map(|(tl, tu, wi)| wi * mint_mix(sp, &sqrt_ratio_at_tick(*tl), &sqrt_ratio_at_tick(*tu), p)).sum()
    }

    fn ratio_swap(&mut self, ranges: &[(i64, i64, f64)], budget0: f64) -> Res<()> {
        let (sp, _, _, p) = self.price()?;
        let (x, y) = self.usable(budget0, p)?;
        let (x, y) = (x as f64, y as f64);
        let f = self.need0_fraction(ranges, &sp, p);
        let diff0 = f * (x + y / p) - x;
        if diff0.abs() / self.unit <= self.cfg.min_swap_value {
            self.say("  e. ratio already close enough; no swap");
            return Ok(());
        }
        let zero_for_one = diff0 < 0.0;
        let key = unminted_key(self.cid, &self.cfg.pool);
        let earlier = UNMINTED_SWAP.lock().unwrap().get_or_insert_with(HashMap::new).get(&key).copied();
        if swaps_back(earlier, now(), zero_for_one, f) {
            let at = earlier.map(|x| x.0).unwrap_or(0);
            self.say_warn(format!("  e. ratio swap skipped: it would undo the swap of {} min ago, after which nothing was minted; minting with the balances on hand",
                                  now().saturating_sub(at) / 60));
            return Ok(());
        }
        let (tin, tout) = if zero_for_one { (self.t0.clone(), self.t1.clone()) } else { (self.t1.clone(), self.t0.clone()) };
        let mut amount_in = if zero_for_one { -diff0 } else { diff0 * p };
        let (din, dout) = (prices::meta(self.cid, &tin)?.decimals, prices::meta(self.cid, &tout)?.decimals);
        let q = prices::quote(self.cid, &tin, &tout, &format!("{}", amount_in as i128), din, dout)?;
        let e = if amount_in > 0.0 { prices::num_out(&q["best"]) / amount_in.floor() } else { 0.0 };
        if e > 0.0 {
            let s = if zero_for_one {
                (x * (1.0 - f) - f * y / p) / ((1.0 - f) + f * e / p)
            } else {
                (f * (x + y / p) - x) / (e * (1.0 - f) + f / p)
            };
            amount_in = s.max(0.0);
        }
        let amount_in = ((amount_in.floor() * 0.999) as i128).min(self.bal(&tin)?.max(0));
        if amount_in <= 0 {
            return Ok(());
        }
        let fair = if zero_for_one { amount_in as f64 * p } else { amount_in as f64 / p };
        let max_cost = self.cfg.max_swap_cost_bps;
        self.swap(&tin, &tout, amount_in, fair, max_cost, "e. ratio swap")?;
        // pending until a mint follows (deploy clears it)
        UNMINTED_SWAP.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, (now(), zero_for_one, f));
        Ok(())
    }

    /// Quote, cost check, exact approval, build, simulate, send. Returns the amount received.
    pub fn swap(&mut self, tin: &str, tout: &str, amount_in: i128, fair_out: f64, max_cost_bps: f64, label: &str) -> Res<i128> {
        let (mi, mo) = (prices::meta(self.cid, tin)?, prices::meta(self.cid, tout)?);
        let q = prices::quote(self.cid, tin, tout, &amount_in.to_string(), mi.decimals, mo.decimals)?;
        let best = q["best"].clone();
        if best.is_null() {
            return Err(StepErr::Fail(format!("no aggregator route: {}", q["failures"])));
        }
        let out = prices::num_out(&best);
        let cost = if fair_out > 0.0 {
            (1.0 - out / fair_out) * 1e4
        } else {
            let (iu, ou) = (prices::fnum(&best["amountInUsd"]), prices::fnum(&best["amountOutUsd"]));
            if iu > 0.0 { (1.0 - ou / iu) * 1e4 } else { 1e9 }
        };
        let mut venues: Vec<String> = vec![];
        for leg in best["legs"].as_array().cloned().unwrap_or_default() {
            for st in leg["steps"].as_array().cloned().unwrap_or_default() {
                for v in st["venues"].as_array().cloned().unwrap_or_default() {
                    if let Some(s) = v.as_str() {
                        if !venues.contains(&s.to_string()) {
                            venues.push(s.to_string());
                        }
                    }
                }
            }
        }
        venues.sort();
        self.say(format!("  {label}: {:.6} {} -> {:.6} {} via {} ({}), cost {cost:+.2} bp", amount_in as f64 / 10f64.powi(mi.decimals as i32), mi.symbol,
                         out / 10f64.powi(mo.decimals as i32), mo.symbol, best["aggregatorId"].as_str().unwrap_or("?"), venues.join(", ")));
        if cost > max_cost_bps {
            return Err(StepErr::Fail(format!("swap would cost {cost:.2} bp > max {max_cost_bps} bp")));
        }
        let spender = best["spender"].as_str().unwrap_or("").to_lowercase();
        let quoted_router = best["routerAddress"].as_str().unwrap_or("").to_lowercase();
        for (what, a) in [("spender", &spender), ("router", &quoted_router)] {
            if !prices::router_allowed(self.cid, a) {
                return Err(StepErr::Abort(format!(
                    "aggregator {} proposed {what} {a}, which is not a known swap router; refused (add it to config/routers.json only if you have verified it)",
                    best["aggregatorId"].as_str().unwrap_or("?"))));
            }
        }
        self.ensure_allowance(tin, amount_in, Some(&spender))?;
        let built = prices::build(self.cid, &best, &self.wallet, self.cfg.slippage_bps / 1e4)?;
        let router = built["routerAddress"].as_str().unwrap_or("").to_lowercase();
        if router != quoted_router {
            return Err(StepErr::Fail("router changed between quote and build; re-quoting".into()));
        }
        let calldata = built["calldata"].as_str().unwrap_or("").to_string();
        // the minimum the wallet must receive: the quote less the slippage tolerance (and a little rounding)
        let min_out = out * (1.0 - self.cfg.slippage_bps / 1e4) * 0.9995;
        let live = small(&chain::erc20_balance(self.cid, tin, &self.wallet)?) >= amount_in;
        if self.execute {
            match chain::simulate_transfers(self.cid, &self.wallet, &router, &calldata) {
                Some(Ok(tr)) => {
                    prices::check_swap(&self.wallet, tin, amount_in as f64, tout, min_out, &tr).map_err(|e| StepErr::Abort(format!("refused: {e}")))?;
                    self.say("    transfer check passed (simulated: only the input token leaves, the output arrives)");
                }
                Some(Err(e)) => return Err(StepErr::Fail(e)),
                None => self.say_warn("    the node cannot trace transfers; relying on the router allowlist and the receipt check"),
            }
        }
        let entry = self.tx(&router, &calldata, &format!("swap via {}", best["aggregatorId"].as_str().unwrap_or("?")), live)?;
        if self.execute {
            // What arrived, from the receipt's transfers (no balance read that could fail after the
            // swap went through). Anything unexpected pauses the app.
            let logs = entry["receipt"]["logs"].as_array().cloned().unwrap_or_default();
            let got = match prices::check_swap(&self.wallet, tin, amount_in as f64, tout, 0.0, &prices::transfers(&logs)) {
                Ok(g) => g,
                Err(e) => {
                    let _ = std::fs::write(crate::paths::stop_file(), format!("swap {} moved unexpected tokens: {e}\n", entry["hash"]));
                    log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "error", "what": "swap check", "error": e, "hash": entry["hash"]}));
                    return Err(StepErr::Abort(format!("swap {} did not do what was expected ({e}); paused", entry["hash"])));
                }
            };
            if got < min_out {
                self.say_warn(format!("    received {got:.0} raw, below the expected minimum {min_out:.0}"));
            }
            self.say(format!("    received {:.6}", got / 10f64.powi(mo.decimals as i32)));
            self.spent(tin, &spender, amount_in);
            return Ok(got as i128);
        }
        self.mv(tin, -amount_in);
        self.spent(tin, &spender, amount_in);
        self.mv(tout, out as i128);
        Ok(out as i128)
    }

    pub fn mint_one(&mut self, tl: i64, tu: i64, share: f64, budget0: f64) -> Res<bool> {
        let (sa, sb) = (sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu));
        let mut shrink = 1.0;
        let mut floor = 0;
        let mut reapproved = false;
        for _ in 0..7 {
            // every attempt sizes from the price and the balances as they are now
            let (sp, _, _, p) = self.price()?;
            let (x, y) = self.usable(budget0, p)?;
            let f0 = mint_mix(&sp, &sa, &sb, p);
            let v0 = (x as f64 + y as f64 / p) * share;
            let x_r = ((f0 * v0) as i128).min(x);
            let y_r = (((1.0 - f0) * v0 * p) as i128).min(y);
            let target = mint_liquidity(&sp, &sa, &sb, &big(x_r), &big(y_r));
            let fit = mint_liquidity(&sp, &sa, &sb, &big(x), &big(y));
            let mut liq = (to_f64(&target.min(fit)) * 0.999 * shrink) as i128;
            let (m0, m1) = crate::tickmath::amounts_for_liquidity(&sp, &sa, &sb, &big(liq.max(0)));
            let (m0, m1) = (small(&m0), small(&m1));
            if liq > 0 && self.value0(m0 as f64, m1 as f64, p) < self.cfg.min_swap_value {
                liq = 0;
            }
            if liq <= 0 {
                self.say(format!("  f. mint [{tl},{tu}): nothing to add (usable {} t0 / {} t1)", self.fmt0(x as f64), self.fmt1(y as f64)));
                return Ok(false);
            }
            let floor_bps = DEPOSIT_FLOOR_BPS[floor].max(self.cfg.slippage_bps);
            let b = mint_bounds(&sp, &sa, &sb, liq, floor_bps);
            let (t0, t1) = (self.t0.clone(), self.t1.clone());
            self.ensure_allowance(&t0, b.max0 + b.max0 / 1000 + 10, None)?;
            self.ensure_allowance(&t1, b.max1 + b.max1 / 1000 + 10, None)?;
            let data = calldata(
                "mint(address,int24,int24,uint128,uint256,uint256,uint256)",
                &[addr(&self.wallet), chain::int(tl), chain::int(tu), u(big(liq)), u(big(b.min0)), u(big(b.min1)), u(now() + DEADLINE_S)],
            );
            self.say(format!("  f. mint [{tl},{tu}) liquidity {liq} uses {} t0 + {} t1 (floors {floor_bps} bp of price)", self.fmt0(m0 as f64), self.fmt1(m1 as f64)));
            let pool = self.cfg.pool.clone();
            let live = small(&chain::erc20_balance(self.cid, &t0, &self.wallet)?) >= b.max0
                && small(&chain::erc20_balance(self.cid, &t1, &self.wallet)?) >= b.max1
                && small(&chain::erc20_allowance(self.cid, &t0, &self.wallet, &pool)?) >= b.max0
                && small(&chain::erc20_allowance(self.cid, &t1, &self.wallet, &pool)?) >= b.max1;
            match self.tx(&pool, &data, &format!("mint {tl}:{tu}"), live) {
                Ok(_) => {}
                Err(e) if simulation_reverted(&e) => {
                    let w = self.mint_recover(&e.reason, &t0, &t1, &pool, &mut shrink, &mut floor, &mut reapproved);
                    self.say_warn(format!("    simulation failed ({}): {w}", e.reason));
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
            self.mv(&t0, -m0);
            self.mv(&t1, -m1);
            self.spent(&t0, &pool, m0);
            self.spent(&t1, &pool, m1);
            if !self.execute {
                self.v_deployed += self.value0(m0 as f64, m1 as f64, p);
            }
            return Ok(true);
        }
        Err(StepErr::Fail(format!("mint [{tl},{tu}) still fails after 7 attempts (floors widened to {} bp)", DEPOSIT_FLOOR_BPS[floor])))
    }

    /// What a failed mint simulation changes for the next attempt (said back for the log); every
    /// attempt reads the price and the balances again. STF, the pool's pull failing on an allowance:
    /// approve both tokens again (unlimited, to this pool), once. TF, the wallet holding less than the
    /// mint pulls (the price moved toward the scarcer token): 0.5% smaller. AL or "Price slippage
    /// check", the deposit below its floors: the floors widen a step. Anything else: 0.5% smaller.
    #[allow(clippy::too_many_arguments)]
    fn mint_recover(&mut self, reason: &str, t0: &str, t1: &str, pool: &str, shrink: &mut f64, floor: &mut usize, reapproved: &mut bool) -> String {
        match mint_failure(reason) {
            MintFailure::Allowance if !*reapproved => {
                *reapproved = true;
                self.virt_allow.insert((t0.to_lowercase(), pool.to_lowercase()), 0);
                self.virt_allow.insert((t1.to_lowercase(), pool.to_lowercase()), 0);
                "approving both tokens again".into()
            }
            MintFailure::Floors if *floor + 1 < DEPOSIT_FLOOR_BPS.len() => {
                *floor += 1;
                format!("price read again, floors widened to {} bp", DEPOSIT_FLOOR_BPS[*floor])
            }
            MintFailure::Shortfall => {
                *shrink *= 0.995;
                "the wallet held less than the mint pulls: balances and price read again, 0.5% smaller".into()
            }
            _ => {
                *shrink *= 0.995;
                "price read again, liquidity 0.5% smaller".into()
            }
        }
    }

    /// Mint exactly `x` token0 + `y` token1 (raw, at most) into [tl, tu); returns the liquidity minted.
    fn mint_exact(&mut self, tl: i64, tu: i64, x: i128, y: i128) -> Res<i128> {
        let (sa, sb) = (sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu));
        let (mut x, mut y) = (x, y);
        let mut shrink = 1.0;
        let mut floor = 0;
        let mut reapproved = false;
        for _ in 0..7 {
            let (sp, _, _, _) = self.price()?;
            let liq = (to_f64(&mint_liquidity(&sp, &sa, &sb, &big(x), &big(y))) * 0.999 * shrink) as i128;
            if liq <= 0 {
                return Err(StepErr::Fail(format!("nothing to mint into [{tl},{tu})")));
            }
            let (m0, m1) = crate::tickmath::amounts_for_liquidity(&sp, &sa, &sb, &big(liq));
            let (m0, m1) = (small(&m0), small(&m1));
            let floor_bps = DEPOSIT_FLOOR_BPS[floor].max(self.cfg.slippage_bps);
            let b = mint_bounds(&sp, &sa, &sb, liq, floor_bps);
            let (t0, t1) = (self.t0.clone(), self.t1.clone());
            self.ensure_allowance(&t0, b.max0 + b.max0 / 1000 + 10, None)?;
            self.ensure_allowance(&t1, b.max1 + b.max1 / 1000 + 10, None)?;
            let data = calldata(
                "mint(address,int24,int24,uint128,uint256,uint256,uint256)",
                &[addr(&self.wallet), chain::int(tl), chain::int(tu), u(big(liq)), u(big(b.min0)), u(big(b.min1)), u(now() + DEADLINE_S)],
            );
            self.say(format!("  park: mint [{tl},{tu}) liquidity {liq} uses {} t0 + {} t1 (floors {floor_bps} bp of price)", self.fmt0(m0 as f64), self.fmt1(m1 as f64)));
            let pool = self.cfg.pool.clone();
            let (h0, h1) = (small(&chain::erc20_balance(self.cid, &t0, &self.wallet)?), small(&chain::erc20_balance(self.cid, &t1, &self.wallet)?));
            let live = h0 >= b.max0
                && h1 >= b.max1
                && small(&chain::erc20_allowance(self.cid, &t0, &self.wallet, &pool)?) >= b.max0
                && small(&chain::erc20_allowance(self.cid, &t1, &self.wallet, &pool)?) >= b.max1;
            match self.tx(&pool, &data, &format!("mint {tl}:{tu}"), live) {
                Ok(_) => {}
                Err(e) if simulation_reverted(&e) => {
                    if mint_failure(&e.reason) == MintFailure::Shortfall && self.execute {
                        // what the wallet holds now caps what is minted
                        (x, y) = (x.min(h0), y.min(h1));
                    }
                    let w = self.mint_recover(&e.reason, &t0, &t1, &pool, &mut shrink, &mut floor, &mut reapproved);
                    self.say_warn(format!("    simulation failed ({}): {w}", e.reason));
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
            self.mv(&t0, -m0);
            self.mv(&t1, -m1);
            self.spent(&t0, &pool, m0);
            self.spent(&t1, &pool, m1);
            return Ok(liq);
        }
        Err(StepErr::Fail(format!("mint [{tl},{tu}) still fails after 7 attempts (floors widened to {} bp)", DEPOSIT_FLOOR_BPS[floor])))
    }

    /// An exit to the dollar stable: this pool's tokens (up to its budget) sold for the chain's USD
    /// stable through the aggregator. The stable must be a funding token on the chain, or re-entry
    /// could not buy the pair back: then the tokens stay as they are.
    fn exit_to_stable(&mut self, budget0: f64) -> Res<()> {
        let usd_tok = crate::chains::get(self.cid).usd.to_lowercase();
        if usd_tok.is_empty() || !crate::settings::funding_tokens(&self.settings, self.cid).iter().any(|t| t.eq_ignore_ascii_case(&usd_tok)) {
            self.say_warn("  exit to stable: the chain's USD stable is not a funding token here (Settings, Funding tokens), so re-entry could not buy the pair back: the tokens stay as they are");
            return Ok(());
        }
        if usd_tok == self.t0 || usd_tok == self.t1 {
            self.say_warn("  exit to stable: the pool holds the stable itself (use all token0 or all token1): the tokens stay as they are");
            return Ok(());
        }
        let (_, _, _, p) = self.price()?;
        let um = prices::meta(self.cid, &usd_tok)?;
        let uu = prices::usd(self.cid, &usd_tok)?;
        let mut left0 = budget0;
        for tok in [self.t0.clone(), self.t1.clone()] {
            let is0 = tok == self.t0;
            let cap = self.budget_raw(&tok, left0.max(0.0), p);
            let amt = self.bal(&tok)?.max(0).min(cap);
            if amt <= 0 {
                continue;
            }
            left0 -= if is0 { amt as f64 / self.unit } else { amt as f64 / p / self.unit };
            let m = prices::meta(self.cid, &tok)?;
            let usd = amt as f64 / 10f64.powi(m.decimals as i32) * prices::usd(self.cid, &tok)?;
            if usd < 1.0 {
                continue;
            }
            let fair = usd / uu * 10f64.powi(um.decimals as i32);
            let mc = self.cfg.max_swap_cost_bps;
            let (a, b) = (tok.clone(), usd_tok.clone());
            if let Err(e) = self.retry("exit swap", |s| s.swap(&a, &b, amt, fair, mc, "exit swap to stable")) {
                self.say_warn(format!("  exit swap {} -> {} skipped: {e}; kept as it is", m.symbol, um.symbol));
            }
        }
        Ok(())
    }

    /// A withdrawal (capital.rs): about `usd` of the pool's capital made USDC (`usdc`), from its idle
    /// tokens first and then from its positions, burned in proportion. The pool's USDC after it (raw).
    /// `max_cost_bps`: the step's own bound (a plan's or an emergency's), else the pool's setting (at least 10 bp).
    pub fn release(&mut self, usd: f64, usdc: &str, others: &[String], max_cost_bps: Option<f64>) -> Res<i128> {
        let usdc = usdc.to_lowercase();
        let (um, uu) = (prices::meta(self.cid, &usdc)?, prices::usd(self.cid, &usdc)?);
        let val = |s: &Self, t: &str| -> Res<f64> {
            let b = s.bal(t)?.max(0);
            if b == 0 {
                return Ok(0.0);
            }
            Ok(b as f64 / 10f64.powi(prices::meta(s.cid, t)?.decimals as i32) * prices::usd(s.cid, t)?)
        };
        let mut toks: Vec<String> = vec![self.t0.clone(), self.t1.clone()];
        toks.extend(others.iter().map(|t| t.to_lowercase()));
        toks.retain(|t| *t != usdc);
        toks.dedup();
        let have_usdc = val(self, &usdc)?;
        let mut idle = 0.0;
        for t in &toks {
            idle += val(self, t)?;
        }
        self.say(format!("  withdrawal: ${} wanted; ${} in USDC and ${} in other idle tokens", commas(usd), commas(have_usdc), commas(idle)));
        let short = usd - have_usdc - idle;
        if short > 1.0 {
            let positions = wallet_positions(self.cid, &self.cfg.pool, &self.wallet)?;
            let (sp, _, _, p) = self.price()?;
            let u0 = prices::usd(self.cid, &self.t0)?;
            let pos_usd: f64 = positions.iter().map(|(tl, tu, l)| {
                let (a0, a1) = amounts_at(&sp, *tl, *tu, l);
                self.value0(to_f64(&a0), to_f64(&a1), p) * u0
            }).sum();
            if pos_usd > 0.0 {
                // a little more than the shortfall: the swaps to USDC cost something
                let f = (short * 1.01 / pos_usd).min(1.0);
                self.say(format!("  withdrawal: burning {:.1}% of each position (${} of ${})", f * 100.0, commas(short), commas(pos_usd)));
                self.begin("withdraw", None);
                let part: Vec<(i64, i64, i128)> = positions.iter()
                    .map(|(a, b, l)| (*a, *b, if f >= 0.999 { small(l) } else { (to_f64(l) * f) as i128 }))
                    .filter(|x| x.2 > 0)
                    .collect();
                self.burn_all(&part)?;
            }
        }
        let mut left = usd - val(self, &usdc)?;
        for t in toks {
            if left < 1.0 {
                break;
            }
            let (m, px) = (prices::meta(self.cid, &t)?, prices::usd(self.cid, &t)?);
            let b = self.bal(&t)?.max(0);
            let want = ((left * 1.003 / px) * 10f64.powi(m.decimals as i32)) as i128;
            let amt = b.min(want);
            let usd_in = amt as f64 / 10f64.powi(m.decimals as i32) * px;
            if usd_in < 1.0 {
                continue;
            }
            let fair = usd_in / uu * 10f64.powi(um.decimals as i32);
            let mc = max_cost_bps.unwrap_or_else(|| self.cfg.max_swap_cost_bps.max(10.0));
            let (a, c) = (t.clone(), usdc.clone());
            match self.retry("withdrawal swap", |s| s.swap(&a, &c, amt, fair, mc, "withdrawal swap to USDC")) {
                Ok(_) => left -= usd_in,
                Err(e) => self.say_warn(format!("  withdrawal swap {} -> {} skipped: {e}", m.symbol, um.symbol)),
            }
        }
        self.bal(&usdc)
    }

    /// `budget0` (token0 units) in raw units of `token` (one of the pool's) at raw price p.
    fn budget_raw(&self, token: &str, budget0: f64, p: f64) -> i128 {
        let raw0 = budget0 * self.unit;
        (if token.eq_ignore_ascii_case(&self.t0) { raw0 } else { raw0 * p }) as i128
    }

    /// What this pool's parking decision depends on now (parking.rs `Situation`).
    fn parking_situation(&self, side: crate::parking::Side, budget0: f64, deployed0: f64) -> Res<crate::parking::Situation> {
        use crate::parking::Held;
        let held = |t: &str| -> Res<Held> {
            let m = prices::meta(self.cid, t)?;
            let px = prices::usd(self.cid, t).unwrap_or(0.0);
            let b = self.bal(t)?.max(0) as f64 / 10f64.powi(m.decimals as i32);
            Ok(Held { token: t.to_lowercase(), usd: b * px, park_pool: crate::parking::pool_for(&self.settings, self.cid, t).map(|w| w.pool) })
        };
        let mut funding = vec![];
        for ft in crate::settings::funding_tokens(&self.settings, self.cid) {
            let ft = ft.to_lowercase();
            if ft == self.t0 || ft == self.t1 {
                continue;
            }
            if let Ok(h) = held(&ft) {
                funding.push(h); // a token the chain does not have is skipped
            }
        }
        let t0usd = prices::usd(self.cid, &self.t0)?;
        Ok(crate::parking::Situation {
            on: crate::parking::enabled(&self.settings) && crate::parking::wanted(&self.cfg.strategy),
            pool: self.cfg.pool.clone(),
            side,
            budget_usd: budget0 * t0usd,
            deployed_usd: deployed0 * t0usd,
            min_usd: crate::parking::min_usd(&self.settings),
            own: [held(&self.t0.clone())?, held(&self.t1.clone())?],
            funding,
            lots: crate::parking::of_origin(self.execute, self.cid, &self.cfg.pool)
                .iter()
                // a claim is already out of this pool's budget (worker): it counts as parked, not again as spent
                .map(|l| (l["token"].as_str().unwrap_or("").to_lowercase(), if crate::parking::is_claim(l) { 0.0 } else { l["usd"].as_f64().unwrap_or(0.0) }))
                .collect(),
        })
    }

    /// Parking for this pool while it is not moving (parking.rs `decide`): its unfinished steps put
    /// right first, then what it holds and does not use parked (or its lots brought back when
    /// parking stopped for it), and lots re-centred where their whitelist entry asks for it.
    fn park_pass(&mut self, side: crate::parking::Side, budget0: f64, deployed0: f64) -> Res<()> {
        self.reconcile_lots()?;
        let s = self.parking_situation(side, budget0, deployed0)?;
        for a in crate::parking::decide(&s) {
            match a {
                crate::parking::Act::UnparkAll => {
                    self.say("  parking stopped for this pool (or a parked token left the whitelist): bringing parked tokens back");
                    self.unpark_all()?;
                }
                crate::parking::Act::Park(tok, usd) => {
                    let m = prices::meta(self.cid, &tok)?;
                    let amt = ((usd / prices::usd(self.cid, &tok)?) * 10f64.powi(m.decimals as i32)) as i128;
                    let amt = amt.min(self.bal(&tok)?.max(0));
                    if !s.own.iter().any(|h| h.token == tok) {
                        self.say(format!("  ${usd:.2} of its ${:.2} budget is still in {} (funding it has not drawn): parking it", s.budget_usd, m.symbol));
                    }
                    if let Err(e) = self.park(&tok, amt) {
                        self.say_warn(format!("  park failed: {e}; retried next cycle"));
                    }
                }
            }
        }
        if s.on {
            if let Err(e) = self.recentre_lots() {
                self.say_warn(format!("  re-centring parked lots failed: {e}; retried next cycle"));
            }
        }
        Ok(())
    }

    /// Put right this pool's lots whose last park or unpark step did not finish (parking.rs
    /// `reconcile`): a mint that happened gets its lot, a swap whose mint never came is swapped
    /// back, a withdrawal that happened gets its swap back. Live only: dry-run lots touch no chain.
    fn reconcile_lots(&mut self) -> Res<()> {
        if !self.execute {
            return Ok(());
        }
        for lot in crate::parking::of_origin(true, self.cid, &self.cfg.pool) {
            if lot["stage"].is_null() {
                continue;
            }
            let id = lot["id"].as_str().unwrap_or("").to_string();
            let pool = lot["pool"].as_str().unwrap_or("").to_string();
            let (tl, tu) = (lot["tl"].as_i64().unwrap_or(0), lot["tu"].as_i64().unwrap_or(0));
            let there = crate::parking::onchain(self.cid, &pool, &self.wallet, tl, tu).map_err(StepErr::Fail)?;
            match crate::parking::reconcile(&lot, &crate::parking::lots(true), there) {
                crate::parking::Fix::Fine => {}
                crate::parking::Fix::Restore(l) => {
                    self.say_warn(format!("  park: lot {id}'s last step ({}) did finish: kept as parked, liquidity {}", lot["stage"].as_str().unwrap_or("?"), l["liquidity"].as_str().unwrap_or("?")));
                    log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "park_recovered", "live": true, "lot": l}));
                    crate::parking::update(true, l);
                }
                crate::parking::Fix::SwapBackAndDrop { from, to, amount } => {
                    self.say_warn(format!("  park: lot {id}'s last step ({}) was cut short: nothing of it is parked", lot["stage"].as_str().unwrap_or("?")));
                    if amount > 0 && !from.is_empty() {
                        let mut pk = self.parking_executor(&pool)?;
                        let amt = (amount as i128).min(pk.bal(&from)?.max(0));
                        if amt > 0 {
                            let (_, _, _, p) = pk.price()?;
                            let fair = if from == pk.t0 { amt as f64 * p } else { amt as f64 / p };
                            let mc = pk.cfg.max_swap_cost_bps.max(10.0);
                            let r = pk.retry("park recovery swap", |s| s.swap(&from, &to, amt, fair, mc, "park recovery: swap back"));
                            for l in std::mem::take(&mut pk.report) {
                                self.report.push(l);
                            }
                            self.broadcasts += pk.broadcasts;
                            if let Err(e) = r {
                                self.say_warn(format!("  park recovery swap failed: {e}; retried next cycle"));
                                continue;
                            }
                        }
                    }
                    log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "park_recovered", "live": true, "lot": lot, "dropped": true}));
                    crate::parking::remove(true, &id);
                }
            }
        }
        Ok(())
    }

    /// An executor for the parking pool, sharing this one's wallet, mode and limits.
    fn parking_executor(&self, pool: &str) -> Res<Executor> {
        let mut cfg = self.cfg.clone();
        cfg.pool = pool.to_lowercase();
        cfg.name = format!("{} parking", self.cfg.name);
        cfg.strategy = Default::default();
        let mut pk = Executor::new(cfg, self.cid, &self.wallet, &self.settings, self.execute, None).map_err(StepErr::Fail)?;
        pk.virt = self.virt.clone();
        pk.virt_allow = self.virt_allow.clone();
        pk.reserve = self.reserve.clone();
        Ok(pk)
    }

    /// Park `amount` (raw) of `token` in its whitelisted pegged pool (parking.rs) as a lot of this
    /// pool. Live, the lot is in the ledger before the swap (stage "parking"), so a step cut short is
    /// put right on the next cycle (`reconcile_lots`); a mint that fails swaps the ratio swap back.
    fn park(&mut self, token: &str, amount: i128) -> Res<()> {
        let Some(w) = crate::parking::pool_for(&self.settings, self.cid, token) else { return Ok(()) };
        if w.pool.eq_ignore_ascii_case(&self.cfg.pool) {
            return Ok(()); // never into the pool itself
        }
        let m = prices::meta(self.cid, token)?;
        let usd = amount.max(0) as f64 / 10f64.powi(m.decimals as i32) * prices::usd(self.cid, token)?;
        let min = crate::parking::min_usd(&self.settings);
        if usd < min {
            self.say(format!("  park: {:.4} {} is worth ${usd:.2} < ${min}: kept in the wallet", amount as f64 / 10f64.powi(m.decimals as i32), m.symbol));
            return Ok(());
        }
        let mut pk = self.parking_executor(&w.pool)?;
        let tok = token.to_lowercase();
        if tok != pk.t0 && tok != pk.t1 {
            return Err(StepErr::Fail(format!("parking pool {} does not hold {}", w.pool, m.symbol)));
        }
        if let Some(host) = crate::parking::host(self.cid, &w.pool) {
            // the parking pool runs its own strategy: the tokens are its to place (parking.rs, claims)
            let lot = json!({"id": format!("{}-{}-{}", now(), &self.cfg.pool[2..8], &tok[2..6]), "kind": "claim", "chain_id": self.cid,
                             "origin": self.cfg.pool, "origin_name": self.cfg.name, "pool": w.pool, "host_name": host, "token": tok,
                             "symbol": m.symbol, "amount": amount.to_string(), "usd": usd, "ts": now()});
            crate::parking::add(self.execute, lot.clone());
            self.say(format!("  park: {:.4} {} (${usd:.2}) handed to {host}'s strategy as a claim: it places them with its own, and pays them back on unpark",
                             amount as f64 / 10f64.powi(m.decimals as i32), m.symbol));
            log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "park", "live": self.execute, "lot": lot}));
            return Ok(());
        }
        self.say(format!("  park: {:.4} {} (${usd:.2}) into the pegged pool {}", amount as f64 / 10f64.powi(m.decimals as i32), m.symbol, &w.pool[..10]));
        let live = self.execute;
        let mut lot = json!({"id": format!("{}-{}-{}", now(), &self.cfg.pool[2..8], &tok[2..6]), "chain_id": self.cid, "origin": self.cfg.pool,
                             "origin_name": self.cfg.name, "pool": w.pool, "token": tok, "symbol": m.symbol, "liquidity": "0",
                             "amount": amount.to_string(), "usd": usd, "ts": now()});
        let (cid, wallet) = (self.cid, self.wallet.clone());
        let r = (|| -> Res<(i64, i64, i128)> {
            let (sp, tick, _, p) = pk.price()?;
            let (tl, tu) = crate::parking::range(tick, pk.spacing, w.range_pct);
            let f0 = pk.need0_fraction(&[(tl, tu, 1.0)], &sp, p);
            let is0 = tok == pk.t0;
            let (tin, tout) = if is0 { (pk.t0.clone(), pk.t1.clone()) } else { (pk.t1.clone(), pk.t0.clone()) };
            lot["tl"] = json!(tl);
            lot["tu"] = json!(tu);
            if live {
                let pre = crate::parking::onchain(cid, &w.pool, &wallet, tl, tu).map_err(StepErr::Fail)?;
                for (k, v) in [("stage", json!("parking")), ("pre", json!(pre.to_string())), ("other", json!(tout)), ("got", json!("0"))] {
                    lot[k] = v;
                }
                crate::parking::add(true, lot.clone());
            }
            // the share of the lot the range wants in the other token
            let give = ((if is0 { 1.0 - f0 } else { f0 }) * amount as f64) as i128;
            let mc = pk.cfg.max_swap_cost_bps.max(10.0);
            let got = if give > 0 {
                let fair = if is0 { give as f64 * p } else { give as f64 / p };
                let (a, b) = (tin.clone(), tout.clone());
                pk.retry("park swap", |s| s.swap(&a, &b, give, fair, mc, "park: ratio swap"))?
            } else {
                0
            };
            if live {
                lot["got"] = json!(got.to_string());
                crate::parking::update(true, lot.clone());
            }
            let keep = amount - give;
            let (x, y) = if is0 { (keep, got) } else { (got, keep) };
            match pk.retry("park mint", |s| s.mint_exact(tl, tu, x, y)) {
                Ok(liq) => Ok((tl, tu, liq)),
                Err(e) => {
                    // undo the ratio swap; if that fails too, the next cycle's reconcile does it
                    if live && got > 0 {
                        let fair = if is0 { got as f64 / p } else { got as f64 * p };
                        let (a, b) = (tout.clone(), tin.clone());
                        if pk.retry("park undo swap", |s| s.swap(&a, &b, got, fair, mc, "park failed: swap back")).is_ok() {
                            crate::parking::remove(true, lot["id"].as_str().unwrap_or(""));
                        }
                    } else if live {
                        crate::parking::remove(true, lot["id"].as_str().unwrap_or(""));
                    }
                    Err(e)
                }
            }
        })();
        for l in std::mem::take(&mut pk.report) {
            self.report.push(l);
        }
        self.broadcasts += pk.broadcasts;
        let (_, _, liq) = r?;
        if !self.execute {
            self.virt = pk.virt.clone();
        }
        lot["liquidity"] = json!(liq.to_string());
        if let Some(o) = lot.as_object_mut() {
            for k in ["stage", "pre", "other", "got"] {
                o.remove(k);
            }
        }
        log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "park", "live": self.execute, "lot": lot}));
        if self.execute {
            crate::parking::update(true, lot);
        } else {
            crate::parking::add(false, lot);
        }
        Ok(())
    }

    /// Bring every lot this pool parked back into its token (before re-entering or switching sides).
    fn unpark_all(&mut self) -> Res<()> {
        self.reconcile_lots()?;
        for lot in crate::parking::of_origin(self.execute, self.cid, &self.cfg.pool) {
            if lot["stage"].is_null() {
                self.unpark(&lot)?;
            }
        }
        Ok(())
    }

    /// Bring one lot back into its token; returns how much of the token came back (raw). Only what
    /// is still on the lot's range is withdrawn (parking.rs): a lot whose liquidity went elsewhere
    /// is dropped from the ledger rather than failing every cycle. Live, each step is in the ledger
    /// first ("unparking", then "burned"), so one cut short is finished by `reconcile_lots`.
    fn unpark(&mut self, lot: &Value) -> Res<i128> {
        if crate::parking::is_claim(lot) {
            return self.unpark_claim(lot);
        }
        let id = lot["id"].as_str().unwrap_or("").to_string();
        let token = lot["token"].as_str().unwrap_or("").to_lowercase();
        let amount: i128 = lot["amount"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
        let (tl, tu) = (lot["tl"].as_i64().unwrap_or(0), lot["tu"].as_i64().unwrap_or(0));
        let liq: u128 = lot["liquidity"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
        let pool = lot["pool"].as_str().unwrap_or("").to_string();
        let short = &pool[..10.min(pool.len())];
        self.say(format!("  unpark: lot {id} ({} {}) from {short}", lot["symbol"].as_str().unwrap_or("?"), lot["usd"].as_f64().map(|u| format!("${u:.2}")).unwrap_or_default()));
        if !self.execute {
            // a dry-run lot was never minted: its tokens simply come back
            self.say("    dry run: the lot's tokens return to the wallet");
            self.mv(&token, amount);
            crate::parking::remove(false, &id);
            return Ok(amount);
        }
        let there = crate::parking::onchain(self.cid, &pool, &self.wallet, tl, tu).map_err(StepErr::Fail)?;
        let w = crate::parking::withdrawable(lot, &crate::parking::lots(true), there);
        if w == 0 {
            self.say_warn(format!("  unpark: lot {id}'s liquidity is no longer on [{tl},{tu}) of {short} (withdrawn elsewhere): dropped from the ledger"));
            log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "unpark", "live": true, "lot": lot, "missing": true}));
            crate::parking::remove(true, &id);
            return Ok(0);
        }
        if w < liq {
            self.say_warn(format!("  unpark: only {w} of lot {id}'s liquidity {liq} is still on [{tl},{tu}): withdrawing that"));
        }
        let mut pk = self.parking_executor(&pool)?;
        let other = if token == pk.t0 { pk.t1.clone() } else { pk.t0.clone() };
        let (before, before_tok) = (pk.bal(&other)?, pk.bal(&token)?);
        let mut step = lot.clone();
        step["stage"] = json!("unparking");
        step["pre"] = json!(there.to_string());
        step["other"] = json!(other);
        crate::parking::update(true, step.clone());
        let r = (|| -> Res<i128> {
            pk.retry(&format!("unpark withdraw [{tl},{tu})"), |s| s.burn_one(tl, tu, w as i128).map(|_| ()))?;
            let got = pk.bal(&other)? - before;
            // withdrawn: the lot holds no liquidity now, only the swap back is left
            step["stage"] = json!("burned");
            step["liquidity"] = json!("0");
            step["got"] = json!(got.max(0).to_string());
            crate::parking::update(true, step.clone());
            if got > 0 {
                let (_, _, _, p) = pk.price()?;
                let fair = if other == pk.t0 { got as f64 * p } else { got as f64 / p };
                let mc = pk.cfg.max_swap_cost_bps.max(10.0);
                let (a, b) = (other.clone(), token.clone());
                if let Err(e) = pk.retry("unpark swap", |s| s.swap(&a, &b, got, fair, mc, "unpark: swap back")) {
                    // left at "burned": the next cycle swaps it back
                    pk.say_warn(format!("  unpark swap failed: {e}; retried next cycle"));
                    return Ok(-1);
                }
            }
            Ok((pk.bal(&token)? - before_tok).max(0))
        })();
        for l in std::mem::take(&mut pk.report) {
            self.report.push(l);
        }
        self.broadcasts += pk.broadcasts;
        let back = match r {
            Ok(b) => b,
            Err(e) => {
                // the withdrawal failed: still parked as it was
                if step["stage"] == "unparking" {
                    crate::parking::update(true, lot.clone());
                }
                return Err(e);
            }
        };
        if back < 0 {
            return Ok(0);
        }
        log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "unpark", "live": true, "lot": lot}));
        crate::parking::remove(true, &id);
        Ok(back)
    }

    /// Pay a claim back (parking.rs, claims): the wallet's balance of the token first (the host may not
    /// have placed it yet), the rest withdrawn from the host's own positions in proportion (a little
    /// over, for rounding) with the other token swapped back. A step cut short leaves the claim as it
    /// was: the next unpark finds what was withdrawn in the wallet and needs less.
    fn unpark_claim(&mut self, lot: &Value) -> Res<i128> {
        let id = lot["id"].as_str().unwrap_or("").to_string();
        let token = lot["token"].as_str().unwrap_or("").to_lowercase();
        let amount: i128 = lot["amount"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
        let host = lot["pool"].as_str().unwrap_or("").to_string();
        let name = lot["host_name"].as_str().map(String::from).unwrap_or_else(|| host[..10.min(host.len())].to_string());
        self.say(format!("  unpark: claim {id} ({} {}) on {name}", lot["symbol"].as_str().unwrap_or("?"), lot["usd"].as_f64().map(|u| format!("${u:.2}")).unwrap_or_default()));
        if !self.execute {
            // dry run: the tokens never left the virtual wallet
            crate::parking::remove(false, &id);
            return Ok(amount);
        }
        let mut pk = self.parking_executor(&host)?;
        let mut keep = false;
        let r = (|| -> Res<()> {
            let have = pk.bal(&token)?.max(0);
            if have >= amount {
                return Ok(());
            }
            let (_, _, _, p) = pk.price()?;
            let is0 = token == pk.t0;
            let other = if is0 { pk.t1.clone() } else { pk.t0.clone() };
            let short = amount - have;
            let short0 = if is0 { short as f64 } else { short as f64 / p };
            let positions = wallet_positions(self.cid, &host, &self.wallet)?;
            let (sp, _, _, _) = pk.price()?;
            let held0: f64 = positions.iter().map(|(tl, tu, l)| {
                let (a0, a1) = amounts_at(&sp, *tl, *tu, l);
                pk.value0(to_f64(&a0), to_f64(&a1), p)
            }).sum();
            if held0 > 0.0 {
                let f = (short0 * 1.01 / held0).min(1.0);
                self.say(format!("    {:.1}% of {name}'s positions withdrawn to pay it back", f * 100.0));
                let before = pk.bal(&other)?;
                for (tl, tu, l) in &positions {
                    let take = (to_f64(l) * f) as i128;
                    if take > 0 {
                        let (a, b) = (*tl, *tu);
                        pk.retry(&format!("claim withdraw [{a},{b})"), |s| s.burn_one(a, b, take).map(|_| ()))?;
                    }
                }
                let got = pk.bal(&other)? - before;
                if got > 0 {
                    let fair = if other == pk.t0 { got as f64 * p } else { got as f64 / p };
                    let mc = pk.cfg.max_swap_cost_bps.max(10.0);
                    let (a, b) = (other.clone(), token.clone());
                    pk.retry("claim swap", |s| s.swap(&a, &b, got, fair, mc, "unpark: swap back"))?;
                }
            } else {
                // the host holds the claim's tokens idle (its capital, capital.rs): paid back once it
                // has deployed them again, so the claim stays
                keep = true;
            }
            Ok(())
        })();
        for l in std::mem::take(&mut pk.report) {
            self.report.push(l);
        }
        self.broadcasts += pk.broadcasts;
        r?;
        if keep {
            self.say_warn(format!("    {name} has no positions to pay it back from: the claim stays until it has"));
            return Ok(0);
        }
        let back = pk.bal(&token)?.max(0).min(amount);
        log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "unpark", "live": true, "lot": lot, "back": back.to_string()}));
        crate::parking::remove(true, &id);
        Ok(back)
    }

    /// Lots whose whitelist entry re-centres them (parking.rs), outside their range for long enough,
    /// move back around the parking pool's price: withdrawn and parked again.
    fn recentre_lots(&mut self) -> Res<()> {
        for mut lot in crate::parking::of_origin(self.execute, self.cid, &self.cfg.pool) {
            if crate::parking::is_claim(&lot) {
                continue; // its host's strategy places it
            }
            let Some(w) = crate::parking::entry_of(&self.settings, &lot) else { continue };
            let Some(after) = w.recentre_after_min else { continue };
            let (tl, tu) = (lot["tl"].as_i64().unwrap_or(0), lot["tu"].as_i64().unwrap_or(0));
            let (_, tick, _) = crate::chain::slot0(self.cid, &w.pool).map_err(StepErr::Fail)?;
            if tl <= tick && tick < tu {
                if !lot["out_since"].is_null() {
                    lot["out_since"] = Value::Null;
                    crate::parking::update(self.execute, lot);
                }
                continue;
            }
            let t = now() as i64;
            let Some(since) = lot["out_since"].as_i64() else {
                lot["out_since"] = json!(t);
                crate::parking::update(self.execute, lot);
                continue;
            };
            if ((t - since) as f64) < after * 60.0 {
                continue;
            }
            let token = lot["token"].as_str().unwrap_or("").to_lowercase();
            self.say(format!("  park: lot {} has been outside [{tl},{tu}) for {} min (price at tick {tick}): moving it back around the price",
                             lot["id"].as_str().unwrap_or("?"), (t - since) / 60));
            let back = self.unpark(&lot)?;
            if back > 0 {
                if let Err(e) = self.park(&token, back) {
                    self.say_warn(format!("  park failed: {e}; the tokens wait in the wallet (retried next cycle)"));
                }
            }
        }
        Ok(())
    }

    fn sell_rewards(&mut self) -> Res<()> {
        let sr = self.settings["sell_rewards"].clone();
        if !sr["enabled"].as_bool().unwrap_or(false) {
            return Ok(());
        }
        let into = if self.cfg.numeraire == 0 { self.t0.clone() } else { self.t1.clone() };
        let solid = crate::chains::get(self.cid).solid;
        let lp_on = self.settings["solid_lp"]["enabled"].as_bool().unwrap_or(false);
        for token in sr["tokens"].as_array().cloned().unwrap_or_default() {
            let token = token.as_str().unwrap_or("").to_lowercase();
            if token.is_empty() || token == self.t0 || token == self.t1 || (token == solid && lp_on) {
                continue;
            }
            if chain::eth_call(self.cid, &token, "0x313ce567", None).is_err() {
                continue; // that reward token does not exist on this chain
            }
            let amount = self.bal(&token)?;
            if amount <= 0 {
                continue;
            }
            let sym = prices::meta(self.cid, &token)?.symbol;
            let (tk, i2, s2) = (token.clone(), into.clone(), sr.clone());
            if let Err(e) = self.retry(&format!("sell reward {sym}"), |s| s.sell_reward(&tk, &i2, amount, &s2)) {
                self.say_warn(format!("  reward sale skipped: {e}"));
            }
        }
        Ok(())
    }

    fn sell_reward(&mut self, token: &str, into: &str, amount: i128, sr: &Value) -> Res<()> {
        let (mi, mo) = (prices::meta(self.cid, token)?, prices::meta(self.cid, into)?);
        let max_impact = sr["max_impact_pct"].as_f64().unwrap_or(3.0);
        let min_usd = sr["min_value_usd"].as_f64().unwrap_or(2.0);
        let mut chunk = amount;
        for _ in 0..6 {
            let q = prices::quote(self.cid, token, into, &chunk.to_string(), mi.decimals, mo.decimals)?;
            let best = &q["best"];
            let (iu, ou) = (prices::fnum(&best["amountInUsd"]), prices::fnum(&best["amountOutUsd"]));
            let loss = if iu > 0.0 { (1.0 - ou / iu) * 100.0 } else { 100.0 };
            if ou < min_usd {
                self.say(format!("  c. reward {}: {:.2} is worth ${ou:.2} < ${min_usd}; kept", mi.symbol, chunk as f64 / 10f64.powi(mi.decimals as i32)));
                return Ok(());
            }
            if loss <= max_impact {
                self.swap(token, into, chunk, 0.0, max_impact * 100.0, &format!("c. reward sale ({loss:.1}% impact)"))?;
                return Ok(());
            }
            chunk /= 2;
        }
        self.say(format!("  c. reward {}: impact above {max_impact}% even for small amounts; kept", mi.symbol));
        Ok(())
    }

    fn fund(&mut self, budget0: f64, ranges: &[(i64, i64, f64)]) -> Res<()> {
        let fts: Vec<String> = crate::settings::funding_tokens(&self.settings, self.cid)
            .into_iter()
            .filter(|t| *t != self.t0 && *t != self.t1)
            .collect();
        if fts.is_empty() {
            return Ok(());
        }
        let (sp, _, _, p) = self.price()?;
        let (t0, t1) = (self.t0.clone(), self.t1.clone());
        let have = self.value0(self.bal(&t0)?.max(0) as f64, self.bal(&t1)?.max(0) as f64, p) + self.deployed_value()?;
        let mut short = (budget0 - have) * prices::usd(self.cid, &self.t0)?;
        if short < 10.0 {
            return Ok(());
        }
        let mut avail = vec![];
        for ft in fts {
            if chain::eth_call(self.cid, &ft, "0x313ce567", None).is_err() {
                continue; // not a token on this chain
            }
            let r: Result<(f64, i128), String> = (|| {
                let b = self.bal(&ft).map_err(|e| e.to_string())?.max(0);
                Ok((b as f64 / 10f64.powi(prices::meta(self.cid, &ft)?.decimals as i32) * prices::usd(self.cid, &ft)?, b))
            })();
            match r {
                Ok((usd, b)) => avail.push((usd, ft, b)),
                Err(e) => self.say_warn(format!("  d. funding: {} skipped ({e})", &ft[..10])),
            }
        }
        avail.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        let into = if self.need0_fraction(ranges, &sp, p) >= 0.5 { t0 } else { t1 };
        for (usd_free, ft, b) in avail {
            if short < 10.0 {
                break;
            }
            if usd_free < 1.0 {
                continue;
            }
            let (px, m) = (prices::usd(self.cid, &ft)?, prices::meta(self.cid, &ft)?);
            let amount = b.min((short / px * 10f64.powi(m.decimals as i32)) as i128);
            let mc = self.cfg.max_swap_cost_bps.max(10.0);
            let (f2, i2) = (ft.clone(), into.clone());
            match self.retry(&format!("funding swap {}", m.symbol), |s| s.swap(&f2, &i2, amount, 0.0, mc, "d. funding swap")) {
                Ok(_) => short -= amount as f64 / 10f64.powi(m.decimals as i32) * px,
                Err(e) => self.say_warn(format!("  funding from {} skipped: {e}", m.symbol)),
            }
        }
        if short >= 10.0 {
            self.say_warn(format!("  d. funding: still ${short:.2} short; the funding tokens are used up"));
        }
        Ok(())
    }

    /// The pool this executor runs (or parks in) gets an unlimited approval once: a mint never waits on
    /// an allowance read again. Anything else (an aggregator's router) is approved the exact amount.
    /// A too-small non-zero allowance is reset to 0 first (USDT).
    pub fn ensure_allowance(&mut self, token: &str, amount: i128, spender: Option<&str>) -> Res<()> {
        let own = spender.map(|s| s.eq_ignore_ascii_case(&self.cfg.pool)).unwrap_or(true);
        let spender = spender.unwrap_or(&self.cfg.pool).to_lowercase();
        let key = (token.to_lowercase(), spender.clone());
        let cur = match self.virt_allow.get(&key) {
            Some(v) => *v,
            None => small(&chain::erc20_allowance(self.cid, token, &self.wallet, &spender)?),
        };
        if amount <= 0 || cur >= amount {
            return Ok(());
        }
        let m = prices::meta(self.cid, token)?;
        if cur > 0 {
            self.say(format!("  approve {} -> {}: reset to 0 first", m.symbol, &spender[..10]));
            let d = calldata("approve(address,uint256)", &[addr(&spender), u(0u8)]);
            let what = format!("approve0 {}", m.symbol);
            let tk = token.to_string();
            self.retry("approve reset", |s| s.tx(&tk, &d, &what, true).map_err(StepErr::from))?;
        }
        let d = if own {
            self.say(format!("  approve {} -> {} (this pool): unlimited", m.symbol, &spender[..10]));
            calldata("approve(address,uint256)", &[addr(&spender), u((BigUint::from(1u8) << 256u32) - 1u8)])
        } else {
            self.say(format!("  approve {} -> {} for {:.6}", m.symbol, &spender[..10], amount as f64 / 10f64.powi(m.decimals as i32)));
            calldata("approve(address,uint256)", &[addr(&spender), u(big(amount))])
        };
        let what = format!("approve {}", m.symbol);
        let tk = token.to_string();
        self.retry("approve", |s| s.tx(&tk, &d, &what, true).map_err(StepErr::from))?;
        self.virt_allow.insert(key, if own { i128::MAX } else { amount });
        Ok(())
    }

    /// This run's own spend of an allowance (a mint, a swap): what is known of it goes down by `amount`.
    fn spent(&mut self, token: &str, spender: &str, amount: i128) {
        if let Some(v) = self.virt_allow.get_mut(&(token.to_lowercase(), spender.to_lowercase())) {
            if *v != i128::MAX {
                *v = (*v - amount.max(0)).max(0);
            }
        }
    }
}

fn simulation_reverted(e: &TxError) -> bool {
    e.message.starts_with("eth_call reverted") || e.message.starts_with("estimateGas failed")
}

#[derive(Debug, PartialEq)]
enum MintFailure {
    /// STF: the pool's transferFrom failed, on an allowance (or a balance)
    Allowance,
    /// TF: the token's transfer failed, the wallet holding less than the mint pulls
    Shortfall,
    /// AL, "Price slippage check": what the mint would deposit is below its floors
    Floors,
    Other,
}

/// A mint simulation's revert reason (as revert_reason gives it: the code, then a hint) by what fixes it.
fn mint_failure(reason: &str) -> MintFailure {
    let r = reason.trim();
    let is = |c: &str| r == c || r.starts_with(&format!("{c} "));
    if is("STF") {
        MintFailure::Allowance
    } else if is("TF") {
        MintFailure::Shortfall
    } else if is("AL") || is("Price slippage check") {
        MintFailure::Floors
    } else {
        MintFailure::Other
    }
}

/// How far the price may move between a mint's sizing and its execution before the wallet would
/// hold too little of a token: a mint is sized to be paid for anywhere in this band, and the ratio
/// swap before it buys the mix that band needs. In a narrow range the ratio a mint pulls swings hard
/// with the price (near an edge of a 7-tick stable range, a third of a basis point moved a third of
/// one side), so a mint sized to the wallet at one price fails its transfer (TF) at the next
/// block's. The band is a share of the range's width (at most MINT_SIZING_BPS): what it holds back
/// is about that share of the value, and a wider band would leave a one-tick range unmintable.
/// What is held back is deployed by the deploy's top-up round.
const MINT_SIZING_SHARE: f64 = 0.05;
const MINT_SIZING_BPS: f64 = 0.5;

/// The sizing band for a range (bp of price).
fn sizing_bps(sa: &BigUint, sb: &BigUint) -> f64 {
    let width_bps = ((to_f64(sb) / to_f64(sa)).powi(2) - 1.0).abs() * 1e4;
    (width_bps * MINT_SIZING_SHARE).min(MINT_SIZING_BPS)
}

/// The share of token0 (by value, `p` token1 per token0) that pays for a mint into [sa, sb) anywhere
/// in the sizing band: token0 as at the band's bottom, token1 as at its top.
fn mint_mix(sp: &BigUint, sa: &BigUint, sb: &BigUint, p: f64) -> f64 {
    let one = BigUint::from(10u8).pow(18);
    let (lo, hi) = price_band(sp, sa, sb, sizing_bps(sa, sb));
    let a0 = to_f64(&crate::tickmath::amounts_for_liquidity(&lo, sa, sb, &one).0);
    let a1 = to_f64(&crate::tickmath::amounts_for_liquidity(&hi, sa, sb, &one).1);
    if a0 + a1 > 0.0 { a0 / (a0 + a1 / p) } else { 1.0 }
}

/// The mint's floors: how far (bp of price) the price may move before the mint reverts instead of
/// depositing at the moved price, widened a step at each revert on them, without asking: the floors
/// only bound the price a mint deposits at. Floors as a share of each amount do not work in a narrow
/// range: the smaller side swings by tens of percent per basis point, so any floor that protects
/// anything reverts on a block's drift (AL). A mint that deposits less than planned leaves the rest
/// in the wallet, where the top-up round picks it up. The pool's slippage setting, when looser, is
/// the first step.
const DEPOSIT_FLOOR_BPS: [f64; 3] = [10.0, 25.0, 50.0];

/// sqrt prices `bps` of price below and above `sp`, kept strictly inside (sa, sb) while `sp` is in
/// it: past an edge the amounts stop changing, so the edge bounds the deposit price there. Out of
/// range the amounts do not depend on the price, and the band is `sp` alone.
fn price_band(sp: &BigUint, sa: &BigUint, sb: &BigUint, bps: f64) -> (BigUint, BigUint) {
    if sp <= sa || sp >= sb {
        return (sp.clone(), sp.clone());
    }
    let e18 = BigUint::from(10u64.pow(18));
    let k = |f: f64| BigUint::from((f.sqrt() * 1e18) as u128);
    let lo = (sp * k(1.0 - bps / 1e4) / &e18).max(sa + 1u8);
    let hi = (sp * k(1.0 + bps / 1e4) / &e18 + 1u8).min(sb - 1u8);
    (lo, hi)
}

/// The liquidity that `x` token0 + `y` token1 pay for wherever in the sizing band the price is.
fn mint_liquidity(sp: &BigUint, sa: &BigUint, sb: &BigUint, x: &BigUint, y: &BigUint) -> BigUint {
    let (lo, hi) = price_band(sp, sa, sb, sizing_bps(sa, sb));
    // token0 binds at the bottom of the band, token1 at the top
    liquidity_for_amounts(&lo, sa, sb, x, y).min(liquidity_for_amounts(&hi, sa, sb, x, y))
}

struct MintBounds {
    /// the most of each token the mint can pull with the price in the sizing band
    max0: i128,
    max1: i128,
    /// its floors: the least of each it deposits with the price in the floor band
    min0: i128,
    min1: i128,
}

fn mint_bounds(sp: &BigUint, sa: &BigUint, sb: &BigUint, liq: i128, floor_bps: f64) -> MintBounds {
    let l = big(liq);
    let at = |s: &BigUint| {
        let (a0, a1) = crate::tickmath::amounts_for_liquidity(s, sa, sb, &l);
        (small(&a0), small(&a1))
    };
    let (slo, shi) = price_band(sp, sa, sb, sizing_bps(sa, sb));
    let (flo, fhi) = price_band(sp, sa, sb, floor_bps);
    // the pool rounds what it pulls up
    MintBounds { max0: at(&slo).0 + 1, max1: at(&shi).1 + 1, min0: at(&fhi).0, min1: at(&flo).1 }
}

#[cfg(test)]
mod mint_tests {
    use super::*;

    // a reverted mint (Base, USDC/USDT [9,16)): the wallet's two balances, and the pool's sqrt price
    // in the block before and in the block that mined it, a third of a basis point higher
    const BAL: (i128, i128) = (3_179_698, 17_196_840);
    const BEFORE: &str = "79287432421297101572492660427";
    const MINED: &str = "79288900042731835809062276645";

    fn sq(s: &str) -> BigUint {
        s.parse().unwrap()
    }

    #[test]
    fn a_mint_sized_before_the_block_goes_through_in_it() {
        let (sa, sb) = (sqrt_ratio_at_tick(9), sqrt_ratio_at_tick(16));
        // a 7-tick range: a band of 0.35 bp
        assert!((sizing_bps(&sa, &sb) - 0.35).abs() < 0.01);
        let liq = (to_f64(&mint_liquidity(&sq(BEFORE), &sa, &sb, &big(BAL.0), &big(BAL.1))) * 0.999) as i128;
        let b = mint_bounds(&sq(BEFORE), &sa, &sb, liq, DEPOSIT_FLOOR_BPS[0]);
        assert!(b.max0 <= BAL.0 && b.max1 <= BAL.1);
        // the block moved the price 0.37 bp up, just past the band: the token1 it pulls is what fits
        // at the band's top plus that last 0.02 bp, within the 0.1% the size is cut by
        let (p0, p1) = crate::tickmath::amounts_for_liquidity(&sq(MINED), &sa, &sb, &big(liq));
        let (p0, p1) = (small(&p0), small(&p1));
        assert!(p0 < BAL.0 && p1 < BAL.1, "pulls {p0} + {p1}: more than the wallet holds (TF)");
        assert!(p0 >= b.min0 && p1 >= b.min1, "deposits {p0} + {p1}, below the floors {} + {} (AL)", b.min0, b.min1);
        // the app's old mint (this liquidity, floors 2 bp of each amount) fell below its USDC floor
        let (o0, _) = crate::tickmath::amounts_for_liquidity(&sq(MINED), &sa, &sb, &big(54_162_105_889));
        assert!(small(&o0) < 2_595_776);
    }

    /// The wallet's value (token1) a mint into [sa, sb) at `sp` uses, after a ratio swap to mint_mix.
    fn used_after_swap(sp: &BigUint, sa: &BigUint, sb: &BigUint, value1: f64) -> f64 {
        let p = (to_f64(sp) / 2f64.powi(96)).powi(2);
        let f = mint_mix(sp, sa, sb, p);
        let (x, y) = ((f * value1 / p) as i128, ((1.0 - f) * value1) as i128);
        let liq = (to_f64(&mint_liquidity(sp, sa, sb, &big(x), &big(y))) * 0.999) as i128;
        let (a0, a1) = crate::tickmath::amounts_for_liquidity(sp, sa, sb, &big(liq));
        (to_f64(&a0) * p + to_f64(&a1)) / value1
    }

    #[test]
    fn a_one_tick_range_still_takes_most_of_the_wallet() {
        // cbETH/WETH [1329,1330), the price near the top of the tick (the position nearly all WETH):
        // 0.6.4's 0.5 bp band was half the tick and left nothing to mint
        let (sa, sb) = (sqrt_ratio_at_tick(1329), sqrt_ratio_at_tick(1330));
        assert!((sizing_bps(&sa, &sb) - 0.05).abs() < 0.001);
        for k in [3u32, 500, 997] {
            let sp = &sa + (&sb - &sa) * BigUint::from(k) / BigUint::from(1000u32);
            // the band holds back about its share of the width on each side it reaches (up to ~10%),
            // which the top-up round mints next
            let used = used_after_swap(&sp, &sa, &sb, 0.13e18);
            assert!(used > 0.88, "{k}/1000 into the tick: only {used:.3} of the wallet minted");
        }
        // and the stable range of the reported mint
        let (sa, sb) = (sqrt_ratio_at_tick(9), sqrt_ratio_at_tick(16));
        let used = used_after_swap(&sq(BEFORE), &sa, &sb, 20.0e6);
        assert!(used > 0.88, "stable range: only {used:.3} minted");
    }

    #[test]
    fn floors_stay_inside_the_range_and_bound_both_sides() {
        let (sa, sb) = (sqrt_ratio_at_tick(9), sqrt_ratio_at_tick(16));
        let sp = sq(BEFORE);
        let (lo, hi) = price_band(&sp, &sa, &sb, 50.0);
        assert!(lo > sa && hi < sb);
        let b = mint_bounds(&sp, &sa, &sb, 54_000_000_000, 50.0);
        assert!(b.min0 <= b.max0 && b.min1 <= b.max1);
        // a wide range keeps the whole band, and real floors on both sides
        let (wa, wb) = (sqrt_ratio_at_tick(-500), sqrt_ratio_at_tick(500));
        let w = mint_bounds(&sqrt_ratio_at_tick(0), &wa, &wb, 10i128.pow(12), 10.0);
        assert!(w.min0 > 0 && w.min1 > 0 && w.min0 < w.max0 && w.min1 < w.max1);
        // out of range the amounts do not move with the price: the floors are the amounts
        let o = mint_bounds(&sqrt_ratio_at_tick(5), &sa, &sb, 10i128.pow(12), 50.0);
        assert_eq!((o.min0 + 1, o.min1 + 1), (o.max0, o.max1));
    }

    #[test]
    fn a_mint_revert_is_told_by_what_fixes_it() {
        assert_eq!(mint_failure("STF (token transferFrom failed (balance or allowance too low))"), MintFailure::Allowance);
        assert_eq!(mint_failure("TF (token transfer failed)"), MintFailure::Shortfall);
        assert_eq!(mint_failure("AL (amounts below the minimums: the price moved (slippage))"), MintFailure::Floors);
        assert_eq!(mint_failure("Price slippage check (pool price moved past the min amounts (slippage))"), MintFailure::Floors);
        for r in ["LOK (pool is locked (reentrancy guard))", "TFX", "ALX", ""] {
            assert_eq!(mint_failure(r), MintFailure::Other, "{r}");
        }
    }
}

/// Ratio swaps that no mint has followed yet, per (chain, pool): when made, which way (true: token0
/// in) and the share of token0 (by value) the ranges wanted then. While a mint keeps failing, the next pass would otherwise size a new ratio swap from the
/// balances the last one left, and can swap back the other way, paying the swap cost each time with
/// nothing deployed. Kept in memory: a restart starts afresh.
static UNMINTED_SWAP: std::sync::Mutex<Option<HashMap<(u64, String), (u64, bool, f64)>>> = std::sync::Mutex::new(None);
/// How long an unminted swap holds back a swap the other way.
const SWAP_BACK_HOLD_S: u64 = 6 * 3600;

/// How far (share of token0 by value) the ranges' wanted mix may move before a swap the other way is
/// the price having moved, not a swap being undone: in a one-tick range the mix runs from all of one
/// token to all of the other across the tick.
const SWAP_BACK_MIX_MOVE: f64 = 0.1;

/// Whether a ratio swap `zero_for_one`, for ranges that want `need0` of token0 now, would undo an
/// earlier one that no mint followed.
fn swaps_back(earlier: Option<(u64, bool, f64)>, now: u64, zero_for_one: bool, need0: f64) -> bool {
    matches!(earlier, Some((at, dir, then)) if dir != zero_for_one && now.saturating_sub(at) < SWAP_BACK_HOLD_S
        && (need0 - then).abs() < SWAP_BACK_MIX_MOVE)
}

fn unminted_key(cid: u64, pool: &str) -> (u64, String) {
    (cid, pool.to_lowercase())
}

#[cfg(test)]
mod swap_back_tests {
    use super::swaps_back;

    #[test]
    fn a_ratio_swap_does_not_undo_an_earlier_one_that_no_mint_followed() {
        let now = 1_000_000;
        // swapped token0 in an hour ago (the ranges wanted 30% token0) and nothing was minted: swapping
        // token1 in now, for the same mix, would undo it
        assert!(swaps_back(Some((now - 3600, true, 0.3)), now, false, 0.33));
        // the same way again is a top-up, not a swap back
        assert!(!swaps_back(Some((now - 3600, true, 0.3)), now, true, 0.3));
        // after the hold, or with nothing pending, the strategy decides again
        assert!(!swaps_back(Some((now - 7 * 3600, true, 0.3)), now, false, 0.3));
        assert!(!swaps_back(None, now, false, 0.3));
        // the price moved across a one-tick range: cbETH was swapped for WETH when the range wanted
        // almost none of it (0.3%), and it wants 37% now; that is a new mix, not the swap undone
        assert!(!swaps_back(Some((now - 250 * 60, true, 0.003)), now, false, 0.37));
    }
}

/// Make the pool's strategy decide afresh on its next cycle instead of on its schedule, and take the
/// new target whatever the turnover: the strategy's last move is forgotten (a ladder block re-solves;
/// a range strategy is past its cooldown).
pub fn resolve_now(cid: u64, cfg: &PoolConfig, live: bool) {
    let mut st = crate::custom::load_state(cid, &cfg.pool, live);
    st.last_move = 0;
    st.force_solve = true;
    crate::custom::save_state(cid, &cfg.pool, live, &st);
    crate::info!("worker", "[{}] re-solving on this cycle (a parked lot was handed over to it)", cfg.name);
}

pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Python's "{:,.4f}".
pub fn commas(v: f64) -> String {
    commas_p(v, 4)
}

pub fn commas_p(v: f64, prec: usize) -> String {
    let s = format!("{:.*}", prec, v.abs());
    let (int, frac) = s.split_once('.').map(|(a, b)| (a.to_string(), format!(".{b}"))).unwrap_or((s.clone(), String::new()));
    let n: u64 = int.parse().unwrap_or(0);
    format!("{}{}{}", if v < 0.0 && v.abs() >= 0.5 * 10f64.powi(-(prec as i32)) { "-" } else { "" }, thousands(n), frac)
}

pub fn q96f() -> f64 {
    to_f64(&q96())
}
