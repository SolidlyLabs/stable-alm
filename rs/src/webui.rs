//! Local web UI server: the dashboard page and its API, the sign-in token (cookie alm_t or
//! X-Token), loopback-only Host, same-origin JSON POSTs. Pools carry chain_id; /api/chains lists
//! the registry so the page can filter by chain.

use crate::engine::{self, wallet_positions};
use crate::settings::{self, load_pools, load_settings, save_settings};
use crate::worker::{pool_chain, Worker};
use serde_json::{json, Value};
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};
use tiny_http::{Header, Method, Request, Response, Server};

const INDEX: &str = include_str!("../static/index.html");
const HELP: &str = include_str!("../static/help.js");
const LAB: &str = include_str!("../static/lab.js");
const POSITIONS: &str = include_str!("../static/positions.js");
const WIZARD: &str = include_str!("../static/wizard.js");
const COOKIE: &str = "alm_t";

pub fn ui_token() -> String {
    static T: OnceLock<String> = OnceLock::new();
    T.get_or_init(|| {
        if let Ok(t) = std::env::var("ALM_UI_TOKEN") {
            return t;
        }
        let p = crate::paths::app_secrets().join("ui_token");
        if let Ok(t) = std::fs::read_to_string(&p) {
            if !t.trim().is_empty() {
                return t.trim().to_string();
            }
        }
        let mut b = [0u8; 24];
        getrandom::getrandom(&mut b).expect("random");
        let t: String = b.iter().map(|x| format!("{x:02x}")).collect();
        let _ = crate::paths::write_secret(&p, t.as_bytes());
        t
    })
    .clone()
}

fn host_ok(v: &str) -> bool {
    let v = v.split("//").last().unwrap_or(v);
    let host = if v.starts_with('[') { v.split(']').next().unwrap_or("").trim_start_matches('[') } else { v.split(':').next().unwrap_or("") };
    let host = host.split('/').next().unwrap_or("");
    matches!(host.to_lowercase().as_str(), "127.0.0.1" | "localhost" | "::1")
}

fn header<'a>(r: &'a Request, name: &str) -> Option<&'a str> {
    r.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str())
}

fn eq_ct(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authorised(r: &Request) -> bool {
    let t = ui_token();
    if let Some(x) = header(r, "X-Token") {
        return eq_ct(x, &t);
    }
    header(r, "Cookie")
        .map(|c| c.split(';').filter_map(|kv| kv.trim().split_once('=')).any(|(k, v)| k == COOKIE && eq_ct(v, &t)))
        .unwrap_or(false)
}

/// A token logo from the app's own copy. Cached by the browser for a day; sandboxed so an SVG
/// opened on its own cannot run anything.
fn send_logo(r: Request, rest: &str) {
    let mut it = rest.splitn(2, '/');
    let (cid, addr) = (it.next().and_then(|c| c.parse::<u64>().ok()), it.next().unwrap_or(""));
    let Some(cid) = cid else { return send_json(r, 404, json!({"error": "not found"})) };
    match crate::store::logo(cid, addr) {
        Ok((bytes, ctype)) => {
            let resp = Response::from_data(bytes)
                .with_header(Header::from_bytes("Content-Type", ctype).unwrap())
                .with_header(Header::from_bytes("Cache-Control", "private, max-age=86400").unwrap())
                .with_header(Header::from_bytes("Content-Security-Policy", "default-src 'none'; style-src 'unsafe-inline'; sandbox").unwrap())
                .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            let _ = r.respond(resp);
        }
        Err(_) => send_json(r, 404, json!({"error": "no logo"})),
    }
}

fn send(r: Request, code: u16, body: Vec<u8>, ctype: &str) {
    let resp = Response::from_data(body)
        .with_status_code(code)
        .with_header(Header::from_bytes("Content-Type", ctype).unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
        // The page may only talk to this app: no scripts, connections, forms or frames elsewhere.
        // Images (token logos) may come from https hosts, and never get a referrer.
        .with_header(Header::from_bytes("Content-Security-Policy",
            "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; \
             connect-src 'self'; form-action 'none'; frame-ancestors 'none'; base-uri 'none'").unwrap())
        .with_header(Header::from_bytes("Referrer-Policy", "no-referrer").unwrap())
        .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap())
        .with_header(Header::from_bytes("X-Frame-Options", "DENY").unwrap());
    let _ = r.respond(resp);
}

fn send_json(r: Request, code: u16, v: Value) {
    send(r, code, v.to_string().into_bytes(), "application/json");
}

static WORKER: OnceLock<Arc<Worker>> = OnceLock::new();
/// The server answering now and its port (a port change starts a new one, see `rebind`).
static CURRENT: std::sync::Mutex<Option<(Arc<Server>, u16)>> = std::sync::Mutex::new(None);

/// `pools` with the lab's limits (`max_actions_per_day`, `max_swap_cost_bps`, `max_spot_dev_ticks`, each
/// only when given) set on the pool `body` names ({chain_id, pool}), checked as a saved pool is; and
/// how many pools matched.
fn set_limits(mut pools: Vec<Value>, body: &Value) -> Result<(Vec<Value>, usize), String> {
    let cid = body["chain_id"].as_u64().ok_or("expected {chain_id, pool, ...limits}")?;
    let addr = body["pool"].as_str().ok_or("expected {chain_id, pool, ...limits}")?.to_lowercase();
    let mut n = 0;
    for p in pools.iter_mut().filter(|p| pool_chain(p) == cid && p["pool"].as_str().map(|a| a.eq_ignore_ascii_case(&addr)).unwrap_or(false)) {
        if let Some(v) = body["max_actions_per_day"].as_f64() {
            p["max_actions_per_day"] = json!(v.round() as i64);
        }
        for k in ["max_swap_cost_bps", "max_spot_dev_ticks"] {
            if let Some(v) = body[k].as_f64() {
                p[k] = json!(v);
            }
        }
        settings::validate_pool(p)?;
        n += 1;
    }
    Ok((pools, n))
}

/// The port the settings ask for (8080 unless changed).
pub fn wanted_port() -> u16 {
    load_settings()["ui_port"].as_u64().filter(|p| (1024..=65535).contains(p)).unwrap_or(8080) as u16
}

/// Serve the UI on 127.0.0.1:`port`; with `fallback`, a port in use moves to the next free one of
/// the following 20, else any free port. Writes the port to HOME/ui.port (the tray's links read it).
pub fn serve(worker: Arc<Worker>, port: u16, fallback: bool) -> Result<u16, String> {
    let _ = WORKER.set(worker);
    let server = match Server::http(("127.0.0.1", port)) {
        Ok(s) => s,
        Err(e) if fallback => {
            let next = (1..=20u16).filter_map(|k| port.checked_add(k)).find_map(|p| Server::http(("127.0.0.1", p)).ok());
            let s = match next {
                Some(s) => s,
                None => Server::http(("127.0.0.1", 0)).map_err(|e| e.to_string())?,
            };
            crate::warn!("ui", "port {port} is in use ({e}); serving the UI on another one");
            s
        }
        Err(e) => return Err(e.to_string()),
    };
    let port = start(server, port);
    crate::info!("ui", "web UI on http://localhost:{port} (sign-in link printed on the console)");
    println!("Stable ALM dashboard: http://localhost:{port}/?t={}", ui_token());
    Ok(port)
}

const UI_THREADS: usize = 16;

fn start(server: Server, asked: u16) -> u16 {
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(asked);
    let server = Arc::new(server);
    let w = WORKER.get().expect("serve() sets the worker").clone();
    // enough threads that a slow answer (positions, wallet tokens) never queues the quick ones: the
    // lab's progress polls during an optimization, the status every 10 s
    for _ in 0..UI_THREADS {
        let (s, w) = (server.clone(), w.clone());
        std::thread::spawn(move || {
            for req in s.incoming_requests() {
                handle(req, &w);
            }
        });
    }
    let _ = std::fs::write(crate::paths::home().join("ui.port"), port.to_string());
    let old = CURRENT.lock().unwrap().replace((server, port));
    if let Some((old, p)) = old {
        // the old port answers a little longer: the page that changed the setting moves over first
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(20));
            old.unblock();
            crate::info!("ui", "stopped answering on port {p}");
        });
    }
    port
}

