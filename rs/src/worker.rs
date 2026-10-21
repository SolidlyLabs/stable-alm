//! The worker loop, per chain: data refresh, account value, claims, SOLID LP,
//! one executor pass per pool, and a snapshot for the value history. Each pool runs on its own
//! capital (capital.rs): what it earns stays with it, and money moves between pools only on request.
//! Several wallets (wallets.rs): each cycle runs them one after another, each inside its scope (its own
//! folders, pools, key and status block); one wallet's failure does not stop the others.

use crate::capital;
use crate::chains;
use crate::engine::{self, wallet_positions, Executor};
use crate::prices;
use crate::settings::{self, PoolConfig};
use crate::tickmath::{amounts_at, to_f64};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Days of swaps a pool's live replay holds: 10, or what its node strategy needs.
fn replay_days(cfg: &PoolConfig) -> i64 {
    if cfg.strategy.get("type").and_then(|t| t.as_str()) == Some("graph") {
        if let Ok(g) = crate::graph::Graph::parse(&cfg.strategy, 1) {
            return g.replay_days();
        }
    }
    10
}

/// How often a pool's strategy decides, in seconds: 60 for a node strategy with a 1-minute block
/// (it is checked every minute between cycles, on 1-minute bars, as its backtest decides), else the
/// cycle's own interval on 5-minute bars.
fn pool_bar_s(cfg: &PoolConfig) -> i64 {
    if cfg.strategy.get("type").and_then(|t| t.as_str()) == Some("graph") {
        if let Ok(g) = crate::graph::Graph::parse(&cfg.strategy, 1) {
            return g.bar_s();
        }
    }
    crate::data::BAR_S
}

fn fresh_status() -> Value {
    json!({"state": "starting", "last_cycle": null, "last_error": null, "pools": {}, "mode": "dry run"})
}

pub fn pool_chain(p: &Value) -> u64 {
    p["chain_id"].as_u64().unwrap_or(chains::BASE)
}

pub struct Worker {
    /// The first wallet's status (the agent and `--once` read this one).
    pub status: Mutex<Value>,
    /// The other wallets' status blocks, by wallet id.
    others: Mutex<BTreeMap<String, Value>>,
    wake: (Mutex<bool>, Condvar),
    pub stopping: Mutex<bool>,
    /// wallets whose rewards are claimed in the next cycle whatever their schedule
    force_claim: Mutex<std::collections::BTreeSet<String>>,
    running: Mutex<()>,
}

impl Worker {
    pub fn new() -> Arc<Worker> {
        Arc::new(Worker {
            status: Mutex::new(fresh_status()),
            others: Mutex::new(BTreeMap::new()),
            wake: (Mutex::new(false), Condvar::new()),
            stopping: Mutex::new(false),
            force_claim: Mutex::new(Default::default()),
            running: Mutex::new(()),
        })
    }

    pub fn request(&self, claim_now: bool) {
        if claim_now {
            self.force_claim.lock().unwrap().insert(crate::paths::wallet());
        }
        *self.wake.0.lock().unwrap() = true;
        self.wake.1.notify_all();
    }

    pub fn stop(&self) {
        *self.stopping.lock().unwrap() = true;
        self.request(false);
    }

    fn set(&self, k: &str, v: Value) {
        self.with_status(|st| st[k] = v);
    }

    /// Change the status block of the wallet this thread works for.
    fn with_status<T>(&self, f: impl FnOnce(&mut Value) -> T) -> T {
        let id = crate::paths::wallet();
        if id == crate::paths::MAIN {
            return f(&mut self.status.lock().unwrap());
        }
        let mut o = self.others.lock().unwrap();
        f(o.entry(id).or_insert_with(fresh_status))
    }

    /// A copy of one wallet's status block.
    pub fn status_of(&self, id: &str) -> Value {
        if id == crate::paths::MAIN {
            return self.status.lock().unwrap().clone();
        }
        self.others.lock().unwrap().get(id).cloned().unwrap_or_else(fresh_status)
    }

