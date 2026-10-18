//! Claimed SOLID -> full-range deposit into the chain's SOLID pool, paired with the same USD value
//! of the other token.

use crate::chain;
use crate::engine::{log, Executor};
use crate::prices;
use crate::settings::PoolConfig;
use crate::tickmath::{amounts_at, amounts_for_liquidity, liquidity_for_amounts, sqrt_ratio_at_tick, to_f64, MAX_TICK};
use num_bigint::BigUint;
use num_traits::ToPrimitive;
use serde_json::{json, Map, Value};

pub fn full_range(spacing: i64) -> (i64, i64) {
    (-(MAX_TICK / spacing) * spacing, (MAX_TICK / spacing) * spacing)
}

pub fn position(cid: u64, pool: &str, wallet: &str) -> Result<Value, String> {
    let (t0, t1) = chain::pool_tokens(cid, pool)?;
    let (tl, tu) = full_range(chain::tick_spacing(cid, pool)?);
    let (sp, _, _) = chain::slot0(cid, pool)?;
    let liq = chain::position_liquidity(cid, pool, wallet, tl, tu)?;
    let (a0, a1) = amounts_at(&sp, tl, tu, &liq);
    let (m0, m1) = (prices::meta(cid, &t0)?, prices::meta(cid, &t1)?);
    let (x0, x1) = (to_f64(&a0) / 10f64.powi(m0.decimals as i32), to_f64(&a1) / 10f64.powi(m1.decimals as i32));
    let usd = if liq > BigUint::default() {
        match (prices::usd(cid, &t0), prices::usd(cid, &t1)) {
            (Ok(p0), Ok(p1)) => json!(x0 * p0 + x1 * p1),
            _ => Value::Null,
        }
    } else {
        json!(0.0)
    };
    Ok(json!({"chain_id": cid, "pool": pool, "ticks": [tl, tu], "liquidity": liq.to_string(), "symbols": [m0.symbol, m1.symbol],
              "amounts": [x0, x1], "usd": usd}))
}

pub fn run(cid: u64, wallet: &str, settings: &Value, execute: bool) -> Value {
    let sl = &settings["solid_lp"];
    let pool = sl["pool"].as_str().map(String::from).filter(|p| !p.is_empty()).unwrap_or_else(|| crate::chains::get(cid).solid_lp_pool).to_lowercase();
    let mut out = json!({"chain_id": cid, "pool": pool, "state": "idle"});
    let mut report = vec![];
    match run_inner(cid, wallet, settings, execute, &pool, &mut out) {
        Ok(r) => report = r,
        Err(e) => {
            crate::error!("pool.solid-lp", "  failed: {e}");
            log(json!({"chain_id": cid, "pool": pool, "kind": "error", "what": "solid-lp", "error": e}));
            out["state"] = json!("error");
            out["error"] = json!(e);
        }
    }
    out["report"] = json!(report.iter().rev().take(40).rev().collect::<Vec<_>>());
    out["position"] = position(cid, &pool, wallet).unwrap_or_else(|e| json!({"error": e}));
    out
}

