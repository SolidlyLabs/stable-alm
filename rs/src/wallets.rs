//! Several wallets managed at once. Each wallet is a profile: its own address and key (none: watched
//! only), pools.json (its pools, strategies and how its money is split between them), live switch,
//! funding, claims, parking, capital book, history, ledger and activity (paths.rs `scope`).
//!
//! config/wallets.json lists them: [{id, label, color}]. The wallet with the id `main` is the one the app
//! had before there were several: its files are the app's folders as they always were (only where they
//! live differs; it is a wallet like any other). Without the file the app has that one wallet.
//! A wallet's address is its settings' "wallet", like before. An address belongs to one wallet only, so
//! no two engines ever trade one wallet.

use serde_json::{json, Value};
use std::sync::Mutex;

pub use crate::paths::MAIN;

static LOCK: Mutex<()> = Mutex::new(());

const COLORS: &[&str] = &["#4d8dff", "#2fc6a0", "#f5a524", "#c77dff", "#ff6b81", "#7bd3ff", "#b5e655", "#ff9f5a"];

fn path() -> std::path::PathBuf {
    crate::paths::app_config().join("wallets.json")
}

fn read() -> Vec<Value> {
    std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok()).unwrap_or_default()
}

fn write(list: &[Value]) -> Result<(), String> {
    crate::settings::write_atomic(&path(), &Value::Array(list.to_vec())).map_err(|e| e.to_string())
}

/// A wallet id: lowercase letters, digits and '-', 1 to 24 characters (it names folders).
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 24 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !id.starts_with('-')
}

/// The wallets in their order: {id, label, color}. A removed `main` is kept in the file, marked removed.
pub fn list() -> Vec<Value> {
    let saved = read();
    let main = saved.iter().find(|w| w["id"] == MAIN).cloned().unwrap_or(json!({}));
    let mut out = vec![];
    if main["removed"] != true {
        out.push(json!({"id": MAIN, "label": main["label"].as_str().unwrap_or("Main"), "color": main["color"].as_str().unwrap_or(COLORS[0])}));
    }
    for w in saved.iter().filter(|w| w["id"] != MAIN) {
        if let Some(id) = w["id"].as_str().filter(|i| valid_id(i)) {
            out.push(json!({"id": id, "label": w["label"].as_str().unwrap_or(id), "color": w["color"].as_str().unwrap_or(COLORS[0])}));
        }
    }
    out
}

pub fn ids() -> Vec<String> {
    list().iter().filter_map(|w| w["id"].as_str().map(String::from)).collect()
}

pub fn exists(id: &str) -> bool {
    ids().iter().any(|i| i == id)
}

/// The address a wallet manages ("" when none is set yet).
pub fn address(id: &str) -> String {
    crate::paths::scope(id, || crate::settings::load_settings()["wallet"].as_str().unwrap_or("").to_lowercase())
}

/// Err when `addr` is already managed by another wallet than `id`.
pub fn check_address(id: &str, addr: &str) -> Result<(), String> {
    let addr = addr.trim().to_lowercase();
    if addr.is_empty() {
        return Ok(());
    }
    for w in list() {
        let other = w["id"].as_str().unwrap_or("");
        if other != id && address(other) == addr {
            return Err(format!("this address is already managed by the wallet \"{}\"", w["label"].as_str().unwrap_or(other)));
        }
    }
    Ok(())
}

fn label_ok(label: &str) -> Result<String, String> {
    let l = label.trim();
    if l.is_empty() || l.chars().count() > 40 {
        return Err("a wallet's name has 1 to 40 characters".into());
    }
    Ok(l.to_string())
}