/// The port the UI answers on now.
pub fn current_port() -> Option<u16> {
    CURRENT.lock().unwrap().as_ref().map(|c| c.1)
}

/// Move the UI to `port` (an error when it is in use; the old port keeps answering for a while).
pub fn rebind(port: u16) -> Result<u16, String> {
    if current_port() == Some(port) {
        return Ok(port);
    }
    let server = Server::http(("127.0.0.1", port)).map_err(|e| format!("port {port} is not free: {e}"))?;
    let p = start(server, port);
    crate::info!("ui", "web UI moved to http://localhost:{p}");
    Ok(p)
}

/// Stop answering (quitting).
pub fn stop() {
    if let Some((s, _)) = CURRENT.lock().unwrap().take() {
        s.unblock();
    }
}

fn handle(mut req: Request, w: &Arc<Worker>) {
    if !host_ok(header(&req, "Host").unwrap_or("")) {
        return send_json(req, 403, json!({"error": "the UI only answers on 127.0.0.1 / localhost"}));
    }
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    let path = path.to_string();
    if *req.method() == Method::Get {
        if path == "/" {
            if let Some(t) = query.split('&').find_map(|kv| kv.strip_prefix("t=")) {
                if !eq_ct(t, &ui_token()) {
                    return send(req, 401, b"Wrong link. Open the dashboard from the tray icon.".to_vec(), "text/plain; charset=utf-8");
                }
                let resp = Response::empty(303)
                    .with_header(Header::from_bytes("Set-Cookie", format!("{COOKIE}={t}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000")).unwrap())
                    .with_header(Header::from_bytes("Location", "/").unwrap());
                let _ = req.respond(resp);
                return;
            }
        }
        if path == "/" || path == "/index.html" {
            return send(req, 200, INDEX.as_bytes().to_vec(), "text/html; charset=utf-8");
        }
        if path == "/help.js" {
            return send(req, 200, HELP.as_bytes().to_vec(), "application/javascript; charset=utf-8");
        }
        if path == "/wizard.js" {
            return send(req, 200, WIZARD.as_bytes().to_vec(), "application/javascript; charset=utf-8");
        }
        if path == "/lab.js" {
            return send(req, 200, LAB.as_bytes().to_vec(), "application/javascript; charset=utf-8");
        }
        if path == "/positions.js" {
            return send(req, 200, POSITIONS.as_bytes().to_vec(), "application/javascript; charset=utf-8");
        }
        if !path.starts_with("/api/") {
            return send_json(req, 404, json!({"error": "not found"}));
        }
        if !authorised(&req) {
            return send_json(req, 401, json!({"error": "not signed in: open the dashboard from the tray icon (or the link in the log)"}));
        }
        if let Some(rest) = path.strip_prefix("/api/logo/") {
            return send_logo(req, rest);
        }
        let t0 = std::time::Instant::now();
        let (code, body) = match wallet_of(&path, query) {
            Ok(id) => crate::paths::scope(&id, || get(&path, w)),
            Err(e) => (400, json!({"error": e})),
        };
        slow(&path, t0);
        return send_json(req, code, body);
    }
    if *req.method() != Method::Post {
        return send_json(req, 405, json!({"error": "method not allowed"}));
    }
    if let Some(o) = header(&req, "Origin") {
        if !host_ok(o) {
            return send_json(req, 403, json!({"error": "cross-site request refused"}));
        }
    }
    if header(&req, "Content-Type").map(|c| c.split(';').next().unwrap_or("").trim().to_lowercase()) != Some("application/json".into()) {
        return send_json(req, 415, json!({"error": "expected application/json"}));
    }
    if !authorised(&req) {
        return send_json(req, 401, json!({"error": "not signed in: open the dashboard from the tray icon (or the link in the log)"}));
    }
    let mut raw = String::new();
    let _ = req.as_reader().take(1 << 20).read_to_string(&mut raw);
    let body: Value = serde_json::from_str(if raw.is_empty() { "{}" } else { &raw }).unwrap_or(json!({}));
    let t0 = std::time::Instant::now();
    let res = wallet_of(&path, query).and_then(|id| crate::paths::scope(&id, || post(&path, body, w)));
    slow(&path, t0);
    match res {
        Ok(v) => send_json(req, 200, v),
        Err(e) => send_json(req, 400, json!({"error": if path == "/api/key" { "invalid private key".to_string() } else { e }})),
    }
}

/// The wallet a request is for: ?wallet=<id>, or without it `main` (so every request made before there
/// were several wallets means the same as before), or the first listed once `main` was removed. The
/// agent's pages and the wallet list are the app's own and run as `main`.
fn wallet_of(path: &str, query: &str) -> Result<String, String> {
    if path.starts_with("/api/agent") || path == "/api/experimental" || path.starts_with("/api/wallets") {
        return Ok(crate::paths::MAIN.into());
    }
    let id = match query.split('&').find_map(|kv| kv.strip_prefix("wallet=")) {
        Some(id) => id.to_string(),
        None => crate::wallets::default_id(),
    };
    if crate::wallets::valid_id(&id) && crate::wallets::exists(&id) {
        Ok(id)
    } else {
        Err("no such wallet".into())
    }
}

