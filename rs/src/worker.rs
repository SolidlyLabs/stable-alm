//! The worker loop, per chain: data refresh, account value, claims, SOLID LP,
//! one executor pass per pool, and a snapshot for the value history. Each chain's pools are
//! budgeted from that chain's own total (no bridging; docs/MULTICHAIN_PLAN.md).

use crate::chains;
use crate::engine::{self, wallet_positions, Executor};
use crate::prices;
use crate::settings::{self, PoolConfig};
use crate::tickmath::{amounts_at, to_f64};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub fn pool_chain(p: &Value) -> u64 {
    p["chain_id"].as_u64().unwrap_or(chains::BASE)
}

pub struct Worker {
    pub status: Mutex<Value>,
    wake: (Mutex<bool>, Condvar),
    pub stopping: Mutex<bool>,
    force_claim: Mutex<bool>,
    running: Mutex<()>,
}

impl Worker {
    pub fn new() -> Arc<Worker> {
        Arc::new(Worker {
            status: Mutex::new(json!({"state": "starting", "last_cycle": null, "last_error": null, "pools": {}, "mode": "dry run"})),
            wake: (Mutex::new(false), Condvar::new()),
            stopping: Mutex::new(false),
            force_claim: Mutex::new(false),
            running: Mutex::new(()),
        })
    }

    pub fn request(&self, claim_now: bool) {
        if claim_now {
            *self.force_claim.lock().unwrap() = true;
        }
        *self.wake.0.lock().unwrap() = true;
        self.wake.1.notify_all();
    }

    pub fn stop(&self) {
        *self.stopping.lock().unwrap() = true;
        self.request(false);
    }

    fn set(&self, k: &str, v: Value) {
        self.status.lock().unwrap()[k] = v;
    }

    pub fn loop_forever(&self) {
        while !*self.stopping.lock().unwrap() {
            self.cycle();
            if *self.stopping.lock().unwrap() {
                break;
            }
            let interval = settings::load_settings()["interval_s"].as_f64().unwrap_or(300.0);
            self.set("next_cycle", json!(engine::now() as f64 + interval));
            let (m, cv) = &self.wake;
            let g = m.lock().unwrap();
            let (mut g, _) = cv.wait_timeout_while(g, Duration::from_secs_f64(interval), |w| !*w).unwrap();
            *g = false;
        }
        self.set("state", json!("stopped"));
    }

    pub fn cycle(&self) {
        let _g = self.running.lock().unwrap();
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
        {
            let mut s = self.status.lock().unwrap();
            s["state"] = json!("idle");
            s["last_cycle"] = json!(engine::now());
            s["cycle_seconds"] = json!((t0.elapsed().as_secs_f64() * 10.0).round() / 10.0);
        }
        let _ = std::fs::create_dir_all(crate::paths::logs());
        let _ = std::fs::write(crate::paths::logs().join("heartbeat"), engine::now().to_string());
    }

