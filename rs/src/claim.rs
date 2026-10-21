//! Reward claims per chain: the UI's proof API, root sync when needed,
//! claimAll after the claim delay. Never re-sent after a broadcast.

use crate::chain;
use crate::engine::{log, now};
use crate::eth::{self, Tok};
use crate::prices;
use num_bigint::BigUint;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SIG: &str = "claimAll((address[],uint8[],address[],address[],uint256[],(bytes32[],bool[])))";

fn state_path(cid: u64) -> std::path::PathBuf {
    // Base keeps the original file name (last_claim.json), so existing installs keep their claim clock.
    let name = if cid == crate::chains::BASE { "last_claim.json".to_string() } else { format!("last_claim_{cid}.json") };
    crate::paths::logs().join(name)
}

pub fn last_claim(cid: u64) -> u64 {
    std::fs::read_to_string(state_path(cid)).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| v["ts"].as_f64()).unwrap_or(0.0) as u64
}

/// Whether `snap` is the claim snapshot this wallet's last claim on the chain already claimed: the
/// same tokens and the same cumulative amounts in its proof.
fn already_claimed(cid: u64, snap: &Value) -> bool {
    let Some(last) = std::fs::read_to_string(state_path(cid)).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { return false };
    same_snapshot(&last, &snap["claim"])
}

fn same_snapshot(last: &Value, claim: &Value) -> bool {
    let norm = |v: &Value| -> Vec<String> { v.as_array().map(|a| a.iter().map(|x| x.as_str().map(|s| s.to_lowercase()).unwrap_or_else(|| x.to_string())).collect()).unwrap_or_default() };
    let (t, a) = (norm(&last["tokens"]), norm(&last["amounts"]));
    !t.is_empty() && !a.is_empty() && t == norm(&claim["tokens"]) && a == norm(&claim["amounts"])
}

pub fn snapshot(cid: u64, wallet: &str) -> Result<Value, String> {
    let url = format!("https://solidly.com/api/rewards/{wallet}/?chainId={cid}&version=1");
    for attempt in 0..4 {
        match crate::rpc::get_json(&url) {
            Ok(v) => return Ok(v),
            Err(e) if attempt < 3 && ["HTTP 502", "HTTP 503", "HTTP 504"].iter().any(|c| e.starts_with(c)) => {
                std::thread::sleep(Duration::from_secs(10 * (attempt + 1)));
            }
            Err(e) => return Err(e),
        }
    }
    Err("rewards API unavailable".into())
}

static CACHE: Mutex<Option<HashMap<(u64, String), (Instant, Value)>>> = Mutex::new(None);
static REFRESHING: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());

/// `claimable` for the dashboard, which must never wait on the network: the cached figures of any
/// age (or the last ones saved, marked loading), with one background refresh when they are older
/// than 10 minutes. A miss read in the request took seconds, and minutes when the RPC rate-limits.
pub fn claimable_cached(cid: u64, wallet: &str) -> Value {
    let key = (cid, wallet.to_lowercase());
    let hit = CACHE.lock().unwrap().get_or_insert_with(HashMap::new).get(&key).cloned();
    if matches!(&hit, Some((t, _)) if t.elapsed() < Duration::from_secs(600)) {
        return hit.unwrap().1;
    }
    {
        let mut busy = REFRESHING.lock().unwrap();
        if !busy.contains(&key) {
            busy.push(key.clone());
            let k = key.clone();
            crate::paths::spawn(move || {
                claimable(k.0, &k.1);
                REFRESHING.lock().unwrap().retain(|x| x != &k);
            });
        }
    }
    if let Some((_, v)) = hit {
        return v;
    }
    let last_path = crate::paths::logs().join(format!("rewards_last_{cid}.json"));
    match std::fs::read_to_string(&last_path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).filter(|v| v["wallet"].as_str() == Some(key.1.as_str())) {
        Some(mut last) => {
            last["loading"] = json!(true);
            last["can_claim"] = json!(false);
            last
        }
        None => json!({"claimable": [], "usd": 0.0, "pending_usd": 0.0, "can_claim": false, "loading": true}),
    }
}

