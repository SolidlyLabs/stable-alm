//! JSON-RPC per chain. Reads go to the chain's `rpc`, signed transactions to its `send_rpc` (both
//! default to dRPC's free public endpoints and can be changed per chain in the dashboard).
//!
//! A batch is cut into requests of at most the chain's `batch_size` (the free dRPC plan accepts 3),
//! and plain `eth_call`s in it are packed into one Multicall3 `aggregate3` call, so many reads cost
//! one request. Rate limits (HTTP 429, or a rate-limit error inside the response) are waited out and
//! retried with growing pauses; other errors are returned to the caller (which may, for example, split
//! a log range that is too large).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// -- pacing -----------------------------------------------------------------------------------------
// Free public endpoints allow ~50 requests a minute per endpoint and IP (dRPC; measured), counting
// each entry of a batch. Every thread asks the pacer before it sends, so the app as a whole stays
// under the budget instead of tripping it and backing off; after a 429 the endpoint is left alone
// for a while by everyone.

/// Requests per minute for `url` of chain `c` (0 = unlimited).
pub fn rate_for(c: &crate::chains::Chain, url: &str) -> u32 {
    c.rate_per_min.unwrap_or(if url.contains(".drpc.org") { 40 } else { 0 })
}

struct Bucket {
    tokens: f64,
    at: Instant,
    cool_until: Instant,
}

static BUCKETS: Mutex<Option<HashMap<String, Bucket>>> = Mutex::new(None);

fn host(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("").to_lowercase()
}

/// Wait until `n` requests may go to `url`.
fn pace(cid: u64, url: &str, n: usize) {
    let rate = rate_for(&crate::chains::get(cid), url) as f64;
    if rate <= 0.0 {
        return;
    }
    let per_s = rate / 60.0;
    let cap = (rate / 4.0).clamp(3.0, 30.0);
    let need = (n as f64).min(cap);
    loop {
        let wait = {
            let mut g = BUCKETS.lock().unwrap();
            let now = Instant::now();
            let b = g.get_or_insert_with(HashMap::new).entry(host(url)).or_insert(Bucket { tokens: cap, at: now, cool_until: now });
            b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * per_s).min(cap);
            b.at = now;
            if now < b.cool_until {
                b.cool_until - now
            } else if b.tokens >= need {
                b.tokens -= need;
                return;
            } else {
                Duration::from_secs_f64((need - b.tokens) / per_s)
            }
        };
        std::thread::sleep(wait.min(Duration::from_secs(30)));
    }
}

/// The endpoint said "too many": nobody sends to it for `secs`, and the budget starts empty.
fn cool_down(url: &str, secs: u64) {
    let mut g = BUCKETS.lock().unwrap();
    let now = Instant::now();
    let b = g.get_or_insert_with(HashMap::new).entry(host(url)).or_insert(Bucket { tokens: 0.0, at: now, cool_until: now });
    b.tokens = 0.0;
    b.at = now;
    b.cool_until = b.cool_until.max(now + Duration::from_secs(secs));
}

pub fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| {
        ureq::AgentBuilder::new()
            .tls_connector(std::sync::Arc::new(native_tls::TlsConnector::new().expect("TLS")))
            .timeout(Duration::from_secs(90))
            .user_agent("stable-alm/0.1")
            .build()
    })
}

fn rpc_url(cid: u64) -> String {
    crate::chains::get(cid).rpc
}

/// An off chain's requests are refused here, whoever asks (chains::set_off).
fn chain_on(cid: u64) -> Result<(), String> {
    if crate::chains::is_off(cid) {
        return Err(format!("{} is switched off (Settings, Chains & RPC)", crate::chains::get(cid).name));
    }
    Ok(())
}

fn send_url(cid: u64) -> Result<String, String> {
    chain_on(cid)?;
    let u = crate::chains::get(cid).send_rpc;
    if u.is_empty() {
        return Err(format!("no send RPC configured for chain {cid}"));
    }
    Ok(u)
}

/// Result of a call: Ok(result) or Err(error object / message), like the Python helpers' return.
pub type R = Result<Value, Value>;

fn unpack(r: Option<&Value>) -> R {
    match r {
        Some(v) if v.get("result").is_some() => Ok(v["result"].clone()),
        Some(v) => Err(v.get("error").cloned().unwrap_or(json!("missing"))),
        None => Err(json!("missing")),
    }
}

