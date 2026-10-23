//! Chain access for the executor: reads pinned past our last mined
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

const HINTS: [(&str, &str); 15] = [
    ("AL", "amounts below the minimums: the price moved (slippage)"),
    ("Price slippage check", "pool price moved past the min amounts (slippage)"),
    ("STF", "token transferFrom failed (balance or allowance too low)"),
    ("TF", "token transfer failed (for a mint: the wallet holds less than it pulls)"),
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
        // a short code (TF, AL, LOK…) is the whole revert string, never a part of another word
        let hit = if k.len() <= 3 { text.trim() == k || msg.trim_end().ends_with(&format!("reverted: {k}")) } else { text.contains(k) || msg.contains(k) };
        if hit {
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

/// A node behind the block the read is pinned to (a load-balanced RPC can route to one): wait and ask
/// again, it is not the call failing. Nodes word it differently.
fn unknown_block(e: &Value) -> bool {
    let m = e.get("message").and_then(|m| m.as_str()).unwrap_or("").to_lowercase();
    m.contains("unknown block") || m.contains("header not found") || m.contains("unsupported block number")
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

/// The pool's active (in-range) liquidity now.
pub fn pool_liquidity(cid: u64, pool: &str) -> Result<BigUint, String> {
    let r = eth_call(cid, pool, &calldata("liquidity()", &[]), None).map_err(|e| e.message)?;
    Ok(eth::word_u(&r, 0))
}

/// Calldata of positions(bytes32) for the owner's range.
pub fn position_calldata(owner: &str, tl: i64, tu: i64) -> String {
    let mut pre = eth::unhex(owner).unwrap_or_default();
    for t in [tl, tu] {
        let b = (t as i32).to_be_bytes();
        pre.extend_from_slice(&b[1..]);
    }
    calldata("positions(bytes32)", &[Tok::B32(eth::keccak(&pre))])
}

pub fn position_liquidity(cid: u64, pool: &str, owner: &str, tl: i64, tu: i64) -> Result<BigUint, String> {
    let r = eth_call(cid, pool, &position_calldata(owner, tl, tu), None).map_err(|e| e.message)?;
    Ok(eth::word_u(&r, 0))
}

/// Several eth_calls at the same (pinned) block, sent as one Multicall3 request by rpc::batch.
pub fn eth_calls(cid: u64, calls: &[(String, String)]) -> Result<Vec<Result<Vec<u8>, String>>, String> {
    if calls.is_empty() {
        return Ok(vec![]);
    }
    for i in 0..15 {
        let t = tag(cid);
        let reqs: Vec<(&str, Value)> = calls.iter().map(|(to, d)| ("eth_call", json!([{"to": to, "data": d}, t]))).collect();
        let res = rpc::batch(cid, &reqs)?;
        if res.iter().any(|r| matches!(r, Err(e) if unknown_block(e))) {
            with_pin(cid, |p| p.at = None);
            std::thread::sleep(Duration::from_millis(1000 + i * 500));
            continue;
        }
        return Ok(res
            .into_iter()
            .map(|r| match r {
                Ok(v) => eth::unhex(v.as_str().unwrap_or("0x")),
                Err(e) => Err(format!("eth_call reverted: {}", revert_reason(&e))),
            })
            .collect());
    }
    Err("eth_call: no RPC node reached the pinned block in time".into())
}

/// balanceOf for several tokens in one request.
pub fn erc20_balances(cid: u64, tokens: &[String], owner: &str) -> Result<Vec<Result<BigUint, String>>, String> {
    let data = calldata("balanceOf(address)", &[eth::addr(owner)]);
    let calls: Vec<(String, String)> = tokens.iter().map(|t| (t.clone(), data.clone())).collect();
    Ok(eth_calls(cid, &calls)?.into_iter().map(|r| r.map(|b| BigUint::from_bytes_be(&b))).collect())
}

/// A pool's immutable facts (token0, token1, tick spacing), read once with one request and cached.
static POOL_INFO: std::sync::Mutex<Option<std::collections::HashMap<(u64, String), (String, String, i64)>>> = std::sync::Mutex::new(None);

pub fn pool_info(cid: u64, pool: &str) -> Result<(String, String, i64), String> {
    let key = (cid, pool.to_lowercase());
    if let Some(v) = POOL_INFO.lock().unwrap().get_or_insert_with(Default::default).get(&key) {
        return Ok(v.clone());
    }
    let sk = format!("pool:{cid}:{}", key.1);
    if let Some(v) = crate::store::get(&sk) {
        if let (Some(a), Some(b), Some(c)) = (v["token0"].as_str(), v["token1"].as_str(), v["tick_spacing"].as_i64()) {
            let v = (a.to_string(), b.to_string(), c);
            POOL_INFO.lock().unwrap().as_mut().unwrap().insert(key, v.clone());
            return Ok(v);
        }
    }
    let calls: Vec<(String, String)> = ["token0()", "token1()", "tickSpacing()"].iter().map(|f| (key.1.clone(), calldata(f, &[]))).collect();
    let r = eth_calls(cid, &calls)?;
    let word = |i: usize| -> Result<Vec<u8>, String> {
        let b = r[i].clone()?;
        if b.len() < 32 { Err(format!("pool {} returned no data; is it a Solidly V3 pool on chain {cid}?", key.1)) } else { Ok(b) }
    };
    let (a, b, c) = (word(0)?, word(1)?, word(2)?);
    let v = (format!("0x{}", eth::hex(&a[12..32])), format!("0x{}", eth::hex(&b[12..32])), eth::word_i(&c, 0).to_i64().unwrap_or(1));
    crate::store::put(&sk, serde_json::json!({"token0": v.0, "token1": v.1, "tick_spacing": v.2}));
    POOL_INFO.lock().unwrap().as_mut().unwrap().insert(key, v.clone());
    Ok(v)
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
    let mut r = eth_calls(cid, &[(token.to_string(), calldata("decimals()", &[])), (token.to_string(), calldata("symbol()", &[]))])?;
    let sym_raw = r.pop().unwrap();
    let r = r.pop().unwrap()?;
    let dec = BigUint::from_bytes_be(&r).to_u32().unwrap_or(18);
    let sym = match sym_raw {
        Ok(raw) if raw.len() > 32 => eth::decode_string(&raw).unwrap_or_else(|| token[..8].to_string()),
        Ok(raw) => String::from_utf8(raw.into_iter().filter(|b| *b != 0).collect()).unwrap_or_else(|_| token[..8].into()),
        Err(_) => token[..8].to_string(),
    };
    Ok((dec, sym))
}

pub fn pool_tokens(cid: u64, pool: &str) -> Result<(String, String), String> {
    pool_info(cid, pool).map(|(a, b, _)| (a, b))
}

pub fn tick_spacing(cid: u64, pool: &str) -> Result<i64, String> {
    pool_info(cid, pool).map(|x| x.2)
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

/// The token transfers a call would make, from eth_simulateV1 with traced transfers on the send RPC.
/// None when the node does not offer it; Some(Err) when the call reverts in the simulation.
pub fn simulate_transfers(cid: u64, from: &str, to: &str, data: &str) -> Option<Result<Vec<crate::prices::Transfer>, String>> {
    let params = json!([{"blockStateCalls": [{"calls": [{"from": from, "to": to, "data": data}]}], "traceTransfers": true, "validation": false}, "latest"]);
    let r = rpc::send_call(cid, "eth_simulateV1", params).ok()?.ok()?;
    let call = r.get(0)?.get("calls")?.get(0)?.clone();
    if call["status"].as_str() != Some("0x1") {
        return Some(Err(format!("swap reverts in simulation: {}", call["error"])));
    }
    Some(Ok(crate::prices::transfers(call["logs"].as_array().map(|v| v.as_slice()).unwrap_or(&[]))))
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
    pub receipt: Value,
}

// -- pending transactions ------------------------------------------------------------------------------
// Every signed transaction is written to logs/pending_tx.json (chain, nonce, every signed copy and its
// locally computed hash) before it is broadcast, and removed once one copy is mined. If a response is
// lost, the app stops or a transaction is not mined, the record stays: until it is reconciled (a copy
// mined, or the nonce used by something else) no new transaction is sent on that chain, so an
// operation is never repeated at a fresh nonce.

static PENDING_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn pending_path() -> std::path::PathBuf {
    crate::paths::logs().join("pending_tx.json")
}

fn pending_load() -> Vec<Value> {
    std::fs::read_to_string(pending_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn pending_save(v: &[Value]) {
    let _ = std::fs::create_dir_all(crate::paths::logs());
    let tmp = pending_path().with_extension("tmp");
    if std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap()).is_ok() {
        let _ = std::fs::rename(tmp, pending_path());
    }
}

fn pending_upsert(cid: u64, from: &str, nonce: u64, hash: &str, raw: &str, to: &str) {
    let _g = PENDING_LOCK.lock().unwrap();
    let mut all = pending_load();
    match all.iter_mut().find(|p| p["chain_id"] == cid && p["nonce"] == nonce && p["from"] == from) {
        Some(p) => {
            if let Some(c) = p["copies"].as_array_mut() {
                c.push(json!({"hash": hash, "raw": raw}));
            }
        }
        None => all.push(json!({"chain_id": cid, "from": from, "nonce": nonce, "to": to, "ts": crate::engine::now(),
                                "copies": [{"hash": hash, "raw": raw}]})),
    }
    pending_save(&all);
}

fn pending_clear(cid: u64, from: &str, nonce: u64) {
    let _g = PENDING_LOCK.lock().unwrap();
    let mut all = pending_load();
    all.retain(|p| !(p["chain_id"] == cid && p["nonce"] == nonce && p["from"] == from));
    pending_save(&all);
}

/// Pending transactions waiting to be reconciled (for the dashboard).
pub fn pending_list() -> Vec<Value> {
    pending_load().into_iter().map(|p| json!({"chain_id": p["chain_id"], "nonce": p["nonce"], "to": p["to"], "ts": p["ts"],
                                                "hashes": p["copies"].as_array().map(|c| c.iter().map(|x| x["hash"].clone()).collect::<Vec<_>>())})).collect()
}

/// Resolve stored pending transactions of `from` on a chain. Ok(()) when none is left unresolved;
/// Err(reason) while one is still outstanding (it is re-broadcast unchanged, never replaced).
pub fn reconcile_pending(cid: u64, from: &str) -> Result<(), String> {
    let from = from.to_lowercase();
    let mine: Vec<Value> = pending_load().into_iter().filter(|p| p["chain_id"] == cid && p["from"] == from.as_str()).collect();
    for p in mine {
        let nonce = p["nonce"].as_u64().unwrap_or(0);
        let copies = p["copies"].as_array().cloned().unwrap_or_default();
        if let Some(rc) = copies.iter().find_map(|c| receipt(cid, c["hash"].as_str().unwrap_or(""))) {
            crate::warn!("chain", "reconciled pending nonce {nonce} on chain {cid}: mined in block {} (status {})",
                         hex_u64(&rc["blockNumber"]), rc["status"].as_str().unwrap_or("?"));
            crate::engine::log(json!({"chain_id": cid, "kind": "reconciled", "nonce": nonce, "hash": rc["transactionHash"], "status": rc["status"],
                                      "to": p["to"]}));
            pending_clear(cid, &from, nonce);
            continue;
        }
        let mined = hex_u64(&rpc::send_call(cid, "eth_getTransactionCount", json!([from, "latest"]))?.map_err(|e| e.to_string())?);
        if mined > nonce {
            crate::warn!("chain", "pending nonce {nonce} on chain {cid} was used by another transaction; clearing it");
            crate::engine::log(json!({"chain_id": cid, "kind": "reconciled", "nonce": nonce, "status": "replaced", "to": p["to"]}));
            pending_clear(cid, &from, nonce);
            continue;
        }
        if let Some(c) = copies.last() {
            let _ = rpc::send_call(cid, "eth_sendRawTransaction", json!([c["raw"]]));
        }
        return Err(format!("transaction nonce {nonce} sent earlier is not mined yet (re-broadcast unchanged); no new transactions on this chain until it is"));
    }
    Ok(())
}

/// How a chain's transactions are priced (Settings, Chains & RPC). Normal is the node's suggested
/// priority fee (tip); low and instant take the median, over the last 20 blocks, of the 10th and
/// 90th percentile tip paid, kept at or below and at or above normal. The fee cap and the wait
/// before a +25% fee bump follow the speed too.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum GasSpeed {
    Low,
    #[default]
    Normal,
    Instant,
}

impl GasSpeed {
    pub const ALL: [GasSpeed; 3] = [GasSpeed::Low, GasSpeed::Normal, GasSpeed::Instant];
    pub fn parse(s: &str) -> Option<GasSpeed> {
        match s.trim().to_lowercase().as_str() {
            "low" => Some(GasSpeed::Low),
            "normal" | "" => Some(GasSpeed::Normal),
            "instant" => Some(GasSpeed::Instant),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            GasSpeed::Low => "low",
            GasSpeed::Normal => "normal",
            GasSpeed::Instant => "instant",
        }
    }
    /// The fee cap: base fee × num / den + tip (headroom for the base fee rising before inclusion).
    fn base_mult(self) -> (u8, u8) {
        match self {
            GasSpeed::Low => (5, 4),
            GasSpeed::Normal => (2, 1),
            GasSpeed::Instant => (3, 1),
        }
    }
    /// How long a copy may go unmined before it is re-sent with fees +25%.
    pub fn bump_after(self) -> Duration {
        Duration::from_secs(match self {
            GasSpeed::Low => 180,
            GasSpeed::Normal => 90,
            GasSpeed::Instant => 45,
        })
    }
}

/// Tips for low, normal and instant, in wei: normal is the node's suggestion (at least 0.001 gwei,
/// as before); low and instant come from `eth_feeHistory` rewards at the 10th and 90th percentile
/// (median over the blocks). Without a history, low is half of normal and instant twice it.
pub fn tips(node_tip: &BigUint, history: Option<&Value>) -> [BigUint; 3] {
    let normal = node_tip.max(&BigUint::from(1_000_000u64)).clone();
    let median = |i: usize| -> Option<BigUint> {
        let mut v: Vec<BigUint> = history?["reward"].as_array()?.iter().filter_map(|r| r[i].as_str().map(eth::parse_uint)).collect();
        if v.is_empty() {
            return None;
        }
        v.sort();
        Some(v[v.len() / 2].clone())
    };
    let low = median(0).unwrap_or_else(|| &normal / 2u8).min(normal.clone()).max(BigUint::from(1_000u64));
    let instant = median(1).unwrap_or_default().max(&normal * 2u8);
    [low, normal, instant]
}

/// The current base fee and the three tips (low, normal, instant) of a chain.
pub fn gas_quote(cid: u64) -> Result<(BigUint, [BigUint; 3]), String> {
    let blk = rpc::call_ok(cid, "eth_getBlockByNumber", json!(["latest", false]))?;
    let base = eth::parse_uint(blk["baseFeePerGas"].as_str().unwrap_or("0x0"));
    let node = eth::parse_uint(rpc::call_ok(cid, "eth_maxPriorityFeePerGas", json!([]))?.as_str().unwrap_or("0x0"));
    let hist = rpc::call_ok(cid, "eth_feeHistory", json!(["0x14", "latest", [10, 90]])).ok();
    Ok((base, tips(&node, hist.as_ref())))
}

/// Tip and fee cap of a speed, given the base fee and the three tips.
pub fn fees_for(speed: GasSpeed, base: &BigUint, tips: &[BigUint; 3]) -> (BigUint, BigUint) {
    let tip = tips[speed as usize].clone();
    let (n, d) = speed.base_mult();
    let max_fee = base * n / d + &tip;
    (tip, max_fee)
}

/// The dashboard's gas preview of a chain: the base fee, and for each speed its tip, fee cap and
/// bump wait, in gwei.
pub fn gas_preview(cid: u64) -> Result<Value, String> {
    let (base, t) = gas_quote(cid)?;
    let gwei = |x: &BigUint| x.to_f64().unwrap_or(0.0) / 1e9;
    let mut levels = serde_json::Map::new();
    for sp in GasSpeed::ALL {
        let (tip, max_fee) = fees_for(sp, &base, &t);
        levels.insert(sp.name().into(), json!({"tip_gwei": gwei(&tip), "max_fee_gwei": gwei(&max_fee), "bump_after_s": sp.bump_after().as_secs()}));
    }
    Ok(json!({"chain_id": cid, "base_gwei": gwei(&base), "speed": crate::chains::get(cid).gas_speed, "levels": levels}))
}

/// Sign and broadcast at the chain's gas speed; re-broadcast with fees +25% (same nonce) when not
/// mined within the speed's wait (low 180 s, normal 90 s, instant 45 s), up to 3 times. Returns once any copy is mined; Err if it reverted. Refuses while execution is paused, and
/// while an earlier transaction on the chain is unresolved.
pub fn send(cid: u64, acct: &Account, to: &str, data: &str, gas: u64) -> Result<Sent, TxError> {
    if let Some(why) = crate::paths::execution_blocked() {
        return Err(err(format!("not sent: {why}"), "paused"));
    }
    reconcile_pending(cid, &acct.address).map_err(|e| err(e, "pending"))?;
    let nonce = hex_u64(&rpc::send_call(cid, "eth_getTransactionCount", json!([acct.address, "pending"]))?.map_err(|e| err(e.to_string(), ""))?);
    let from = acct.address.to_lowercase();
    let speed = GasSpeed::parse(&crate::chains::get(cid).gas_speed).unwrap_or_default();
    let (base, t) = gas_quote(cid)?;
    let (mut tip, mut max_fee) = fees_for(speed, &base, &t);
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
        let signed = tx.sign(acct);
        let raw = format!("0x{}", eth::hex(&signed));
        let local = format!("0x{}", eth::hex(&eth::keccak(&signed)));
        pending_upsert(cid, &from, nonce, &local, &raw, to);
        if !hashes.contains(&local) {
            hashes.push(local.clone());
        }
        let res = match rpc::send_call(cid, "eth_sendRawTransaction", json!([raw])) {
            Ok(r) => r,
            Err(e) => {
                // The node may have accepted it before the response was lost: keep the record and
                // look for a receipt instead of assuming it failed.
                crate::warn!("chain", "broadcast of {local} (nonce {nonce}) had no response: {e}");
                Ok(json!(local))
            }
        };
        match res {
            Ok(h) => {
                let h = h.as_str().unwrap_or("").to_lowercase();
                crate::info!("chain", "broadcast {h} nonce {nonce} (attempt {}, {} gas)", attempt + 1, speed.name());
                if !h.is_empty() && !hashes.contains(&h) {
                    hashes.push(h);
                }
            }
            Err(e) => {
                let m = revert_reason(&e);
                let low = m.to_lowercase();
                if low.contains("nonce too low") || low.contains("already known") {
                } else if attempt == 0 {
                    // rejected outright: this copy was never accepted, so nothing is outstanding
                    pending_clear(cid, &from, nonce);
                    return Err(err(format!("send rejected: {m}"), m));
                } else {
                    crate::warn!("chain", "replacement rejected: {m}");
                }
            }
        }
        let t0 = Instant::now();
        while t0.elapsed() < speed.bump_after() {
            for h in &hashes {
                if let Some(rc) = receipt(cid, h) {
                    pending_clear(cid, &from, nonce);
                    if rc["status"].as_str() != Some("0x1") {
                        return Err(err(format!("transaction {h} reverted on-chain"), "reverted"));
                    }
                    let block = hex_u64(&rc["blockNumber"]);
                    note_mined(cid, block);
                    crate::ledger::invalidate();
                    return Ok(Sent { hash: h.clone(), gas_used: hex_u64(&rc["gasUsed"]), block, receipt: rc });
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        tip = &tip * 5u8 / 4u8 + 1u8;
        max_fee = &max_fee * 5u8 / 4u8 + 1u8;
        crate::warn!("chain", "not mined in {}s ({} gas); re-broadcasting with higher fees", speed.bump_after().as_secs(), speed.name());
    }
    Err(err(format!("transaction (nonce {nonce}) not mined after 3 fee bumps: {hashes:?}; kept as pending, no new transactions on this chain until it resolves"), "not mined"))
}

/// The emergency path's replacement of a stuck transaction (plan section 4): a zero-value transfer
/// to itself at the same nonce, with fees well above what the stuck copies paid (four times the
/// current base fee and tip), so the nonce is freed and the emergency's own transactions can follow.
/// Returns the replacement's hash; it is recorded with the pending copies, so whichever is mined
/// resolves the nonce as before.
pub fn replace_stuck(cid: u64, acct: &Account, max_fee_gwei: f64) -> Result<Option<String>, String> {
    let from = acct.address.to_lowercase();
    let Some(p) = pending_load().into_iter().find(|p| p["chain_id"] == cid && p["from"] == from.as_str()) else { return Ok(None) };
    let nonce = p["nonce"].as_u64().unwrap_or(0);
    let blk = rpc::call_ok(cid, "eth_getBlockByNumber", json!(["latest", false]))?;
    let base = eth::parse_uint(blk["baseFeePerGas"].as_str().unwrap_or("0x0"));
    let tip0 = eth::parse_uint(rpc::call_ok(cid, "eth_maxPriorityFeePerGas", json!([]))?.as_str().unwrap_or("0x0"));
    let tip = tip0.max(BigUint::from(1_000_000u64)) * 4u8;
    let mut max_fee = &base * 4u8 + &tip;
    let cap = BigUint::from((max_fee_gwei * 1e9) as u128);
    if max_fee > cap {
        max_fee = cap;
    }
    if max_fee < tip {
        return Err(format!("the emergency gas limit ({max_fee_gwei} gwei) is below the tip needed"));
    }
    let tx = eth::Tx1559 { chain_id: cid, nonce, max_priority_fee: tip, max_fee, gas: 21_000, to: from.clone(), data: vec![] };
    let signed = tx.sign(acct);
    let raw = format!("0x{}", eth::hex(&signed));
    let local = format!("0x{}", eth::hex(&eth::keccak(&signed)));
    pending_upsert(cid, &from, nonce, &local, &raw, &from);
    crate::warn!("chain", "replacing stuck nonce {nonce} on chain {cid} with a self-transfer {local}");
    crate::engine::log(json!({"chain_id": cid, "kind": "replace_stuck", "nonce": nonce, "hash": local}));
    match rpc::send_call(cid, "eth_sendRawTransaction", json!([raw])) {
        Ok(Ok(_)) => Ok(Some(local)),
        Ok(Err(e)) => Err(format!("replacement rejected: {}", revert_reason(&e))),
        Err(e) => Err(format!("replacement not broadcast: {e}")),
    }
}

/// The oldest pending transaction's age on a chain (seconds), if any.
pub fn pending_age(cid: u64, from: &str) -> Option<i64> {
    let from = from.to_lowercase();
    pending_load().into_iter().filter(|p| p["chain_id"] == cid && p["from"] == from.as_str())
        .filter_map(|p| p["ts"].as_i64()).min().map(|t| crate::engine::now() as i64 - t)
}

pub fn int(v: i64) -> Tok {
    Tok::Int(BigInt::from(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_behind_the_pinned_block_is_waited_for() {
        for m in ["unknown block", "header not found", "execution reverted: unsupported block number", "Unsupported block number 52954619"] {
            assert!(unknown_block(&json!({"code": -32000, "message": m})), "{m}");
        }
        assert!(!unknown_block(&json!({"code": 3, "message": "execution reverted: AL"})));
    }

    #[test]
    fn short_revert_codes_match_only_themselves() {
        assert!(revert_reason(&json!({"message": "execution reverted: AL"})).contains("slippage"));
        assert!(revert_reason(&json!({"message": "execution reverted: TF"})).contains("token transfer failed"));
        assert!(revert_reason(&json!({"message": "execution reverted: STF"})).contains("transferFrom"));
        // a longer message that merely contains the letters gets no code's hint
        let r = revert_reason(&json!({"message": "execution reverted: INSUFFICIENT_ALLOWANCE"}));
        assert!(!r.contains("slippage") && !r.contains("token transfer failed"), "{r}");
    }

    #[test]
    fn gas_speeds_order_their_tips_and_fee_caps() {
        let w = |x: u64| BigUint::from(x);
        // node suggests 0.002 gwei; the last blocks paid 0.0005 gwei (10th pct) and 0.03 gwei (90th)
        let hist = json!({"reward": [["0x7a120", "0x1c9c380"], ["0x7a120", "0x1c9c380"], ["0x0", "0x0"]]});
        let t = tips(&w(2_000_000), Some(&hist));
        assert_eq!(t, [w(500_000), w(2_000_000), w(30_000_000)]);
        // normal keeps today's floor of 0.001 gwei; low never exceeds normal, instant is at least twice it
        let t = tips(&w(0), Some(&json!({"reward": [["0x77359400", "0x1"]]})));
        assert_eq!(t, [w(1_000_000), w(1_000_000), w(2_000_000)]);
        // no fee history: half and twice normal
        assert_eq!(tips(&w(4_000_000), None), [w(2_000_000), w(4_000_000), w(8_000_000)]);
        let base = w(10_000_000);
        let t = [w(1_000), w(2_000), w(3_000)];
        assert_eq!(fees_for(GasSpeed::Low, &base, &t), (w(1_000), w(12_501_000)));
        assert_eq!(fees_for(GasSpeed::Normal, &base, &t), (w(2_000), w(20_002_000)));
        assert_eq!(fees_for(GasSpeed::Instant, &base, &t), (w(3_000), w(30_003_000)));
        assert!(GasSpeed::Low.bump_after() > GasSpeed::Normal.bump_after() && GasSpeed::Normal.bump_after() > GasSpeed::Instant.bump_after());
        assert_eq!(GasSpeed::parse(""), Some(GasSpeed::Normal));
        assert_eq!(GasSpeed::parse("Instant"), Some(GasSpeed::Instant));
        assert_eq!(GasSpeed::parse("fast"), None);
    }
}
