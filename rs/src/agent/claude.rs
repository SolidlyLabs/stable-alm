//! The Messages API adapter (plan sections 13 and 15). The key is kept like the signer key
//! (`signer::save_secret("anthropic_api_key")`), sent only to api.anthropic.com in the x-api-key
//! header, and never logged or shown back.
//!
//! Calls use their own HTTP agent with a 10-minute timeout (a high-effort turn can take minutes).
//! 429, 529 and 5xx are retried up to 3 times, after the server's retry-after when it gives one.

use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

const KEY: &str = "anthropic_api_key";
const URL: &str = "https://api.anthropic.com/v1/messages";
const VERSION: &str = "2023-06-01";

fn agent() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| {
        ureq::AgentBuilder::new()
            .tls_connector(std::sync::Arc::new(native_tls::TlsConnector::new().expect("TLS")))
            .timeout(Duration::from_secs(600))
            .user_agent("stable-alm/0.1")
            .build()
    })
}

fn key() -> Option<String> {
    crate::signer::load_secret(KEY).map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

pub fn has_key() -> bool {
    key().is_some()
}

/// Store the key after checking it (empty: remove it).
pub fn set_key(k: &str) -> Result<(), String> {
    let k = k.trim();
    if k.is_empty() {
        crate::signer::delete_secret(KEY);
        return Ok(());
    }
    if !k.starts_with("sk-ant-") || k.len() > 300 || k.chars().any(|c| c.is_whitespace()) {
        return Err("that does not look like an Anthropic API key (sk-ant-…)".into());
    }
    test_with(k)?;
    crate::signer::save_secret(KEY, k)
}

/// Check the stored key: lists the models (free).
pub fn test_key() -> Result<Value, String> {
    let k = key().ok_or("no API key set")?;
    test_with(&k)
}

fn test_with(k: &str) -> Result<Value, String> {
    match agent().get("https://api.anthropic.com/v1/models?limit=20").set("x-api-key", k).set("anthropic-version", VERSION).call() {
        Ok(r) => {
            let v: Value = r.into_json().map_err(|e| e.to_string())?;
            let ids: Vec<Value> = v["data"].as_array().into_iter().flatten().map(|m| m["id"].clone()).collect();
            Ok(json!({"ok": true, "models": ids}))
        }
        Err(ureq::Error::Status(c, r)) => Err(format!("the key was refused (HTTP {c}: {})", err_msg(r))),
        Err(_) => Err("api.anthropic.com is not reachable".into()),
    }
}

fn err_msg(r: ureq::Response) -> String {
    let v: Value = r.into_json().unwrap_or(Value::Null);
    v["error"]["message"].as_str().unwrap_or("").chars().take(300).collect()
}

#[derive(Debug)]
pub struct ApiError {
    pub status: Option<u16>,
    pub message: String,
    /// true when the request may have been billed (it was sent and no answer came back)
    pub billed: bool,
}

/// One Messages API call; returns the response body.
pub fn call(body: &Value) -> Result<Value, ApiError> {
    let k = key().ok_or(ApiError { status: None, message: "no API key set".into(), billed: false })?;
    let mut tries = 0;
    loop {
        tries += 1;
        let r = agent().post(URL).set("x-api-key", &k).set("anthropic-version", VERSION).set("content-type", "application/json").send_json(body.clone());
        match r {
            Ok(r) => return r.into_json().map_err(|e| ApiError { status: Some(200), message: e.to_string(), billed: true }),
            Err(ureq::Error::Status(c, r)) if (c == 429 || c == 529 || c >= 500) && tries < 3 => {
                let wait = r.header("retry-after").and_then(|s| s.parse::<u64>().ok()).unwrap_or(10 * tries as u64).min(120);
                crate::warn!("agent", "Claude API HTTP {c}; retrying in {wait} s");
                std::thread::sleep(Duration::from_secs(wait));
            }
            Err(ureq::Error::Status(c, r)) => return Err(ApiError { status: Some(c), message: err_msg(r), billed: false }),
            Err(e) if tries < 3 => {
                crate::warn!("agent", "Claude API: {}; retrying", e.kind());
                std::thread::sleep(Duration::from_secs(10));
            }
            Err(e) => return Err(ApiError { status: None, message: format!("not reachable ({})", e.kind()), billed: true }),
        }
    }
}