/// A new wallet, watched only until a key is set: its folders, an empty pool list and settings with
/// `address` (may be empty). `copy_pools_from`: start from that wallet's pools, all switched off at 0%.
pub fn add(label: &str, address: &str, copy_pools_from: Option<&str>) -> Result<Value, String> {
    let label = label_ok(label)?;
    let address = address.trim().to_lowercase();
    if !address.is_empty() && !crate::agent::is_addr(&address) {
        return Err("the address must be 0x + 40 hex digits".into());
    }
    check_address("", &address)?;
    let pools = match copy_pools_from {
        Some(src) if !exists(src) => return Err(format!("no wallet {src}")),
        Some(src) => crate::paths::scope(src, crate::settings::load_pools)
            .into_iter()
            .map(|mut p| {
                p["enabled"] = json!(false);
                p["allocation_pct"] = json!(0.0);
                p
            })
            .collect(),
        None => vec![],
    };
    let _g = LOCK.lock().unwrap();
    let mut saved = read();
    let taken: Vec<String> = list().iter().filter_map(|w| w["id"].as_str().map(String::from)).collect();
    // ids are never reused, not even those of removed wallets (their folders are kept as <id>.removed-…)
    let archived: Vec<String> = std::fs::read_dir(crate::paths::home().join("wallets")).into_iter().flatten().flatten()
        .filter_map(|e| e.file_name().to_string_lossy().split(".removed-").next().map(String::from)).collect();
    let mut n = taken.len() + 1;
    let id = loop {
        let id = format!("w{n}");
        if !taken.contains(&id) && !archived.contains(&id) && !crate::paths::wallet_root(&id).exists() {
            break id;
        }
        n += 1;
    };
    let color = COLORS[taken.len() % COLORS.len()];
    crate::paths::scope(&id, || -> Result<(), String> {
        crate::paths::seed();
        crate::settings::save_settings(&json!({"wallet": address, "execute": false})).map_err(|e| e.to_string())?;
        crate::settings::save_pools(&pools)
    })?;
    if !saved.iter().any(|w| w["id"] == MAIN) && saved.is_empty() {
        saved.push(json!({"id": MAIN, "label": "Main", "color": COLORS[0]}));
    }
    let w = json!({"id": id, "label": label, "color": color});
    saved.push(w.clone());
    write(&saved)?;
    crate::info!("wallets", "added the wallet \"{label}\" ({id})");
    Ok(w)
}

/// The wallet that was there before several wallets existed becomes wallet 1 of the list: wallets.json
/// is written with it once, at start. Its key, pools and settings stay where they are (they are already
/// its own), so nothing else is moved or rewritten.
pub fn ensure_registry() {
    if path().exists() {
        return;
    }
    let _g = LOCK.lock().unwrap();
    if let Err(e) = write(&[json!({"id": MAIN, "label": "Main", "color": COLORS[0]})]) {
        crate::warn!("wallets", "could not write wallets.json: {e}");
    }
}

/// The address a new wallet's key (or recovery phrase account) gives, refused if another wallet has it.
/// Nothing is stored. Returns (address, private key).
pub fn key_from(body: &Value) -> Result<Option<(String, String)>, String> {
    let k = if let Some(k) = body["private_key"].as_str().filter(|k| !k.trim().is_empty()) {
        let k = k.trim();
        if k.starts_with("0x") { k.to_string() } else { format!("0x{k}") }
    } else if let Some(p) = body["phrase"].as_str().filter(|p| !p.trim().is_empty()) {
        let i = body["index"].as_u64().unwrap_or(0).min(1000) as u32;
        crate::wallet::from_phrase(p, body["passphrase"].as_str().unwrap_or(""), i)?
    } else {
        return Ok(None);
    };
    let a = crate::eth::Account::from_key(&k)?.address.to_lowercase();
    check_address("", &a)?;
    Ok(Some((a, k)))
}

/// Rename or recolour a wallet.
pub fn edit(id: &str, label: Option<&str>, color: Option<&str>) -> Result<(), String> {
    if !exists(id) {
        return Err(format!("no wallet {id}"));
    }
    let _g = LOCK.lock().unwrap();
    let mut saved = read();
    if !saved.iter().any(|w| w["id"] == id) {
        saved.insert(0, json!({"id": id}));
    }
    for w in saved.iter_mut().filter(|w| w["id"] == id) {
        if let Some(l) = label {
            w["label"] = json!(label_ok(l)?);
        }
        if let Some(c) = color {
            if !(c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|x| x.is_ascii_hexdigit())) {
                return Err("a colour is #rrggbb".into());
            }
            w["color"] = json!(c);
        }
    }
    write(&saved)
}

