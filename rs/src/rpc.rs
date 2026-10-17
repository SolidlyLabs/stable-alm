//! JSON-RPC per chain (alm/rpc.py). Reads go to the chain's `rpc` (default the Solidly read proxy,
//! batches of up to 50, 429 retried); signed transactions to its `send_rpc` (see chains.rs).

use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

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

fn send_url(cid: u64) -> Result<String, String> {
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

pub fn batch(cid: u64, calls: &[(&str, Value)]) -> Result<Vec<R>, String> {
    let payload: Vec<Value> = calls
        .iter()
        .enumerate()
        .map(|(i, (m, p))| json!({"jsonrpc": "2.0", "id": i, "method": m, "params": p}))
        .collect();
    let mut delay = 5u64;
    for _ in 0..8 {
        match agent().post(&rpc_url(cid)).set("Content-Type", "application/json").send_json(Value::Array(payload.clone())) {
            Ok(resp) => {
                let out: Value = resp.into_json().map_err(|e| e.to_string())?;
                let arr = if out.is_array() { out.as_array().unwrap().clone() } else { vec![out] };
                return Ok((0..calls.len())
                    .map(|i| unpack(arr.iter().find(|r| r["id"].as_u64() == Some(i as u64))))
                    .collect());
            }
            Err(ureq::Error::Status(code, _)) if [429, 502, 503, 504].contains(&code) => {}
            Err(ureq::Error::Status(code, r)) => return Err(format!("RPC HTTP {code}: {}", r.into_string().unwrap_or_default())),
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_secs(delay));
        delay = (delay * 2).min(60);
    }
    Err("RPC batch failed after retries".into())
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
        match agent().post(&send_url(cid)?).timeout(Duration::from_secs(30)).send_json(body) {
            Ok(resp) => {
                let v: Value = resp.into_json().map_err(|e| e.to_string())?;
                return Ok(unpack(Some(&v)));
            }
            Err(ureq::Error::Status(_, r)) => {
                let v: Value = r.into_json().unwrap_or(json!({}));
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

/// GET or POST JSON over HTTPS (Solidly API routes).
pub fn get_json(url: &str) -> Result<Value, String> {
    match agent().get(url).set("Accept", "application/json").call() {
        Ok(r) => r.into_json().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(c, r)) => Err(format!("HTTP {c}: {}", r.into_string().unwrap_or_default().chars().take(300).collect::<String>())),
        Err(e) => Err(e.to_string()),
    }
}

pub fn post_json(url: &str, body: &Value, headers: &[(&str, &str)]) -> Result<Value, String> {
    let mut req = agent().post(url).set("Content-Type", "application/json").set("Accept", "application/json");
    for (k, v) in headers {
        req = req.set(k, v);
    }
    match req.send_json(body.clone()) {
        Ok(r) => r.into_json().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(c, r)) => Err(format!("HTTP {c}: {}", r.into_string().unwrap_or_default().chars().take(300).collect::<String>())),
        Err(e) => Err(e.to_string()),
    }
}
