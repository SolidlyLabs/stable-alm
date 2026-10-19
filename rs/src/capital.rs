//! Per-pool capital: which of the wallet's free tokens belong to which pool.
//!
//! A pool's capital is its positions, the tokens it holds idle (this book) and the lots it parked
//! elsewhere. It grows with what the pool earns and shrinks with what it loses: nothing is moved
//! between pools by itself. Free tokens that no pool holds are Unallocated: deposits, and what a
//! withdrawal released. Money moves only by request (Add, Withdraw, Move), carried out by the worker.
//!
//! Pools run one after another, so what changes in the wallet while a pool runs is that pool's:
//! before its run everything above its own tokens is reserved (other pools' and Unallocated), and
//! after the run what is above the reserve is its new holding. The book never holds more of a token
//! than the wallet has: when the wallet has less (tokens sent out of it), every holding of that
//! token shrinks in proportion.
//!
//! live: logs/capital.json, dry run: logs/capital_dry.json; requests: logs/capital_requests.json.

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Mutex;

/// token address (lowercase) -> raw amount
pub type Tokens = BTreeMap<String, i128>;

static LOCK: Mutex<()> = Mutex::new(());

#[derive(Default, Clone, Debug, PartialEq)]
pub struct Book {
    /// Chains whose pools were given their first holdings (from their allocation %).
    pub initialized: std::collections::BTreeSet<u64>,
    /// chain -> pool address (lowercase) -> its idle tokens
    pub chains: BTreeMap<u64, BTreeMap<String, Tokens>>,
}

fn path(live: bool) -> std::path::PathBuf {
    crate::paths::logs().join(if live { "capital.json" } else { "capital_dry.json" })
}

fn req_path() -> std::path::PathBuf {
    crate::paths::logs().join("capital_requests.json")
}

impl Book {
    pub fn from_json(v: &Value) -> Book {
        let initialized = v["initialized"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64()).collect()).unwrap_or_default();
        let mut b = Book { initialized, chains: BTreeMap::new() };
        for (cid, pools) in v["chains"].as_object().cloned().unwrap_or_default() {
            let Ok(cid) = cid.parse::<u64>() else { continue };
            let c = b.chains.entry(cid).or_default();
            for (pool, toks) in pools.as_object().cloned().unwrap_or_default() {
                let t: Tokens = toks.as_object().cloned().unwrap_or_default().into_iter()
                    .filter_map(|(k, a)| Some((k.to_lowercase(), a.as_str()?.parse::<i128>().ok()?)))
                    .filter(|(_, a)| *a > 0)
                    .collect();
                c.insert(pool.to_lowercase(), t);
            }
        }
        b
    }

    pub fn to_json(&self) -> Value {
        let mut chains = Map::new();
        for (cid, pools) in &self.chains {
            let mut pm = Map::new();
            for (pool, toks) in pools {
                let t: Map<String, Value> = toks.iter().filter(|(_, a)| **a > 0).map(|(k, a)| (k.clone(), json!(a.to_string()))).collect();
                pm.insert(pool.clone(), Value::Object(t));
            }
            chains.insert(cid.to_string(), Value::Object(pm));
        }
        json!({"initialized": self.initialized, "chains": chains})
    }

    pub fn pool(&self, cid: u64, pool: &str) -> Tokens {
        self.chains.get(&cid).and_then(|c| c.get(&pool.to_lowercase())).cloned().unwrap_or_default()
    }

    pub fn set_pool(&mut self, cid: u64, pool: &str, t: Tokens) {
        let t: Tokens = t.into_iter().filter(|(_, a)| *a > 0).collect();
        self.chains.entry(cid).or_default().insert(pool.to_lowercase(), t);
    }

    /// Every pool's holding of each token on the chain, summed.
    pub fn held(&self, cid: u64) -> Tokens {
        let mut out = Tokens::new();
        for t in self.chains.get(&cid).map(|c| c.values().collect::<Vec<_>>()).unwrap_or_default() {
            for (k, a) in t {
                *out.entry(k.clone()).or_default() += a;
            }
        }
        out
    }
}

pub fn load(live: bool) -> Book {
    let _g = LOCK.lock().unwrap();
    std::fs::read_to_string(path(live)).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).map(|v| Book::from_json(&v)).unwrap_or_default()
}