/// The wallet requests go to when they name none: `main` while it is listed, else the first listed.
pub fn default_id() -> String {
    ids().into_iter().next().unwrap_or_else(|| MAIN.to_string())
}

/// Remove a wallet that holds no positions and is not live; one wallet always remains. Nothing is
/// deleted: its folders (and its key) are renamed to <id>.removed-<time>, so it can be brought back by
/// hand. `main`'s files are the app's own folders, so they stay where they are and it is only marked
/// removed in wallets.json.
pub fn remove(id: &str, has_positions: bool) -> Result<(), String> {
    if !exists(id) {
        return Err(format!("no wallet {id}"));
    }
    if ids().len() < 2 {
        return Err("the only wallet cannot be removed: add another first".into());
    }
    if crate::paths::scope(id, || crate::settings::load_settings()["execute"].as_bool().unwrap_or(false)) {
        return Err("switch the wallet to dry run first".into());
    }
    if has_positions {
        return Err("the wallet still holds positions: switch its pools off and let the worker withdraw first".into());
    }
    let _g = LOCK.lock().unwrap();
    if id == MAIN {
        let mut saved = read();
        match saved.iter_mut().find(|w| w["id"] == MAIN) {
            Some(w) => w["removed"] = json!(true),
            None => saved.insert(0, json!({"id": MAIN, "label": "Main", "color": COLORS[0], "removed": true})),
        }
        write(&saved)?;
        crate::info!("wallets", "removed the wallet {id} (its files are kept where they are)");
        return Ok(());
    }
    let stamp = crate::engine::now();
    let root = crate::paths::wallet_root(id);
    let keys = crate::paths::app_secrets().join("wallets").join(id);
    for d in [root, keys] {
        if d.exists() {
            let to = d.with_file_name(format!("{id}.removed-{stamp}"));
            std::fs::rename(&d, &to).map_err(|e| format!("could not archive {}: {e}", d.display()))?;
        }
    }
    let saved: Vec<Value> = read().into_iter().filter(|w| w["id"] != id).collect();
    write(&saved)?;
    crate::info!("wallets", "removed the wallet {id} (its folders are kept as {id}.removed-{stamp})");
    Ok(())
}

