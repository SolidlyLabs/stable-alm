//! Market data for the UI, per chain: the Solidly UI's getInitData (JWT signed
//! with the UI's public key, read from solidly.com's bundle), hourly APR windows, wallet tokens.

use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

static CACHE: Mutex<Option<HashMap<String, (Instant, Value)>>> = Mutex::new(None);

fn cached(key: String, ttl: u64, f: impl FnOnce() -> Result<Value, String>) -> Result<Value, String> {
    if let Some((t, v)) = CACHE.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        if t.elapsed() < Duration::from_secs(ttl) {
            return Ok(v.clone());
        }
    }
    let v = f()?;
    CACHE.lock().unwrap().as_mut().unwrap().insert(key, (Instant::now(), v.clone()));
    Ok(v)
}

fn b64url(b: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    s
}

fn jwt(user: &str, key: &str) -> String {
    let head = b64url(br#"{"alg":"HS256","typ":"JWT"}"#);
    let body = b64url(format!(r#"{{"userId":"{user}","iat":{}}}"#, crate::engine::now()).as_bytes());
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(format!("{head}.{body}").as_bytes());
    format!("{head}.{body}.{}", b64url(&mac.finalize().into_bytes()))
}

fn api_key() -> Result<String, String> {
    if let Ok(k) = std::env::var("ALM_SOLIDLY_API_KEY") {
        return Ok(k);
    }
    cached("api_key".into(), 86400, || {
        let get = |u: &str| crate::rpc::agent().get(u).set("User-Agent", "Mozilla/5.0 stable-alm").call().map_err(|e| e.to_string())?.into_string().map_err(|e| e.to_string());
        let html = get("https://solidly.com/")?;
        let mut srcs: Vec<String> = html.split("src=\"").skip(1).filter_map(|s| s.split('"').next()).filter(|s| s.starts_with("/_next/static/") && s.ends_with(".js")).map(String::from).collect();
        srcs.sort_by_key(|s| !s.contains("_app"));
        for src in srcs {
            let js = get(&format!("https://solidly.com{src}"))?;
            if let Some(i) = js.find("sign({userId:") {
                let rest = &js[i..(i + 400).min(js.len())];
                if let Some(j) = rest.find("},\"") {
                    let k = &rest[j + 3..];
                    if let Some(end) = k.find('"') {
                        return Ok(json!(k[..end]));
                    }
                }
            }
        }
        Err("could not find the Solidly UI's public API key in its bundle; set ALM_SOLIDLY_API_KEY".into())
    })
    .map(|v| v.as_str().unwrap_or("").to_string())
}

fn fetch_init_data(cid: u64, wallet: &str) -> Result<Value, String> {
    let key = api_key()?;
    let auth = format!("Bearer {}", jwt(wallet, &key));
    crate::rpc::post_json("https://solidly.com/api/getInitData/", &json!({"userAddress": wallet, "chainId": cid}), &[("Authorization", &auth)])
}

static REFRESHING: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);
const INIT_TTL_S: u64 = 300;

/// The chain's pool list. Served from memory, or after a restart from the copy saved on disk, at
/// once; when that copy is older than 5 minutes a fresh one is fetched in the background. Only
/// with no copy at all does the caller wait for the network. A failed refresh keeps the old copy.
fn init_data(cid: u64, wallet: &str) -> Result<Value, String> {
    let w = wallet.to_lowercase();
    let mkey = format!("init:{cid}:{w}");
    let snap = format!("pools_{cid}_{w}");
    let mem = CACHE.lock().unwrap().get_or_insert_with(HashMap::new).get(&mkey).cloned();
    let (value, age) = match mem {
        Some((t, v)) => (Some(v), t.elapsed().as_secs()),
        None => match crate::store::load_snapshot(&snap) {
            Some((v, saved)) => {
                let age = crate::engine::now().saturating_sub(saved);
                let at = Instant::now().checked_sub(Duration::from_secs(age)).unwrap_or_else(Instant::now);
                CACHE.lock().unwrap().as_mut().unwrap().insert(mkey.clone(), (at, v.clone()));
                (Some(v), age)
            }
            None => (None, u64::MAX),
        },
    };
    let store = move |v: &Value| keep_init(cid, &w, v);
    match value {
        Some(v) => {
            if age >= INIT_TTL_S && REFRESHING.lock().unwrap().get_or_insert_with(Default::default).insert(mkey.clone()) {
                let wallet = wallet.to_string();
                std::thread::spawn(move || {
                    match fetch_init_data(cid, &wallet) {
                        Ok(fresh) => store(&fresh),
                        Err(e) => crate::warn!("market", "pool list for chain {cid}: refresh failed, keeping the saved copy ({e})"),
                    }
                    REFRESHING.lock().unwrap().as_mut().unwrap().remove(&mkey);
                });
            }
            Ok(v)
        }
        None => {
            let v = fetch_init_data(cid, wallet)?;
            store(&v);
            Ok(v)
        }
    }
}

fn keep_init(cid: u64, w: &str, v: &Value) {
    CACHE.lock().unwrap().get_or_insert_with(HashMap::new).insert(format!("init:{cid}:{w}"), (Instant::now(), v.clone()));
    crate::store::save_snapshot(&format!("pools_{cid}_{w}"), v);
    remember_tokens(cid, v);
}

/// The pools table's Refresh: read every chain's pool list from the Solidly UI now, and drop the
/// cached APR windows. A chain whose fetch fails keeps its old copy; the errors are returned.
pub fn reload(wallet: &str, configured: &[(u64, String)]) -> Vec<String> {
    let w = wallet.to_lowercase();
    CACHE.lock().unwrap().get_or_insert_with(HashMap::new).retain(|k, _| !k.starts_with("apr:"));
    let mut errors = vec![];
    for cid in chain_ids(configured) {
        match fetch_init_data(cid, wallet) {
            Ok(v) => keep_init(cid, &w, &v),
            Err(e) => errors.push(format!("{}: {e}", crate::chains::get(cid).name)),
        }
    }
    errors
}

fn chain_ids(configured: &[(u64, String)]) -> Vec<u64> {
    let mut chains: Vec<u64> = crate::chains::all().into_iter().filter(|c| c.enabled && !c.off).map(|c| c.id).collect();
    for (c, _) in configured {
        if !chains.contains(c) && !crate::chains::is_off(*c) {
            chains.push(*c);
        }
    }
    chains
}

/// Keep the facts about every listed token that never change (symbol, name, decimals, logo).
fn remember_tokens(cid: u64, v: &Value) {
    for p in v["filledPools"].as_array().cloned().unwrap_or_default() {
        for t in [&p["token0"], &p["token1"]] {
            let addr = t["id"].as_str().unwrap_or("").to_lowercase();
            if addr.is_empty() {
                continue;
            }
            let dec = crate::prices::fnum(&t["decimals"]);
            crate::store::merge(&crate::store::token_key(cid, &addr), json!({
                "symbol": t["symbol"], "name": t["name"], "decimals": if dec > 0.0 { json!(dec as u64) } else { Value::Null },
                "logo_src": t["logoURI"]}));
        }
    }
}

const PEGS: [(&str, &[&str]); 6] = [
    ("USD", &["USDC", "USDT", "USDBC", "DAI", "DOLA", "USD+", "USDE", "SUSDE", "USDS", "GHO", "CRVUSD", "FRAX", "LUSD", "PYUSD", "USDA", "EUSD", "USDZ", "USDPLUS", "AXLUSDC", "USDT0", "USDC.E", "SCUSD"]),
    ("ETH", &["WETH", "ETH", "CBETH", "WSTETH", "WEETH", "RETH", "EZETH", "WRSETH", "RSETH", "SUPEROETHB", "OETH", "METH"]),
    ("BTC", &["CBBTC", "WBTC", "TBTC", "LBTC", "SOLVBTC"]),
    ("EUR", &["EURC", "EURE", "EURA", "AGEUR"]),
    ("MXN", &["MXNE", "XOC"]),
    ("S", &["S", "WS", "STS", "OS", "WOS"]),
];

pub fn is_pegged(a: &str, b: &str, ratio: Option<f64>) -> bool {
    let fam = |s: &str| PEGS.iter().find(|(_, m)| m.contains(&s.trim().to_uppercase().as_str())).map(|x| x.0);
    match (fam(a), fam(b)) {
        (Some(x), Some(y)) if x == y => ratio.map(|r| (0.8..=1.25).contains(&r)).unwrap_or(true),
        _ => false,
    }
}

/// Logo address the dashboard loads: served by the app from its own copy (see store::logo).
pub fn logo_path(cid: u64, addr: &str, src: &Value) -> Value {
    if src.as_str().map(|s| !s.is_empty()).unwrap_or(false) { json!(format!("/api/logo/{cid}/{}", addr.to_lowercase())) } else { Value::Null }
}

fn token(cid: u64, t: &Value) -> Value {
    let addr = t["id"].as_str().unwrap_or("").to_lowercase();
    json!({"address": addr, "symbol": t["symbol"], "name": t["name"],
           "decimals": crate::prices::fnum(&t["decimals"]) as i64, "price_usd": crate::prices::fnum(&t["priceUSD"]),
           "logo": logo_path(cid, &addr, &t["logoURI"])})
}

pub fn pools(cid: u64, wallet: &str) -> Result<Vec<Value>, String> {
    let n = crate::prices::fnum;
    let mut out: Vec<Value> = init_data(cid, wallet)?["filledPools"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|p| {
            let ia = &p["poolAggregation"]["intervalAggregations"]["HOUR_24"];
            let (t0, t1) = (token(cid, &p["token0"]), token(cid, &p["token1"]));
            let (p0, p1) = (t0["price_usd"].as_f64().unwrap_or(0.0), t1["price_usd"].as_f64().unwrap_or(0.0));
            let ratio = if p0 > 0.0 && p1 > 0.0 { Some(p0 / p1) } else { None };
            json!({"chain_id": cid, "chain": crate::chains::get(cid).name,
                   "pool": p["id"].as_str().unwrap_or("").to_lowercase(), "symbol": p["symbol"], "token0": t0, "token1": t1,
                   "tick_spacing": p["tickSpacing"], "fee_tier": p["feeTier"], "fee_pips": p["feeCurrent"],
                   "tvl_usd": n(&p["totalTvlUsd"]), "active_usd": n(&p["poolAggregation"]["activeLiquidityUSD"]),
                   "volume_24h": n(&ia["volumeUSD"]),
                   "processor_apr_24h": n(&ia["aprFromFeesPercentage"]) + n(&ia["aprFromEmissionsPercentage"]),
                   "whitelisted": p["whitelisted"].as_bool().unwrap_or(false), "supported": true,
                   "pegged": is_pegged(t0["symbol"].as_str().unwrap_or(""), t1["symbol"].as_str().unwrap_or(""), ratio)})
        })
        .collect();
    out.sort_by(|a, b| b["tvl_usd"].as_f64().partial_cmp(&a["tvl_usd"].as_f64()).unwrap());
    Ok(out)
}

pub fn apr_windows(cid: u64, pool: &str) -> Result<Value, String> {
    cached(format!("apr:{cid}:{pool}"), 600, || {
        let url = format!("https://solidly.com/api/getPoolAprHistory/?chainId={cid}&poolId={pool}&interval=hour&limit=25&offset=0");
        let mut rows = crate::rpc::get_json(&url)?["history"].as_array().cloned().unwrap_or_default();
        let n = crate::prices::fnum;
        rows.sort_by(|a, b| n(&b["timestamp"]).partial_cmp(&n(&a["timestamp"])).unwrap());
        let mut out = json!({});
        for h in [1usize, 6, 24] {
            let part = &rows[..h.min(rows.len())];
            if part.is_empty() {
                continue;
            }
            let f = |k: &str| part.iter().map(|r| n(&r[k])).sum::<f64>();
            out[format!("h{h}")] = json!({"hours": part.len(), "fees": f("feesUSD"), "emissions": f("emissionsUSD"), "bribes": f("bribesUSD"),
                                          "volume": f("volumeUSD"), "active_tvl": f("activeTvlUSD") / part.len() as f64,
                                          "tvl": f("tvlUSD") / part.len() as f64});
        }
        Ok(out)
    })
}

/// Pools with TVL >= $50 on every enabled chain (plus the configured ones), with APR windows.
pub fn shortlist(wallet: &str, configured: &[(u64, String)]) -> Result<Vec<Value>, String> {
    let chains = chain_ids(configured);
    let mut rows = vec![];
    let mut errors = vec![];
    for cid in chains {
        match pools(cid, wallet) {
            Ok(ps) => {
                for (i, p) in ps.into_iter().enumerate() {
                    let addr = p["pool"].as_str().unwrap_or("").to_string();
                    let conf = configured.iter().any(|(c, a)| *c == cid && *a == addr);
                    if (i < 40 && p["tvl_usd"].as_f64().unwrap_or(0.0) >= 50.0) || conf {
                        let mut p = p;
                        // Configured pools: from their stored events and hourly fee rates. Others: from the
                        // chain-wide swap download at the pool's current fee.
                        let d = |t: &str| p[t]["decimals"].as_i64().unwrap_or(18) as i32;
                        let v = conf.then(|| crate::data::cached_flvr(cid, &addr)).flatten().or_else(|| {
                            crate::market_flvr::flvr(cid, &addr, d("token0"), d("token1"), crate::prices::fnum(&p["fee_pips"]))
                        });
                        if let Some(v) = v {
                            p["flvr"] = v;
                        }
                        rows.push(p);
                    }
                }
            }
            Err(e) => errors.push(format!("{}: {e}", crate::chains::get(cid).name)),
        }
    }
    if rows.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    // APR windows, 4 at a time.
    let rows = std::sync::Mutex::new(rows);
    let n = rows.lock().unwrap().len();
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for _ in 0..4 {
            sc.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if i >= n {
                    break;
                }
                let (cid, pool) = {
                    let r = rows.lock().unwrap();
                    (r[i]["chain_id"].as_u64().unwrap_or(8453), r[i]["pool"].as_str().unwrap_or("").to_string())
                };
                let w = apr_windows(cid, &pool);
                let mut r = rows.lock().unwrap();
                match w {
                    Ok(w) => r[i]["windows"] = w,
                    Err(e) => {
                        r[i]["windows"] = json!({});
                        r[i]["error"] = json!(e);
                    }
                }
            });
        }
    });
    Ok(rows.into_inner().unwrap())
}