/// True for errors that mean "slow down" rather than "this request is wrong".
pub fn is_rate_limit(e: &Value) -> bool {
    let code = e["code"].as_i64().unwrap_or(0);
    let m = e.to_string().to_lowercase();
    code == 429 || code == -32005 || m.contains("rate limit") || m.contains("too many requests") || m.contains("rate-limit")
        || m.contains("exceeded the quota") || m.contains("capacity exceeded") || m.contains("throttl")
}

pub const MULTICALL3: &str = "0xca11bde05977b3631167028862be2a173976ca11";

/// aggregate3((address target, bool allowFailure, bytes callData)[]) with allowFailure = true.
pub fn encode_aggregate3(calls: &[(String, Vec<u8>)]) -> Vec<u8> {
    let w = |n: usize| { let mut b = [0u8; 32]; b[24..].copy_from_slice(&(n as u64).to_be_bytes()); b.to_vec() };
    let pad = |d: &[u8]| { let mut v = d.to_vec(); v.resize(d.len().div_ceil(32) * 32, 0); v };
    let tuples: Vec<Vec<u8>> = calls
        .iter()
        .map(|(to, data)| {
            let mut t = vec![0u8; 12];
            t.extend(crate::eth::unhex(to).unwrap_or_else(|_| vec![0u8; 20]));
            t.extend(w(1));
            t.extend(w(0x60));
            t.extend(w(data.len()));
            t.extend(pad(data));
            t
        })
        .collect();
    let mut out = vec![0x82, 0xad, 0x56, 0xcb];
    out.extend(w(0x20));
    out.extend(w(calls.len()));
    let mut off = 32 * calls.len();
    for t in &tuples {
        out.extend(w(off));
        off += t.len();
    }
    for t in tuples {
        out.extend(t);
    }
    out
}

/// The (success, returnData)[] result of aggregate3.
pub fn decode_aggregate3(r: &[u8]) -> Option<Vec<(bool, Vec<u8>)>> {
    let num = |b: &[u8], at: usize| -> Option<usize> {
        let s = b.get(at..at + 32)?;
        if s[..24].iter().any(|x| *x != 0) { return None; }
        Some(u64::from_be_bytes(s[24..].try_into().ok()?) as usize)
    };
    let base = num(r, 0)?;
    let n = num(r, base)?;
    let head = base + 32;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = head + num(r, head + 32 * i)?;
        let ok = num(r, t)? != 0;
        let d = t + num(r, t + 32)?;
        let len = num(r, d)?;
        out.push((ok, r.get(d + 32..d + 32 + len)?.to_vec()));
    }
    Some(out)
}

/// A plain eth_call (no sender) that can go into a multicall: (to, data, block tag).
fn packable(m: &str, p: &Value) -> Option<(String, Vec<u8>, Value)> {
    if m != "eth_call" {
        return None;
    }
    let tx = p.get(0)?;
    if tx.get("from").is_some() || tx.get("value").is_some() || tx.as_object()?.len() != 2 {
        return None;
    }
    let data = crate::eth::unhex(tx["data"].as_str()?).ok()?;
    Some((tx["to"].as_str()?.to_lowercase(), data, p.get(1).cloned().unwrap_or(json!("latest"))))
}

pub fn batch(cid: u64, calls: &[(&str, Value)]) -> Result<Vec<R>, String> {
    batch_with(cid, calls, 10, Duration::from_secs(90))
}

/// batch() for a page that must not wait long: 3 tries and 20 s per request instead of 10 tries
/// (about 3 minutes of pauses when the node cannot be reached) and 90 s.
pub fn batch_quick(cid: u64, calls: &[(&str, Value)]) -> Result<Vec<R>, String> {
    batch_with(cid, calls, 3, Duration::from_secs(20))
}