/// One wallet's card for the big-picture overview, from its status block (worker), settings, signer
/// status, pools, value history (`snaps`: rows with ts and value_usd/total_usd, oldest first), queued
/// capital requests and the chains' minimum gas. No network.
#[allow(clippy::too_many_arguments)]
pub fn summary(meta: &Value, st: &Value, s: &Value, signer: &Value, pools: &[Value], snaps: &[Value], requests: usize, min_gas: &dyn Fn(u64) -> f64,
               now: i64) -> Value {
    let acct = &st["account"];
    let f = |v: &Value| v.as_f64().filter(|x| x.is_finite());
    let sum = |o: &Value, k: &str| o.as_object().map(|m| m.values().filter_map(|x| f(&x[k])).sum::<f64>()).unwrap_or(0.0);
    let deployed = sum(&acct["pools"], "deployed_usd");
    let idle = sum(&acct["tokens"], "usd");
    let value = f(&acct["value_usd"]).or(f(&acct["total_usd"]));
    let led = &st["ledger"];
    let lsum = |k: &str| led.as_object().filter(|m| m.values().any(|c| f(&c[k]).is_some())).map(|_| sum(led, k));
    let val_of = |r: &Value| f(&r["value_usd"]).or(f(&r["total_usd"]));
    // the change over a window: now against the last snapshot at or before its start
    let change = |secs: i64| -> Option<f64> {
        let v = value?;
        let then = snaps.iter().rev().find(|r| r["ts"].as_i64().unwrap_or(i64::MAX) <= now - secs).and_then(val_of)?;
        Some(v - then)
    };
    let step = (snaps.len() / 160).max(1);
    let mut hist: Vec<Value> = snaps.iter().step_by(step).filter_map(|r| Some(json!([r["ts"].as_i64()?, val_of(r)?]))).collect();
    if let Some(l) = snaps.last().filter(|_| (snaps.len() - 1) % step != 0) {
        if let (Some(t), Some(v)) = (l["ts"].as_i64(), val_of(l)) {
            hist.push(json!([t, v]));
        }
    }
    let mut attention: Vec<Value> = vec![];
    let label = meta["label"].as_str().unwrap_or("");
    let mut say = |kind: &str, text: String, pool: Option<&str>| {
        attention.push(json!({"kind": kind, "text": text, "pool": pool, "wallet": meta["id"], "label": label}))
    };
    if let Some(e) = st["last_error"].as_str() {
        say("error", e.to_string(), None);
    }
    let (mut on, mut in_range, mut with_pos) = (0, 0, 0);
    for p in pools.iter().filter(|p| p["enabled"].as_bool().unwrap_or(false)) {
        on += 1;
        let name = p["name"].as_str().unwrap_or("");
        let info = &acct["pools"][name];
        let run = &st["pools"][name];
        if run["mode"] == "error" || run["mode"] == "hold" {
            let kind = if run["mode"] == "error" { "error" } else { "hold" };
            say(kind, run["error"].as_str().unwrap_or("held").to_string(), Some(name));
        }
        if info["positions"].as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            with_pos += 1;
            if info["in_range"] == true {
                in_range += 1;
            } else {
                say("range", "out of range".into(), Some(name));
            }
        }
    }
    let mut chains = serde_json::Map::new();
    for (cid, c) in acct["chains"].as_object().cloned().unwrap_or_default() {
        let id: u64 = cid.parse().unwrap_or(0);
        let need = min_gas(id);
        let cname = c["name"].as_str().unwrap_or(&cid).to_string();
        if let Some(g) = f(&c["gas"]) {
            let used = pools.iter().any(|p| p["enabled"] == true && p["chain_id"].as_u64().unwrap_or(crate::chains::BASE) == id);
            if g < need && used {
                say("gas", format!("low gas on {cname}: {g:.5} {}", c["gas_symbol"].as_str().unwrap_or("")), None);
            }
        }
        if c["incomplete"].as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            say("hold", format!("valuation incomplete on {cname}"), None);
        }
        chains.insert(cid, json!({"name": c["name"], "total_usd": c["total_usd"], "gas": c["gas"], "gas_symbol": c["gas_symbol"], "min_gas": need}));
    }
    if requests > 0 {
        say("request", format!("{requests} capital request(s) queued"), None);
    }
    let watch = signer["stored"] != true;
    let key_addr = signer["address"].as_str().unwrap_or("").to_lowercase();
    let key = json!({"stored": !watch, "storage": signer["storage"], "error": signer["error"],
                     "matches": !watch && !key_addr.is_empty() && key_addr == s["wallet"].as_str().unwrap_or("").to_lowercase()});
    let by_pool: Vec<Value> = pools.iter().map(|p| {
        let name = p["name"].as_str().unwrap_or("");
        let info = &acct["pools"][name];
        json!({"name": name, "pool": p["pool"], "chain_id": p["chain_id"].as_u64().unwrap_or(crate::chains::BASE), "symbols": p["symbols"],
               "enabled": p["enabled"], "allocation_pct": p["allocation_pct"], "deployed_usd": info["deployed_usd"], "in_range": info["in_range"]})
    }).collect();
    json!({
        "id": meta["id"], "label": meta["label"], "color": meta["color"], "address": s["wallet"],
        "live": s["execute"].as_bool().unwrap_or(false) && !watch, "watch": watch, "key": key, "mode": st["mode"], "state": st["state"],
        "last_cycle": st["last_cycle"], "value_usd": value, "deployed_usd": deployed, "idle_usd": idle, "rewards_usd": acct["rewards_usd"],
        "earned_usd": lsum("earned_usd"), "profit_usd": lsum("profit_usd"), "gas_usd": lsum("gas_usd"), "swap_cost_usd": lsum("swap_cost_usd"),
        "il_usd": lsum("il_usd"), "change_24h": change(86400), "change_7d": change(7 * 86400),
        "pools_on": on, "pools_with_positions": with_pos, "pools_in_range": in_range, "pools_total": pools.len(),
        "chains": chains, "history": hist, "attention": attention, "by_pool": by_pool,
    })
}