/// Every wallet's card and the totals over them (the Overview's "All wallets").
fn wallets_overview(w: &Arc<Worker>) -> Value {
    let chains = crate::chains::as_json();
    let min_gas = |id: u64| chains.as_array().and_then(|a| a.iter().find(|c| c["id"].as_u64() == Some(id))).and_then(|c| c["min_gas"].as_f64()).unwrap_or(0.0);
    let cards: Vec<Value> = crate::wallets::list()
        .iter()
        .map(|meta| {
            let id = meta["id"].as_str().unwrap_or(crate::paths::MAIN).to_string();
            crate::paths::scope(&id, || {
                let mut c = crate::wallets::summary(meta, &w.status_of(&id), &load_settings(), &crate::signer::status(), &load_pools(), &snapshots(),
                                                    crate::capital::requests().len(), &min_gas, engine::now() as i64);
                c["paused"] = json!(crate::paths::wallet_pause_file().exists());
                c
            })
        })
        .collect();
    json!({"totals": crate::wallets::totals(&cards), "wallets": cards, "paused": crate::paths::stop_file().exists(), "now": engine::now()})
}

/// Requests that keep one of the 4 handler threads busy for long are logged by path (never the
/// query), so a slow dashboard names its cause in alm.log.
fn slow(path: &str, t0: std::time::Instant) {
    let ms = t0.elapsed().as_millis();
    if ms >= 2000 {
        crate::warn!("ui", "{path} took {:.1} s", ms as f64 / 1000.0);
    }
}

fn tail_text(p: &std::path::Path, n: usize) -> Vec<String> {
    let s = std::fs::read_to_string(p).unwrap_or_default();
    let lines: Vec<String> = s.lines().map(String::from).collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

type Stamp = (u64, std::time::SystemTime);
static SNAPS: Mutex<Option<std::collections::HashMap<std::path::PathBuf, (Stamp, Vec<Value>)>>> = Mutex::new(None);

/// The chart series of the current wallet, re-read only when its snapshots.jsonl changes (once a
/// cycle), not on every poll.
fn snapshots() -> Vec<Value> {
    let p = crate::paths::logs().join("snapshots.jsonl");
    let stamp = std::fs::metadata(&p).ok().map(|m| (m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)));
    if let Some(st) = stamp {
        if let Some((k, v)) = SNAPS.lock().unwrap().get_or_insert_with(Default::default).get(&p) {
            if *k == st {
                return v.clone();
            }
        }
    }
    let rows = snapshots_read(&p);
    if let Some(st) = stamp {
        SNAPS.lock().unwrap().get_or_insert_with(Default::default).insert(p, (st, rows.clone()));
    }
    rows
}

fn snapshots_read(p: &std::path::Path) -> Vec<Value> {
    let s = std::fs::read_to_string(p).unwrap_or_default();
    let rows: Vec<Value> = s.lines().rev().take(20000).filter_map(|l| serde_json::from_str(l).ok()).collect::<Vec<_>>().into_iter().rev().collect();
    let rows = clean_history(rows);
    let step = (rows.len() / 600).max(1);
    let mut keep: Vec<&Value> = rows.iter().step_by(step).collect();
    if !rows.is_empty() && (rows.len() - 1) % step != 0 {
        keep.push(rows.last().unwrap());
    }
    keep.iter()
        .map(|r| {
            let idle: f64 = r["tokens"].as_object().map(|t| t.values().map(|x| x["usd"].as_f64().unwrap_or(0.0)).sum()).unwrap_or(0.0);
            let pools: serde_json::Map<String, Value> =
                r["pools"].as_object().map(|p| p.iter().map(|(k, v)| (k.clone(), v["deployed_usd"].clone())).collect()).unwrap_or_default();
            let chains: serde_json::Map<String, Value> =
                r["chains"].as_object().map(|c| c.iter().map(|(k, v)| (k.clone(), v["total_usd"].clone())).collect()).unwrap_or_default();
            json!({"ts": r["ts"], "total_usd": r["total_usd"], "mode": r["mode"], "value_usd": r["value_usd"], "rewards_usd": r["rewards_usd"],
                   "solid_usd": r["solid_usd"], "idle_usd": idle, "pools": pools, "chains": chains})
        })
        .collect()
}

/// The value history without the cycles that were mis-measured, as they are read (the file keeps every
/// line): a cycle whose valuation was incomplete (a pool, balance or parked lot could not be read, so
/// the total lacks it; each chain lists those in `incomplete`, written since 0.3), and a dip or spike
/// of one to three cycles that the cycles either side of it, agreeing with each other, contradict.
/// Money moved in or out is a step, not a V, and stays.
fn clean_history(rows: Vec<Value>) -> Vec<Value> {
    const AGREE: f64 = 0.01; // the flanks within 1% of each other
    const OFF: f64 = 0.03; // every cycle of the run at least 3% away from them
    let flagged = |r: &Value| r["chains"].as_object().is_some_and(|c| c.values().any(|v| v["incomplete"].as_array().is_some_and(|a| !a.is_empty())));
    let rows: Vec<Value> = rows.into_iter().filter(|r| !flagged(r)).collect();
    let v = |r: &Value| r["value_usd"].as_f64().or_else(|| r["total_usd"].as_f64()).unwrap_or(0.0);
    let mut drop = vec![false; rows.len()];
    let mut i = 1;
    while i + 1 < rows.len() {
        let mut hit = 0;
        for k in 1..=3 {
            if i + k >= rows.len() {
                break;
            }
            let (a, b) = (v(&rows[i - 1]), v(&rows[i + k]));
            let mid = (a + b) / 2.0;
            if mid <= 0.0 || (a - b).abs() > AGREE * a.max(b) {
                continue;
            }
            let run = &rows[i..i + k];
            let below = run.iter().all(|r| v(r) < mid * (1.0 - OFF));
            let above = run.iter().all(|r| v(r) > mid * (1.0 + OFF));
            if below || above {
                hit = k;
                break;
            }
        }
        if hit > 0 {
            drop[i..i + hit].iter_mut().for_each(|d| *d = true);
            i += hit;
        } else {
            i += 1;
        }
    }
    rows.into_iter().zip(drop).filter(|(_, d)| !d).map(|(r, _)| r).collect()
}

fn configured() -> Vec<(u64, String)> {
    load_pools().iter().map(|p| (pool_chain(p), p["pool"].as_str().unwrap_or("").to_lowercase())).collect()
}