/// Unclaimed rewards in USD (cached 10 min; cleared after a claim): `claimable` is in the latest
/// claim snapshot; `pending` accrued after it and becomes claimable with the next one (about daily).
pub fn claimable(cid: u64, wallet: &str) -> Value {
    let key = (cid, wallet.to_lowercase());
    if let Some((t, v)) = CACHE.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        if t.elapsed() < Duration::from_secs(600) {
            return v.clone();
        }
    }
    let res = (|| -> Result<Value, String> {
        let snap = snapshot(cid, wallet)?;
        // the API may serve the snapshot this wallet already claimed until it sees the claim: its
        // claimable is then in the claims already, and counting it again would double the earnings
        let claimed = already_claimed(cid, &snap);
        let mut data = vec![];
        let (mut total, mut pending) = (0.0, 0.0);
        // the API has one row per pool and token: one line per token (SOLID is earned in every pool)
        let mut by_token: Vec<(String, f64, f64)> = vec![];
        for r in snap["rewards"].as_array().cloned().unwrap_or_default() {
            let t = r["token"].as_str().unwrap_or("").to_lowercase();
            let (c, p) = (prices::fnum(&r["claimable"]), prices::fnum(&r["pending"]).max(0.0));
            match by_token.iter_mut().find(|x| x.0 == t) {
                Some(x) => {
                    x.1 += c;
                    x.2 += p;
                }
                None => by_token.push((t, c, p)),
            }
        }
        for (t, c, p) in by_token {
            let m = prices::meta(cid, &t)?;
            let c = if claimed { 0.0 } else { c };
            let amt = c / 10f64.powi(m.decimals as i32);
            let pend = p / 10f64.powi(m.decimals as i32);
            let px = prices::usd(cid, &t).ok();
            let usd = px.map(|p| p * amt);
            total += usd.unwrap_or(0.0);
            pending += px.map(|p| p * pend).unwrap_or(0.0);
            data.push(json!({"symbol": m.symbol, "amount": amt, "usd": usd, "pending": pend, "pending_usd": px.map(|p| p * pend)}));
        }
        Ok(json!({"claimable": data, "can_claim": !claimed && snap["claimState"]["canClaim"].as_bool().unwrap_or(false), "usd": total, "pending_usd": pending,
                  "snapshot_claimed": claimed}))
    })();
    let last_path = crate::paths::logs().join(format!("rewards_last_{cid}.json"));
    let res = match res {
        Ok(v) => {
            let mut keep = v.clone();
            keep["as_of"] = json!(crate::engine::now());
            keep["wallet"] = json!(key.1);
            let _ = crate::settings::write_atomic(&last_path, &keep);
            v
        }
        // The rewards API is down: show the last figures read, marked stale, rather than nothing.
        Err(e) => match std::fs::read_to_string(&last_path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(|v| v["wallet"].as_str() == Some(key.1.as_str()))
        {
            Some(mut last) => {
                last["stale"] = json!(true);
                last["error"] = json!(e);
                last["can_claim"] = json!(false);
                last
            }
            None => json!({"error": e}),
        },
    };
    CACHE.lock().unwrap().as_mut().unwrap().insert(key, (Instant::now(), res.clone()));
    res
}

fn root(cid: u64, rewarder: &str, f: &str) -> Result<String, String> {
    let r = chain::eth_call(cid, rewarder, &eth::calldata(&format!("{f}()"), &[]), None).map_err(|e| e.message)?;
    Ok(format!("0x{}", eth::hex(&r[..32])))
}

fn sync_root(cid: u64, wallet: &str, snap: &Value, execute: bool) -> Result<Value, String> {
    let rw = snap["rewarder"].as_str().unwrap_or("");
    let (a, b, cur) = (root(cid, rw, "rootCandidateA")?, root(cid, rw, "rootCandidateB")?, root(cid, rw, "root")?);
    let want = snap["sync"]["candidateRoot"].as_str().unwrap_or("").to_lowercase();
    let zero = format!("0x{}", "0".repeat(64));
    if a == zero || a != b {
        return Ok(json!({"status": "waiting", "reason": "root candidates do not agree yet; sync not possible"}));
    }
    if a == cur {
        return Ok(json!({"status": "waiting", "reason": "root already synced; waiting for the rewards API to catch up"}));
    }
    if !want.is_empty() && a != want {
        return Ok(json!({"status": "waiting", "reason": "on-chain candidate differs from the API's; re-checked next cycle"}));
    }
    let data = eth::calldata("triggerRoot()", &[]);
    let gas = chain::simulate(cid, wallet, rw, &data).map_err(|e| e.message)?;
    crate::info!("claim", "claim: root sync {} -> {} simulated OK, gas {}", &cur[..10], &a[..10], crate::engine::thousands(gas));
    if !execute {
        log(json!({"chain_id": cid, "kind": "dry_run", "what": "sync root", "to": rw, "data": data}));
        return Ok(json!({"status": "dry_run", "reason": "would sync the root (triggerRoot), then claim after the delay"}));
    }
    crate::agent::gate::pre_sign(cid, None, "claim root sync")?;
    let acct = crate::signer::load_account()?;
    let res = chain::send(cid, &acct, rw, &data, gas).map_err(|e| e.message)?;
    crate::info!("claim", "claim: root synced in {}", res.hash);
    log(json!({"chain_id": cid, "kind": "sent", "what": "sync root", "root": a, "hash": res.hash, "block": res.block, "gas_used": res.gas_used}));
    Ok(json!({"status": "synced", "hash": res.hash}))
}