    fn cycle_inner(&self) -> Result<(), String> {
        let s = settings::load_settings();
        let raw = settings::load_pools();
        let wallet = s["wallet"].as_str().unwrap_or("").to_lowercase();
        let has_signer = crate::signer::configured();
        let stop = crate::paths::stop_file().exists();
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
        let mode = if execute { "LIVE" } else if stop { "paused (STOP)" } else { "dry run" };
        self.set("mode", json!(mode));

        let mut by_chain: BTreeMap<u64, Vec<(Value, PoolConfig)>> = BTreeMap::new();
        for p in &raw {
            by_chain.entry(pool_chain(p)).or_default().push((p.clone(), PoolConfig::from(p)));
        }
        for (cid, pools) in &by_chain {
            for (_, cfg) in pools {
                if let Err(e) = crate::data::sync_pool(*cid, &cfg.pool, &wallet, 14) {
                    crate::error!("data", "[{}] data refresh failed: {e}", cfg.name);
                }
            }
        }
        let acct = account_value(&by_chain, &wallet, &s);
        self.set("account", acct.clone());

        let force = std::mem::take(&mut *self.force_claim.lock().unwrap());
        if s["claim"]["enabled"].as_bool().unwrap_or(true) || force {
            let mut claims = Map::new();
            for cid in by_chain.keys() {
                let every = s["claim"]["every_days"].as_f64().unwrap_or(7.0);
                claims.insert(cid.to_string(), crate::claim::run(*cid, &wallet, execute, every, force));
            }
            let base = claims.get(&chains::BASE.to_string()).cloned().or_else(|| claims.values().next().cloned()).unwrap_or(Value::Null);
            self.set("claim", base);
            self.set("claims", Value::Object(claims));
        }
        if s["solid_lp"]["enabled"].as_bool().unwrap_or(false) {
            let cid = s["solid_lp"]["chain_id"].as_u64().unwrap_or(chains::BASE);
            self.set("solid_lp", crate::solid_lp::run(cid, &wallet, &s, execute));
        } else if let Some(o) = self.status.lock().unwrap().as_object_mut() {
            o.remove("solid_lp");
        }

        for (cid, pools) in &by_chain {
            let chain_total = acct["chains"][cid.to_string()]["total_usd"].as_f64().unwrap_or(0.0);
            for (_, cfg) in pools {
                let info = &acct["pools"][&cfg.name];
                let has_pos = info["positions"].as_array().map(|a| !a.is_empty()).unwrap_or(false);
                if !cfg.enabled && !has_pos {
                    self.status.lock().unwrap()["pools"][&cfg.name] = json!({"mode": "disabled", "chain_id": cid});
                    continue;
                }
                let budget = if cfg.enabled { chain_total * cfg.allocation_pct / 100.0 } else { 0.0 };
                let res = (|| -> Result<Value, String> {
                    let mut ex = Executor::new(cfg.clone(), *cid, &wallet, &s, execute, Some(budget))?;
                    let num = cfg.numeraire;
                    let m = crate::data::live_market(*cid, &cfg.pool, &wallet, ex.dec0, ex.dec1, num, 10)?;
                    let r = ex.run(&m);
                    let mut summary = match r {
                        Ok(v) => v,
                        Err(e) => {
                            ex.say_err(format!("  failed: {e}"));
                            json!({"chain_id": cid, "pool": cfg.pool, "name": cfg.name, "mode": "error", "error": e.to_string()})
                        }
                    };
                    let n = ex.report.len();
                    summary["report"] = json!(ex.report[n.saturating_sub(60)..]);
                    summary["budget_usd"] = json!(budget);
                    // Monitor only (reports/VOLATILE.md): shown on the dashboard, not used for trading.
                    summary["flvr"] = crate::data::pool_flvr(*cid, &cfg.pool, ex.dec0, ex.dec1, num);
                    Ok(summary)
                })();
                let entry = res.unwrap_or_else(|e| {
                    crate::error!("worker", "[{}] executor failed: {e}", cfg.name);
                    engine::log(json!({"chain_id": cid, "pool": cfg.pool, "kind": "error", "error": e}));
                    json!({"mode": "error", "error": e, "chain_id": cid})
                });
                self.status.lock().unwrap()["pools"][&cfg.name] = entry;
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

/// USD value of everything managed, per chain and in total: positions of every configured pool +
/// free balances of the pools' tokens and the funding tokens.
pub fn account_value(by_chain: &BTreeMap<u64, Vec<(Value, PoolConfig)>>, wallet: &str, s: &Value) -> Value {
    let mut out = json!({"pools": {}, "tokens": {}, "total_usd": 0.0, "chains": {}});
    let mut grand = 0.0;
    for (cid, pools) in by_chain {
        let mut total = 0.0;
        let mut tokens: Vec<String> = vec![];
        for (_, cfg) in pools {
            let r = (|| -> Result<(Value, f64, String, String), String> {
                let (t0, t1) = crate::chain::pool_tokens(*cid, &cfg.pool)?;
                let (sp, tick, fee) = crate::chain::slot0(*cid, &cfg.pool)?;
                let p = (to_f64(&sp) / 2f64.powi(96)).powi(2);
                let (u0, u1) = (prices::usd(*cid, &t0)?, prices::usd(*cid, &t1)?);
                let (m0, m1) = (prices::meta(*cid, &t0)?, prices::meta(*cid, &t1)?);
                let (d0, d1) = (10f64.powi(m0.decimals as i32), 10f64.powi(m1.decimals as i32));
                let mut pos = vec![];
                let mut val = 0.0;
                for (tl, tu, liq) in wallet_positions(*cid, &cfg.pool, wallet)? {
                    let (a0, a1) = amounts_at(&sp, tl, tu, &liq);
                    let (a0, a1) = (to_f64(&a0) / d0, to_f64(&a1) / d1);
                    let v = a0 * u0 + a1 * u1;
                    val += v;
                    pos.push(json!({"tl": tl, "tu": tu, "liquidity": liq.to_string(), "amount0": a0, "amount1": a1, "usd": v,
                                    "in_range": tl <= tick && tick < tu}));
                }
                let in_range = pos.iter().any(|x| x["in_range"] == true);
                Ok((json!({"chain_id": cid, "tick": tick, "fee_pips": fee, "price": p, "positions": pos, "deployed_usd": val,
                           "symbols": [m0.symbol, m1.symbol], "in_range": in_range, "allocation_pct": cfg.allocation_pct,
                           "enabled": cfg.enabled}), val, t0, t1))
            })();
            match r {
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
                    out["pools"][&cfg.name] = json!({"error": e, "chain_id": cid});
                }
            }
        }
        for ft in s["funding_tokens"].as_array().cloned().unwrap_or_default() {
            let ft = ft.as_str().unwrap_or("").to_lowercase();
            if !ft.is_empty() && !tokens.contains(&ft) && crate::chain::eth_call(*cid, &ft, "0x313ce567", None).is_ok() {
                tokens.push(ft);
            }
        }
        let mut chain_tokens = Map::new();
        for t in tokens {
            let r = (|| -> Result<(String, Value, f64), String> {
                let m = prices::meta(*cid, &t)?;
                let amt = to_f64(&crate::chain::erc20_balance(*cid, &t, wallet)?) / 10f64.powi(m.decimals as i32);
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
                Err(e) => crate::error!("worker", "balance of {t} failed: {e}"),
            }
        }
        let gas = crate::chain::eth_balance(*cid, wallet).map(|g| to_f64(&g) / 1e18).ok();
        if *cid == chains::BASE {
            out["eth_gas"] = json!(gas);
        }
        out["chains"][cid.to_string()] = json!({"name": chains::get(*cid).name, "total_usd": total, "gas": gas,
                                                "gas_symbol": chains::get(*cid).gas_symbol, "tokens": chain_tokens});
        grand += total;
    }
    out["total_usd"] = json!(grand);
    out
}