fn batch_with(cid: u64, calls: &[(&str, Value)], rounds: usize, timeout: Duration) -> Result<Vec<R>, String> {
    // Group packable eth_calls by block tag; each group of 2+ becomes one aggregate3 call.
    let mut out: Vec<Option<R>> = vec![None; calls.len()];
    let mut reqs: Vec<(String, Value)> = vec![]; // (method, params) actually sent
    let mut map: Vec<Vec<usize>> = vec![]; // request -> the calls it answers (several = multicall)
    let mut groups: Vec<(Value, Vec<(usize, String, Vec<u8>)>)> = vec![];
    for (i, (m, p)) in calls.iter().enumerate() {
        match packable(m, p) {
            Some((to, data, tag)) => match groups.iter_mut().find(|g| g.0 == tag) {
                Some(g) => g.1.push((i, to, data)),
                None => groups.push((tag, vec![(i, to, data)])),
            },
            None => {
                reqs.push((m.to_string(), p.clone()));
                map.push(vec![i]);
            }
        }
    }
    for (tag, g) in groups {
        for part in g.chunks(150) {
            if part.len() == 1 {
                let (i, ..) = part[0];
                reqs.push((calls[i].0.to_string(), calls[i].1.clone()));
                map.push(vec![i]);
            } else {
                let body: Vec<(String, Vec<u8>)> = part.iter().map(|(_, t, d)| (t.clone(), d.clone())).collect();
                let data = format!("0x{}", crate::eth::hex(&encode_aggregate3(&body)));
                reqs.push(("eth_call".into(), json!([{"to": MULTICALL3, "data": data}, tag])));
                map.push(part.iter().map(|x| x.0).collect());
            }
        }
    }
    let res = send_requests(cid, &reqs, rounds, timeout)?;
    for (k, r) in res.into_iter().enumerate() {
        let idx = &map[k];
        if idx.len() == 1 && reqs[k].1[0]["to"] != MULTICALL3 {
            out[idx[0]] = Some(r);
            continue;
        }
        let decoded = r.as_ref().ok().and_then(|v| v.as_str()).and_then(|h| crate::eth::unhex(h).ok()).and_then(|b| decode_aggregate3(&b));
        match decoded {
            Some(rows) if rows.len() == idx.len() => {
                for (j, (ok, data)) in rows.into_iter().enumerate() {
                    let hexd = format!("0x{}", crate::eth::hex(&data));
                    out[idx[j]] = Some(if ok { Ok(json!(hexd)) } else { Err(json!({"code": 3, "message": "execution reverted", "data": hexd})) });
                }
            }
            _ => {
                // The multicall itself failed (no Multicall3 on this chain, or a node error): ask one by one.
                let single: Vec<(String, Value)> = idx.iter().map(|i| (calls[*i].0.to_string(), calls[*i].1.clone())).collect();
                for (j, r) in send_requests(cid, &single, rounds, timeout)?.into_iter().enumerate() {
                    out[idx[j]] = Some(r);
                }
            }
        }
    }
    Ok(out.into_iter().map(|r| r.unwrap_or(Err(json!("missing")))).collect())
}