    pub fn loop_forever(&self) {
        while !*self.stopping.lock().unwrap() {
            self.cycle();
            if *self.stopping.lock().unwrap() {
                break;
            }
            let interval = settings::load_settings()["interval_s"].as_f64().filter(|x| x.is_finite()).unwrap_or(300.0).clamp(30.0, 86400.0);
            let deadline = std::time::Instant::now() + Duration::from_secs_f64(interval);
            self.set("next_cycle", json!(engine::now() as f64 + interval));
            // between cycles: pools whose strategy decides every minute are checked every minute
            loop {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    break;
                }
                let minute = crate::wallets::ids().iter().any(|id| crate::paths::scope(id, || settings::load_pools().iter().any(|p| {
                    let c = PoolConfig::from(p);
                    c.enabled && pool_bar_s(&c) == 60
                })));
                // the next minute boundary (bars end on the minute), or the cycle, whichever is first
                let to_minute = Duration::from_secs(60 - engine::now() as u64 % 60) + Duration::from_millis(1500);
                let wait = if minute { left.min(to_minute) } else { left };
                let (m, cv) = &self.wake;
                let g = m.lock().unwrap();
                let (mut g, res) = cv.wait_timeout_while(g, wait, |w| !*w).unwrap();
                let woken = !res.timed_out();
                *g = false;
                drop(g);
                if woken || *self.stopping.lock().unwrap() || std::time::Instant::now() >= deadline {
                    break;
                }
                if minute {
                    self.minute_pass();
                }
            }
        }
        self.set("state", json!("stopped"));
    }

    /// The pools that decide every minute, between full cycles: their swaps topped up, their strategy
    /// run on 1-minute bars with the budget of the last cycle's valuation.
    fn minute_pass(&self) {
        let _g = self.running.lock().unwrap();
        for id in crate::wallets::ids() {
            crate::paths::scope(&id, || self.minute_pass_one());
        }
    }

    fn minute_pass_one(&self) {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
            let s = settings::load_settings();
            let wallet = s["wallet"].as_str().unwrap_or("").to_lowercase();
            if wallet.len() != 42 {
                return Ok(());
            }
            let (execute, _) = self.mode(&s, &wallet)?;
            let acct = self.with_status(|st| st["account"].clone());
            for p in settings::load_pools() {
                let cfg = PoolConfig::from(&p);
                if !cfg.enabled || pool_bar_s(&cfg) != 60 {
                    continue;
                }
                let cid = pool_chain(&p);
                if let Err(e) = crate::data::sync_pool(cid, &cfg.pool, &wallet, replay_days(&cfg).max(13) + 1) {
                    crate::warn!("data", "[{}] minute refresh failed: {e}", cfg.name);
                    continue;
                }
                let mut chain_execute = execute;
                if execute {
                    if let Err(e) = crate::chain::reconcile_pending(cid, &wallet) {
                        crate::warn!("worker", "chain {cid}: {e}");
                        chain_execute = false;
                    }
                }
                let incomplete = acct["chains"][cid.to_string()]["incomplete"].as_array().map(|a| !a.is_empty()).unwrap_or(true);
                if incomplete {
                    continue; // nothing valued yet, or not everything: the full cycle decides
                }
                let entry = self.run_pool(cid, &cfg, &acct, &wallet, &s, chain_execute);
                self.publish(cid, &cfg, &wallet, entry);
            }
            Ok(())
        }));
        match r {
            Ok(Err(e)) => crate::warn!("worker", "minute check failed: {e}"),
            Err(_) => crate::error!("worker", "minute check panicked"),
            _ => {}
        }
    }

    /// Live or dry run, and how the dashboard names it.
    fn mode(&self, s: &Value, wallet: &str) -> Result<(bool, &'static str), String> {
        let has_signer = crate::signer::configured();
        let stop = crate::paths::execution_blocked().is_some();
        let mut execute = s["execute"].as_bool().unwrap_or(false) && has_signer && !stop;
        if s["execute"].as_bool().unwrap_or(false) && !has_signer {
            crate::error!("worker", "live trading is on but no private key is set; running as a dry run");
        }
        if execute {
            let a = crate::signer::load_account()?.address.to_lowercase();
            if a != wallet {
                crate::error!("worker", "private key address {a} does not match the wallet {wallet}; running as a dry run");
                execute = false;
            }
        }
        let label = if execute { "LIVE" } else if crate::paths::stop_file().exists() { "paused (STOP)" } else if stop { "paused" } else { "dry run" };
        Ok((execute, label))
    }

    /// One pool's executor pass: its replay (on the bars its strategy decides on), the decision and
    /// any transactions; the dashboard entry for it.
    /// A pool's decision on the dashboard as soon as it is made. When the pool acted (moved, topped up,
    /// withdrew) its positions are read again now, so the dashboard shows what is on chain instead of
    /// what was there when the cycle began (the account's totals follow at the end of the cycle).
    fn publish(&self, cid: u64, cfg: &PoolConfig, wallet: &str, entry: Value) {
        let acted = ["rebalance", "topup", "exit"].contains(&entry["mode"].as_str().unwrap_or("")) || entry["mode"] == "error";
        self.with_status(|st| st["pools"][&cfg.name] = entry);
        if acted {
            match pool_value(cid, cfg, wallet) {
                Ok((info, ..)) => self.with_status(|st| st["account"]["pools"][&cfg.name] = info),
                Err(e) => crate::warn!("worker", "[{}] positions not re-read after acting: {e}", cfg.name),
            }
        }
    }

    /// A pool's withdrawals (capital.rs releases), before its run: live, its idle tokens and then a
    /// part of its positions are made USDC, and that USDC leaves its capital; in a dry run its idle
    /// tokens are moved in the book as they are. The wallet's balances after them, and the report.
    fn release(&self, cid: u64, cfg: &PoolConfig, wallet: &str, s: &Value, execute: bool, book: &mut capital::Book, uni: &[String], mut before: capital::Tokens)
        -> Result<(capital::Tokens, Vec<String>, bool), String> {
        let mut report = vec![];
        let mut any = false;
        for r in capital::releases(cid, &cfg.pool) {
            let id = r["id"].as_str().unwrap_or("").to_string();
            let usd = r["usd"].as_f64().unwrap_or(0.0);
            let to = r["to"].as_str().map(|x| x.to_lowercase());
            let dest = to.clone().unwrap_or_else(|| "Unallocated".into());
            any = true;
            if !execute {
                let moved = capital::move_in_book(book, cid, &cfg.pool, usd, to.as_deref());
                report.push(format!("  withdrawal (dry run): ${} of idle tokens moved to {dest} as they are (live, positions would be burned and sold to USDC as needed)", engine::commas(moved)));
                capital::remove(&id);
                capital::save(execute, book);
                continue;
            }
            let own = book.pool(cid, &cfg.pool);
            let reserve = capital::reserve(&own, &before);
            // the asset it is sold to: the chain's USDC, or what an emergency exit or a plan chose
            let usdc = r["safe"].as_str().filter(|a| crate::agent::is_addr(a)).map(|a| a.to_lowercase()).unwrap_or_else(|| chains::get(cid).usd.to_lowercase());
            let mut rx = Executor::new(crate::agent::exec::bounded(cfg), cid, wallet, s, execute, None)?;
            rx.set_capital(0.0, reserve.clone());
            let others: Vec<String> = own.keys().cloned().collect();
            let res = rx.release(usd, &usdc, &others, r["max_cost_bp"].as_f64());
            report.extend(rx.report.drain(..));
            let after = capital::balances(cid, uni, wallet)?;
            let mut mine = capital::after_run(&reserve, &after);
            let want = (usd / capital::px_raw(cid, &usdc)?) as i128;
            let moved = mine.get(&usdc).copied().unwrap_or(0).min(want);
            *mine.entry(usdc.clone()).or_default() -= moved;
            book.set_pool(cid, &cfg.pool, mine);
            if let Some(t) = &to {
                let mut tb = book.pool(cid, t);
                *tb.entry(usdc.clone()).or_default() += moved;
                book.set_pool(cid, t, tb);
            }
            let moved_usd = moved as f64 * capital::px_raw(cid, &usdc).unwrap_or(0.0);
            report.push(format!("  withdrawal: ${} USDC moved to {dest} (asked ${}){}", engine::commas(moved_usd), engine::commas(usd),
                                res.as_ref().err().map(|e| format!("; stopped early: {e}")).unwrap_or_default()));
            engine::log(json!({"chain_id": cid, "pool": cfg.pool, "kind": "capital", "what": "withdraw", "usd": moved_usd, "asked_usd": usd, "to": to,
                               "asset": usdc, "plan": r["plan"], "emergency": r["emergency"]}));
            capital::remove(&id);
            capital::save(execute, book);
            before = after;
        }
        Ok((before, report, any))
    }

    fn run_pool(&self, cid: u64, cfg: &PoolConfig, acct: &Value, wallet: &str, s: &Value, execute: bool) -> Value {
        let all = settings::load_pools();
        let chain_pools: Vec<PoolConfig> = all.iter().filter(|p| pool_chain(p) == cid).map(PoolConfig::from).collect();
        let refs: Vec<&PoolConfig> = chain_pools.iter().collect();
        let mut budget = 0.0;
        let mut book = capital::load(execute);
        let res = (|| -> Result<Value, String> {
            // its own tokens only: the rest of the wallet is reserved for the other pools and Unallocated
            let uni = capital::universe(cid, &refs, s, acct, &book);
            let before = capital::balances(cid, &uni, wallet).map_err(|e| format!("wallet balances not read, so the pool's own tokens are not known: {e}"))?;
            let (before, released, any) = self.release(cid, cfg, wallet, s, execute, &mut book, &uni, before)?;
            let mut acct = acct.clone();
            if any && execute {
                if let Ok((info, ..)) = pool_value(cid, cfg, wallet) {
                    acct["pools"][&cfg.name] = info;
                }
            }
            let own = book.pool(cid, &cfg.pool);
            let reserve = capital::reserve(&own, &before);
            let parts = capital::parts(execute, cid, cfg, &acct, &own);
            budget = if cfg.enabled { parts.budget() } else { 0.0 };
            let mut ex = Executor::new(crate::agent::exec::bounded(cfg), cid, wallet, s, execute, None)?;
            ex.set_capital(budget, reserve.clone());
            ex.report.extend(released);
            let num = cfg.numeraire;
            let bar_s = pool_bar_s(cfg);
            let m = crate::data::live_market_bars(cid, &cfg.pool, wallet, ex.dec0, ex.dec1, num, replay_days(cfg), bar_s)?;
            let r = ex.run(&m);
            let mut summary = match r {
                Ok(v) => v,
                Err(e) => {
                    ex.say_err(format!("  failed: {e}"));
                    json!({"chain_id": cid, "pool": cfg.pool, "name": cfg.name, "mode": "error", "error": e.to_string()})
                }
            };
            if execute {
                // what changed in the wallet during the run is the pool's
                match capital::balances(cid, &uni, wallet) {
                    Ok(after) => {
                        book.set_pool(cid, &cfg.pool, capital::after_run(&reserve, &after));
                        capital::save(execute, &book);
                    }
                    Err(e) => crate::error!("worker", "[{}] balances after the run not read ({e}): the pool's idle tokens are as before the run until the next one", cfg.name),
                }
            }
            let n = ex.report.len();
            summary["report"] = json!(ex.report[n.saturating_sub(60)..]);
            summary["budget_usd"] = json!(budget);
            summary["capital_usd"] = json!(parts.capital());
            summary["decides_every_s"] = json!(bar_s);
            // Monitor only (reports/VOLATILE.md): shown on the dashboard, not used for trading.
            summary["flvr"] = crate::data::pool_flvr(cid, &cfg.pool, ex.dec0, ex.dec1, num);
            Ok(summary)
        })();
        res.unwrap_or_else(|e| {
            crate::error!("worker", "[{}] executor failed: {e}", cfg.name);
            engine::log(json!({"chain_id": cid, "pool": cfg.pool, "kind": "error", "error": e}));
            json!({"mode": "error", "error": e, "chain_id": cid})
        })
    }

    /// One cycle of every wallet, one after another.
    pub fn cycle(&self) {
        let _g = self.running.lock().unwrap();
        let ids = crate::wallets::ids();
        self.others.lock().unwrap().retain(|k, _| ids.contains(k));
        for id in &ids {
            crate::paths::scope(id, || self.cycle_one());
        }
        let _ = std::fs::create_dir_all(crate::paths::app_logs());
        let _ = std::fs::write(crate::paths::app_logs().join("heartbeat"), engine::now().to_string());
    }

    fn cycle_one(&self) {
        let t0 = std::time::Instant::now();
        self.set("state", json!("running"));
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.cycle_inner())) {
            Ok(Ok(())) => self.set("last_error", Value::Null),
            Ok(Err(e)) => {
                crate::error!("worker", "cycle failed: {e}");
                self.set("last_error", json!(e));
            }
            Err(_) => {
                crate::error!("worker", "cycle panicked");
                self.set("last_error", json!("internal error (panic)"));
            }
        }
        self.with_status(|s| {
            s["state"] = json!("idle");
            s["last_cycle"] = json!(engine::now());
            s["cycle_seconds"] = json!((t0.elapsed().as_secs_f64() * 10.0).round() / 10.0);
        });
    }

    fn cycle_inner(&self) -> Result<(), String> {
        let s = settings::load_settings();
        let raw = settings::load_pools();
        let wallet = s["wallet"].as_str().unwrap_or("").to_lowercase();
        if !(wallet.len() == 42 && wallet.starts_with("0x") && wallet[2..].chars().all(|c| c.is_ascii_hexdigit())) {
            return Err("no managed wallet set: add a private key or the wallet address in Settings".into());
        }
        let (execute, mode) = self.mode(&s, &wallet)?;
        self.set("mode", json!(mode));

        let mut by_chain: BTreeMap<u64, Vec<(Value, PoolConfig)>> = BTreeMap::new();
        for p in &raw {
            by_chain.entry(pool_chain(p)).or_default().push((p.clone(), PoolConfig::from(p)));
        }
        for (cid, pools) in &by_chain {
            for (_, cfg) in pools {
                if let Err(e) = crate::data::sync_pool(*cid, &cfg.pool, &wallet, replay_days(cfg).max(13) + 1) {
                    crate::error!("data", "[{}] data refresh failed: {e}", cfg.name);
                }
            }
        }
        // lots parked by a pool that is now switched off (or at 0%) become the pool's they sit in,
        // when that pool runs a strategy of its own: before valuing, so they count there this cycle
        for (cid, pools) in &by_chain {
            let on = |addr: &str| pools.iter().any(|(_, c)| c.pool.eq_ignore_ascii_case(addr) && c.enabled);
            let got = crate::parking::hand_over(execute, *cid, &on, &on);
            // the lot keeps the parking range it was minted in (wide, so it stayed in range without
            // care): the pool that now owns it re-solves on this cycle instead of on its schedule, so
            // the lot is folded into its own ranges
            for (_, cfg) in pools.iter().filter(|(_, c)| got.iter().any(|x| x["pool"].as_str().map(|p| p.eq_ignore_ascii_case(&c.pool)).unwrap_or(false))) {
                engine::resolve_now(*cid, cfg, execute);
            }
        }
        let acct = account_value(&by_chain, &wallet, &s);
        self.set("account", acct.clone());

        // each pool's capital: kept within the wallet, and the queued Add / Withdraw / Reset requests
        let mut book = capital::load(execute);
        let mut unis: BTreeMap<u64, Vec<String>> = BTreeMap::new();
        for (cid, pools) in &by_chain {
            let refs: Vec<&PoolConfig> = pools.iter().map(|x| &x.1).collect();
            let uni = capital::universe(*cid, &refs, &s, &acct, &book);
            match capital::balances(*cid, &uni, &wallet) {
                Ok(w) => capital::prepare(&mut book, *cid, &refs, &acct, &w, execute),
                Err(e) => crate::warn!("capital", "chain {cid}: wallet balances not read ({e}); capital requests wait for the next cycle"),
            }
            unis.insert(*cid, uni);
        }
        capital::save(execute, &book);
        self.publish_capital(&by_chain, &wallet, &acct, execute);

        let force = self.force_claim.lock().unwrap().remove(&crate::paths::wallet());
        if s["claim"]["enabled"].as_bool().unwrap_or(true) || force {
            let mut claims = Map::new();
            for (cid, pools) in &by_chain {
                let every = s["claim"]["every_days"].as_f64().unwrap_or(7.0);
                let uni = unis.get(cid).cloned().unwrap_or_default();
                let before = capital::balances(*cid, &uni, &wallet).ok();
                let r = crate::claim::run(*cid, &wallet, execute, every, force);
                // the rewards claimed go to the pools that earned them
                if r["status"] == "claimed" {
                    match (before, capital::balances(*cid, &uni, &wallet)) {
                        (Some(b), Ok(a)) => {
                            let refs: Vec<&PoolConfig> = pools.iter().map(|x| &x.1).collect();
                            let mut book = capital::load(execute);
                            capital::credit_claim(&mut book, *cid, &refs, &r["rewards"], &b, &a);
                            capital::save(execute, &book);
                        }
                        _ => crate::warn!("capital", "chain {cid}: balances around the claim not read: the rewards stay Unallocated"),
                    }
                }
                claims.insert(cid.to_string(), r);
            }
            let base = claims.get(&chains::BASE.to_string()).cloned().or_else(|| claims.values().next().cloned()).unwrap_or(Value::Null);
            self.set("claim", base);
            self.set("claims", Value::Object(claims));
        }
        if s["solid_lp"]["enabled"].as_bool().unwrap_or(false) {
            let cid = s["solid_lp"]["chain_id"].as_u64().unwrap_or(chains::BASE);
            self.set("solid_lp", crate::solid_lp::run(cid, &wallet, &s, execute));
        } else {
            self.with_status(|st| st.as_object_mut().map(|o| o.remove("solid_lp")));
        }

        for (cid, pools) in &by_chain {
            // An earlier transaction on this chain that is still unresolved: nothing new is signed here
            // until it is (it is re-broadcast unchanged meanwhile).
            let mut chain_execute = execute;
            if execute {
                if let Err(e) = crate::chain::reconcile_pending(*cid, &wallet) {
                    crate::warn!("worker", "chain {cid}: {e}");
                    chain_execute = false;
                }
            }
            // Unknown value is not zero value: while anything on the chain could not be valued, its
            // allocations are not acted on (a pool switched off or set to 0% is still withdrawn).
            let incomplete: Vec<String> = acct["chains"][cid.to_string()]["incomplete"].as_array().cloned().unwrap_or_default()
                .iter().filter_map(|x| x.as_str().map(String::from)).collect();
            for (_, cfg) in pools {
                let info = &acct["pools"][&cfg.name];
                let has_pos = info["positions"].as_array().map(|a| !a.is_empty()).unwrap_or(false);
                if !cfg.enabled && !has_pos {
                    // a pool that is off and holds no positions gives its idle tokens back to Unallocated
                    // (or to the pool a move asked for), as they are
                    let mut book = capital::load(execute);
                    for r in capital::releases(*cid, &cfg.pool) {
                        capital::move_in_book(&mut book, *cid, &cfg.pool, r["usd"].as_f64().unwrap_or(0.0), r["to"].as_str());
                        capital::remove(r["id"].as_str().unwrap_or(""));
                    }
                    if !book.pool(*cid, &cfg.pool).is_empty() {
                        crate::info!("capital", "[{}] is off: its idle tokens are Unallocated now", cfg.name);
                        book.set_pool(*cid, &cfg.pool, capital::Tokens::new());
                    }
                    capital::save(execute, &book);
                    self.with_status(|st| st["pools"][&cfg.name] = json!({"mode": "disabled", "chain_id": cid}));
                    continue;
                }
                let explicit_exit = !cfg.enabled;
                if !incomplete.is_empty() && !explicit_exit {
                    let why = format!("valuation incomplete on this chain ({}); holding until everything can be valued", incomplete.join("; "));
                    crate::warn!("worker", "[{}] {why}", cfg.name);
                    engine::log(json!({"chain_id": cid, "pool": cfg.pool, "kind": "blocked", "problems": [why.clone()]}));
                    self.with_status(|st| st["pools"][&cfg.name] = json!({"mode": "hold", "chain_id": cid, "error": why, "flvr": info["flvr"]}));
                    continue;
                }
                let entry = self.run_pool(*cid, cfg, &acct, &wallet, &s, chain_execute);
                self.publish(*cid, cfg, &wallet, entry);
            }
        }

        // Fee vs arbitrage loss for every pool (Discover pairs), refreshed in the background.
        let mut flvr_chains: Vec<u64> = chains::all().into_iter().filter(|c| c.enabled).map(|c| c.id).collect();
        flvr_chains.extend(by_chain.keys().filter(|c| !flvr_chains.contains(c)).copied().collect::<Vec<_>>());
        crate::market_flvr::refresh_in_background(flvr_chains, wallet.clone());

        // Earned, costs and profit per chain, from the app's own transactions.
        let ledger_chains: Vec<u64> = by_chain.keys().copied().collect();
        self.set("ledger", crate::ledger::summary(&wallet, &ledger_chains));

        let mut snap = account_value(&by_chain, &wallet, &s);
        snap["mode"] = json!(mode);
        extra_value(&by_chain, &wallet, &mut snap);
        self.publish_capital(&by_chain, &wallet, &snap, execute);
        // each pool's capital (address → USD), for per-pool performance (agent::perf)
        let mut pc = Map::new();
        if let Some(chs) = self.with_status(|st| st["capital"].as_object().cloned()) {
            for ch in chs.values() {
                for p in ch["pools"].as_object().into_iter().flat_map(|m| m.values()) {
                    if let (Some(a), Some(u)) = (p["pool"].as_str(), p["capital_usd"].as_f64()) {
                        pc.insert(a.to_lowercase(), json!(u));
                    }
                }
            }
        }
        snap["pool_capital"] = Value::Object(pc);
        let line = {
            let mut o = Map::new();
            o.insert("ts".into(), json!(engine::now()));
            o.extend(snap.as_object().cloned().unwrap_or_default());
            Value::Object(o)
        };
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(crate::paths::logs().join("snapshots.jsonl")) {
            let _ = writeln!(f, "{line}");
        }
        self.set("account", snap);
        Ok(())
    }
}

