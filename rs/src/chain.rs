//! Chain access for the executor (alm/executor/chain.py): reads pinned past our last mined
//! transaction, simulation (eth_call + estimateGas), EIP-1559 signing and sending with fee bumps.

use crate::eth::{self, calldata, Account, Tok};
use crate::rpc::{self, hex_u64};
use num_bigint::{BigInt, BigUint};
use num_traits::ToPrimitive;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub fn max_u128() -> BigUint {
    (BigUint::from(1u8) << 128) - 1u8
}

#[derive(Debug, Clone)]
pub struct TxError {
    pub message: String,
    pub reason: String,
}

impl std::fmt::Display for TxError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub fn err(message: impl Into<String>, reason: impl Into<String>) -> TxError {
    TxError { message: message.into(), reason: reason.into() }
}

impl From<String> for TxError {
    fn from(s: String) -> TxError {
        err(s, "")
    }
}

const HINTS: [(&str, &str); 14] = [
    ("Price slippage check", "pool price moved past the min amounts (slippage)"),
    ("STF", "token transferFrom failed (balance or allowance too low)"),
    ("TF", "token transfer failed"),
    ("LOK", "pool is locked (reentrancy guard)"),
    ("TLU", "tickLower must be below tickUpper"),
    ("TLM", "tickLower below the minimum tick"),
    ("TUM", "tickUpper above the maximum tick"),
    ("AS", "swap amount is zero"),
    ("SPL", "price limit is outside the allowed range"),
    ("Transaction too old", "deadline passed"),
    ("EXPIRED", "deadline passed"),
    ("Too little received", "aggregator min-out not met (price moved)"),
    ("Return amount is not enough", "aggregator min-out not met (price moved)"),
    ("insufficient funds", "not enough ETH for gas"),
];

pub fn revert_reason(e: &Value) -> String {
    let Some(o) = e.as_object() else {
        return e.as_str().map(String::from).unwrap_or_else(|| e.to_string());
    };
    let msg = o.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
    let mut data = o.get("data").cloned().unwrap_or(Value::Null);
    if data.is_object() {
        data = data.get("data").or_else(|| data.get("result")).cloned().unwrap_or(Value::Null);
    }
    let mut text = msg.clone();
    if let Some(d) = data.as_str() {
        if d.starts_with("0x08c379a0") && d.len() >= 138 {
            if let Ok(b) = eth::unhex(&d[10..]) {
                if let Some(s) = eth::decode_string(&b) {
                    text = s;
                }
            }
        } else if d.starts_with("0x4e487b71") {
            text = format!("panic 0x{}", d[10..].trim_start_matches('0'));
        }
    }
    for (k, hint) in HINTS {
        if text.contains(k) || msg.contains(k) {
            return format!("{text} ({hint})");
        }
    }
    if text.is_empty() {
        e.to_string()
    } else {
        text
    }
}

// -- pinned reads -------------------------------------------------------------------------------------
#[derive(Default)]
struct Pin {
    floor: u64,
    head: u64,
    at: Option<Instant>,
}
static PINS: Mutex<Option<std::collections::HashMap<u64, Pin>>> = Mutex::new(None);

fn with_pin<T>(cid: u64, f: impl FnOnce(&mut Pin) -> T) -> T {
    let mut g = PINS.lock().unwrap();
    f(g.get_or_insert_with(Default::default).entry(cid).or_default())
}

fn tag(cid: u64) -> String {
    let (floor, stale) = with_pin(cid, |p| (p.floor, p.at.map(|t| t.elapsed() > Duration::from_secs(1)).unwrap_or(true)));
    if floor == 0 {
        return "latest".into();
    }
    if stale {
        if let Ok(h) = rpc::block_number(cid) {
            with_pin(cid, |p| {
                p.head = h;
                p.at = Some(Instant::now());
            });
        }
    }
    with_pin(cid, |p| format!("0x{:x}", p.head.max(p.floor)))
}

fn unknown_block(e: &Value) -> bool {
    let m = e.get("message").and_then(|m| m.as_str()).unwrap_or("").to_lowercase();
    m.contains("unknown block") || m.contains("header not found")
}