/// Send requests in batches of the chain's batch size; rate-limited entries are retried after a pause.
fn send_requests(cid: u64, reqs: &[(String, Value)], rounds: usize, timeout: Duration) -> Result<Vec<R>, String> {
    chain_on(cid)?;
    let size = crate::chains::get(cid).batch_size.max(1);
    let mut out: Vec<Option<R>> = vec![None; reqs.len()];
    let mut todo: Vec<usize> = (0..reqs.len()).collect();
    let mut delay = 2u64;
    let mut why = String::new(); // last reason a retry was needed, for the log
    for round in 0..rounds {
        let mut again = vec![];
        for part in todo.chunks(size) {
            let payload: Vec<Value> = part.iter().map(|i| json!({"jsonrpc": "2.0", "id": i, "method": reqs[*i].0, "params": reqs[*i].1})).collect();
            let body = if payload.len() == 1 { payload[0].clone() } else { Value::Array(payload) };
            let url = rpc_url(cid);
            pace(cid, &url, part.len());
            let before = again.len();
            let res = agent().post(&url).timeout(timeout).set("Content-Type", "application/json").send_json(body);
            let limited = matches!(&res, Err(ureq::Error::Status(429, _)));
            match res {
                Ok(resp) => {
                    let v: Value = resp.into_json().map_err(|e| e.to_string())?;
                    let arr = if v.is_array() { v.as_array().unwrap().clone() } else { vec![v] };
                    for i in part {
                        let r = unpack(arr.iter().find(|r| r["id"].as_u64() == Some(*i as u64)).or(if arr.len() == 1 { arr.first() } else { None }));
                        match &r {
                            Err(e) if is_rate_limit(e) || e == &json!("missing") => {
                                why = e.to_string().chars().take(160).collect();
                                again.push(*i)
                            }
                            _ => out[*i] = Some(r),
                        }
                    }
                }
                Err(ureq::Error::Status(code, r)) if [408, 429, 500, 502, 503, 504].contains(&code) => {
                    why = format!("HTTP {code} {}", r.into_string().unwrap_or_default().chars().take(120).collect::<String>());
                    again.extend(part.iter().copied());
                }
                Err(ureq::Error::Status(code, r)) => {
                    let text = r.into_string().unwrap_or_default();
                    // some providers answer a JSON-RPC error with an HTTP error status
                    match serde_json::from_str::<Value>(&text) {
                        Ok(v) if v.get("error").is_some() && is_rate_limit(&v["error"]) => again.extend(part.iter().copied()),
                        Ok(v) if v.get("error").is_some() && part.len() == 1 => out[part[0]] = Some(Err(v["error"].clone())),
                        _ => return Err(format!("RPC HTTP {code}: {}", text.chars().take(300).collect::<String>())),
                    }
                }
                Err(e) => {
                    why = e.to_string().chars().take(160).collect();
                    again.extend(part.iter().copied())
                }
            }
            if limited || (again.len() > before && why.to_lowercase().contains("rate limit")) {
                cool_down(&url, 15);
            }
        }
        if again.is_empty() {
            break;
        }
        if round + 1 == rounds {
            return Err(format!("RPC: still rate limited or unreachable after retries ({} requests, {}): {why}", again.len(), reqs[again[0]].0));
        }
        if round >= 1 {
            crate::warn!("rpc", "chain {cid}: rate limited or unreachable; retrying {} requests ({}) in {delay}s: {why}", again.len(), reqs[again[0]].0);
        }
        std::thread::sleep(Duration::from_secs(delay));
        delay = (delay * 2).min(30);
        todo = again;
    }
    Ok(out.into_iter().map(|r| r.unwrap_or(Err(json!("missing")))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_server_failures_and_rate_limits_are_asked_again() {
        for c in [429, 500, 502, 503, 504, 522] {
            assert!(transient(c), "{c}");
        }
        for c in [200, 400, 401, 403, 404, 422] {
            assert!(!transient(c), "{c}");
        }
    }

    #[test]
    fn aggregate3_round_trip_shape() {
        let calls = vec![("0x00000000000000000000000000000000000000a1".to_string(), vec![1, 2, 3, 4]),
                         ("0x00000000000000000000000000000000000000b2".to_string(), vec![9; 36])];
        let enc = encode_aggregate3(&calls);
        assert_eq!(&enc[..4], &[0x82, 0xad, 0x56, 0xcb]);
        // the call data are where the offsets say
        let body = &enc[4..];
        let num = |at: usize| u64::from_be_bytes(body[at + 24..at + 32].try_into().unwrap()) as usize;
        assert_eq!(num(0), 32);
        assert_eq!(num(32), 2);
        let t1 = 64 + num(64 + 32);
        assert_eq!(&body[t1 + 12..t1 + 32], &crate::eth::unhex(&calls[1].0).unwrap()[..]);
        let d1 = t1 + num(t1 + 64);
        assert_eq!(num(d1), 36);
        assert_eq!(&body[d1 + 32..d1 + 68], &[9u8; 36][..]);
    }

    #[test]
    fn aggregate3_result_decodes() {
        // (bool,bytes)[] = [(true, 0x01..20 bytes), (false, 0x)]
        let w = |n: u64| { let mut b = [0u8; 32]; b[24..].copy_from_slice(&n.to_be_bytes()); b.to_vec() };
        let mut r = w(0x20);
        r.extend(w(2));
        r.extend(w(0x40)); // tuple 0 at head+0x40
        r.extend(w(0x40 + 0x80)); // tuple 1 after tuple 0 (2 words + len + 1 data word)
        r.extend(w(1)); r.extend(w(0x40)); r.extend(w(20)); let mut d = vec![7u8; 20]; d.resize(32, 0); r.extend(d);
        r.extend(w(0)); r.extend(w(0x40)); r.extend(w(0));
        let rows = decode_aggregate3(&r).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].0 && rows[0].1 == vec![7u8; 20]);
        assert!(!rows[1].0 && rows[1].1.is_empty());
    }

    #[test]
    fn rate_limit_detection() {
        assert!(is_rate_limit(&json!({"code": 429, "message": "x"})));
        assert!(is_rate_limit(&json!({"code": -1, "message": "Too Many Requests"})));
        assert!(!is_rate_limit(&json!({"code": 35, "message": "ranges over 10000 blocks are not supported on free plan"})));
    }
}