impl Worker {
    /// Each pool's capital and Unallocated, per chain, for the dashboard (status "capital").
    fn publish_capital(&self, by_chain: &BTreeMap<u64, Vec<(Value, PoolConfig)>>, wallet: &str, acct: &Value, execute: bool) {
        let book = capital::load(execute);
        let mut out = Map::new();
        for (cid, pools) in by_chain {
            let refs: Vec<&PoolConfig> = pools.iter().map(|x| &x.1).collect();
            let uni = capital::universe(*cid, &refs, &settings::load_settings(), acct, &book);
            match capital::balances(*cid, &uni, wallet) {
                Ok(w) => {
                    out.insert(cid.to_string(), capital::status(&book, *cid, &refs, acct, &w, execute));
                }
                Err(e) => crate::warn!("capital", "chain {cid}: capital not shown ({e})"),
            }
        }
        self.set("capital", Value::Object(out));
    }
}

fn extra_value(by_chain: &BTreeMap<u64, Vec<(Value, PoolConfig)>>, wallet: &str, snap: &mut Value) {
    let mut rewards = 0.0;
    let mut any_rewards = false;
    let mut solid_usd = 0.0;
    let mut any_solid = false;
    for cid in by_chain.keys() {
        let r = crate::claim::claimable(*cid, wallet);
        if r.get("error").is_none() {
            rewards += r["usd"].as_f64().unwrap_or(0.0);
            any_rewards = true;
        }
        let solid = chains::get(*cid).solid;
        if solid.is_empty() {
            continue;
        }
        let counted = snap["tokens"].as_object().map(|t| t.values().any(|x| x["address"] == solid.as_str() && x["chain_id"] == *cid)).unwrap_or(false);
        if !counted {
            let v = (|| -> Result<f64, String> {
                let b = to_f64(&crate::chain::erc20_balance(*cid, &solid, wallet)?);
                Ok(b / 10f64.powi(prices::meta(*cid, &solid)?.decimals as i32) * prices::usd(*cid, &solid)?)
            })();
            match v {
                Ok(x) => {
                    solid_usd += x;
                    any_solid = true;
                }
                Err(e) => crate::warn!("worker", "SOLID valuation failed: {e}"),
            }
        }
    }
    snap["rewards_usd"] = if any_rewards { json!(rewards) } else { Value::Null };
    snap["solid_usd"] = if any_solid { json!(solid_usd) } else { Value::Null };
    snap["value_usd"] = json!(snap["total_usd"].as_f64().unwrap_or(0.0) + rewards + solid_usd);
}