fn get(path: &str, w: &Arc<Worker>) -> (u16, Value) {
    let s = load_settings();
    let wallet = s["wallet"].as_str().unwrap_or("").to_string();
    match path {
        "/api/status" => {
            let events: Vec<Value> = engine::log_tail(400_000).into_iter().rev().take(150).collect();
            let mut rewards = json!({});
            let mut total = 0.0;
            let mut chains: Vec<u64> = configured().into_iter().map(|c| c.0).collect();
            chains.sort();
            chains.dedup();
            for cid in &chains {
                let r = crate::claim::claimable_cached(*cid, &wallet);
                total += r["usd"].as_f64().unwrap_or(0.0);
                rewards[cid.to_string()] = r;
            }
            let base = rewards[crate::chains::BASE.to_string()].clone();
            let mut rw = if base.is_null() { json!({"claimable": [], "usd": total}) } else { base };
            rw["by_chain"] = rewards;
            rw["usd_all_chains"] = json!(total);
            let errors: Vec<String> = tail_text(&crate::paths::app_logs().join("errors.log"), 80).into_iter().rev().collect();
            let id = crate::paths::wallet();
            let me = crate::wallets::list().into_iter().find(|x| x["id"] == id.as_str()).unwrap_or(json!({"id": id}));
            (200, json!({"worker": w.status_of(&id), "wallet": me, "wallets": crate::wallets::list(),
                         "settings": s, "pools": load_pools(), "signer": crate::signer::status(),
                         "paused": crate::paths::stop_file().exists(), "wallet_paused": crate::paths::wallet_pause_file().exists(), "events": events, "errors": errors, "snapshots": snapshots(),
                         "rewards": rw, "chains": crate::chains::as_json(), "now": engine::now(), "runtime": "rust",
                         "ladder_defaults": crate::ladder::plain_defaults(), "update": crate::update::status(), "pool_defaults": settings::pool_defaults()}))
        }
        "/api/chains" => (200, json!({"chains": crate::chains::as_json()})),
        "/api/ui_prefs" => (200, ui_prefs()),
        "/api/wallets" => (200, wallets_overview(w)),
        "/api/update" => (200, crate::update::status()),
        "/api/onboard" => (200, crate::onboard::status()),
        "/api/onboard/parking" => (200, crate::onboard::parking_suggestions()),
        p if p.starts_with("/api/agent") => {
            if !crate::experimental::enabled(&s) {
                return (404, json!({"error": crate::experimental::OFF}));
            }
            match crate::agent::api_get(&p["/api/agent".len()..], w) {
                Ok(v) => (200, v),
                Err(e) => (400, json!({"error": e})),
            }
        }
        "/api/experimental" => {
            if !crate::experimental::enabled(&s) {
                return (404, json!({"error": crate::experimental::OFF}));
            }
            (200, crate::experimental::overview(&s))
        }
        p if p.starts_with("/api/market") => {
            // /api/market/fresh: the Refresh button, which re-reads the pool lists from the Solidly UI
            let errors = if p == "/api/market/fresh" { crate::market_data::reload(&wallet, &configured()) } else { vec![] };
            match crate::market_data::shortlist(&wallet, &configured()) {
                Ok(mut rows) => {
                    let conf = configured();
                    for r in rows.iter_mut() {
                        let c = (r["chain_id"].as_u64().unwrap_or(8453), r["pool"].as_str().unwrap_or("").to_string());
                        r["configured"] = json!(conf.contains(&c));
                    }
                    (200, json!({"pools": rows, "errors": errors}))
                }
                Err(e) => (502, json!({"error": format!("Solidly market data unavailable: {e}")})),
            }
        }
        // the Positions tab: /api/positions or /api/positions/<days> (1 to 90)
        p if p == "/api/positions" || p.starts_with("/api/positions/") => {
            let days = p.strip_prefix("/api/positions/").and_then(|d| d.parse::<u64>().ok()).unwrap_or(30).clamp(1, 90);
            match crate::positions::view(&wallet, days) {
                Some(v) => (200, v),
                None => (503, json!({"error": "positions are still loading from the Solidly UI; try again in a moment"})),
            }
        }
        p if p.starts_with("/api/solidpools") => {
            let mut rows = vec![];
            for c in crate::chains::all().into_iter().filter(|c| !c.solid.is_empty() && !c.off) {
                if let Ok(ps) = crate::market_data::pools(c.id, &wallet) {
                    rows.extend(ps.into_iter().filter(|p| p["token0"]["address"] == c.solid.as_str() || p["token1"]["address"] == c.solid.as_str()));
                }
            }
            (200, json!({"pools": rows}))
        }
        "/api/strategies" => (200, crate::library::get()),
        "/api/parking/names" => {
            // the whitelist's tokens and pools by name, from the cached token metadata, so the list
            // reads before the market list has loaded
            let sym = |cid: u64, a: &str| crate::prices::meta(cid, a).map(|m| m.symbol).ok();
            let names: Vec<Value> = s["parking"]["pools"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|e| {
                    let cid = e["chain_id"].as_u64()?;
                    let (token, pool) = (e["token"].as_str()?, e["pool"].as_str()?);
                    let pair = crate::chain::pool_tokens(cid, pool).ok().map(|(a, b)| [sym(cid, &a), sym(cid, &b)]);
                    Some(json!({"chain_id": cid, "token": token, "pool": pool, "symbol": sym(cid, token), "pair": pair}))
                })
                .collect();
            (200, json!({"names": names}))
        }
        p if p.starts_with("/api/gas/") => {
            // /api/gas/<chain>: base fee and the tip, fee cap and bump wait of each gas speed
            match p["/api/gas/".len()..].parse::<u64>() {
                Ok(cid) => match crate::chain::gas_preview(cid) {
                    Ok(v) => (200, v),
                    Err(e) => (502, json!({"error": e})),
                },
                Err(_) => (400, json!({"error": "use /api/gas/<chain id>"})),
            }
        }
        p if p.starts_with("/api/price_sources/") => {
            // /api/price_sources/<chain>: tokens a node strategy can watch there, their pools and active liquidity
            match p["/api/price_sources/".len()..].parse::<u64>() {
                Ok(cid) => match crate::external::sources(cid) {
                    Ok(v) => (200, v),
                    Err(e) => (502, json!({"error": e})),
                },
                Err(_) => (400, json!({"error": "use /api/price_sources/<chain id>"})),
            }
        }
        p if p.starts_with("/api/backtest/") => match p["/api/backtest/".len()..].parse::<u64>().ok().and_then(crate::backtest::job) {
            Some(j) => (200, j),
            None => (404, json!({"error": "no such backtest"})),
        },
        p if p.starts_with("/api/tokens") => {
            // (chain, token) pairs to list even without a pool: funding tokens per chain, reward tokens on Base
            let mut extra: Vec<(u64, String)> = vec![];
            for c in crate::chains::all() {
                extra.extend(crate::settings::funding_tokens(&s, c.id).into_iter().map(|a| (c.id, a)));
            }
            for t in s["sell_rewards"]["tokens"].as_array().cloned().unwrap_or_default() {
                if let Some(a) = t.as_str() {
                    extra.push((crate::chains::BASE, a.to_lowercase()));
                }
            }
            // the saved list at once; `refreshing` while a background refresh runs (the page asks again)
            let (t, refreshing) = crate::market_data::tokens(&wallet, &extra);
            (200, json!({"tokens": t, "refreshing": refreshing}))
        }
        _ => (404, json!({"error": "not found"})),
    }
}