fn read(cid: u64, method: &str, params: impl Fn(&str) -> Value) -> Result<rpc::R, String> {
    for i in 0..15 {
        let r = rpc::call(cid, method, params(&tag(cid)))?;
        match &r {
            Err(e) if unknown_block(e) => {
                with_pin(cid, |p| p.at = None);
                std::thread::sleep(Duration::from_millis(1000 + i * 500));
            }
            _ => return Ok(r),
        }
    }
    Err(format!("{method}: no RPC node reached the pinned block in time"))
}

pub fn note_mined(cid: u64, block: u64) {
    with_pin(cid, |p| p.floor = p.floor.max(block));
}

pub fn eth_call(cid: u64, to: &str, data: &str, from: Option<&str>) -> Result<Vec<u8>, TxError> {
    let mut tx = json!({"to": to, "data": data});
    if let Some(f) = from {
        tx["from"] = json!(f);
    }
    match read(cid, "eth_call", |t| json!([tx, t]))? {
        Ok(v) => eth::unhex(v.as_str().unwrap_or("0x")).map_err(TxError::from),
        Err(e) => {
            let r = revert_reason(&e);
            Err(err(format!("eth_call reverted: {r}"), r))
        }
    }
}

pub fn slot0(cid: u64, pool: &str) -> Result<(BigUint, i64, u64), String> {
    let r = eth_call(cid, pool, &calldata("slot0()", &[]), None).map_err(|e| e.message)?;
    Ok((eth::word_u(&r, 0), eth::word_i(&r, 1).to_i64().unwrap(), eth::word_u(&r, 2).to_u64().unwrap()))
}

pub fn position_liquidity(cid: u64, pool: &str, owner: &str, tl: i64, tu: i64) -> Result<BigUint, String> {
    let mut pre = eth::unhex(owner).map_err(|e| e.to_string())?;
    for t in [tl, tu] {
        let b = (t as i32).to_be_bytes();
        pre.extend_from_slice(&b[1..]);
    }
    let key = eth::keccak(&pre);
    let r = eth_call(cid, pool, &calldata("positions(bytes32)", &[Tok::B32(key)]), None).map_err(|e| e.message)?;
    Ok(eth::word_u(&r, 0))
}

pub fn erc20_balance(cid: u64, token: &str, owner: &str) -> Result<BigUint, String> {
    let r = eth_call(cid, token, &calldata("balanceOf(address)", &[eth::addr(owner)]), None).map_err(|e| e.message)?;
    Ok(BigUint::from_bytes_be(&r))
}

pub fn erc20_allowance(cid: u64, token: &str, owner: &str, spender: &str) -> Result<BigUint, String> {
    let r = eth_call(cid, token, &calldata("allowance(address,address)", &[eth::addr(owner), eth::addr(spender)]), None)
        .map_err(|e| e.message)?;
    Ok(BigUint::from_bytes_be(&r))
}

pub fn erc20_meta(cid: u64, token: &str) -> Result<(u32, String), String> {
    let r = eth_call(cid, token, &calldata("decimals()", &[]), None).map_err(|e| e.message)?;
    let dec = BigUint::from_bytes_be(&r).to_u32().unwrap_or(18);
    let sym = match eth_call(cid, token, &calldata("symbol()", &[]), None) {
        Ok(raw) if raw.len() > 32 => eth::decode_string(&raw).unwrap_or_else(|| token[..8].to_string()),
        Ok(raw) => String::from_utf8(raw.into_iter().filter(|b| *b != 0).collect()).unwrap_or_else(|_| token[..8].into()),
        Err(_) => token[..8].to_string(),
    };
    Ok((dec, sym))
}

pub fn pool_tokens(cid: u64, pool: &str) -> Result<(String, String), String> {
    let a = eth_call(cid, pool, &calldata("token0()", &[]), None).map_err(|e| e.message)?;
    let b = eth_call(cid, pool, &calldata("token1()", &[]), None).map_err(|e| e.message)?;
    Ok((format!("0x{}", eth::hex(&a[12..32])), format!("0x{}", eth::hex(&b[12..32]))))
}

pub fn tick_spacing(cid: u64, pool: &str) -> Result<i64, String> {
    let r = eth_call(cid, pool, &calldata("tickSpacing()", &[]), None).map_err(|e| e.message)?;
    Ok(eth::word_i(&r, 0).to_i64().unwrap())
}

pub fn eth_balance(cid: u64, addr: &str) -> Result<BigUint, String> {
    match read(cid, "eth_getBalance", |t| json!([addr, t]))? {
        Ok(v) => Ok(eth::parse_uint(v.as_str().unwrap_or("0x0"))),
        Err(e) => Err(e.to_string()),
    }
}