/// One pool's positions and their USD value now: (the dashboard's entry, deployed USD, token0, token1).
fn pool_value(cid: u64, cfg: &PoolConfig, wallet: &str) -> Result<(Value, f64, String, String), String> {
    let (t0, t1) = crate::chain::pool_tokens(cid, &cfg.pool)?;
    let (sp, tick, fee) = crate::chain::slot0(cid, &cfg.pool)?;
    let p = (to_f64(&sp) / 2f64.powi(96)).powi(2);
    let (u0, u1) = (prices::usd(cid, &t0)?, prices::usd(cid, &t1)?);
    let (m0, m1) = (prices::meta(cid, &t0)?, prices::meta(cid, &t1)?);
    let (d0, d1) = (10f64.powi(m0.decimals as i32), 10f64.powi(m1.decimals as i32));
    let mut pos = vec![];
    let mut val = 0.0;
    // the pool's in-range liquidity: the dashboard's estimate takes our in-range share of it
    let pool_liq = crate::chain::pool_liquidity(cid, &cfg.pool).ok().map(|l| l.to_string());
    for (tl, tu, liq) in wallet_positions(cid, &cfg.pool, wallet)? {
        let (a0, a1) = amounts_at(&sp, tl, tu, &liq);
        let (a0, a1) = (to_f64(&a0) / d0, to_f64(&a1) / d1);
        let v = a0 * u0 + a1 * u1;
        val += v;
        pos.push(json!({"tl": tl, "tu": tu, "liquidity": liq.to_string(), "amount0": a0, "amount1": a1, "usd": v,
                        "in_range": tl <= tick && tick < tu}));
    }
    let in_range = pos.iter().any(|x| x["in_range"] == true);
    Ok((json!({"chain_id": cid, "tick": tick, "fee_pips": fee, "price": p, "positions": pos, "deployed_usd": val, "pool_liquidity": pool_liq,
               "symbols": [m0.symbol, m1.symbol], "in_range": in_range, "allocation_pct": cfg.allocation_pct,
               "enabled": cfg.enabled}), val, t0, t1))
}