const TOKENS_TTL_S: u64 = 60;

fn tokens_key(wallet: &str) -> String {
    format!("tokens_{}", wallet.to_lowercase())
}

/// Every token of every chain's Solidly pools (+ extra addresses) with the wallet's balance, and
/// whether a refresh is running. Every supported chain, whatever the pools table shows.
///
/// The list is kept on disk (`data/cache/tokens_<wallet>.json`) and answered from there at once; a
/// refresh runs in the background once a minute, or straight away when an extra token is not on the
/// list yet. A refresh reads the chains side by side, reuses what the last list knew about each token
/// (only new tokens are looked up), and reads the balances in one quick multicall per chain; a chain
/// that cannot be read keeps its last rows. With no list at all the caller waits up to 5 s.
pub fn tokens(wallet: &str, extra: &[(u64, String)]) -> (Vec<Value>, bool) {
    let key = tokens_key(wallet);
    let saved = |k: &str| crate::store::background_peek(k).or_else(|| crate::store::load_snapshot(k).map(|x| x.0));
    let missing = saved(&key).map(|v| extra.iter().any(|(c, a)| !on_list(&v, *c, a))).unwrap_or(false);
    let (w, ex, k) = (wallet.to_string(), extra.to_vec(), key.clone());
    let fetch = move || {
        let prev = saved(&k).and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let rows = json!(refresh_tokens(&w, &ex, &prev));
        crate::store::save_snapshot(&k, &rows);
        Ok(rows)
    };
    let v = crate::store::background(&key, if missing { 0 } else { TOKENS_TTL_S }, Duration::from_secs(5), || crate::store::load_snapshot(&key), fetch);
    (v.and_then(|v| v.as_array().cloned()).unwrap_or_default(), crate::store::refreshing(&key))
}

