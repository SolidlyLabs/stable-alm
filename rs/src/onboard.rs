//! The first-start wizard's server side (rs/static/index.html, "welcome wizard").
//!
//! | Route | What |
//! |---|---|
//! | GET /api/onboard | whether the wizard should open (until it was finished or skipped once, so
//! |  | an install updated from before the wizard sees it too, with its key and parking kept) |
//! | GET /api/onboard/parking | suggested parking pools per enabled chain |
//! | POST /api/onboard/generate | a new random key and its address (shown once; not stored yet) |
//! | POST /api/onboard/phrase | the first addresses a recovery phrase gives (nothing stored) |
//! | POST /api/onboard/phrase_save | store the key of one of those addresses |
//! | POST /api/onboard/done | mark the wizard finished |
//!
//! A key is stored only when the user confirms (the generated one through /api/key, the same as
//! the Settings tab, or a phrase's through phrase_save); the managed wallet follows the key. Keys
//! and phrases are never logged.

use serde_json::{json, Value};

use crate::settings::{load_settings, save_settings};

/// Store a key as the signer and make its address the managed wallet.
pub fn save_key(k: &str) -> Result<Value, String> {
    // one wallet per address (wallets.rs): refused before anything is stored
    let k = k.trim();
    let probe = crate::eth::Account::from_key(&if k.starts_with("0x") { k.to_string() } else { format!("0x{k}") })?;
    crate::wallets::check_address(&crate::paths::wallet(), &probe.address)?;
    let address = crate::signer::save_key(k)?;
    let mut s = load_settings();
    let changed = !s["wallet"].as_str().unwrap_or("").eq_ignore_ascii_case(&address);
    if changed {
        s["wallet"] = json!(address.to_lowercase());
        save_settings(&s).map_err(|e| e.to_string())?;
    }
    crate::info!("ui", "private key set for {address}");
    Ok(json!({"ok": true, "address": address, "wallet_changed": changed}))
}

pub fn status() -> Value {
    let s = load_settings();
    let sg = crate::signer::status();
    // installs from before the wizard have no "onboarded" yet (it defaults to false): they see it once
    let needed = !s["onboarded"].as_bool().unwrap_or(false);
    json!({"needed": needed, "onboarded": s["onboarded"], "signer": sg, "wallet": s["wallet"], "execute": s["execute"],
           "chains": crate::chains::as_json(), "parking": s["parking"], "interval_s": s["interval_s"]})
}

/// One suggestion per token per chain: the pegged pool (both tokens of one peg family: dollars,
/// ETH and its staked forms, BTC, S and stS, …) with the most active liquidity holding it.
pub fn parking_suggestions() -> Value {
    let s = load_settings();
    let wallet = s["wallet"].as_str().unwrap_or("").to_string();
    json!({"chains": crate::chains::all().into_iter().filter(|c| c.enabled).map(|c| suggest(c.id, &c.name, &wallet)).collect::<Vec<_>>()})
}

/// Pairs offered on their chain however thin they are: the owner parks ETH in cbETH/WETH on Base and
/// S in wS/stS on Sonic (where the parked tokens are most of the liquidity anyway).
const ALWAYS: &[(u64, &str, &str)] = &[(crate::chains::BASE, "CBETH", "WETH"), (146, "WS", "STS")];
/// Below this a pegged pool is not suggested (unless it is in `ALWAYS`): Solidly's pegged pools are
/// small on most chains (USDC/USDT on Base about $47k, on Optimism about $19k in October 2026).
const MIN_TVL_USD: f64 = 5_000.0;

