//! Strategy decision -> exact Solidly V3 transactions (alm/executor/engine.py, ladder only).
//! Same modes (exit / rebalance / topup / keep), checks, partial resize, swaps through the
//! aggregator, mints with shrink-on-failure, reward sales and cross-pool funding.

use crate::chain::{self, TxError};
use crate::data::{self, Market};
use crate::eth::{self, addr, calldata, u, Tok};
use crate::ladder::{self, Params};
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
    let mut out = vec![];
    for (tl, tu) in keys {
        let liq = chain::position_liquidity(cid, pool, wallet, tl, tu)?;
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

/// Turn current positions into the target with the fewest movements (see engine.py resize_plan).
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
        StepErr::Fail(e.message)
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
    attempts: usize,
    backoff: f64,
    virt: HashMap<String, i128>,
    virt_allow: HashMap<(String, String), i128>,
    v_deployed: f64,
    v_removed: f64,
    withdrawn: HashSet<(i64, i64)>,
    pub pos_val0: f64,
    pub report: Vec<String>,
    pub is_solid_lp: bool,
    pub solid_pair: Option<(String, String)>, // (solid, pair) for the SOLID LP usable() rule
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

    pub fn bal(&self, token: &str) -> Res<i128> {
        Ok(small(&chain::erc20_balance(self.cid, token, &self.wallet)?) + self.virt.get(&token.to_lowercase()).copied().unwrap_or(0))
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
            match f(self) {
                Ok(v) => return Ok(v),
                Err(StepErr::Abort(m)) => return Err(StepErr::Abort(m)),
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
            let res = chain::send(self.cid, self.account.as_ref().unwrap(), to, data, gas)?;
            entry["kind"] = json!("sent");
            entry["hash"] = json!(res.hash);
            entry["block"] = json!(res.block);
            entry["gas_used"] = json!(res.gas_used);
            self.say(format!("    sent {} (block {}, gas {})", res.hash, res.block, thousands(res.gas_used)));
        } else {
            entry["kind"] = json!("dry_run");
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

    fn begin(&self, mode: &str, target: Option<&Target>) {
        let ranges: Vec<Value> = target.map(|t| t.ranges.iter().map(|(a, b, w)| json!([a, b, w])).collect()).unwrap_or_default();
        log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "rebalance", "mode": mode, "live": self.execute, "ranges": ranges}));
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
        let idle0 = self.value0(b0 as f64, b1 as f64, p);
        let budget0 = self.budget_usd.map(|b| b / usd0).unwrap_or(pos_val + idle0);
        self.say(format!("  deployed {}  idle {}  budget {} (token0 units, {}% of the account)", commas(pos_val), commas(idle0), commas(budget0), self.cfg.allocation_pct));
        let pos_json: Vec<Value> = positions.iter().map(|(a, b, l)| json!([a, b, l.to_string()])).collect();
        let mut summary = json!({"chain_id": self.cid, "pool": self.cfg.pool, "name": self.cfg.name, "tick": tick, "fee_pips": fee,
                                 "deployed0": pos_val, "idle0": idle0, "budget0": budget0, "usd0": usd0, "positions": pos_json, "mode": "keep"});
        if (budget0 <= 0.0 || !self.cfg.enabled) && !positions.is_empty() {
            self.say("  allocation is 0 / pool disabled: withdrawing all positions");
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
        let params = Params::from(&self.cfg.strategy)?;
        if ladder::frozen(m, &params) {
            self.say(format!("  DEPEG GUARD: holding, no action (tick {tick} too far from the 7-day median)"));
            log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "guard_freeze", "tick": tick}));
            summary["mode"] = json!("guard");
            return Ok(summary);
        }
        let spf = to_f64(&sp) / 2f64.powi(96);
        let mut target = self.strategy_target(m, &params, &positions, &sp, p, budget0, spf, tick)?;
        let idle_usable = idle0.min((budget0 - pos_val).max(0.0));
        let thr = budget0 * self.settings["idle_redeploy_pct"].as_f64().unwrap_or(2.0) / 100.0;
        if target.is_none() && pos_val > budget0 * 1.10 {
            self.say(format!("  pool holds {} > 110% of its budget {}: rebalance down to budget", commas(pos_val), commas(budget0)));
            target = self.current_as_target(&positions, &sp, p).or_else(|| Some(self.fresh_target(m, &params, budget0, spf, tick)));
        }
        if let Some(t) = target {
            summary["mode"] = json!("rebalance");
            let wants: Vec<String> = t.ranges.iter().map(|(a, c, w)| format!("[{a},{c}) {:.0}%", w * 100.0)).collect();
            self.say(format!("  {} wants: {}", params.label(), wants.join(", ")));
            if self.blocked(m, tick, false)? {
                return Ok(summary);
            }
            self.begin("rebalance", Some(&t));
            self.resize(&positions, &t, budget0, &sp, p)?;
        } else if idle_usable > thr.max(self.cfg.min_swap_value) {
            summary["mode"] = json!("topup");
            let t = self.current_as_target(&positions, &sp, p).unwrap_or_else(|| self.fresh_target(m, &params, budget0, spf, tick));
            let rs: Vec<String> = t.ranges.iter().map(|(a, c, _)| format!("[{a},{c})")).collect();
            self.say(format!("  {} idle (> {}): deploying into {}", commas(idle_usable), commas(thr), rs.join(", ")));
            if self.blocked(m, tick, false)? {
                return Ok(summary);
            }
            self.begin("topup", Some(&t));
            self.deploy(&t, budget0)?;
        } else {
            self.say(format!("  {}: keep current position", params.label()));
            log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "keep", "tick": tick}));
        }
        Ok(summary)
    }

    // -- targets -----------------------------------------------------------------------------------
    #[allow(clippy::too_many_arguments)]
    fn strategy_target(&mut self, m: &Market, params: &Params, positions: &[(i64, i64, BigUint)], sp: &BigUint, p: f64, budget0: f64,
                       spf: f64, tick: i64) -> Res<Option<Target>> {
        let state = crate::paths::logs().join(format!("last_solve_{}.json", self.cfg.name));
        let last = std::fs::read_to_string(&state).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v["ts"].as_f64()).unwrap_or(0.0);
        let period = (params.every * 300) as f64;
        let el = now() as f64 - last;
        if !positions.is_empty() && el < period {
            self.say(format!("  ladder: next re-solve in {:.1} h", (period - el) / 3600.0));
            return Ok(None);
        }
        if self.execute {
            let _ = std::fs::create_dir_all(state.parent().unwrap());
            let _ = std::fs::write(&state, json!({"ts": now()}).to_string());
        }
        let ranges = cap_ranges(snap_ranges(ladder::decide(m, params, budget0, spf, tick), self.spacing), self.cfg.max_ranges);
        if !positions.is_empty() {
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
            for (a, c, w) in &ranges {
                for t in *a..*c {
                    *new_w.entry(t).or_default() += w / (c - a) as f64;
                }
            }
            let keys: HashSet<i64> = cur_w.keys().chain(new_w.keys()).cloned().collect();
            let moved: f64 = keys.iter().map(|t| (new_w.get(t).unwrap_or(&0.0) - cur_w.get(t).unwrap_or(&0.0) / s).abs()).sum::<f64>() / 2.0;
            if moved < params.turnover {
                self.say(format!("  ladder target differs by {:.0}% < turnover {:.0}%: keep", moved * 100.0, params.turnover * 100.0));
                return Ok(None);
            }
        }
        Ok(Some(Target { ranges, swap: true }))
    }

    fn fresh_target(&self, m: &Market, params: &Params, budget0: f64, spf: f64, tick: i64) -> Target {
        Target { ranges: cap_ranges(snap_ranges(ladder::decide(m, params, budget0, spf, tick), self.spacing), self.cfg.max_ranges), swap: true }
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
        log(json!({"chain_id": self.cid, "pool": self.cfg.pool, "kind": "plan", "burns": bj, "adds": aj, "tolerance": tol}));
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
                    Ok(true) => minted_any = true,
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

    fn need0_fraction(&self, ranges: &[(i64, i64, f64)], sp: &BigUint, p: f64) -> f64 {
        let one = BigUint::from(10u8).pow(18);
        ranges
            .iter()
            .map(|(tl, tu, wi)| {
                let (a0, a1) = amounts_at(sp, *tl, *tu, &one);
                let (a0, a1) = (to_f64(&a0), to_f64(&a1));
                wi * if a0 + a1 > 0.0 { a0 / (a0 + a1 / p) } else { 1.0 }
            })
            .sum()
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
        self.ensure_allowance(tin, amount_in, Some(&spender))?;
        let built = prices::build(self.cid, &best, &self.wallet, self.cfg.slippage_bps / 1e4)?;
        let router = built["routerAddress"].as_str().unwrap_or("").to_lowercase();
        if router != best["routerAddress"].as_str().unwrap_or("").to_lowercase() {
            return Err(StepErr::Fail("router changed between quote and build; re-quoting".into()));
        }
        let before = small(&chain::erc20_balance(self.cid, tout, &self.wallet)?);
        let live = small(&chain::erc20_balance(self.cid, tin, &self.wallet)?) >= amount_in;
        self.tx(&router, built["calldata"].as_str().unwrap(), &format!("swap via {}", best["aggregatorId"].as_str().unwrap_or("?")), live)?;
        if self.execute {
            let got = small(&chain::erc20_balance(self.cid, tout, &self.wallet)?) - before;
            self.say(format!("    received {:.6}", got as f64 / 10f64.powi(mo.decimals as i32)));
            return Ok(got);
        }
        self.mv(tin, -amount_in);
        self.mv(tout, out as i128);
        Ok(out as i128)
    }

    pub fn mint_one(&mut self, tl: i64, tu: i64, share: f64, budget0: f64) -> Res<bool> {
        let (sp, _, _, p) = self.price()?;
        let (sa, sb) = (sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu));
        let (x, y) = self.usable(budget0, p)?;
        let one = BigUint::from(10u8).pow(18);
        let (a0, a1) = crate::tickmath::amounts_for_liquidity(&sp, &sa, &sb, &one);
        let (fa0, fa1) = (to_f64(&a0), to_f64(&a1));
        let f0 = if fa0 + fa1 > 0.0 { fa0 / (fa0 + fa1 / p) } else { 1.0 };
        let v0 = (x as f64 + y as f64 / p) * share;
        let x_r = ((f0 * v0) as i128).min(x);
        let y_r = (((1.0 - f0) * v0 * p) as i128).min(y);
        let mut liq = (to_f64(&liquidity_for_amounts(&sp, &sa, &sb, &big(x_r), &big(y_r))) * 0.999) as i128;
        if liq > 0 {
            let (m0, m1) = crate::tickmath::amounts_for_liquidity(&sp, &sa, &sb, &big(liq));
            if self.value0(to_f64(&m0), to_f64(&m1), p) < self.cfg.min_swap_value {
                liq = 0;
            }
        }
        if liq <= 0 {
            self.say(format!("  f. mint [{tl},{tu}): nothing to add (usable {} t0 / {} t1)", self.fmt0(x as f64), self.fmt1(y as f64)));
            return Ok(false);
        }
        let slip = if self.is_solid_lp { self.cfg.slippage_bps / 1e4 } else { self.cfg.slippage_bps / 1e4 };
        for _ in 0..6 {
            let (m0, m1) = crate::tickmath::amounts_for_liquidity(&sp, &sa, &sb, &big(liq));
            let (m0, m1) = (small(&m0), small(&m1));
            let (t0, t1) = (self.t0.clone(), self.t1.clone());
            self.ensure_allowance(&t0, m0 + m0 / 1000 + 10, None)?;
            self.ensure_allowance(&t1, m1 + m1 / 1000 + 10, None)?;
            let data = calldata(
                "mint(address,int24,int24,uint128,uint256,uint256,uint256)",
                &[addr(&self.wallet), chain::int(tl), chain::int(tu), u(big(liq)), u(big((m0 as f64 * (1.0 - slip)) as i128)),
                  u(big((m1 as f64 * (1.0 - slip)) as i128)), u(now() + DEADLINE_S)],
            );
            self.say(format!("  f. mint [{tl},{tu}) liquidity {liq} uses {} t0 + {} t1", self.fmt0(m0 as f64), self.fmt1(m1 as f64)));
            let pool = self.cfg.pool.clone();
            let live = small(&chain::erc20_balance(self.cid, &t0, &self.wallet)?) >= m0
                && small(&chain::erc20_balance(self.cid, &t1, &self.wallet)?) >= m1
                && small(&chain::erc20_allowance(self.cid, &t0, &self.wallet, &pool)?) >= m0
                && small(&chain::erc20_allowance(self.cid, &t1, &self.wallet, &pool)?) >= m1;
            match self.tx(&pool, &data, &format!("mint {tl}:{tu}"), live) {
                Ok(_) => {}
                Err(e) if e.message.starts_with("eth_call reverted") || e.message.starts_with("estimateGas failed") => {
                    liq = (liq as f64 * 0.995) as i128;
                    self.say_warn(format!("    simulation failed ({}); shrinking liquidity 0.5%", e.reason));
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
            self.mv(&t0, -m0);
            self.mv(&t1, -m1);
            if !self.execute {
                self.v_deployed += self.value0(m0 as f64, m1 as f64, p);
            }
            return Ok(true);
        }
        Err(StepErr::Fail(format!("mint [{tl},{tu}) still fails after shrinking liquidity by ~3%")))
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
        let fts: Vec<String> = self.settings["funding_tokens"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|t| t.as_str().map(|s| s.to_lowercase()))
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
                let b = small(&chain::erc20_balance(self.cid, &ft, &self.wallet)?) + self.virt.get(&ft).copied().unwrap_or(0);
                let b = b.max(0);
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

    /// Exact-amount approvals only; a too-small non-zero allowance is reset to 0 first (USDT).
    pub fn ensure_allowance(&mut self, token: &str, amount: i128, spender: Option<&str>) -> Res<()> {
        let spender = spender.unwrap_or(&self.cfg.pool).to_lowercase();
        let key = (token.to_lowercase(), spender.clone());
        let onchain = small(&chain::erc20_allowance(self.cid, token, &self.wallet, &spender)?);
        let cur = self.virt_allow.get(&key).copied().unwrap_or(onchain);
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
        self.say(format!("  approve {} -> {} for {:.6}", m.symbol, &spender[..10], amount as f64 / 10f64.powi(m.decimals as i32)));
        let d = calldata("approve(address,uint256)", &[addr(&spender), u(big(amount))]);
        let what = format!("approve {}", m.symbol);
        let tk = token.to_string();
        self.retry("approve", |s| s.tx(&tk, &d, &what, true).map_err(StepErr::from))?;
        if !self.execute {
            self.virt_allow.insert(key, amount);
        }
        Ok(())
    }
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