fn on_list(rows: &Value, cid: u64, addr: &str) -> bool {
    let a = addr.to_lowercase();
    rows.as_array().into_iter().flatten().any(|t| t["chain_id"].as_u64() == Some(cid) && t["address"] == a.as_str())
}

fn refresh_tokens(wallet: &str, extra: &[(u64, String)], prev: &[Value]) -> Vec<Value> {
    let chains: Vec<_> = crate::chains::all().into_iter().filter(|c| !c.off).collect();
    let mut out: Vec<Value> = std::thread::scope(|sc| {
        let hs: Vec<_> = chains.iter().map(|c| sc.spawn(move || chain_tokens(c.id, &c.name, wallet, extra, prev))).collect();
        hs.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    out.sort_by(|a, b| b["balance_usd"].as_f64().partial_cmp(&a["balance_usd"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn chain_tokens(cid: u64, name: &str, wallet: &str, extra: &[(u64, String)], prev: &[Value]) -> Vec<Value> {
    let old: Vec<&Value> = prev.iter().filter(|t| t["chain_id"].as_u64() == Some(cid)).collect();
    let listed = pools(cid, wallet).ok().map(|ps| {
        let mut ts: Vec<Value> = vec![];
        for p in ps {
            for t in [&p["token0"], &p["token1"]] {
                if !ts.iter().any(|k| k["address"] == t["address"]) {
                    ts.push(t.clone());
                }
            }
        }
        ts
    });
    let extras: Vec<String> = extra.iter().filter(|(c, _)| *c == cid).map(|(_, a)| a.to_lowercase()).collect();
    let mut rows = chain_list(cid, listed, &old, &extras, |a| {
        // not in any pool: what the store knows (from earlier pool lists or the chain); no price lookup,
        // so the list never waits on a price feed
        let m = crate::prices::meta(cid, a).ok();
        let k = crate::store::get(&crate::store::token_key(cid, a)).unwrap_or(Value::Null);
        json!({"address": a, "symbol": m.as_ref().map(|m| m.symbol.clone()).unwrap_or_else(|| a[..8.min(a.len())].to_string()),
               "name": k["name"], "logo": logo_path(cid, a, &k["logo_src"]),
               "decimals": m.as_ref().map(|m| m.decimals), "price_usd": if *a == crate::chains::get(cid).usd.to_lowercase() { 1.0 } else { 0.0 }})
    });
    let data = format!("0x70a08231{}{}", "0".repeat(24), wallet.trim_start_matches("0x").to_lowercase());
    let calls: Vec<(&str, Value)> = rows.iter().map(|t| ("eth_call", json!([{"to": t["address"], "data": data}, "latest"]))).collect();
    let res = if calls.is_empty() { Ok(vec![]) } else { crate::rpc::batch_quick(cid, &calls) };
    if let Err(e) = &res {
        crate::warn!("market", "{name}: wallet balances not read, showing the last ones: {e}");
    }
    apply_balances(&mut rows, res.ok(), &old);
    rows
}

/// The chain's token rows: the pool list's tokens (None when the pool list is unavailable: the last
/// rows instead), then the extra addresses. An extra the last list had is taken from it; only one
/// never seen before goes through `new_extra` (which may ask the chain).
fn chain_list(cid: u64, listed: Option<Vec<Value>>, old: &[&Value], extras: &[String], mut new_extra: impl FnMut(&str) -> Value) -> Vec<Value> {
    let mut rows: Vec<Value> = listed.unwrap_or_else(|| old.iter().map(|t| (*t).clone()).collect());
    for a in extras {
        if rows.iter().any(|k| k["address"] == a.as_str()) {
            continue;
        }
        let row = match old.iter().find(|t| t["address"] == a.as_str()) {
            Some(t) => (*t).clone(),
            None => new_extra(a),
        };
        rows.push(row);
    }
    for t in rows.iter_mut() {
        t["chain_id"] = json!(cid);
    }
    rows
}

/// Balances from the balanceOf results; without results (the node did not answer) each row keeps the
/// balance the last list had, valued at the current price.
fn apply_balances(rows: &mut [Value], res: Option<Vec<crate::rpc::R>>, old: &[&Value]) {
    for (i, t) in rows.iter_mut().enumerate() {
        let bal = match &res {
            Some(res) => {
                let raw = res.get(i).and_then(|r| r.as_ref().ok()).and_then(|v| v.as_str().map(crate::eth::parse_uint)).map(|b| crate::tickmath::to_f64(&b)).unwrap_or(0.0);
                t["decimals"].as_i64().map(|d| raw / 10f64.powi(d as i32))
            }
            None => old.iter().find(|o| o["address"] == t["address"]).and_then(|o| o["balance"].as_f64()),
        };
        t["balance"] = json!(bal);
        t["balance_usd"] = json!(bal.unwrap_or(0.0) * t["price_usd"].as_f64().unwrap_or(0.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(a: &str, sym: &str, price: f64, bal: Option<f64>) -> Value {
        json!({"address": a, "symbol": sym, "decimals": 6, "price_usd": price, "balance": bal, "chain_id": 8453})
    }

    #[test]
    fn a_refresh_looks_up_only_tokens_the_last_list_did_not_have() {
        let old = [row("0xaa", "AA", 1.0, Some(5.0)), row("0xee", "EE", 2.0, Some(1.0))];
        let old: Vec<&Value> = old.iter().collect();
        let listed = vec![row("0xaa", "AA", 1.1, None), row("0xbb", "BB", 3.0, None)];
        let mut asked = vec![];
        let rows = chain_list(8453, Some(listed), &old, &["0xee".into(), "0xff".into(), "0xbb".into()], |a| {
            asked.push(a.to_string());
            row(a, "FF", 0.0, None)
        });
        // 0xee comes from the last list, 0xbb is in a pool; only 0xff is new
        assert_eq!(asked, vec!["0xff".to_string()]);
        let addrs: Vec<&str> = rows.iter().map(|r| r["address"].as_str().unwrap()).collect();
        assert_eq!(addrs, vec!["0xaa", "0xbb", "0xee", "0xff"]);
        assert_eq!(rows[2]["symbol"], "EE");
        // no pool list (the source is down): the last rows stand
        let rows = chain_list(8453, None, &old, &["0xee".into()], |_| panic!("nothing new"));
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn balances_from_the_chain_or_else_the_last_ones_at_todays_price() {
        let old = [row("0xaa", "AA", 1.0, Some(5.0))];
        let old: Vec<&Value> = old.iter().collect();
        let mut rows = vec![row("0xaa", "AA", 2.0, None), row("0xbb", "BB", 3.0, None)];
        apply_balances(&mut rows, None, &old);
        assert_eq!(rows[0]["balance"], 5.0);
        assert_eq!(rows[0]["balance_usd"], 10.0);
        assert_eq!(rows[1]["balance"], Value::Null);
        // 2.5 and 0 tokens with 6 decimals
        let res = vec![Ok(json!("0x2625a0")), Err(json!("reverted"))];
        apply_balances(&mut rows, Some(res), &old);
        assert_eq!(rows[0]["balance"], 2.5);
        assert_eq!(rows[0]["balance_usd"], 5.0);
        assert_eq!(rows[1]["balance"], 0.0);
    }

    /// On a copy of the app folder (never the live one), with the network:
    /// ALM_HOME=<copy> cargo test -- --ignored tokens_timing_on_a_copy --nocapture
    #[test]
    #[ignore]
    fn tokens_timing_on_a_copy() {
        let home = std::env::var("ALM_HOME").expect("set ALM_HOME to a copy of the app folder");
        assert!(!home.to_lowercase().replace('\\', "/").trim_end_matches('/').ends_with("appdata/local/stablealm"), "not on the live folder");
        let s = crate::settings::load_settings();
        let wallet = s["wallet"].as_str().unwrap().to_string();
        let mut extra = vec![];
        for c in crate::chains::all() {
            extra.extend(crate::settings::funding_tokens(&s, c.id).into_iter().map(|a| (c.id, a)));
        }
        for i in 0..3 {
            let t = Instant::now();
            let (r, busy) = tokens(&wallet, &extra);
            println!("tokens call {i}: {:.2} s, {} rows, refreshing {busy}", t.elapsed().as_secs_f64(), r.len());
        }
        while crate::store::refreshing(&tokens_key(&wallet)) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let t = Instant::now();
        let (r, _) = tokens(&wallet, &extra);
        println!("tokens after the background refresh: {:.2} s, {} rows", t.elapsed().as_secs_f64(), r.len());
    }
}