pub fn save(live: bool, b: &Book) {
    let _g = LOCK.lock().unwrap();
    let _ = std::fs::create_dir_all(crate::paths::logs());
    if let Err(e) = crate::settings::write_atomic(&path(live), &b.to_json()) {
        crate::error!("capital", "capital book not saved: {e}");
    }
}

/// What a pool's run may not touch: the wallet's balance beyond the pool's own tokens.
pub fn reserve(own: &Tokens, wallet: &Tokens) -> Tokens {
    wallet.iter().map(|(t, w)| (t.clone(), (w - own.get(t).copied().unwrap_or(0)).max(0))).collect()
}

/// The pool's tokens after its run: what the wallet has above the reserve.
pub fn after_run(reserve: &Tokens, wallet: &Tokens) -> Tokens {
    wallet.iter().map(|(t, w)| (t.clone(), (w - reserve.get(t).copied().unwrap_or(0)).max(0))).filter(|(_, a)| *a > 0).collect()
}

/// Free tokens that no pool holds.
pub fn unallocated(held: &Tokens, wallet: &Tokens) -> Tokens {
    wallet.iter().map(|(t, w)| (t.clone(), (w - held.get(t).copied().unwrap_or(0)).max(0))).filter(|(_, a)| *a > 0).collect()
}

/// No pool holds more than the wallet has: a token the wallet has less of than the pools hold (sent
/// out of the wallet) is taken from every pool in proportion. Tokens the wallet was not read for are
/// left alone. Whether anything changed.
pub fn clamp(pools: &mut BTreeMap<String, Tokens>, wallet: &Tokens) -> bool {
    let mut changed = false;
    for (t, w) in wallet {
        let sum: i128 = pools.values().map(|p| p.get(t).copied().unwrap_or(0)).sum();
        if sum <= *w {
            continue;
        }
        let k = (*w).max(0) as f64 / sum as f64;
        for p in pools.values_mut() {
            if let Some(a) = p.get_mut(t) {
                *a = ((*a as f64) * k).floor() as i128;
            }
        }
        changed = true;
    }
    changed
}

/// Tokens worth `usd`, taken in the order given from what is available: (token, raw amount, USD per
/// raw unit). Less when not enough is available.
pub fn take(avail: &[(String, i128, f64)], usd: f64) -> Vec<(String, i128)> {
    let mut left = usd;
    let mut out = vec![];
    for (t, raw, px) in avail {
        if left <= 0.0 {
            break;
        }
        if *raw <= 0 || !(*px > 0.0) {
            continue;
        }
        let have = *raw as f64 * px;
        let n = if have <= left { *raw } else { ((left / px) as i128).min(*raw) };
        if n > 0 {
            left -= n as f64 * px;
            out.push((t.clone(), n));
        }
    }
    out
}

/// `total` USD split equally over the pools, none more than it has (`caps`): what one cannot give is
/// shared by the others.
pub fn split_equal(caps: &[(String, f64)], total: f64) -> Vec<(String, f64)> {
    let mut out: Vec<(String, f64)> = caps.iter().map(|(p, _)| (p.clone(), 0.0)).collect();
    let mut left = total.max(0.0);
    let mut open: Vec<usize> = (0..caps.len()).filter(|i| caps[*i].1 > 0.0).collect();
    while left > 1e-9 && !open.is_empty() {
        let each = left / open.len() as f64;
        let mut next = vec![];
        for i in open {
            let room = caps[i].1 - out[i].1;
            let g = each.min(room);
            out[i].1 += g;
            left -= g;
            if caps[i].1 - out[i].1 > 1e-9 {
                next.push(i);
            }
        }
        open = next;
    }
    out
}

/// A raw amount split by weights (the remainder of the rounding goes to the first).
pub fn split_by(amount: i128, weights: &[(String, f64)]) -> Vec<(String, i128)> {
    let sum: f64 = weights.iter().map(|w| w.1.max(0.0)).sum();
    if amount <= 0 || !(sum > 0.0) {
        return vec![];
    }
    let mut out: Vec<(String, i128)> = weights.iter().map(|(p, w)| (p.clone(), ((amount as f64) * w.max(0.0) / sum).floor() as i128)).collect();
    let given: i128 = out.iter().map(|x| x.1).sum();
    if let Some(first) = out.iter_mut().find(|x| x.1 > 0) {
        first.1 += (amount - given).max(0);
    }
    out.retain(|x| x.1 > 0);
    out
}