fn is_addr(s: &str) -> bool {
    s.len() == 42 && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn add_pool(addr: &str, name: Option<&str>, cid: u64) -> Result<Value, String> {
    let addr = addr.trim().to_lowercase();
    if !is_addr(&addr) {
        return Err("not a pool address".into());
    }
    let configured = |pools: &[Value]| pools.iter().any(|p| p["pool"].as_str().unwrap_or("").eq_ignore_ascii_case(&addr) && pool_chain(p) == cid);
    if configured(&load_pools()) {
        return Err("pool already configured".into());
    }
    let (t0, t1) = crate::chain::pool_tokens(cid, &addr)?;
    let (m0, m1) = (crate::prices::meta(cid, &t0)?, crate::prices::meta(cid, &t1)?);
    let spacing = crate::chain::tick_spacing(cid, &addr)?;
    let (sp, _, _) = crate::chain::slot0(cid, &addr)?;
    let price = (crate::tickmath::to_f64(&sp) / 2f64.powi(96)).powi(2) * 10f64.powi(m0.decimals as i32 - m1.decimals as i32);
    let pegged = crate::market_data::is_pegged(&m0.symbol, &m1.symbol, Some(price));
    let min_swap = crate::prices::usd(cid, &t0).map(|u| 5.0 / u).unwrap_or(if m0.decimals <= 8 { 5.0 } else { 0.002 });
    let mut e = settings::pool_defaults();
    let numeraire = if ["WETH", "ETH"].contains(&m1.symbol.to_uppercase().as_str()) && !["USDC", "USDT"].contains(&m0.symbol.to_uppercase().as_str()) { 1 } else { 0 };
    for (k, v) in [
        ("name", json!(name.map(String::from).unwrap_or_else(|| format!("{}-{}", m0.symbol, m1.symbol).to_lowercase()))),
        ("pool", json!(addr)),
        ("chain_id", json!(cid)),
        ("decimals", json!(m0.decimals)),
        ("decimals1", json!(m1.decimals)),
        ("tick_spacing", json!(spacing)),
        ("symbols", json!([m0.symbol, m1.symbol])),
        ("enabled", json!(false)),
        ("allocation_pct", json!(0.0)),
        ("pegged", json!(pegged)),
        ("backtested", json!(false)),
        ("numeraire", json!(numeraire)),
        ("min_swap_value", json!(format!("{min_swap:.6}").parse::<f64>().unwrap_or(min_swap))),
    ] {
        e[k] = v;
    }
    e["strategy"] = crate::ladder::as_graph(&crate::ladder::Params::for_pool(spacing, pegged));
    if !pegged {
        e["max_spot_dev_ticks"] = json!((10 * spacing).max(100));
        e["slippage_bps"] = json!(10.0);
        e["max_swap_cost_bps"] = json!(15.0);
    }
    // added to the pools as they are now (the reads above take a while): nothing changed meanwhile is undone
    settings::update_pools(|pools| {
        if configured(pools) {
            return Err("pool already configured".into());
        }
        pools.push(e.clone());
        Ok(())
    })?;
    Ok(e)
}

fn post(path: &str, body: Value, w: &Arc<Worker>) -> Result<Value, String> {
    match path {
        "/api/settings" => {
            let mut s = load_settings();
            let mut body = body;
            if body.get("funding_tokens").is_some() {
                return Err("funding tokens are set per chain now: send funding_by_chain {chain id: [addresses]}".into());
            }
            if let Some(fb) = body.get("funding_by_chain") {
                let Some(m) = fb.as_object() else { return Err("funding_by_chain must map chain ids to token lists".into()) };
                let mut clean = serde_json::Map::new();
                for (k, v) in m {
                    let id: u64 = k.parse().map_err(|_| format!("funding_by_chain: {k} is not a chain id"))?;
                    if !crate::chains::all().iter().any(|c| c.id == id) {
                        return Err(format!("funding_by_chain: chain {id} is not supported"));
                    }
                    if v.is_null() {
                        clean.insert(k.clone(), Value::Null); // back to the chain's default
                        continue;
                    }
                    let arr = v.as_array().ok_or(format!("funding_by_chain[{k}] must be a list or null"))?;
                    if arr.len() > 20 || !arr.iter().all(|t| t.as_str().map(is_addr).unwrap_or(false)) {
                        return Err(format!("funding_by_chain[{k}] must be at most 20 token addresses"));
                    }
                    clean.insert(k.clone(), Value::Array(settings::clean_addrs(v)));
                }
                // chains not sent keep their lists
                let mut merged = s["funding_by_chain"].as_object().cloned().unwrap_or_default();
                for (k, v) in clean {
                    if v.is_null() {
                        merged.remove(&k);
                    } else {
                        merged.insert(k, v);
                    }
                }
                body["funding_by_chain"] = Value::Object(merged.clone());
                s["funding_by_chain"] = Value::Object(merged);
            }
            let old_x = s["experimental"].clone();
            for (k, v) in body.as_object().cloned().unwrap_or_default() {
                if s.get(&k).is_none() {
                    return Err(format!("unknown setting {k}"));
                }
                s[&k] = if s[&k].is_object() && v.is_object() { settings::merge(&s[&k], &v) } else { v };
            }
            if let Some(pool) = body["solid_lp"]["pool"].as_str() {
                let pool = pool.to_lowercase();
                let cid = s["solid_lp"]["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
                let solid = crate::chains::get(cid).solid;
                let (a, b) = crate::chain::pool_tokens(cid, &pool).unwrap_or_default();
                if !is_addr(&pool) || (a != solid && b != solid) {
                    return Err("the SOLID LP pool must be a Solidly V3 pool with SOLID in it".into());
                }
                s["solid_lp"]["pool"] = json!(pool);
            }
            if s["execute"].as_bool().unwrap_or(false) && !crate::signer::configured() {
                return Err("set a private key before turning live trading on".into());
            }
            // a wallet with a key has the key's address; only a watched wallet's address is set by hand
            if let (Some(a), Ok(Some(acct))) = (body["wallet"].as_str(), crate::signer::account()) {
                if !a.eq_ignore_ascii_case(&acct.address) {
                    return Err("this wallet has a key: its address is the key's (remove the key to watch another address)".into());
                }
            }
            // the linked Telegram chat is set by linking only
            for k in ["chat_id", "user_id", "user_name"] {
                s["experimental"]["telegram"][k] = old_x["telegram"][k].clone();
            }
            settings::validate_settings(&s)?;
            crate::wallets::check_address(&crate::paths::wallet(), s["wallet"].as_str().unwrap_or(""))?;
            crate::agent::settings_changed(&old_x, &s["experimental"]);
            // a new port is bound before the setting is saved: a port in use is refused, not saved
            let mut moved = None;
            if let Some(p) = body["ui_port"].as_u64() {
                if current_port() != Some(p as u16) {
                    moved = Some(rebind(p as u16)?);
                }
            }
            save_settings(&s).map_err(|e| e.to_string())?;
            Ok(json!({"ok": true, "settings": s, "ui_port": current_port(), "moved": moved.is_some()}))
        }
        "/api/pools" => {
            let pools = body["pools"].as_array().cloned().ok_or("expected {pools: [...]}")?;
            let mut out = vec![];
            for mut p in pools {
                for k in ["name", "pool", "decimals", "strategy"] {
                    if p.get(k).is_none() {
                        return Err(format!("pool entry missing {k}"));
                    }
                }
                let a = crate::prices::fnum(&p["allocation_pct"]);
                if !(0.0..=100.0).contains(&a) {
                    return Err("allocation_pct must be 0-100".into());
                }
                p["allocation_pct"] = json!(a);
                p["strategy"] = crate::ladder::normalized(&p["strategy"]);
                settings::validate_pool(&p)?;
                out.push(p);
            }
            let kept = settings::update_pools(|pools| {
                let kept = settings::keep_switched_off(pools, &mut out);
                *pools = std::mem::take(&mut out);
                Ok(kept)
            })?;
            w.request(false);
            Ok(json!({"ok": true, "kept_off": kept}))
        }
        // the Strategy lab's limits saved to the pool they were tested on (only the keys given)
        "/api/pools/limits" => {
            settings::update_pools(|pools| {
                let (p2, n) = set_limits(std::mem::take(pools), &body)?;
                if n == 0 {
                    return Err("no such pool in this wallet".into());
                }
                *pools = p2;
                Ok(())
            })?;
            w.request(false);
            Ok(json!({"ok": true}))
        }
        "/api/update" => match body["action"].as_str().unwrap_or("") {
            "check" => crate::update::check(),
            "install" => crate::update::install(),
            _ => Err("expected {action: check | install}".into()),
        },
        "/api/chains" => {
            let id = body["id"].as_u64().ok_or("expected {id, enabled}")?;
            if body.get("rpc").is_some() || body.get("send_rpc").is_some() || body.get("batch_size").is_some() {
                // rate_per_min: a number sets it (0 = no limit), null restores the default, absent keeps it
                let rate = body.get("rate_per_min").map(|v| v.as_u64());
                crate::chains::set_rpc(id, body["rpc"].as_str(), body["send_rpc"].as_str(), body["batch_size"].as_u64(), rate)?;
                return Ok(json!({"ok": true, "chains": crate::chains::as_json()}));
            }
            if let Some(sp) = body["gas_speed"].as_str() {
                crate::chains::set_gas_speed(id, sp)?;
                return Ok(json!({"ok": true, "chains": crate::chains::as_json()}));
            }
            if let Some(url) = body["test"].as_str() {
                return crate::chains::test_rpc(id, url);
            }
            if let Some(off) = body["off"].as_bool() {
                crate::chains::set_off(id, off)?;
                w.request(false);
                return Ok(json!({"ok": true, "chains": crate::chains::as_json()}));
            }
            crate::chains::set_enabled(id, body["enabled"].as_bool().unwrap_or(false))?;
            w.request(false);
            Ok(json!({"ok": true, "chains": crate::chains::as_json()}))
        }
        // /api/backtest/<id>/stop: end an optimization early (it keeps the best found so far)
        p if p.starts_with("/api/onboard/") => crate::onboard::post(&p["/api/onboard/".len()..], &body),
        p if p.starts_with("/api/agent/") => {
            if !crate::agent::enabled() {
                return Err(crate::experimental::OFF.into());
            }
            crate::agent::api_post(&p["/api/agent/".len()..], &body, w)
        }
        p if p.starts_with("/api/backtest/") && p.ends_with("/stop") => {
            let id = p["/api/backtest/".len()..p.len() - "/stop".len()].parse::<u64>().map_err(|_| "use /api/backtest/<id>/stop")?;
            Ok(json!({"ok": crate::optimize::stop(id)}))
        }
        "/api/backtest" => {
            let wallet = load_settings()["wallet"].as_str().unwrap_or("").to_string();
            Ok(json!({"ok": true, "id": crate::backtest::start(body, wallet)?}))
        }
        "/api/strategies" => crate::library::post(&body),
        "/api/ui_prefs" => set_ui_prefs(&body),
        "/api/pools/add" => {
            let cid = body["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
            Ok(json!({"ok": true, "pool": add_pool(body["pool"].as_str().unwrap_or(""), body["name"].as_str(), cid)?}))
        }
        "/api/pools/remove" => {
            let name = body["name"].as_str().unwrap_or("");
            let pools = load_pools();
            let p = pools.iter().find(|x| x["name"] == name).ok_or("unknown pool")?;
            let wallet = load_settings()["wallet"].as_str().unwrap_or("").to_string();
            if !wallet_positions(pool_chain(p), p["pool"].as_str().unwrap_or(""), &wallet)?.is_empty() {
                return Err("pool still holds positions: switch it off and let the worker withdraw first".into());
            }
            settings::update_pools(|pools| {
                pools.retain(|x| x["name"] != name);
                Ok(())
            })?;
            Ok(json!({"ok": true}))
        }
        // Per-pool capital (capital.rs): queued, carried out by the worker on its next pass
        "/api/capital" => {
            let cid = body["chain_id"].as_u64().unwrap_or(crate::chains::BASE);
            let pools = load_pools();
            let known = |a: &str| pools.iter().any(|p| pool_chain(p) == cid && p["pool"].as_str().map(|x| x.eq_ignore_ascii_case(a)).unwrap_or(false));
            let usd = crate::prices::fnum(&body["usd"]);
            let need_usd = || if usd.is_finite() && usd > 0.0 { Ok(()) } else { Err("enter an amount in $ above 0".to_string()) };
            let req = match body["action"].as_str().unwrap_or("") {
                // chosen tokens ({address, amount} or {address, all}), or a dollar amount
                "add" if body["tokens"].is_array() => {
                    let pool = body["pool"].as_str().filter(|a| known(a)).ok_or("unknown pool")?;
                    let mut toks = vec![];
                    for t in body["tokens"].as_array().into_iter().flatten() {
                        let a = t["address"].as_str().filter(|a| crate::agent::is_addr(a)).ok_or("a token address is not valid")?.to_lowercase();
                        let m = crate::prices::meta(cid, &a).map_err(|e| format!("{a}: not a token on this chain ({e})"))?;
                        let all = t["all"] == true;
                        let amount = crate::prices::fnum(&t["amount"]);
                        if !all && !(amount.is_finite() && amount > 0.0) {
                            return Err(format!("enter an amount of {} above 0", m.symbol));
                        }
                        toks.push(if all { json!({"address": a, "symbol": m.symbol, "all": true}) } else { json!({"address": a, "symbol": m.symbol, "amount": amount}) });
                    }
                    if toks.is_empty() {
                        return Err("choose at least one token".into());
                    }
                    json!({"kind": "add", "chain_id": cid, "pool": pool.to_lowercase(), "tokens": toks})
                }
                "add" => {
                    need_usd()?;
                    let pool = body["pool"].as_str().filter(|a| known(a)).ok_or("unknown pool")?;
                    json!({"kind": "add", "chain_id": cid, "pool": pool.to_lowercase(), "usd": usd})
                }
                "withdraw" | "move" => {
                    need_usd()?;
                    let from: Vec<String> = body["pools"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str().map(|s| s.to_lowercase())).collect();
                    if from.is_empty() || from.iter().any(|a| !known(a)) {
                        return Err("choose the pools to take it from".into());
                    }
                    let to = match body["to"].as_str() {
                        Some(t) if known(t) => Some(t.to_lowercase()),
                        Some(_) => return Err("unknown pool to move it to".into()),
                        None => None,
                    };
                    // `all`: everything the pools hold (a pool being switched off), done once they are empty;
                    // `sell`: sold to the chain's USDC even where the tokens would otherwise move as they are
                    let safe = if body["sell"] == true { Some(crate::chains::get(cid).usd.to_lowercase()) } else { None };
                    json!({"kind": "withdraw", "chain_id": cid, "pools": from, "usd": usd, "to": to, "all": body["all"] == true, "safe": safe})
                }
                "reset" => json!({"kind": "reset", "chain_id": cid}),
                "cancel" => {
                    let id = body["id"].as_str().unwrap_or("");
                    let gone: Vec<String> = crate::capital::requests().iter().filter_map(|r| r["id"].as_str())
                        .filter(|r| *r == id || r.starts_with(&format!("{id}-"))).map(String::from).collect();
                    for g in &gone {
                        crate::capital::remove(g);
                    }
                    return Ok(json!({"ok": !gone.is_empty()}));
                }
                a => return Err(format!("unknown action {a}")),
            };
            let id = crate::capital::push(req);
            w.request(false);
            Ok(json!({"ok": true, "id": id}))
        }
        "/api/control" => {
            match body["action"].as_str().unwrap_or("") {
                "run_now" => w.request(false),
                "claim_now" => w.request(true),
                "pause" => std::fs::write(crate::paths::stop_file(), "paused from the web UI\n").map_err(|e| e.to_string())?,
                "resume" => {
                    let _ = std::fs::remove_file(crate::paths::stop_file());
                    w.request(false);
                }
                // one wallet (?wallet=<id>): it signs nothing while paused (checked before every send)
                "pause_wallet" => {
                    let f = crate::paths::wallet_pause_file();
                    let _ = std::fs::create_dir_all(crate::paths::config());
                    std::fs::write(&f, "paused from the web UI\n").map_err(|e| e.to_string())?;
                    crate::info!("ui", "wallet {} paused", crate::paths::wallet());
                    w.request(false);
                }
                "resume_wallet" => {
                    let _ = std::fs::remove_file(crate::paths::wallet_pause_file());
                    crate::info!("ui", "wallet {} started", crate::paths::wallet());
                    w.request(false);
                }
                "resolve_now" => {
                    let _ = std::fs::remove_file(crate::paths::logs().join(format!("last_solve_{}.json", body["name"].as_str().unwrap_or(""))));
                    w.request(false);
                }
                "quit" => {
                    crate::desktop::request_quit_local();
                }
                a => return Err(format!("unknown action {a}")),
            }
            Ok(json!({"ok": true}))
        }
        // the wallet list (wallets.rs): add (watched until a key is set), rename / recolour, remove
        "/api/wallets" => match body["action"].as_str().unwrap_or("") {
            // with a private key or a recovery phrase account the wallet is created with its key in one
            // step (checked before anything is stored); without, it is watched only
            "add" => {
                let key = crate::wallets::key_from(&body)?;
                let addr = key.as_ref().map(|k| k.0.clone()).unwrap_or_else(|| body["address"].as_str().unwrap_or("").to_string());
                let w2 = crate::wallets::add(body["label"].as_str().unwrap_or(""), &addr, body["copy_pools_from"].as_str())?;
                if let Some((_, k)) = key {
                    let id = w2["id"].as_str().unwrap_or("").to_string();
                    crate::paths::scope(&id, || crate::onboard::save_key(&k))?;
                }
                w.request(false);
                Ok(json!({"ok": true, "wallet": w2, "address": addr}))
            }
            "edit" => {
                crate::wallets::edit(body["id"].as_str().unwrap_or(""), body["label"].as_str(), body["color"].as_str())?;
                Ok(json!({"ok": true, "wallets": crate::wallets::list()}))
            }
            "remove" => {
                let id = body["id"].as_str().unwrap_or("");
                if !crate::wallets::exists(id) {
                    return Err("no such wallet".into());
                }
                // positions on chain, read now: a wallet is removed only once it holds none
                let has = crate::paths::scope(id, || -> Result<bool, String> {
                    let wallet = load_settings()["wallet"].as_str().unwrap_or("").to_string();
                    if wallet.is_empty() {
                        return Ok(false);
                    }
                    for p in load_pools() {
                        if !wallet_positions(pool_chain(&p), p["pool"].as_str().unwrap_or(""), &wallet)?.is_empty() {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                })?;
                crate::wallets::remove(id, has)?;
                Ok(json!({"ok": true}))
            }
            a => Err(format!("unknown action {a}")),
        },
        "/api/key" => crate::onboard::save_key(body["private_key"].as_str().unwrap_or("")),
        "/api/key/delete" => {
            crate::signer::forget();
            let mut s = load_settings();
            if s["execute"].as_bool().unwrap_or(false) {
                s["execute"] = json!(false);
                save_settings(&s).map_err(|e| e.to_string())?;
            }
            Ok(json!({"ok": true}))
        }
        _ => Err("not found".into()),
    }
}

// -- dashboard preferences ----------------------------------------------------------------------------

/// The dashboard's own choices that should outlive a restart and a change of port (browser storage is
/// per port): config/ui.json, shared by every wallet. Only the keys below are kept.
const UI_PREFS: &[(&str, &[&str])] = &[("range_units", &["price", "tick"])];

fn ui_prefs_path() -> std::path::PathBuf {
    crate::paths::app_config().join("ui.json")
}

pub fn ui_prefs() -> Value {
    let saved: Value = std::fs::read_to_string(ui_prefs_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(json!({}));
    let mut out = json!({});
    for (k, allowed) in UI_PREFS {
        if let Some(v) = saved[*k].as_str().filter(|v| allowed.contains(v)) {
            out[*k] = json!(v);
        }
    }
    out
}

fn set_ui_prefs(body: &Value) -> Result<Value, String> {
    let mut cur = ui_prefs();
    for (k, v) in body.as_object().into_iter().flatten() {
        let Some((_, allowed)) = UI_PREFS.iter().find(|(name, _)| name == k) else { return Err(format!("unknown preference {k}")) };
        match v.as_str().filter(|v| allowed.contains(v)) {
            Some(v) => cur[k.as_str()] = json!(v),
            None => return Err(format!("{k}: expected one of {}", allowed.join(", "))),
        }
    }
    std::fs::create_dir_all(crate::paths::app_config()).map_err(|e| e.to_string())?;
    crate::settings::write_atomic(&ui_prefs_path(), &cur).map_err(|e| e.to_string())?;
    Ok(json!({"ok": true, "prefs": cur}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_labs_limits_land_on_the_pool_they_name_and_are_checked() {
        let g = crate::ladder::as_graph(&crate::ladder::Params::from(&crate::ladder::plain_defaults()).unwrap());
        let pools = vec![
            json!({"name": "a", "pool": "0x00000000000000000000000000000000000000a1", "chain_id": 8453, "max_actions_per_day": 5, "max_swap_cost_bps": 3.0, "strategy": g}),
            json!({"name": "b", "pool": "0x00000000000000000000000000000000000000b1", "chain_id": 8453, "max_actions_per_day": 5, "strategy": g}),
        ];
        let body = json!({"chain_id": 8453, "pool": "0x00000000000000000000000000000000000000A1", "max_actions_per_day": 12.0, "max_swap_cost_bps": 4.5});
        let (out, n) = super::set_limits(pools.clone(), &body).unwrap();
        assert_eq!(n, 1);
        assert_eq!((out[0]["max_actions_per_day"].clone(), out[0]["max_swap_cost_bps"].clone()), (json!(12), json!(4.5)));
        assert_eq!(out[1], pools[1]);
        // a key left out keeps the pool's own; another chain or address matches nothing
        let (out, _) = super::set_limits(pools.clone(), &json!({"chain_id": 8453, "pool": "0x00000000000000000000000000000000000000a1", "max_swap_cost_bps": 2.0})).unwrap();
        assert_eq!(out[0]["max_actions_per_day"], json!(5));
        assert_eq!(super::set_limits(pools.clone(), &json!({"chain_id": 10, "pool": "0x00000000000000000000000000000000000000a1"})).unwrap().1, 0);
        // out of range is refused as the Pools card would
        assert!(super::set_limits(pools, &json!({"chain_id": 8453, "pool": "0x00000000000000000000000000000000000000a1", "max_actions_per_day": 5000.0})).is_err());
    }

    #[test]
    fn the_value_history_leaves_out_mis_measured_cycles_and_keeps_real_moves() {
        let row = |ts: u64, v: f64| json!({"ts": ts, "total_usd": v, "value_usd": v, "chains": {"8453": {"incomplete": []}}});
        let ts = |rows: &[Value]| rows.iter().map(|r| r["ts"].as_u64().unwrap()).collect::<Vec<_>>();
        // one cycle where a pool could not be read: flagged, left out
        let mut bad = row(3, 6000.0);
        bad["chains"]["8453"]["incomplete"] = json!(["usdc-usdt: rate limited"]);
        let h = clean_history(vec![row(1, 15000.0), row(2, 15010.0), bad, row(4, 15005.0)]);
        assert_eq!(ts(&h), [1, 2, 4]);
        // the same dip without the flag (an old file, or an answer that was wrong but not an error)
        let h = clean_history(vec![row(1, 15000.0), row(2, 15010.0), row(3, 6000.0), row(4, 15005.0), row(5, 15002.0)]);
        assert_eq!(ts(&h), [1, 2, 4, 5]);
        // two and three cycles low, and a one-cycle spike
        let h = clean_history(vec![row(1, 100.0), row(2, 60.0), row(3, 61.0), row(4, 100.5), row(5, 160.0), row(6, 100.2)]);
        assert_eq!(ts(&h), [1, 4, 6]);
        // a withdrawal is a step: kept, and so is ordinary noise under 3%
        let h = clean_history(vec![row(1, 15000.0), row(2, 9000.0), row(3, 9001.0), row(4, 9002.0), row(5, 8900.0), row(6, 9003.0)]);
        assert_eq!(ts(&h), [1, 2, 3, 4, 5, 6]);
        // the newest cycle has no cycle after it to judge it by: kept until the next one
        let h = clean_history(vec![row(1, 100.0), row(2, 100.0), row(3, 40.0)]);
        assert_eq!(ts(&h), [1, 2, 3]);
    }

    #[test]
    fn ui_prefs_keep_only_known_values_and_survive_a_reread() {
        let d = crate::wallets::tests::temp_home("uiprefs");
        assert_eq!(ui_prefs(), json!({}));
        set_ui_prefs(&json!({"range_units": "tick"})).unwrap();
        assert_eq!(ui_prefs(), json!({"range_units": "tick"}));
        assert!(set_ui_prefs(&json!({"range_units": "bananas"})).is_err());
        assert!(set_ui_prefs(&json!({"wallet": "x"})).is_err());
        assert_eq!(ui_prefs(), json!({"range_units": "tick"}));
        // a hand-edited file with a bad value is read as nothing
        std::fs::write(ui_prefs_path(), r#"{"range_units": 3, "other": "x"}"#).unwrap();
        assert_eq!(ui_prefs(), json!({}));
        let _ = std::fs::remove_dir_all(d);
    }

    /// How long each page request takes on a copy of a real app folder (never the live one):
    /// ALM_HOME=<copy> cargo test -- --ignored page_timings --nocapture
    #[test]
    #[ignore]
    fn page_timings() {
        let home = std::env::var("ALM_HOME").expect("set ALM_HOME to a copy of the app folder");
        assert!(!home.to_lowercase().replace('\\', "/").trim_end_matches('/').ends_with("appdata/local/stablealm"), "not on the live folder");
        let w = Worker::new();
        for round in 0..2 {
            for p in ["/api/status", "/api/onboard", "/api/experimental", "/api/agent", "/api/chains", "/api/strategies", "/api/parking/names"] {
                let t = std::time::Instant::now();
                let (code, v) = get(p, &w);
                println!("round {round} {p:<22} {code} {:>7} ms {:>8} bytes", t.elapsed().as_millis(), v.to_string().len());
            }
        }
    }
}