/// The overview's totals over every wallet's card (a figure no wallet knows stays unknown, not 0).
pub fn totals(cards: &[Value]) -> Value {
    let add = |k: &str| -> Value {
        let xs: Vec<f64> = cards.iter().filter_map(|c| c[k].as_f64()).collect();
        if xs.is_empty() { Value::Null } else { json!(xs.iter().sum::<f64>()) }
    };
    let mut t = json!({});
    for k in ["value_usd", "deployed_usd", "idle_usd", "rewards_usd", "earned_usd", "profit_usd", "gas_usd", "swap_cost_usd", "il_usd",
              "change_24h", "change_7d", "pools_on", "pools_with_positions", "pools_in_range"] {
        t[k] = add(k);
    }
    t["wallets"] = json!(cards.len());
    t["live"] = json!(cards.iter().filter(|c| c["live"] == true).count());
    t
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::paths;

    #[test]
    fn a_wallets_card() {
        let st = json!({"mode": "LIVE", "state": "idle", "last_error": null,
            "account": {"value_usd": 1500.0, "total_usd": 1490.0,
                "pools": {"a": {"deployed_usd": 1000.0, "in_range": true, "positions": [{}]}, "b": {"deployed_usd": 200.0, "in_range": false, "positions": [{}]}},
                "tokens": {"USDC": {"usd": 290.0}},
                "chains": {"8453": {"name": "Base", "total_usd": 1490.0, "gas": 0.0001, "gas_symbol": "ETH", "incomplete": []}}},
            "pools": {"b": {"mode": "hold", "error": "waiting"}},
            "ledger": {"8453": {"earned_usd": 12.0, "profit_usd": 9.5}}});
        let pools = vec![json!({"name": "a", "enabled": true, "chain_id": 8453}), json!({"name": "b", "enabled": true, "chain_id": 8453}),
                         json!({"name": "c", "enabled": false})];
        let snaps: Vec<Value> = (0..10).map(|i| json!({"ts": 1000 + i * 86400, "value_usd": 1000.0 + i as f64 * 50.0})).collect();
        let now = 1000 + 9 * 86400;
        let c = summary(&json!({"id": "w2", "label": "Two"}), &st, &json!({"execute": true, "wallet": "0xab"}), &json!({"stored": true}),
                        &pools, &snaps, 1, &|_| 0.0002, now);
        assert_eq!(c["value_usd"], 1500.0);
        assert_eq!(c["deployed_usd"], 1200.0);
        assert_eq!(c["idle_usd"], 290.0);
        assert_eq!(c["earned_usd"], 12.0);
        assert_eq!(c["live"], true);
        assert_eq!(c["pools_on"], 2);
        assert_eq!(c["pools_in_range"], 1);
        assert_eq!(c["change_24h"], 1500.0 - 1400.0);
        assert_eq!(c["change_7d"], 1500.0 - 1100.0);
        assert_eq!(c["history"].as_array().unwrap().len(), 10);
        let kinds: Vec<&str> = c["attention"].as_array().unwrap().iter().map(|a| a["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"range") && kinds.contains(&"hold") && kinds.contains(&"gas") && kinds.contains(&"request"), "{kinds:?}");
        // a watched wallet is never live, whatever its switch says
        let w = summary(&json!({"id": "w3"}), &st, &json!({"execute": true}), &json!({"stored": false}), &pools, &[], 0, &|_| 0.0, now);
        assert_eq!(w["live"], false);
        assert_eq!(w["watch"], true);
        assert!(w["change_7d"].is_null());
        let t = totals(&[c, w]);
        assert_eq!(t["value_usd"], 3000.0);
        assert_eq!(t["wallets"], 2);
        assert_eq!(t["live"], 1);
        assert!(totals(&[json!({})])["earned_usd"].is_null(), "unknown is not zero");
    }

    /// A fresh app home in the temp folder, for this test's thread only.
    pub fn temp_home(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("alm-test-{name}-{}-{}", std::process::id(), crate::engine::now()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        paths::set_test_home(&d);
        d
    }

    fn legacy_home(name: &str) -> std::path::PathBuf {
        let d = temp_home(name);
        std::fs::create_dir_all(d.join("config")).unwrap();
        std::fs::create_dir_all(d.join("logs")).unwrap();
        std::fs::write(d.join("config/settings.json"), r#"{"wallet": "0x00000000000000000000000000000000000000a1", "execute": false, "interval_s": 120, "claim": {"enabled": false}}"#).unwrap();
        let g = crate::ladder::as_graph(&crate::ladder::Params::from(&crate::ladder::plain_defaults()).unwrap());
        let pools = json!([{"name": "usdc-usdt", "pool": "0x00000000000000000000000000000000000000b1", "decimals": 6, "chain_id": 8453,
                            "allocation_pct": 60.0, "enabled": true, "strategy": g}]);
        std::fs::write(d.join("config/pools.json"), serde_json::to_string_pretty(&pools).unwrap()).unwrap();
        std::fs::write(d.join("logs/snapshots.jsonl"), "{\"ts\": 1, \"total_usd\": 10}\n").unwrap();
        d
    }

    fn files(d: &std::path::Path) -> Vec<(String, Vec<u8>)> {
        let mut out = vec![];
        for sub in ["config", "logs"] {
            for e in std::fs::read_dir(d.join(sub)).unwrap().flatten() {
                if e.path().is_file() {
                    out.push((format!("{sub}/{}", e.file_name().to_string_lossy()), std::fs::read(e.path()).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn a_single_wallet_setup_reads_as_before() {
        let d = legacy_home("legacy");
        assert_eq!(list().len(), 1);
        assert_eq!(list()[0]["id"], MAIN);
        assert_eq!(paths::config(), d.join("config"));
        assert_eq!(paths::logs(), d.join("logs"));
        assert_eq!(paths::secrets(), d.join("secrets"));
        assert_eq!(address(MAIN), "0x00000000000000000000000000000000000000a1");
        let s = crate::settings::load_settings();
        assert_eq!(s["interval_s"], 120);
        assert_eq!(s["claim"]["enabled"], false);
        assert_eq!(s["claim"]["every_days"], 7); // defaults still merged in
        let p = crate::settings::load_pools();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0]["allocation_pct"], 60.0);
        assert!(!path().exists(), "no wallets.json until a second wallet is added");
    }

    #[test]
    fn adding_a_wallet_leaves_the_first_untouched() {
        let d = legacy_home("add");
        let before = files(&d);
        let w = add("Second", "0x00000000000000000000000000000000000000a2", Some(MAIN)).unwrap();
        let id = w["id"].as_str().unwrap().to_string();
        assert_eq!(id, "w2");
        // the first wallet's files are byte for byte what they were (wallets.json is new, and the only change)
        let after: Vec<_> = files(&d).into_iter().filter(|(n, _)| n != "config/wallets.json").collect();
        assert_eq!(before, after);
        assert_eq!(ids(), vec![MAIN.to_string(), id.clone()]);
        // the new wallet: its own folders, its pools copied switched off, its address
        assert_eq!(paths::scope(&id, paths::config), d.join("wallets/w2/config"));
        assert_eq!(paths::scope(&id, paths::secrets), d.join("secrets/wallets/w2"));
        let p = paths::scope(&id, crate::settings::load_pools);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0]["enabled"], false);
        assert_eq!(p[0]["allocation_pct"], 0.0);
        assert_eq!(address(&id), "0x00000000000000000000000000000000000000a2");
        // the app's settings are shared, the wallet's own are not
        let s = paths::scope(&id, crate::settings::load_settings);
        assert_eq!(s["interval_s"], 120);
        assert_eq!(s["claim"]["enabled"], true, "a new wallet starts from the defaults for its own settings");
        // outside the scope, nothing changed for the first wallet
        assert_eq!(crate::settings::load_pools()[0]["enabled"], true);
        assert_eq!(address(MAIN), "0x00000000000000000000000000000000000000a1");
    }

    #[test]
    fn an_address_belongs_to_one_wallet() {
        legacy_home("dup");
        assert!(add("Again", "0x00000000000000000000000000000000000000A1", None).is_err());
        let w = add("Other", "", None).unwrap();
        let id = w["id"].as_str().unwrap();
        assert!(check_address(id, "0x00000000000000000000000000000000000000a1").is_err());
        assert!(check_address(MAIN, "0x00000000000000000000000000000000000000a1").is_ok());
        assert!(add("bad", "0x12", None).is_err());
        assert!(add("", "", None).is_err());
    }

    #[test]
    fn settings_split_between_the_wallet_and_the_app() {
        let d = legacy_home("split");
        let id = add("Two", "", None).unwrap()["id"].as_str().unwrap().to_string();
        paths::scope(&id, || {
            let mut s = crate::settings::load_settings();
            s["execute"] = json!(false);
            s["claim"]["every_days"] = json!(3.0);
            s["interval_s"] = json!(600);
            crate::settings::save_settings(&s).unwrap();
        });
        let own: Value = serde_json::from_str(&std::fs::read_to_string(d.join("wallets/w2/config/settings.json")).unwrap()).unwrap();
        assert!(own.get("interval_s").is_none(), "app settings are not copied into a wallet");
        assert_eq!(own["claim"]["every_days"], 3.0);
        let root = crate::settings::load_settings();
        assert_eq!(root["interval_s"], 600, "an app setting saved from any wallet is the app's");
        assert_eq!(root["claim"]["enabled"], false, "the first wallet keeps its own claim settings");
        assert_eq!(root["wallet"], "0x00000000000000000000000000000000000000a1");
        assert_eq!(paths::scope(&id, crate::settings::load_settings)["claim"]["every_days"], 3.0);
    }

    #[test]
    fn scopes_nest_and_threads_keep_them() {
        temp_home("scope");
        assert_eq!(paths::wallet(), MAIN);
        paths::scope("w3", || {
            assert_eq!(paths::wallet(), "w3");
            paths::scope("w4", || assert_eq!(paths::wallet(), "w4"));
            assert_eq!(paths::wallet(), "w3");
            let (tx, rx) = std::sync::mpsc::channel();
            paths::spawn(move || tx.send(paths::wallet()).unwrap()).join().unwrap();
            assert_eq!(rx.recv().unwrap(), "w3");
        });
        let r = std::panic::catch_unwind(|| paths::scope("w5", || panic!("x")));
        assert!(r.is_err());
        assert_eq!(paths::wallet(), MAIN, "restored after a panic");
    }

    #[test]
    fn removing_archives_instead_of_deleting() {
        let d = legacy_home("remove");
        let id = add("Gone", "0x00000000000000000000000000000000000000a3", None).unwrap()["id"].as_str().unwrap().to_string();
        assert!(remove(&id, true).is_err());
        remove(&id, false).unwrap();
        assert_eq!(ids(), vec![MAIN.to_string()]);
        assert!(!d.join("wallets/w2").exists());
        assert!(std::fs::read_dir(d.join("wallets")).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("w2.removed-")));
        // the next wallet does not reuse the archived folder's id
        let again = add("New", "", None).unwrap();
        assert_ne!(again["id"], "w2");
    }

    #[test]
    fn keys_are_kept_per_wallet() {
        legacy_home("keys");
        let id = add("Keyed", "", None).unwrap()["id"].as_str().unwrap().to_string();
        let (k1, a1) = crate::wallet::generate().unwrap();
        let (k2, a2) = crate::wallet::generate().unwrap();
        assert_eq!(crate::signer::save_key(&k1).unwrap(), a1);
        assert_eq!(paths::scope(&id, || crate::signer::save_key(&k2)).unwrap(), a2);
        assert_eq!(crate::signer::load_account().unwrap().address, a1);
        assert_eq!(paths::scope(&id, crate::signer::load_account).unwrap().address, a2);
        paths::scope(&id, crate::signer::forget);
        assert!(!paths::scope(&id, crate::signer::configured));
        assert!(crate::signer::configured(), "forgetting one wallet's key keeps the others");
        crate::signer::forget();
    }

    #[test]
    fn the_existing_wallet_is_registered_as_wallet_one() {
        let d = legacy_home("registry");
        let before = files(&d);
        ensure_registry();
        let after: Vec<_> = files(&d).into_iter().filter(|(n, _)| n != "config/wallets.json").collect();
        assert_eq!(before, after, "only wallets.json is new");
        assert_eq!(list().len(), 1);
        assert_eq!(list()[0]["id"], MAIN);
        let raw = std::fs::read_to_string(path()).unwrap();
        ensure_registry();
        assert_eq!(std::fs::read_to_string(path()).unwrap(), raw, "written once");
        // a second wallet after that: the list keeps the first
        add("Two", "", None).unwrap();
        assert_eq!(ids(), vec![MAIN.to_string(), "w2".to_string()]);
    }

    #[test]
    fn a_new_wallets_key_is_checked_before_anything_is_stored() {
        legacy_home("keyfrom");
        let (k, a) = crate::wallet::generate().unwrap();
        let got = key_from(&json!({"private_key": k.trim_start_matches("0x")})).unwrap().unwrap();
        assert_eq!(got.0, a.to_lowercase());
        assert!(key_from(&json!({})).unwrap().is_none());
        assert!(key_from(&json!({"private_key": "0x12"})).is_err());
        // the address the first wallet manages is refused for a new one
        let p = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
        let first = key_from(&json!({"phrase": p, "index": 0})).unwrap().unwrap().0;
        crate::settings::save_settings(&json!({"wallet": first})).unwrap();
        assert!(key_from(&json!({"phrase": p, "index": 0})).is_err());
        assert!(key_from(&json!({"phrase": p, "index": 1})).unwrap().is_some());
    }

    #[test]
    fn pausing_one_wallet_blocks_only_its_sends() {
        legacy_home("pause");
        let id = add("Two", "", None).unwrap()["id"].as_str().unwrap().to_string();
        paths::scope(&id, || std::fs::write(paths::wallet_pause_file(), "x").unwrap());
        assert!(paths::scope(&id, paths::execution_blocked).is_some());
        assert!(paths::execution_blocked().is_none(), "the first wallet still sends");
        assert_ne!(paths::wallet_pause_file(), paths::stop_file(), "the first wallet's pause is not the kill switch");
        paths::scope(&id, || std::fs::remove_file(paths::wallet_pause_file()).unwrap());
        assert!(paths::scope(&id, paths::execution_blocked).is_none());
    }

    #[test]
    fn any_wallet_can_be_removed_while_another_remains() {
        let d = legacy_home("rmmain");
        assert!(remove(MAIN, false).is_err(), "the only wallet stays");
        let before = files(&d);
        let id = add("Two", "", None).unwrap()["id"].as_str().unwrap().to_string();
        remove(MAIN, false).unwrap();
        assert_eq!(ids(), vec![id.clone()]);
        assert_eq!(default_id(), id);
        assert!(!exists(MAIN));
        let after: Vec<_> = files(&d).into_iter().filter(|(n, _)| n != "config/wallets.json").collect();
        assert_eq!(before, after, "its files are kept as they were");
        // its address is free again, and the remaining wallet cannot be removed
        assert!(check_address(&id, "0x00000000000000000000000000000000000000a1").is_ok());
        assert!(remove(&id, false).is_err());
        ensure_registry();
        assert!(!exists(MAIN), "not brought back at the next start");
    }

    #[test]
    fn ids_name_folders_safely() {
        assert!(valid_id("w2") && valid_id("main") && valid_id("cold-1"));
        assert!(!valid_id("") && !valid_id("../x") && !valid_id("W2") && !valid_id("-a") && !valid_id(&"a".repeat(25)));
    }
}