fn run_inner(cid: u64, wallet: &str, settings: &Value, execute: bool, pool: &str, out: &mut Value) -> Result<Vec<String>, String> {
    let sl = &settings["solid_lp"];
    let solid = crate::chains::get(cid).solid;
    let num = |k: &str, d: f64| sl[k].as_f64().unwrap_or(d);
    let cfg = PoolConfig {
        name: "solid-lp".into(),
        pool: pool.to_string(),
        strategy: Map::new(),
        slippage_bps: num("slippage_bps", 50.0),
        max_spot_dev_ticks: 0,
        max_actions_per_day: 0,
        min_swap_value: 0.0,
        max_swap_cost_bps: num("max_swap_cost_bps", 50.0),
        numeraire: 0,
        allocation_pct: 0.0,
        max_ranges: 1,
        enabled: true,
    };
    let mut ex = Executor::new(cfg, cid, wallet, settings, execute, None)?;
    if solid.is_empty() || (ex.t0 != solid && ex.t1 != solid) {
        return Err(format!("pool {pool} has no SOLID"));
    }
    let pair = if ex.t0 == solid { ex.t1.clone() } else { ex.t0.clone() };
    ex.is_solid_lp = true;
    ex.solid_pair = Some((solid.clone(), pair.clone()));
    let e = |x: crate::engine::StepErr| x.to_string();
    let (ms, mp) = (prices::meta(cid, &solid)?, prices::meta(cid, &pair)?);
    let amount = ex.bal(&solid).map_err(e)?.max(0);
    let solid_usd = prices::usd(cid, &solid)?;
    let value = amount as f64 / 10f64.powi(ms.decimals as i32) * solid_usd;
    out["solid"] = json!(amount as f64 / 10f64.powi(ms.decimals as i32));
    out["solid_usd"] = json!(value);
    ex.say(format!("[solid-lp] {:.4} SOLID in the wallet (${value:.2})", amount as f64 / 10f64.powi(ms.decimals as i32)));
    if value < num("min_value_usd", 5.0) {
        ex.say(format!("  waiting: worth less than ${}", num("min_value_usd", 5.0)));
        out["state"] = json!("waiting");
        return Ok(ex.report);
    }
    if crate::paths::stop_file().exists() || std::env::var("ALM_DISABLED").as_deref() == Ok("1") {
        ex.say_warn("  BLOCKED: kill switch is on");
        out["state"] = json!("blocked");
        return Ok(ex.report);
    }
    let c = crate::chains::get(cid);
    if to_f64(&chain::eth_balance(cid, wallet)?) < c.min_gas_wei {
        ex.say_warn(format!("  BLOCKED: wallet has too little {} for gas", c.gas_symbol));
        out["state"] = json!("blocked");
        return Ok(ex.report);
    }
    let (sp, _, _, p) = ex.price().map_err(e)?;
    let pair_per_solid = (if ex.t0 == solid { p } else { 1.0 / p }) * 10f64.powi(ms.decimals as i32 - mp.decimals as i32);
    let pool_px = pair_per_solid * prices::usd(cid, &pair)?;
    let dev = (pool_px / solid_usd - 1.0) * 100.0;
    out["pool_price_usd"] = json!(pool_px);
    out["market_price_usd"] = json!(solid_usd);
    out["deviation_pct"] = json!(dev);
    ex.say(format!("  SOLID ${pool_px:.6} in the pool vs ${solid_usd:.6} market ({dev:+.2}%)"));
    if dev.abs() > num("max_price_dev_pct", 3.0) {
        ex.say_warn(format!("  BLOCKED: pool price is more than {}% off the market; SOLID stays in the wallet until it is back in line", num("max_price_dev_pct", 3.0)));
        log(json!({"chain_id": cid, "pool": pool, "kind": "blocked", "problems": [format!("SOLID pool price {dev:+.2}% off market")]}));
        out["state"] = json!("blocked");
        return Ok(ex.report);
    }
    let (tl, tu) = full_range(ex.spacing);
    let (sa, sb) = (sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu));
    let huge = BigUint::from(10u8).pow(60);
    let a = BigUint::from(amount as u128);
    let liq = if ex.t0 == solid { liquidity_for_amounts(&sp, &sa, &sb, &a, &huge) } else { liquidity_for_amounts(&sp, &sa, &sb, &huge, &a) };
    let (m0, m1) = amounts_for_liquidity(&sp, &sa, &sb, &liq);
    let need = (if ex.t0 == solid { m1 } else { m0 }).to_i128().unwrap_or(0) * 1005 / 1000;
    let have = ex.bal(&pair).map_err(e)?.max(0);
    let pu = prices::usd(cid, &pair)?;
    out["pair_needed"] = json!(need as f64 / 10f64.powi(mp.decimals as i32));
    ex.say(format!("  needs {:.6} {} (${:.2}); wallet has {:.6}", need as f64 / 10f64.powi(mp.decimals as i32), mp.symbol,
                   need as f64 / 10f64.powi(mp.decimals as i32) * pu, have as f64 / 10f64.powi(mp.decimals as i32)));
    if have < need {
        buy_pair(&mut ex, cid, &solid, &pair, (need - have) as f64 / 10f64.powi(mp.decimals as i32) * pu, settings)?;
    }
    log(json!({"chain_id": cid, "pool": pool, "kind": "solid_lp", "live": execute, "solid": amount as f64 / 10f64.powi(ms.decimals as i32), "usd": value}));
    match ex.retry(&format!("mint full range [{tl},{tu})"), |s| s.mint_one(tl, tu, 1.0, f64::INFINITY)) {
        Ok(minted) => out["state"] = json!(if minted && execute { "deposited" } else if minted { "planned" } else { "nothing" }),
        Err(err) => {
            ex.say_err(format!("  deposit gave up: {err}; tokens stay in the wallet for the next cycle"));
            out["state"] = json!("error");
        }
    }
    let (ls, lpair) = (ex.bal(&solid).map_err(e)?, ex.bal(&pair).map_err(e)?);
    ex.say(format!("  left in the wallet: {:.4} SOLID + {:.6} {}", ls as f64 / 10f64.powi(ms.decimals as i32), lpair as f64 / 10f64.powi(mp.decimals as i32), mp.symbol));
    Ok(ex.report)
}

fn buy_pair(ex: &mut Executor, cid: u64, solid: &str, pair: &str, mut short: f64, settings: &Value) -> Result<(), String> {
    let mut avail = vec![];
    for ft in crate::settings::funding_tokens(settings, cid) {
        if ft == solid || ft == pair {
            continue;
        }
        let r: Result<(f64, i128), String> = (|| {
            let b = ex.bal(&ft).map_err(|e| e.to_string())?.max(0);
            Ok((b as f64 / 10f64.powi(prices::meta(cid, &ft)?.decimals as i32) * prices::usd(cid, &ft)?, b))
        })();
        match r {
            Ok((u, b)) => avail.push((u, ft, b)),
            Err(e) => ex.say_warn(format!("  funding: {} skipped ({e})", &ft[..10.min(ft.len())])),
        }
    }
    avail.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let psym = prices::meta(cid, pair)?.symbol;
    for (usd_free, ft, b) in avail {
        if short < 0.5 {
            break;
        }
        if usd_free < 0.5 {
            continue;
        }
        let (px, m) = (prices::usd(cid, &ft)?, prices::meta(cid, &ft)?);
        let amount = b.min((short * 1.01 / px * 10f64.powi(m.decimals as i32)) as i128);
        let mc = ex.cfg.max_swap_cost_bps;
        let (f2, p2) = (ft.clone(), pair.to_string());
        match ex.retry(&format!("buy {psym} with {}", m.symbol), |s| s.swap(&f2, &p2, amount, 0.0, mc, "buy pair token")) {
            Ok(_) => short -= amount as f64 / 10f64.powi(m.decimals as i32) * px,
            Err(e) => ex.say_warn(format!("  buying with {} skipped: {e}", m.symbol)),
        }
    }
    if short >= 0.5 {
        ex.say_warn(format!("  still ${short:.2} of {psym} short: the deposit uses what is there and the rest of the SOLID waits"));
    }
    Ok(())
}