pub fn call(cid: u64, method: &str, params: Value) -> Result<R, String> {
    Ok(batch(cid, &[(method, params)])?.remove(0))
}

/// call() that turns an RPC error into Err(String).
pub fn call_ok(cid: u64, method: &str, params: Value) -> Result<Value, String> {
    call(cid, method, params)?.map_err(|e| format!("{method}: {e}"))
}

pub fn send_call(cid: u64, method: &str, params: Value) -> Result<R, String> {
    let mut delay = 2;
    for attempt in 0..4 {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let url = send_url(cid)?;
        pace(cid, &url, 1);
        match agent().post(&url).timeout(Duration::from_secs(30)).send_json(body) {
            Ok(resp) => {
                let v: Value = resp.into_json().map_err(|e| e.to_string())?;
                if v.get("error").map(is_rate_limit).unwrap_or(false) && attempt < 3 {
                    std::thread::sleep(Duration::from_secs(delay));
                    delay *= 2;
                    continue;
                }
                return Ok(unpack(Some(&v)));
            }
            Err(ureq::Error::Status(code, r)) => {
                let v: Value = r.into_json().unwrap_or(json!({}));
                if (code == 429 || v.get("error").map(is_rate_limit).unwrap_or(false)) && attempt < 3 {
                    cool_down(&url, 10);
                    std::thread::sleep(Duration::from_secs(delay));
                    delay *= 2;
                    continue;
                }
                return Ok(unpack(Some(&v)));
            }
            Err(e) if attempt == 3 => return Err(e.to_string()),
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_secs(delay));
        delay *= 2;
    }
    Err("send failed".into())
}

pub fn hex_u64(v: &Value) -> u64 {
    v.as_str().and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0)
}

pub fn block_number(cid: u64) -> Result<u64, String> {
    Ok(hex_u64(&call_ok(cid, "eth_blockNumber", json!([]))?))
}

/// Whether an HTTP answer is worth asking again: the server or a proxy in front of it failing (5xx,
/// e.g. Cloudflare's 502 when the origin is overloaded) or a rate limit (429). Other codes are answers.
pub fn transient(code: u16) -> bool {
    code == 429 || (500..600).contains(&code)
}

/// Seconds to wait before attempt `n` (from 1) of a web API call: 0, 1, 3.
const API_WAITS: [u64; 3] = [0, 1, 3];

/// A web API call, asked again (up to three times in all) when it fails in a way that may pass: a
/// transient HTTP code, or no answer at all (connection reset, timeout).
fn api_call(send: impl Fn() -> Result<ureq::Response, ureq::Error>) -> Result<Value, String> {
    let mut last = String::new();
    for wait in API_WAITS {
        std::thread::sleep(std::time::Duration::from_secs(wait));
        match send() {
            Ok(r) => return r.into_json().map_err(|e| e.to_string()),
            Err(ureq::Error::Status(c, r)) => {
                last = format!("HTTP {c}: {}", r.into_string().unwrap_or_default().chars().take(300).collect::<String>());
                if !transient(c) {
                    return Err(last);
                }
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// GET or POST JSON over HTTPS (Solidly API routes), asked again on a transient failure.
pub fn get_json(url: &str) -> Result<Value, String> {
    api_call(|| agent().get(url).set("Accept", "application/json").call())
}

pub fn post_json(url: &str, body: &Value, headers: &[(&str, &str)]) -> Result<Value, String> {
    api_call(|| {
        let mut req = agent().post(url).set("Content-Type", "application/json").set("Accept", "application/json");
        for (k, v) in headers {
            req = req.set(k, v);
        }
        req.send_json(body.clone())
    })
}