/// USD value of everything managed, per chain and in total: positions of every configured pool +
/// free balances of the pools' tokens and the funding tokens.
pub fn account_value(by_chain: &BTreeMap<u64, Vec<(Value, PoolConfig)>>, wallet: &str, s: &Value) -> Value {
    let mut out = json!({"pools": {}, "tokens": {}, "total_usd": 0.0, "chains": {}});
    let mut grand = 0.0;
    for (cid, pools) in by_chain {
        let mut total = 0.0;
        let mut tokens: Vec<String> = vec![];
        let mut incomplete: Vec<String> = vec![];
        for (_, cfg) in pools {
            match pool_value(*cid, cfg, wallet) {
                Ok((info, val, t0, t1)) => {
                    out["pools"][&cfg.name] = info;
                    total += val;
                    for t in [t0, t1] {
                        if !tokens.contains(&t) {
                            tokens.push(t);
                        }
                    }
                }
                Err(e) => {
                    crate::error!("worker", "[{}] valuation failed: {e}", cfg.name);
                    incomplete.push(format!("{}: {e}", cfg.name));
                    out["pools"][&cfg.name] = json!({"error": e, "chain_id": cid});
                }
            }
        }
        // Funding tokens that exist on this chain (decimals() answers), then every balance in one request.
        let funding: Vec<String> = crate::settings::funding_tokens(&s, *cid).into_iter()
            .filter(|a| !tokens.contains(a))
            .collect();
        let probe: Vec<(String, String)> = funding.iter().map(|a| (a.clone(), "0x313ce567".to_string())).collect();
        if let Ok(res) = crate::chain::eth_calls(*cid, &probe) {
            for (a, r) in funding.into_iter().zip(res) {
                if r.map(|b| b.len() >= 32).unwrap_or(false) {
                    tokens.push(a);
                }
            }
        }
        let balances = crate::chain::erc20_balances(*cid, &tokens, wallet)
            .unwrap_or_else(|e| tokens.iter().map(|_| Err(e.clone())).collect());
        let mut chain_tokens = Map::new();
        for (t, bal) in tokens.into_iter().zip(balances) {
            let r = (|| -> Result<(String, Value, f64), String> {
                let m = prices::meta(*cid, &t)?;
                let amt = to_f64(&bal?) / 10f64.powi(m.decimals as i32);
                let usd = if amt > 0.0 { amt * prices::usd(*cid, &t)? } else { 0.0 };
                Ok((m.symbol, json!({"address": t, "amount": amt, "usd": usd, "chain_id": cid}), usd))
            })();
            match r {
                Ok((sym, v, usd)) => {
                    total += usd;
                    let key = if *cid == chains::BASE { sym.clone() } else { format!("{sym} ({})", chains::get(*cid).name) };
                    out["tokens"][&key] = v.clone();
                    chain_tokens.insert(sym, v);
                }
                Err(e) => {
                    crate::error!("worker", "balance of {t} failed: {e}");
                    incomplete.push(format!("balance of {t}: {e}"));
                }
            }
        }
        // tokens parked by strategies (parking.rs) are in no pool's positions: counted here
        let (parked_usd, parked) = crate::parking::value(*cid);
        total += parked_usd;
        // a lot left out would shrink every pool's budget for a cycle (a pool holding the lots'
        // parking pool would then rebalance down): hold until it can be valued
        for x in parked.iter().filter(|x| !x["error"].is_null()) {
            incomplete.push(format!("parked lot {}: {}", x["id"].as_str().unwrap_or("?"), x["error"].as_str().unwrap_or("?")));
        }
        if !parked.is_empty() {
            out["parked"][cid.to_string()] = json!(parked);
        }
        let gas = crate::chain::eth_balance(*cid, wallet).map(|g| to_f64(&g) / 1e18).ok();
        if *cid == chains::BASE {
            out["eth_gas"] = json!(gas);
        }
        out["chains"][cid.to_string()] = json!({"name": chains::get(*cid).name, "total_usd": total, "gas": gas,
                                                "gas_symbol": chains::get(*cid).gas_symbol, "tokens": chain_tokens,
                                                "incomplete": incomplete, "parked_usd": parked_usd});
        grand += total;
    }
    out["total_usd"] = json!(grand);
    out
}