// -- transactions ------------------------------------------------------------------------------------
pub fn simulate(cid: u64, from: &str, to: &str, data: &str) -> Result<u64, TxError> {
    eth_call(cid, to, data, Some(from))?;
    match read(cid, "eth_estimateGas", |t| json!([{"from": from, "to": to, "data": data}, t]))? {
        Ok(v) => Ok(hex_u64(&v)),
        Err(e) => {
            let r = revert_reason(&e);
            Err(err(format!("estimateGas failed: {r}"), r))
        }
    }
}

fn receipt(cid: u64, h: &str) -> Option<Value> {
    let ok = |r: &rpc::R| r.as_ref().ok().filter(|v| !v["blockNumber"].is_null()).cloned();
    rpc::call(cid, "eth_getTransactionReceipt", json!([h]))
        .ok()
        .and_then(|r| ok(&r))
        .or_else(|| rpc::send_call(cid, "eth_getTransactionReceipt", json!([h])).ok().and_then(|r| ok(&r)))
}

pub struct Sent {
    pub hash: String,
    pub gas_used: u64,
    pub block: u64,
}

/// Sign and broadcast; re-broadcast with fees +25% (same nonce) when not mined within 90 s, up to 3
/// times. Returns once any copy is mined; Err if it reverted.
pub fn send(cid: u64, acct: &Account, to: &str, data: &str, gas: u64) -> Result<Sent, TxError> {
    let nonce = hex_u64(&rpc::send_call(cid, "eth_getTransactionCount", json!([acct.address, "pending"]))?.map_err(|e| err(e.to_string(), ""))?);
    let blk = rpc::call_ok(cid, "eth_getBlockByNumber", json!(["latest", false]))?;
    let base = eth::parse_uint(blk["baseFeePerGas"].as_str().unwrap_or("0x0"));
    let tip0 = eth::parse_uint(rpc::call_ok(cid, "eth_maxPriorityFeePerGas", json!([]))?.as_str().unwrap_or("0x0"));
    let mut tip = tip0.max(BigUint::from(1_000_000u64));
    let mut max_fee = &base * 2u8 + &tip;
    let mut hashes: Vec<String> = vec![];
    let body = eth::unhex(data).map_err(TxError::from)?;
    for attempt in 0..4 {
        let tx = eth::Tx1559 {
            chain_id: cid,
            nonce,
            max_priority_fee: tip.clone(),
            max_fee: max_fee.clone(),
            gas: gas * 5 / 4 + 10_000,
            to: to.to_string(),
            data: body.clone(),
        };
        let raw = format!("0x{}", eth::hex(&tx.sign(acct)));
        match rpc::send_call(cid, "eth_sendRawTransaction", json!([raw]))? {
            Ok(h) => {
                let h = h.as_str().unwrap_or("").to_string();
                crate::info!("chain", "broadcast {h} nonce {nonce} (attempt {})", attempt + 1);
                hashes.push(h);
            }
            Err(e) => {
                let m = revert_reason(&e);
                let low = m.to_lowercase();
                if low.contains("nonce too low") || low.contains("already known") {
                } else if hashes.is_empty() {
                    return Err(err(format!("send rejected: {m}"), m));
                } else {
                    crate::warn!("chain", "replacement rejected: {m}");
                }
            }
        }
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(90) {
            for h in &hashes {
                if let Some(rc) = receipt(cid, h) {
                    if rc["status"].as_str() != Some("0x1") {
                        return Err(err(format!("transaction {h} reverted on-chain"), "reverted"));
                    }
                    let block = hex_u64(&rc["blockNumber"]);
                    note_mined(cid, block);
                    return Ok(Sent { hash: h.clone(), gas_used: hex_u64(&rc["gasUsed"]), block });
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        tip = &tip * 5u8 / 4u8 + 1u8;
        max_fee = &max_fee * 5u8 / 4u8 + 1u8;
        crate::warn!("chain", "not mined in 90s; re-broadcasting with higher fees");
    }
    Err(err(format!("transaction (nonce {nonce}) not mined after 3 fee bumps: {hashes:?}"), "not mined"))
}

pub fn int(v: i64) -> Tok {
    Tok::Int(BigInt::from(v))
}