fn suggest(cid: u64, name: &str, wallet: &str) -> Value {
    let pools = match crate::market_data::pools(cid, wallet) {
        Ok(p) => p,
        Err(e) => return json!({"chain_id": cid, "chain": name, "error": e, "pools": []}),
    };
    let mut picks: Vec<Value> = vec![];
    let sym = |p: &Value, k: &str| p[k]["symbol"].as_str().unwrap_or("").to_uppercase();
    let always = |p: &Value| {
        let (a, b) = (sym(p, "token0"), sym(p, "token1"));
        ALWAYS.iter().any(|(c, x, y)| *c == cid && ((a == *x && b == *y) || (a == *y && b == *x))) && p["active_usd"].as_f64().unwrap_or(0.0) > 0.0
    };
    for p in pools.iter().filter(|p| p["pegged"] == true && (p["tvl_usd"].as_f64().unwrap_or(0.0) >= MIN_TVL_USD || always(p))) {
        for t in [&p["token0"], &p["token1"]] {
            let addr = t["address"].as_str().unwrap_or("").to_lowercase();
            let better = |q: &Value| q["active_usd"].as_f64().unwrap_or(0.0) < p["active_usd"].as_f64().unwrap_or(0.0);
            match picks.iter().position(|q| q["token"] == addr.as_str()) {
                Some(i) if better(&picks[i]) => picks[i] = row(cid, &addr, t, p),
                Some(_) => {}
                None => picks.push(row(cid, &addr, t, p)),
            }
        }
    }
    picks.sort_by(|a, b| b["tvl_usd"].as_f64().partial_cmp(&a["tvl_usd"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    picks.truncate(6);
    json!({"chain_id": cid, "chain": name, "pools": picks})
}

fn row(cid: u64, token: &str, t: &Value, p: &Value) -> Value {
    json!({"chain_id": cid, "token": token, "symbol": t["symbol"], "pool": p["pool"],
           "pair": format!("{}/{}", p["token0"]["symbol"].as_str().unwrap_or("?"), p["token1"]["symbol"].as_str().unwrap_or("?")),
           "tvl_usd": p["tvl_usd"], "active_usd": p["active_usd"], "apr": p["processor_apr_24h"], "fee_pips": p["fee_pips"]})
}

pub fn post(action: &str, body: &Value) -> Result<Value, String> {
    let s = |k: &str| body[k].as_str().unwrap_or("").to_string();
    match action {
        "generate" => {
            let (k, a) = crate::wallet::generate()?;
            Ok(json!({"private_key": k, "address": a}))
        }
        "phrase" => {
            let n = body["count"].as_u64().unwrap_or(5).clamp(1, 20) as u32;
            let from = body["from"].as_u64().unwrap_or(0).min(1000) as u32;
            let mut rows = vec![];
            for i in from..from + n {
                let k = crate::wallet::from_phrase(&s("phrase"), &s("passphrase"), i)?;
                rows.push(json!({"index": i, "address": crate::eth::Account::from_key(&k)?.address}));
            }
            Ok(json!({"addresses": rows}))
        }
        "phrase_save" => {
            let i = body["index"].as_u64().unwrap_or(0).min(1000) as u32;
            let k = crate::wallet::from_phrase(&s("phrase"), &s("passphrase"), i)?;
            save_key(&k)
        }
        "done" => {
            let mut st = load_settings();
            st["onboarded"] = json!(true);
            save_settings(&st).map_err(|e| e.to_string())?;
            Ok(json!({"ok": true}))
        }
        _ => Err(format!("unknown onboarding step {action}")),
    }
}

#[cfg(test)]
mod tests {
    /// What the wizard would suggest on every chain, enabled or not (reads the live market data).
    #[test]
    #[ignore]
    fn suggestions_all_chains() {
        for c in crate::chains::all() {
            let w = crate::settings::load_settings()["wallet"].as_str().unwrap_or("").to_string();
            let v = super::suggest(c.id, &c.name, &w);
            println!("{} ({}){}", c.name, c.id, v["error"].as_str().map(|e| format!(": {e}")).unwrap_or_default());
            for r in v["pools"].as_array().unwrap() {
                println!("  {:<8} in {:<16} tvl ${:>12.0} active ${:>12.0} apr {}", r["symbol"].as_str().unwrap_or(""), r["pair"].as_str().unwrap_or(""),
                         r["tvl_usd"].as_f64().unwrap_or(0.0), r["active_usd"].as_f64().unwrap_or(0.0), r["apr"]);
            }
        }
    }
}