fn addrs(v: &Value) -> Tok {
    Tok::Arr(v.as_array().cloned().unwrap_or_default().iter().map(|x| eth::addr(x.as_str().unwrap_or(""))).collect())
}

pub fn run(cid: u64, wallet: &str, execute: bool, every_days: f64, force: bool) -> Value {
    match run_inner(cid, wallet, execute, every_days, force) {
        Ok(v) => v,
        Err(e) => {
            crate::error!("claim", "claim failed: {e}");
            json!({"status": "error", "error": e})
        }
    }
}

fn run_inner(cid: u64, wallet: &str, execute: bool, every_days: f64, force: bool) -> Result<Value, String> {
    let age = now().saturating_sub(last_claim(cid)) as f64;
    if !force && age < every_days * 86400.0 {
        crate::info!("claim", "claim: last claim {:.1} days ago; next in {:.1} days", age / 86400.0, every_days - age / 86400.0);
        return Ok(json!({"status": "not_due"}));
    }
    let mut snap = snapshot(cid, wallet)?;
    if snap["syncRequired"].as_bool().unwrap_or(false) {
        let sync = match sync_root(cid, wallet, &snap, execute) {
            Ok(s) => s,
            Err(e) => {
                crate::error!("claim", "claim: root sync failed: {e}");
                log(json!({"chain_id": cid, "kind": "error", "what": "sync root", "error": e}));
                return Ok(json!({"status": "error", "error": format!("root sync failed: {e}")}));
            }
        };
        if sync["status"] != "synced" {
            crate::info!("claim", "claim: {}", sync["reason"].as_str().unwrap_or(""));
            return Ok(json!({"status": "waiting", "reason": sync["reason"]}));
        }
        let rw = snap["rewarder"].as_str().unwrap_or("").to_string();
        let r = chain::eth_call(cid, &rw, &eth::calldata("claimDelay()", &[]), None).map_err(|e| e.message)?;
        let delay: u64 = num_traits::ToPrimitive::to_u64(&BigUint::from_bytes_be(&r)).unwrap_or(u64::MAX);
        if delay > 600 {
            return Ok(json!({"status": "waiting", "reason": format!("root synced; claimable in {} min (next cycles)", delay / 60)}));
        }
        std::thread::sleep(Duration::from_secs(delay + 15));
        for _ in 0..6 {
            snap = snapshot(cid, wallet)?;
            if !snap["syncRequired"].as_bool().unwrap_or(false) && snap["claimState"]["canClaim"].as_bool().unwrap_or(false) {
                break;
            }
            std::thread::sleep(Duration::from_secs(20));
        }
    }
    let cs = snap["claimState"].clone();
    if snap["syncRequired"].as_bool().unwrap_or(false) || !cs["canClaim"].as_bool().unwrap_or(false) || snap["claim"].is_null() {
        let why = if snap["syncRequired"].as_bool().unwrap_or(false) {
            "root synced; waiting for the rewards API to show it".to_string()
        } else if cs["claimableAt"].is_number() && !cs["canClaim"].as_bool().unwrap_or(false) {
            let (y, mo, d, h, mi, _) = crate::log::civil(cs["claimableAt"].as_i64().unwrap_or(0));
            format!("security buffer until {y:04}-{mo:02}-{d:02} {h:02}:{mi:02} UTC")
        } else {
            "nothing claimable".to_string()
        };
        crate::info!("claim", "claim: not now ({why}); retried next cycle");
        return Ok(json!({"status": "waiting", "reason": why, "claimable_at": cs["claimableAt"]}));
    }
    let c = &snap["claim"];
    let types = Tok::Arr(c["types"].as_array().cloned().unwrap_or_default().iter().map(|t| eth::u(eth::parse_uint(&t.to_string().replace('"', "")))).collect());
    let amounts = Tok::Arr(c["amounts"].as_array().cloned().unwrap_or_default().iter().map(|t| eth::u(eth::parse_uint(t.as_str().unwrap_or(&t.to_string())))).collect());
    let mut path = vec![];
    for h in c["proof"]["path"].as_array().cloned().unwrap_or_default() {
        let b = eth::unhex(h.as_str().unwrap_or(""))?;
        let mut w = [0u8; 32];
        w.copy_from_slice(&b[..32]);
        path.push(Tok::B32(w));
    }
    let flags = Tok::Arr(c["proof"]["flags"].as_array().cloned().unwrap_or_default().iter().map(|f| Tok::Bool(f.as_bool().unwrap_or(false))).collect());
    let arg = Tok::Tup(vec![addrs(&c["earners"]), types, addrs(&c["pools"]), addrs(&c["tokens"]), amounts, Tok::Tup(vec![Tok::Arr(path), flags])]);
    let data = eth::calldata(SIG, &[arg]);
    let rewards: Vec<Value> = snap["rewards"].as_array().cloned().unwrap_or_default().iter().map(|r| json!({"token": r["token"], "claimable": r["claimable"], "pool": r["pool"]})).collect();
    for r in &rewards {
        crate::info!("claim", "claim: {} claimable {}", r["token"].as_str().unwrap_or(""), r["claimable"]);
    }
    let earners: Vec<String> = c["earners"].as_array().cloned().unwrap_or_default().iter().filter_map(|e| e.as_str().map(|s| s.to_lowercase())).collect();
    if !earners.contains(&wallet.to_lowercase()) {
        crate::error!("claim", "claim: snapshot earners do not include the wallet {wallet}; not claiming");
        return Ok(json!({"status": "error", "error": "earner mismatch"}));
    }
    let rw = snap["rewarder"].as_str().unwrap_or("").to_string();
    let mut gas = 0;
    for attempt in 0..3 {
        match chain::simulate(cid, wallet, &rw, &data) {
            Ok(g) => {
                gas = g;
                break;
            }
            Err(e) => {
                crate::warn!("claim", "claim simulation attempt {} failed: {}", attempt + 1, e.message);
                if attempt == 2 {
                    log(json!({"chain_id": cid, "kind": "error", "what": "claim", "error": e.message}));
                    return Ok(json!({"status": "error", "error": e.message}));
                }
                std::thread::sleep(Duration::from_secs(15 * (attempt + 1)));
            }
        }
    }
    crate::info!("claim", "claim: simulated OK, gas {}", crate::engine::thousands(gas));
    if !execute {
        log(json!({"chain_id": cid, "kind": "dry_run", "what": "claim", "to": rw, "data": data}));
        return Ok(json!({"status": "dry_run", "rewards": rewards}));
    }
    crate::agent::gate::pre_sign(cid, None, "claim")?;
    let acct = crate::signer::load_account()?;
    let res = match chain::send(cid, &acct, &rw, &data, gas) {
        Ok(r) => r,
        Err(e) => {
            crate::error!("claim", "claim transaction failed (not retried this cycle): {}", e.message);
            log(json!({"chain_id": cid, "kind": "error", "what": "claim", "error": e.message}));
            return Ok(json!({"status": "error", "error": e.message}));
        }
    };
    crate::info!("claim", "claim: sent {}; rewards compound at the next rebalance", res.hash);
    log(json!({"chain_id": cid, "kind": "sent", "what": "claim", "rewards": rewards, "hash": res.hash, "block": res.block, "gas_used": res.gas_used}));
    let _ = std::fs::write(state_path(cid), json!({"ts": now(), "hash": res.hash, "rewards": rewards, "tokens": c["tokens"], "amounts": c["amounts"]}).to_string());
    crate::ledger::invalidate();
    CACHE.lock().unwrap().as_mut().map(|m| m.remove(&(cid, wallet.to_lowercase())));
    Ok(json!({"status": "claimed", "hash": res.hash, "rewards": rewards}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claimed_snapshot_is_recognised_until_the_next_one() {
        let last = json!({"tokens": ["0xAA", "0xbb"], "amounts": ["100", "7"]});
        assert!(same_snapshot(&last, &json!({"tokens": ["0xaa", "0xbb"], "amounts": ["100", "7"]})));
        // the next snapshot: more accrued, a different proof
        assert!(!same_snapshot(&last, &json!({"tokens": ["0xaa", "0xbb"], "amounts": ["130", "9"]})));
        // a claim from before amounts were recorded is never taken for one
        assert!(!same_snapshot(&json!({"rewards": []}), &json!({"tokens": [], "amounts": []})));
    }
}
