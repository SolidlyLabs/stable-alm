//! The decision record (plan section 3), written by code: every review, plan transition, risk
//! action, approval and strategy change, in data/agent/journal.jsonl. Each record carries the hash
//! of the one before it, so an edit or a deletion anywhere breaks the chain (`verify`).

use serde_json::{json, Value};
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());
const FILE: &str = "journal.jsonl";

fn hash_of(r: &Value) -> String {
    // the record without its own hash, as written (serde_json keeps the insertion order)
    let mut c = r.clone();
    if let Some(o) = c.as_object_mut() {
        o.remove("hash");
    }
    super::sha256_hex(&c.to_string())
}

/// Append a record; returns its sequence number.
pub fn record(kind: &str, data: Value) -> u64 {
    let _g = LOCK.lock().unwrap();
    let last = super::tail(FILE, 1).pop();
    let seq = last.as_ref().and_then(|r| r["seq"].as_u64()).map(|s| s + 1).unwrap_or(1);
    let prev = last.as_ref().and_then(|r| r["hash"].as_str().map(String::from)).unwrap_or_else(|| "0".repeat(64));
    let mut r = json!({"seq": seq, "ts": super::now(), "kind": kind, "data": data, "prev": prev});
    let h = hash_of(&r);
    r["hash"] = json!(h);
    super::append(FILE, &r);
    seq
}

/// The last `n` records, newest last, optionally of some kinds only.
pub fn recent(n: usize, kinds: &[&str]) -> Vec<Value> {
    let all = super::tail(FILE, 20_000);
    let f: Vec<Value> = all.into_iter().filter(|r| kinds.is_empty() || kinds.contains(&r["kind"].as_str().unwrap_or(""))).collect();
    f[f.len().saturating_sub(n)..].to_vec()
}

/// Check the whole chain: Ok(records) or Err(the first broken sequence number and why).
pub fn verify() -> Result<u64, String> {
    let all = super::tail(FILE, usize::MAX);
    verify_rows(&all)
}

pub fn verify_rows(all: &[Value]) -> Result<u64, String> {
    let mut prev = "0".repeat(64);
    for (i, r) in all.iter().enumerate() {
        let seq = r["seq"].as_u64().unwrap_or(0);
        if seq != i as u64 + 1 {
            return Err(format!("record {} has sequence {seq}", i + 1));
        }
        if r["prev"].as_str() != Some(prev.as_str()) {
            return Err(format!("record {seq} does not follow the one before it"));
        }
        let h = hash_of(r);
        if r["hash"].as_str() != Some(h.as_str()) {
            return Err(format!("record {seq} was changed after it was written"));
        }
        prev = h;
    }
    Ok(all.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(n: usize) -> Vec<Value> {
        let mut out: Vec<Value> = vec![];
        let mut prev = "0".repeat(64);
        for i in 0..n {
            let mut r = json!({"seq": i + 1, "ts": 100 + i, "kind": "x", "data": {"i": i}, "prev": prev});
            let h = hash_of(&r);
            r["hash"] = json!(h.clone());
            prev = h;
            out.push(r);
        }
        out
    }

    #[test]
    fn an_edit_breaks_the_chain() {
        let mut c = chain(5);
        assert_eq!(verify_rows(&c), Ok(5));
        c[2]["data"]["i"] = json!(99);
        assert!(verify_rows(&c).unwrap_err().contains("record 3"));
        let mut c = chain(5);
        c.remove(1);
        assert!(verify_rows(&c).is_err());
    }
}