// -- requests (Add, Withdraw, Move, Reset), queued by the dashboard, carried out by the worker -------

pub fn requests() -> Vec<Value> {
    let _g = LOCK.lock().unwrap();
    std::fs::read_to_string(req_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn write_requests(r: &[Value]) {
    let _ = std::fs::create_dir_all(crate::paths::logs());
    if let Err(e) = crate::settings::write_atomic(&req_path(), &json!(r)) {
        crate::error!("capital", "capital requests not saved: {e}");
    }
}

/// Queues a request; its id.
pub fn push(mut req: Value) -> String {
    let id = format!("{:x}{:04x}", crate::engine::now(), rand_u16());
    req["id"] = json!(id);
    req["ts"] = json!(crate::engine::now());
    let mut r = requests();
    let _g = LOCK.lock().unwrap();
    r.push(req);
    write_requests(&r);
    id
}

fn rand_u16() -> u16 {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    (n ^ (n >> 16)) as u16
}

/// Removes a request (cancelled, or carried out). Whether it was there.
pub fn remove(id: &str) -> bool {
    let mut r = requests();
    let _g = LOCK.lock().unwrap();
    let n = r.len();
    r.retain(|x| x["id"].as_str() != Some(id));
    write_requests(&r);
    r.len() != n
}

/// Replaces a request by its id (a withdrawal split into one release per pool).
pub fn replace(id: &str, with: Vec<Value>) {
    let mut r = requests();
    let _g = LOCK.lock().unwrap();
    if let Some(i) = r.iter().position(|x| x["id"].as_str() == Some(id)) {
        r.splice(i..i + 1, with);
        write_requests(&r);
    }
}

// -- the worker's side: tokens followed, balances, values, requests carried out -------------------

use crate::settings::PoolConfig;

/// USD per raw unit of `token`.
pub fn px_raw(cid: u64, token: &str) -> Result<f64, String> {
    let m = crate::prices::meta(cid, token)?;
    Ok(crate::prices::usd(cid, token)? / 10f64.powi(m.decimals as i32))
}

/// USD value of some tokens (a token that cannot be priced counts 0).
pub fn value(cid: u64, toks: &Tokens) -> f64 {
    toks.iter().filter(|(_, a)| **a > 0).map(|(t, a)| px_raw(cid, t).map(|p| *a as f64 * p).unwrap_or(0.0)).sum()
}

/// The tokens the book follows on a chain: the pools' own, the funding tokens, the chain's USD stable,
/// SOLID and the reward tokens sold, and whatever a pool holds already (tokens that exist there).
pub fn universe(cid: u64, pools: &[&PoolConfig], s: &Value, acct: &Value, book: &Book) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut add = |t: &str| {
        let t = t.to_lowercase();
        if t.len() == 42 && !out.contains(&t) {
            out.push(t);
        }
    };
    for v in acct["chains"][cid.to_string()]["tokens"].as_object().map(|o| o.values().cloned().collect::<Vec<_>>()).unwrap_or_default() {
        add(v["address"].as_str().unwrap_or(""));
    }
    for c in pools {
        if let Ok((a, b)) = crate::chain::pool_tokens(cid, &c.pool) {
            add(&a);
            add(&b);
        }
    }
    for t in crate::settings::funding_tokens(s, cid) {
        add(&t);
    }
    let ch = crate::chains::get(cid);
    add(&ch.usd);
    add(&ch.solid);
    for t in s["sell_rewards"]["tokens"].as_array().cloned().unwrap_or_default() {
        add(t.as_str().unwrap_or(""));
    }
    for t in book.held(cid).keys() {
        add(t);
    }
    out.retain(|t| crate::prices::meta(cid, t).is_ok());
    out.sort();
    out
}

/// The wallet's raw balances of `toks`, all or nothing (tried 3 times).
pub fn balances(cid: u64, toks: &[String], wallet: &str) -> Result<Tokens, String> {
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        match crate::chain::erc20_balances(cid, toks, wallet) {
            Ok(rs) => {
                let mut out = Tokens::new();
                let mut bad = None;
                for (t, r) in toks.iter().zip(rs) {
                    match r {
                        Ok(b) => {
                            out.insert(t.clone(), num_traits::ToPrimitive::to_i128(&b).unwrap_or(i128::MAX));
                        }
                        Err(e) => bad = Some(format!("balance of {t}: {e}")),
                    }
                }
                match bad {
                    None => return Ok(out),
                    Some(e) => last = e,
                }
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// What a pool's capital is made of, in USD.
#[derive(Default, Clone, Debug)]
pub struct Parts {
    /// its own positions (lots parked in it by other pools are theirs)
    pub positions: f64,
    /// its idle tokens (the book)
    pub idle: f64,
    /// the lots it parked in other pools
    pub lots: f64,
    /// claims it holds for pools that parked with it (inside its positions or idle tokens)
    pub hosted: f64,
    /// claims it has on other pools
    pub owed: f64,
}

impl Parts {
    /// What the pool manages: its positions, idle tokens and lots.
    pub fn budget(&self) -> f64 {
        (self.positions + self.idle + self.lots).max(0.0)
    }
    /// What is the pool's own: what it manages, less what it holds for others, plus what others hold for it.
    pub fn capital(&self) -> f64 {
        (self.budget() - self.hosted + self.owed).max(0.0)
    }
}

pub fn parts(live: bool, cid: u64, cfg: &PoolConfig, acct: &Value, own: &Tokens) -> Parts {
    let lots = acct["parked"][cid.to_string()].as_array().cloned().unwrap_or_default().iter()
        .filter(|x| x["kind"] != "claim" && x["origin"].as_str().map(|o| o.eq_ignore_ascii_case(&cfg.pool)).unwrap_or(false))
        .map(|x| x["usd_now"].as_f64().unwrap_or(0.0))
        .sum();
    let (hosted, owed) = crate::parking::claims_usd(live, cid, &cfg.pool);
    Parts { positions: acct["pools"][&cfg.name]["deployed_usd"].as_f64().unwrap_or(0.0), idle: value(cid, own), lots, hosted, owed }
}

/// `avail` as (token, raw, USD per raw), the tokens in `first` first (in that order), then the rest
/// by value.
fn ordered(cid: u64, avail: &Tokens, first: &[String]) -> Vec<(String, i128, f64)> {
    let mut rest: Vec<(String, i128, f64)> = avail.iter()
        .filter(|(t, a)| **a > 0 && !first.contains(t))
        .filter_map(|(t, a)| Some((t.clone(), *a, px_raw(cid, t).ok()?)))
        .collect();
    rest.sort_by(|a, b| (b.1 as f64 * b.2).partial_cmp(&(a.1 as f64 * a.2)).unwrap_or(std::cmp::Ordering::Equal));
    let mut seen: Vec<&String> = vec![];
    let mut out: Vec<(String, i128, f64)> = first.iter()
        .filter(|t| if seen.contains(t) { false } else { seen.push(t); true })
        .filter_map(|t| Some((t.clone(), *avail.get(t).filter(|a| **a > 0)?, px_raw(cid, t).ok()?)))
        .collect();
    out.extend(rest);
    out
}

fn add_to(book: &mut Book, cid: u64, pool: &str, got: &[(String, i128)]) {
    let mut own = book.pool(cid, pool);
    for (t, a) in got {
        *own.entry(t.clone()).or_default() += a;
    }
    book.set_pool(cid, pool, own);
}

fn describe(cid: u64, got: &[(String, i128)]) -> String {
    let v: Vec<String> = got.iter().map(|(t, a)| {
        let m = crate::prices::meta(cid, t);
        let sym = m.as_ref().map(|m| m.symbol.clone()).unwrap_or_else(|_| t[..8.min(t.len())].to_string());
        let n = m.map(|m| *a as f64 / 10f64.powi(m.decimals as i32)).unwrap_or(*a as f64);
        format!("{} {sym}", crate::engine::commas_p(n, 4))
    }).collect();
    if v.is_empty() { "nothing".into() } else { v.join(" + ") }
}

/// Funds the enabled pools from Unallocated up to their allocation % of the chain's value (what they
/// manage already counts): how the book starts, and what Reset does. Each pool gets its own tokens
/// first, then the chain's stables, then anything else.
fn fill_from_allocations(book: &mut Book, cid: u64, pools: &[&PoolConfig], acct: &Value, wallet: &Tokens, live: bool) {
    let total = acct["chains"][cid.to_string()]["total_usd"].as_f64().unwrap_or(0.0);
    for cfg in pools.iter().filter(|c| c.enabled && c.allocation_pct > 0.0) {
        let own = book.pool(cid, &cfg.pool);
        let p = parts(live, cid, cfg, acct, &own);
        let share = total * cfg.allocation_pct / 100.0 + p.hosted - p.owed;
        let need = share - p.budget();
        if need < 1.0 {
            continue;
        }
        let mut first = crate::chain::pool_tokens(cid, &cfg.pool).map(|(a, b)| vec![a, b]).unwrap_or_default();
        first.push(crate::chains::get(cid).usd.to_lowercase());
        first.extend(crate::chains::default_funding(cid));
        let free = unallocated(&book.held(cid), wallet);
        let got = take(&ordered(cid, &free, &first), need);
        crate::info!("capital", "[{}] given {} (${}) from Unallocated, to reach its {}%", cfg.name, describe(cid, &got), crate::engine::commas(value(cid, &got.iter().cloned().collect())), cfg.allocation_pct);
        add_to(book, cid, &cfg.pool, &got);
    }
}

/// Moves about `usd` of a pool's idle tokens, as they are (USDC first), to another pool or to
/// Unallocated (`to` None): a withdrawal in a dry run, or from a pool that no longer runs. USD moved.
pub fn move_in_book(book: &mut Book, cid: u64, pool: &str, usd: f64, to: Option<&str>) -> f64 {
    let own = book.pool(cid, pool);
    let usdc = crate::chains::get(cid).usd.to_lowercase();
    let got = take(&ordered(cid, &own, &[usdc]), usd);
    let mut left = own;
    for (t, a) in &got {
        *left.entry(t.clone()).or_default() -= a;
    }
    book.set_pool(cid, pool, left);
    if let Some(to) = to {
        add_to(book, cid, to, &got);
    }
    value(cid, &got.into_iter().collect())
}

fn name_of(pools: &[&PoolConfig], addr: &str) -> String {
    pools.iter().find(|c| c.pool.eq_ignore_ascii_case(addr)).map(|c| c.name.clone()).unwrap_or_else(|| addr.to_string())
}

/// At the start of a cycle, for one chain: the book kept within the wallet, started (once) from the
/// allocation %, and the queued Add, Reset and Withdraw requests carried out (a withdrawal becomes one
/// release per pool, carried out when that pool runs).
pub fn prepare(book: &mut Book, cid: u64, pools: &[&PoolConfig], acct: &Value, wallet: &Tokens, live: bool) {
    if clamp(book.chains.entry(cid).or_default(), wallet) {
        crate::warn!("capital", "chain {cid}: the wallet holds less of some tokens than the pools did (sent out?): every pool's share of them shrank in proportion");
    }
    if !book.initialized.contains(&cid) {
        fill_from_allocations(book, cid, pools, acct, wallet, live);
        book.initialized.insert(cid);
    }
    for r in requests().into_iter().filter(|r| r["chain_id"].as_u64() == Some(cid)) {
        let id = r["id"].as_str().unwrap_or("").to_string();
        let usd = r["usd"].as_f64().unwrap_or(0.0);
        match r["kind"].as_str().unwrap_or("") {
            "add" => {
                let pool = r["pool"].as_str().unwrap_or("").to_lowercase();
                let mut first = vec![crate::chains::get(cid).usd.to_lowercase()];
                first.extend(crate::chains::default_funding(cid));
                if let Ok((a, b)) = crate::chain::pool_tokens(cid, &pool) {
                    first.extend([a, b]);
                }
                let free = unallocated(&book.held(cid), wallet);
                let got = take(&ordered(cid, &free, &first), usd);
                let moved = value(cid, &got.iter().cloned().collect());
                crate::info!("capital", "[{}] added {} (${}) from Unallocated (asked ${})", name_of(pools, &pool), describe(cid, &got), crate::engine::commas(moved), crate::engine::commas(usd));
                crate::engine::log(json!({"chain_id": cid, "pool": pool, "kind": "capital", "what": "add", "usd": moved, "asked_usd": usd}));
                add_to(book, cid, &pool, &got);
                remove(&id);
            }
            "reset" => {
                if let Some(c) = book.chains.get_mut(&cid) {
                    c.clear();
                }
                fill_from_allocations(book, cid, pools, acct, wallet, live);
                crate::info!("capital", "chain {cid}: capital reset to the allocation %");
                crate::engine::log(json!({"chain_id": cid, "kind": "capital", "what": "reset"}));
                remove(&id);
            }
            "withdraw" => {
                let chosen: Vec<String> = r["pools"].as_array().cloned().unwrap_or_default().iter().filter_map(|p| p.as_str().map(|s| s.to_lowercase())).collect();
                let to = r["to"].as_str().map(|s| s.to_lowercase());
                let caps: Vec<(String, f64)> = chosen.iter()
                    .filter(|p| Some(p.as_str()) != to.as_deref())
                    .filter_map(|p| pools.iter().find(|c| c.pool.eq_ignore_ascii_case(p)).map(|c| (p.clone(), parts(live, cid, c, acct, &book.pool(cid, p)).capital())))
                    .collect();
                let split = split_equal(&caps, usd);
                let given: f64 = split.iter().map(|x| x.1).sum();
                if given + 1.0 < usd {
                    crate::warn!("capital", "withdrawal of ${} asked; the pools chosen have ${} between them", crate::engine::commas(usd), crate::engine::commas(given));
                }
                let rel: Vec<Value> = split.into_iter().filter(|x| x.1 >= 1.0).enumerate()
                    // a plan step or an emergency exit carries its target asset, its cost bound and its tag
                    .map(|(i, (p, u))| json!({"id": format!("{id}-{i}"), "ts": r["ts"], "kind": "release", "chain_id": cid, "pool": p, "usd": u, "to": to,
                                             "safe": r["safe"], "max_cost_bp": r["max_cost_bp"], "plan": r["plan"], "emergency": r["emergency"]}))
                    .collect();
                crate::info!("capital", "withdrawal of ${} split over {} pools; each pool releases its part when it runs", crate::engine::commas(usd), rel.len());
                replace(&id, rel);
            }
            _ => {}
        }
    }
}

/// The releases (withdrawal parts) waiting for a pool.
pub fn releases(cid: u64, pool: &str) -> Vec<Value> {
    requests().into_iter().filter(|r| r["kind"] == "release" && r["chain_id"].as_u64() == Some(cid) && r["pool"].as_str().map(|p| p.eq_ignore_ascii_case(pool)).unwrap_or(false)).collect()
}

/// Reward tokens a claim brought in, given to the pools that earned them (the claim's per-pool
/// amounts); what unmanaged positions earned stays Unallocated.
pub fn credit_claim(book: &mut Book, cid: u64, pools: &[&PoolConfig], rewards: &Value, before: &Tokens, after: &Tokens) {
    for (t, a) in after {
        let got = a - before.get(t).copied().unwrap_or(0);
        if got <= 0 {
            continue;
        }
        let w: Vec<(String, f64)> = rewards.as_array().cloned().unwrap_or_default().iter()
            .filter(|r| r["token"].as_str().map(|x| x.eq_ignore_ascii_case(t)).unwrap_or(false))
            .map(|r| (r["pool"].as_str().unwrap_or("").to_lowercase(), crate::prices::fnum(&r["claimable"])))
            .collect();
        let parts = if w.iter().any(|x| x.1 > 0.0) { split_by(got, &w) } else { vec![] };
        for (p, n) in parts {
            if pools.iter().any(|c| c.pool.eq_ignore_ascii_case(&p)) {
                add_to(book, cid, &p, &[(t.clone(), n)]);
            }
        }
    }
}

/// The dashboard's view of one chain: each pool's capital and its parts, and Unallocated.
pub fn status(book: &Book, cid: u64, pools: &[&PoolConfig], acct: &Value, wallet: &Tokens, live: bool) -> Value {
    let toks_json = |t: &Tokens| -> Vec<Value> {
        t.iter().filter(|(_, a)| **a > 0).filter_map(|(k, a)| {
            let m = crate::prices::meta(cid, k).ok()?;
            let amt = *a as f64 / 10f64.powi(m.decimals as i32);
            Some(json!({"address": k, "symbol": m.symbol, "amount": amt, "usd": px_raw(cid, k).map(|p| *a as f64 * p).ok()}))
        }).collect()
    };
    let mut pm = Map::new();
    let mut total = 0.0;
    for c in pools {
        let own = book.pool(cid, &c.pool);
        let p = parts(live, cid, c, acct, &own);
        total += p.capital();
        pm.insert(c.name.clone(), json!({"pool": c.pool, "capital_usd": p.capital(), "positions_usd": p.positions, "idle_usd": p.idle,
                                        "lots_usd": p.lots, "hosted_usd": p.hosted, "owed_usd": p.owed, "idle": toks_json(&own)}));
    }
    let free = unallocated(&book.held(cid), wallet);
    let pending: Vec<Value> = requests().into_iter().filter(|r| r["chain_id"].as_u64() == Some(cid)).collect();
    json!({"pools": pm, "pools_usd": total, "unallocated_usd": value(cid, &free), "unallocated": toks_json(&free), "pending": pending,
           "usdc": crate::chains::get(cid).usd.to_lowercase()})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(v: &[(&str, i128)]) -> Tokens {
        v.iter().map(|(k, a)| (k.to_string(), *a)).collect()
    }

    #[test]
    fn a_run_keeps_what_it_earned_and_never_touches_the_rest() {
        let wallet = toks(&[("usdc", 1000), ("weth", 50)]);
        let own = toks(&[("usdc", 300)]);
        let r = reserve(&own, &wallet);
        assert_eq!(r, toks(&[("usdc", 700), ("weth", 50)]));
        // it spent 200 USDC and got 3 WETH
        let after = after_run(&r, &toks(&[("usdc", 800), ("weth", 53)]));
        assert_eq!(after, toks(&[("usdc", 100), ("weth", 3)]));
        // it can never end with more than it was given plus its own gains
        assert_eq!(after_run(&r, &toks(&[("usdc", 600), ("weth", 50)])), toks(&[]));
    }

    #[test]
    fn unallocated_is_what_no_pool_holds() {
        let held = toks(&[("usdc", 300), ("weth", 60)]);
        assert_eq!(unallocated(&held, &toks(&[("usdc", 1000), ("weth", 50)])), toks(&[("usdc", 700)]));
    }

    #[test]
    fn tokens_sent_out_shrink_every_holding_in_proportion() {
        let mut pools = BTreeMap::new();
        pools.insert("a".to_string(), toks(&[("usdc", 600), ("weth", 5)]));
        pools.insert("b".to_string(), toks(&[("usdc", 400)]));
        assert!(clamp(&mut pools, &toks(&[("usdc", 500), ("weth", 5)])));
        assert_eq!(pools["a"]["usdc"], 300);
        assert_eq!(pools["b"]["usdc"], 200);
        assert_eq!(pools["a"]["weth"], 5);
        assert!(!clamp(&mut pools, &toks(&[("usdc", 500)])));
    }

    #[test]
    fn take_goes_in_order_and_stops_at_the_amount() {
        let avail = vec![("usdc".to_string(), 100_000_000i128, 1e-6), ("usdt".to_string(), 500_000_000, 1e-6)];
        assert_eq!(take(&avail, 250.0), vec![("usdc".to_string(), 100_000_000), ("usdt".to_string(), 150_000_000)]);
        assert_eq!(take(&avail, 1000.0).iter().map(|x| x.1).sum::<i128>(), 600_000_000);
    }

    #[test]
    fn an_equal_split_passes_on_what_a_small_pool_cannot_give() {
        let caps = vec![("a".to_string(), 1000.0), ("b".to_string(), 100.0), ("c".to_string(), 1000.0)];
        let s = split_equal(&caps, 900.0);
        assert!((s[0].1 - 400.0).abs() < 1e-6 && (s[1].1 - 100.0).abs() < 1e-6 && (s[2].1 - 400.0).abs() < 1e-6, "{s:?}");
        let all = split_equal(&caps, 5000.0);
        assert!((all.iter().map(|x| x.1).sum::<f64>() - 2100.0).abs() < 1e-6);
    }

    #[test]
    fn split_by_weights_hands_out_the_whole_amount() {
        let s = split_by(1001, &[("a".into(), 1.0), ("b".into(), 1.0)]);
        assert_eq!(s.iter().map(|x| x.1).sum::<i128>(), 1001);
        assert!(split_by(10, &[("a".into(), 0.0)]).is_empty());
    }

    #[test]
    fn the_book_round_trips_through_json() {
        let mut b = Book { initialized: [8453].into_iter().collect(), ..Default::default() };
        b.set_pool(8453, "0xAB", toks(&[("0xt", 12345678901234567890123)]));
        let back = Book::from_json(&b.to_json());
        assert_eq!(back, b);
        assert_eq!(back.pool(8453, "0xab")["0xt"], 12345678901234567890123);
        assert_eq!(back.held(8453)["0xt"], 12345678901234567890123);
    }
}
