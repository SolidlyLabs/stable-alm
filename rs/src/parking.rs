//! Parking: while a strategy waits out of its pool in one token (an exit into token0 or token1), the
//! tokens it withdrew earn in a whitelisted pegged pool of that token (USDC in USDC/USDT, wS in
//! wS/stS) until the strategy needs them again (reports/DIRECTIONAL.md).
//!
//! Opt-in twice: settings `parking.enabled` with its whitelist (`parking.pools`: chain, token, pool)
//! and the strategy's own `"park": true`.
//!
//! What is parked is kept as lots, one per park, tagged with the pool it came from (the origin):
//! the parking pool's range and the liquidity minted for it. A lot's liquidity is not the parking
//! pool's own: `engine::wallet_positions` leaves it out, so a strategy running on the parking pool
//! never moves it, and a position the wallet already had there (even on the same range) is never
//! touched by an unpark: only the lot's own liquidity is withdrawn, then swapped back into the
//! origin's token. The ledger is logs/parking.json (logs/parking_dry.json for dry runs).
//!
//! Lots come first on a range they share with the parking pool's own positions, in the order they
//! were parked: whatever that pool's strategy does (split into more ranges, withdraw), it only sees
//! and moves what is left after them. An unpark withdraws at most what is still there, so a lot
//! whose liquidity went elsewhere (withdrawn by hand) is dropped instead of failing every cycle.
//!
//! Each whitelist entry may set how its lots are held (`strategy`): "fixed" (the default: one range
//! ± `range_pct` at the time of the park) or "recentre" (moved back around the price once it has
//! been outside the lot's range for `recentre_after_min` minutes), and its own `range_pct`.
//!
//! Claims: a parking pool that runs a strategy of its own (configured, switched on, a share above
//! 0%) is not given a range of the lot's own. The tokens become that pool's to place: its budget
//! grows by the claim and its strategy (the ladder, say) deploys them with everything else, while the
//! parking pool's budget shrinks by it. The claim is the token amount parked; on Solidly V3 swap
//! fees and emissions are paid to the wallet through reward claims rather than into positions, so
//! what a pegged host's positions add or lose on top of that is its price drift, near nothing. An
//! unpark pays the claim back: from the wallet first, the rest withdrawn from the host's own
//! positions in proportion and swapped into the token.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug)]
pub struct Whitelisted {
    pub pool: String,
    /// half the parking range's width, in percent of the price
    pub range_pct: f64,
    /// move the lot back around the price after this many minutes outside its range (None: fixed)
    pub recentre_after_min: Option<f64>,
}

/// The parking pool for `token` on chain `cid`, when parking is on and the token is whitelisted.
pub fn pool_for(s: &Value, cid: u64, token: &str) -> Option<Whitelisted> {
    if !s["parking"]["enabled"].as_bool().unwrap_or(false) {
        return None;
    }
    whitelisted(s, cid, token)
}

/// Whether live parking is switched on (Settings › Options).
pub fn enabled(s: &Value) -> bool {
    s["parking"]["enabled"].as_bool().unwrap_or(false)
}

/// The token's whitelisted pool whether or not live parking is on: what a backtest uses (it risks
/// nothing, so the strategy's own parking switch and the whitelist are enough).
pub fn whitelisted(s: &Value, cid: u64, token: &str) -> Option<Whitelisted> {
    let p = &s["parking"];
    let range_pct = p["range_pct"].as_f64().filter(|x| *x > 0.0).unwrap_or(0.5);
    p["pools"].as_array()?.iter().find_map(|e| {
        let same = e["chain_id"].as_u64() == Some(cid) && e["token"].as_str().map(|t| t.eq_ignore_ascii_case(token)).unwrap_or(false);
        same.then(|| entry(e, range_pct)).filter(|w| w.pool.len() == 42)
    })
}

/// A whitelist entry with its holding strategy; `range_pct` is the global default.
fn entry(e: &Value, range_pct: f64) -> Whitelisted {
    let recentre = e["strategy"].as_str() == Some("recentre");
    Whitelisted {
        pool: e["pool"].as_str().unwrap_or("").to_lowercase(),
        range_pct: e["range_pct"].as_f64().filter(|x| *x > 0.0).unwrap_or(range_pct),
        recentre_after_min: recentre.then(|| e["recentre_after_min"].as_f64().filter(|x| *x >= 0.0).unwrap_or(60.0)),
    }
}

/// The whitelist entry a lot was parked under, by its chain, token and pool (it may have changed
/// since: then None, and the lot keeps its range).
pub fn entry_of(s: &Value, lot: &Value) -> Option<Whitelisted> {
    let (cid, token, pool) = (lot["chain_id"].as_u64()?, lot["token"].as_str()?, lot["pool"].as_str()?);
    whitelisted(s, cid, token).filter(|w| w.pool.eq_ignore_ascii_case(pool))
}

/// The smallest value worth parking (two transactions out, two back).
pub fn min_usd(s: &Value) -> f64 {
    s["parking"]["min_usd"].as_f64().unwrap_or(50.0)
}

/// Whether a strategy has parking switched on.
pub fn wanted(strategy: &serde_json::Map<String, Value>) -> bool {
    strategy.get("park").and_then(|v| v.as_bool()).unwrap_or(false)
}

fn path(live: bool) -> std::path::PathBuf {
    crate::paths::logs().join(if live { "parking.json" } else { "parking_dry.json" })
}

/// Every lot: {id, chain_id, origin, pool, token, tl, tu, liquidity (string), amount (raw token units), usd, ts}.
pub fn lots(live: bool) -> Vec<Value> {
    let _g = LOCK.lock().unwrap();
    read(live)
}

/// The ledger as saved; the caller holds LOCK across its write too (a change in between is lost).
fn read(live: bool) -> Vec<Value> {
    std::fs::read_to_string(path(live)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Whether it was saved (an error is logged).
fn write(live: bool, l: &[Value]) -> bool {
    let p = path(live);
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    match crate::settings::write_atomic(&p, &Value::Array(l.to_vec())) {
        Ok(()) => true,
        Err(e) => {
            crate::error!("parking", "parking ledger not saved: {e}");
            false
        }
    }
}

pub fn add(live: bool, lot: Value) -> bool {
    let _g = LOCK.lock().unwrap();
    let mut l = read(live);
    l.push(lot);
    write(live, &l)
}

/// Replace the lot with the same id.
pub fn update(live: bool, lot: Value) -> bool {
    let _g = LOCK.lock().unwrap();
    let mut l = read(live);
    for x in l.iter_mut() {
        if x["id"] == lot["id"] {
            *x = lot.clone();
        }
    }
    write(live, &l)
}

pub fn remove(live: bool, id: &str) -> bool {
    let _g = LOCK.lock().unwrap();
    let mut l = read(live);
    l.retain(|x| x["id"].as_str() != Some(id));
    write(live, &l)
}

/// The lots parked by the strategy of `origin` on chain `cid`.
pub fn of_origin(live: bool, cid: u64, origin: &str) -> Vec<Value> {
    lots(live).into_iter().filter(|x| x["chain_id"].as_u64() == Some(cid) && x["origin"].as_str().map(|o| o.eq_ignore_ascii_case(origin)).unwrap_or(false)).collect()
}

/// Liquidity parked in `pool` per range (live lots only): left out of that pool's own positions.
pub fn parked_in(cid: u64, pool: &str) -> HashMap<(i64, i64), u128> {
    by_range(&lots(true), cid, pool)
}

/// A lot held as a claim on a pool that runs its own strategy (no range of its own).
pub fn is_claim(x: &Value) -> bool {
    x["kind"] == "claim"
}

/// The strategy pool `pool` is, when it runs one now (configured on chain `cid`, switched on): what is
/// parked there becomes a claim on it. Its name.
pub fn host(cid: u64, pool: &str) -> Option<String> {
    crate::settings::load_pools().iter().find_map(|p| {
        let c = crate::settings::PoolConfig::from(p);
        (crate::worker::pool_chain(p) == cid && c.pool.eq_ignore_ascii_case(pool) && c.enabled).then(|| c.name.clone())
    })
}

/// A claim's value now (USD): its token amount at today's price (what was parked, when the price
/// cannot be read).
fn claim_usd(cid: u64, x: &Value) -> f64 {
    let raw: f64 = x["amount"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let t = x["token"].as_str().unwrap_or("");
    match (crate::prices::meta(cid, t), crate::prices::usd(cid, t)) {
        (Ok(m), Ok(px)) => raw / 10f64.powi(m.decimals as i32) * px,
        _ => x["usd"].as_f64().unwrap_or(0.0),
    }
}

/// Claims on chain `cid` that `pool` holds for others and that it owes (as their origin), in USD:
/// (hosted, parked by it). A pool's budget is its share plus what it hosts less what it parked.
pub fn claims_usd(live: bool, cid: u64, pool: &str) -> (f64, f64) {
    split_claims(&lots(live), cid, pool, &|x| claim_usd(cid, x))
}

fn split_claims(lots: &[Value], cid: u64, pool: &str, usd: &dyn Fn(&Value) -> f64) -> (f64, f64) {
    let (mut hosted, mut owed) = (0.0, 0.0);
    // a claim paid back ("repaid") is settled in the book after the run: no longer owed or hosted
    for x in lots.iter().filter(|x| is_claim(x) && x["chain_id"].as_u64() == Some(cid) && x["stage"] != "repaid") {
        if x["pool"].as_str().map(|p| p.eq_ignore_ascii_case(pool)).unwrap_or(false) {
            hosted += usd(x);
        }
        if x["origin"].as_str().map(|p| p.eq_ignore_ascii_case(pool)).unwrap_or(false) {
            owed += usd(x);
        }
    }
    (hosted, owed)
}

fn in_pool(x: &Value, cid: u64, pool: &str) -> bool {
    // a claim holds no liquidity of its own: the host's positions are all the host's
    !is_claim(x) && x["chain_id"].as_u64() == Some(cid) && x["pool"].as_str().map(|p| p.eq_ignore_ascii_case(pool)).unwrap_or(false)
}

fn liq(x: &Value) -> u128 {
    x["liquidity"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0)
}

fn rng(x: &Value) -> (i64, i64) {
    (x["tl"].as_i64().unwrap_or(0), x["tu"].as_i64().unwrap_or(0))
}

/// The lots' liquidity in `pool` per range.
pub fn by_range(lots: &[Value], cid: u64, pool: &str) -> HashMap<(i64, i64), u128> {
    let mut out = HashMap::new();
    for x in lots.iter().filter(|x| in_pool(x, cid, pool)) {
        *out.entry(rng(x)).or_default() += liq(x);
    }
    out
}

/// What the parking pool's own strategy may treat as its own on a range holding `onchain` liquidity.
pub fn own_share(onchain: u128, parked: u128) -> u128 {
    onchain.saturating_sub(parked)
}

/// How much of `lot`'s liquidity an unpark can withdraw from its range, which holds `onchain`
/// liquidity: the lots parked there before it come first, the pool's own positions last.
pub fn withdrawable(lot: &Value, lots: &[Value], onchain: u128) -> u128 {
    let (cid, pool) = (lot["chain_id"].as_u64().unwrap_or(0), lot["pool"].as_str().unwrap_or(""));
    let mut ahead = 0u128;
    for x in lots.iter().filter(|x| in_pool(x, cid, pool) && rng(x) == rng(lot)) {
        if x["id"] == lot["id"] {
            break;
        }
        ahead += liq(x);
    }
    onchain.saturating_sub(ahead).min(liq(lot))
}

/// The wallet's liquidity on one range of `pool`, on-chain.
pub fn onchain(cid: u64, pool: &str, wallet: &str, tl: i64, tu: i64) -> Result<u128, String> {
    let calls = vec![(pool.to_lowercase(), crate::chain::position_calldata(wallet, tl, tu))];
    let r = crate::chain::eth_calls(cid, &calls)?.pop().ok_or("no answer")??;
    u128::try_from(crate::eth::word_u(&r, 0)).map_err(|e| e.to_string())
}

/// USD value of the live lots on chain `cid` now, and each lot with its value (for the dashboard and
/// the account total: parked liquidity is not counted in any pool's positions).
pub fn value(cid: u64) -> (f64, Vec<Value>) {
    let mut total = 0.0;
    let mut out = vec![];
    for mut x in lots(true).into_iter().filter(|x| x["chain_id"].as_u64() == Some(cid)) {
        if is_claim(&x) {
            // its tokens are in the host's positions or the wallet, both counted already
            x["usd_now"] = json!(claim_usd(cid, &x));
            out.push(x);
            continue;
        }
        let r = (|| -> Result<f64, String> {
            let pool = x["pool"].as_str().unwrap_or("");
            let (t0, t1) = crate::chain::pool_tokens(cid, pool)?;
            let (sp, _, _) = crate::chain::slot0(cid, pool)?;
            let liq: u128 = x["liquidity"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
            let (a0, a1) = crate::tickmath::amounts_at(&sp, x["tl"].as_i64().unwrap_or(0), x["tu"].as_i64().unwrap_or(0), &num_bigint::BigUint::from(liq));
            let (m0, m1) = (crate::prices::meta(cid, &t0)?, crate::prices::meta(cid, &t1)?);
            Ok(crate::tickmath::to_f64(&a0) / 10f64.powi(m0.decimals as i32) * crate::prices::usd(cid, &t0)?
                + crate::tickmath::to_f64(&a1) / 10f64.powi(m1.decimals as i32) * crate::prices::usd(cid, &t1)?)
        })();
        match r {
            Ok(v) => {
                total += v;
                x["usd_now"] = json!(v);
            }
            Err(e) => x["error"] = json!(e),
        }
        out.push(x);
    }
    (total, out)
}

// -- what to park ---------------------------------------------------------------------------------

/// Which of the pool's two tokens it holds on purpose: both (in its ranges, or out keeping them as
/// they are) or the one an exit went into.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Side {
    Both,
    Token0,
    Token1,
}

/// A token the wallet holds that a pool may park: its value now (USD) and the pegged pool the
/// whitelist gives it, if any.
#[derive(Clone, Debug)]
pub struct Held {
    pub token: String,
    pub usd: f64,
    pub park_pool: Option<String>,
}

/// Everything one pool's parking decision depends on.
#[derive(Clone, Debug)]
pub struct Situation {
    /// parking switched on (Settings) and the pool's strategy parks
    pub on: bool,
    pub pool: String,
    pub side: Side,
    pub budget_usd: f64,
    /// what its own positions are worth now
    pub deployed_usd: f64,
    pub min_usd: f64,
    /// its two tokens in the wallet (token0, token1)
    pub own: [Held; 2],
    /// the chain's funding tokens other than its own two
    pub funding: Vec<Held>,
    /// its lots: (token, USD value when parked)
    pub lots: Vec<(String, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    /// park this much (USD) of the token
    Park(String, f64),
    /// bring every lot back: parking is off for this pool, or a lot's token is no longer whitelisted
    UnparkAll,
}

fn finite(x: f64) -> f64 {
    if x.is_finite() && x > 0.0 { x } else { 0.0 }
}

/// What a pool parks now, when it is not moving. Everything it holds and does not use waits in
/// whitelisted pegged pools until a move needs it: out of its pool, the token it waits in (both,
/// when it kept them as they are); in its pool, what its ranges left over; and the share of its
/// budget it never drew from the funding tokens. All of it within the pool's budget (the wallet's
/// tokens are shared with other pools), never into the pool itself, never twice for a token
/// already parked, nothing under the minimum.
pub fn decide(s: &Situation) -> Vec<Act> {
    let parks_in = |h: &Held| h.park_pool.as_deref().filter(|p| !p.eq_ignore_ascii_case(&s.pool)).is_some();
    let all: Vec<&Held> = s.own.iter().chain(s.funding.iter()).collect();
    if !s.lots.is_empty() {
        let stale = |t: &str| !all.iter().any(|h| h.token.eq_ignore_ascii_case(t) && parks_in(h));
        if !s.on || s.lots.iter().any(|(t, _)| stale(t)) {
            return vec![Act::UnparkAll];
        }
    }
    if !s.on {
        return vec![];
    }
    let parked = |t: &str| s.lots.iter().any(|(x, _)| x.eq_ignore_ascii_case(t));
    let lots_usd: f64 = s.lots.iter().map(|l| finite(l.1)).sum();
    let free = (finite(s.budget_usd) - finite(s.deployed_usd) - lots_usd).max(0.0);
    let mut out = vec![];
    // its own tokens, on the side it holds, scaled down together to what the budget leaves
    let sides: &[usize] = match s.side {
        Side::Both => &[0, 1],
        Side::Token0 => &[0],
        Side::Token1 => &[1],
    };
    let own: Vec<&Held> = sides.iter().map(|i| &s.own[*i]).filter(|h| parks_in(h) && !parked(&h.token) && finite(h.usd) > 0.0).collect();
    let total: f64 = own.iter().map(|h| finite(h.usd)).sum();
    let k = if total > free { free / total } else { 1.0 };
    for h in own {
        let take = finite(h.usd) * k;
        if take >= s.min_usd {
            out.push(Act::Park(h.token.clone(), take));
        }
    }
    // the funding share: the budget less everything it holds of its own tokens (parked now or not)
    let held: f64 = s.own.iter().map(|h| finite(h.usd)).sum();
    let mut left = (free - held).max(0.0);
    for h in s.funding.iter().filter(|h| parks_in(h) && !parked(&h.token) && !s.own.iter().any(|o| o.token.eq_ignore_ascii_case(&h.token))) {
        let take = left.min(finite(h.usd));
        if take >= s.min_usd {
            out.push(Act::Park(h.token.clone(), take));
            left -= take;
        }
    }
    out
}

// -- recovering steps that did not finish ---------------------------------------------------------

/// A lot is written to the ledger before each step that changes the chain, with `stage` saying what
/// was under way: "parking" (before its swap and mint; `pre` = the range's liquidity before,
/// `got` = the other token the swap returned, once it did), "unparking" (before the withdrawal;
/// `pre` likewise) and "burned" (withdrawn; `got` = the other token to swap back). A step that
/// finishes clears it; one cut short (a crash, a failed mint whose swap-back failed too) is put
/// right on the next cycle by what `reconcile` says, from the range's liquidity on-chain then.
#[derive(Clone, Debug, PartialEq)]
pub enum Fix {
    /// nothing pending
    Fine,
    /// the step did finish: the lot as it should read (stage cleared, liquidity set)
    Restore(Value),
    /// nothing of the lot is in the pool: forget it, after swapping `amount` of `from` back into
    /// `to` (0: nothing to swap back)
    SwapBackAndDrop { from: String, to: String, amount: u128 },
}

fn get_u(x: &Value, k: &str) -> u128 {
    match &x[k] {
        Value::String(s) => s.parse().unwrap_or(0),
        v => v.as_u64().map(u128::from).unwrap_or(0),
    }
}

/// What to do about a lot whose last step may not have finished, with its range now holding
/// `onchain` liquidity. `lots` is the whole ledger (lots parked on the same range later count as
/// theirs, not this one's).
pub fn reconcile(lot: &Value, lots: &[Value], onchain: u128) -> Fix {
    let stage = lot["stage"].as_str().unwrap_or("");
    let token = lot["token"].as_str().unwrap_or("").to_string();
    let other = lot["other"].as_str().unwrap_or("").to_string();
    let (cid, pool) = (lot["chain_id"].as_u64().unwrap_or(0), lot["pool"].as_str().unwrap_or(""));
    let later: u128 = lots
        .iter()
        .filter(|x| in_pool(x, cid, pool) && rng(x) == rng(lot) && x["id"] != lot["id"] && x["ts"].as_f64().unwrap_or(0.0) > lot["ts"].as_f64().unwrap_or(0.0))
        .map(liq)
        .sum();
    let pre = get_u(lot, "pre");
    let back = |amount: u128| Fix::SwapBackAndDrop { from: other.clone(), to: token.clone(), amount };
    match stage {
        "parking" => {
            let minted = onchain.saturating_sub(pre).saturating_sub(later);
            if minted > 0 {
                let mut l = lot.clone();
                l["liquidity"] = json!(minted.to_string());
                for k in ["stage", "pre", "got", "other"] {
                    l.as_object_mut().map(|o| o.remove(k));
                }
                Fix::Restore(l)
            } else {
                back(get_u(lot, "got")) // swapped (maybe) but never minted: the swap is undone
            }
        }
        "unparking" => {
            // withdrawn if the range lost the lot's liquidity, else still parked
            if onchain.saturating_add(liq(lot)) <= pre.saturating_add(later) {
                back(0) // its tokens are in the wallet; how much of the other one is not known
            } else {
                let mut l = lot.clone();
                for k in ["stage", "pre"] {
                    l.as_object_mut().map(|o| o.remove(k));
                }
                Fix::Restore(l)
            }
        }
        "burned" => back(get_u(lot, "got")),
        _ => Fix::Fine,
    }
}

/// The parking range: `range_pct` either side of the tick, on the spacing.
pub fn range(tick: i64, spacing: i64, range_pct: f64) -> (i64, i64) {
    let sp = spacing.max(1);
    let w = ((1.0 + range_pct / 100.0).ln() / 1.0001f64.ln()).ceil() as i64;
    let tl = (tick - w).div_euclid(sp) * sp;
    let tu = (tick + w).div_euclid(sp) * sp + sp;
    (tl, tu)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_needs_parking_on_and_the_token() {
        let usdc = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
        let pool = "0x9ab7730b09ebd9ff2df70f06339bd289a1680a46";
        let mut s = json!({"parking": {"enabled": false, "pools": [{"chain_id": 8453, "token": usdc, "pool": pool}]}});
        assert!(pool_for(&s, 8453, usdc).is_none());
        s["parking"]["enabled"] = json!(true);
        assert_eq!(pool_for(&s, 8453, &usdc.to_uppercase().replace("0X", "0x")).unwrap().pool, pool);
        assert!(pool_for(&s, 10, usdc).is_none());
        assert!(pool_for(&s, 8453, "0x4200000000000000000000000000000000000006").is_none());
    }

    fn lot(id: &str, origin: &str, tl: i64, tu: i64, l: u128) -> Value {
        json!({"id": id, "chain_id": 8453, "origin": origin, "pool": "0xpark", "token": "0xusdc", "tl": tl, "tu": tu, "liquidity": l.to_string()})
    }

    /// The wallet's positions in the parking pool, per range, as the chain holds them.
    type Chain = HashMap<(i64, i64), u128>;

    /// What the parking pool's own strategy sees: every range less the lots parked on it.
    fn own_view(chain: &Chain, lots: &[Value]) -> Chain {
        let parked = by_range(lots, 8453, "0xpark");
        chain.iter().map(|(r, l)| (*r, own_share(*l, parked.get(r).copied().unwrap_or(0)))).filter(|(_, l)| *l > 0).collect()
    }

    /// Unpark every lot in order, as `unpark` does: withdraw what is withdrawable, drop the lot.
    fn unpark_all(chain: &mut Chain, lots: &mut Vec<Value>) -> Vec<(String, u128)> {
        let mut got = vec![];
        while let Some(x) = lots.first().cloned() {
            let r = rng(&x);
            let w = withdrawable(&x, lots, chain.get(&r).copied().unwrap_or(0));
            *chain.entry(r).or_default() -= w;
            got.push((x["id"].as_str().unwrap().to_string(), w));
            lots.remove(0);
        }
        got
    }

    #[test]
    fn the_parking_pools_own_strategy_never_moves_a_lot() {
        let mut lots = vec![lot("a", "weth-usdc", -5, 5, 1000), lot("b", "other", -5, 5, 500), lot("c", "weth-usdc", 0, 10, 70)];
        // the wallet's own position shares the lots' range: on-chain it is one position
        let mut chain: Chain = [((-5, 5), 1000 + 500 + 300), ((0, 10), 70), ((5, 10), 200)].into_iter().collect();
        let own = own_view(&chain, &lots);
        assert_eq!(own, [((-5, 5), 300), ((5, 10), 200)].into_iter().collect::<Chain>());
        // the pool's strategy withdraws everything it sees and splits into four ranges, one of them
        // the lots' own and one the third lot's
        for (r, l) in own {
            *chain.get_mut(&r).unwrap() -= l;
        }
        for (r, l) in [((-10, -5), 100), ((-5, 0), 100), ((-5, 5), 150), ((0, 10), 150)] {
            *chain.entry(r).or_default() += l;
        }
        assert_eq!(own_view(&chain, &lots), [((-10, -5), 100), ((-5, 0), 100), ((-5, 5), 150), ((0, 10), 150)].into_iter().collect::<Chain>());
        // and later withdraws all of it: the lots are still whole
        for (r, l) in own_view(&chain, &lots) {
            *chain.get_mut(&r).unwrap() -= l;
        }
        let left: Chain = chain.iter().filter(|(_, l)| **l > 0).map(|(r, l)| (*r, *l)).collect();
        assert_eq!(by_range(&lots, 8453, "0xpark"), left);
        let got = unpark_all(&mut chain, &mut lots);
        assert_eq!(got, vec![("a".into(), 1000), ("b".into(), 500), ("c".into(), 70)]);
        assert!(chain.values().all(|l| *l == 0));
    }

    #[test]
    fn a_pool_holding_more_parked_than_its_own_budget_leaves_the_lots_alone() {
        // USDC/USDT has 30% of the account ($3,000) and runs its own ladder; WETH/USDC (70%) parked
        // all of its $7,000 there, partly on the ladder's own ranges. On-chain the wallet holds $10,000
        // in the pool; the ladder sees its $3,000 only, so it is at its budget, not 330% over it.
        let mut lots = vec![lot("a", "weth-usdc", 14, 15, 5000), lot("b", "weth-usdc", 10, 20, 2000)];
        let mut chain: Chain = [((14, 15), 5000 + 2000), ((12, 13), 1000), ((10, 20), 2000)].into_iter().collect();
        let own = own_view(&chain, &lots);
        assert_eq!(own.values().sum::<u128>(), 3000);
        assert_eq!(own, [((14, 15), 2000), ((12, 13), 1000)].into_iter().collect::<Chain>());
        // its rebalance withdraws what it sees and re-deploys it elsewhere: the lots stay whole
        for (r, l) in own {
            *chain.get_mut(&r).unwrap() -= l;
        }
        *chain.entry((15, 16)).or_default() += 3000;
        assert_eq!(by_range(&lots, 8453, "0xpark"), [((14, 15), 5000), ((10, 20), 2000)].into_iter().collect::<Chain>());
        assert_eq!(chain[&(14, 15)], 5000);
        assert_eq!(chain[&(10, 20)], 2000);
        // and they all come back to WETH/USDC when it re-enters
        assert_eq!(unpark_all(&mut chain, &mut lots), vec![("a".into(), 5000), ("b".into(), 2000)]);
        assert_eq!(own_view(&chain, &lots), [((15, 16), 3000)].into_iter().collect::<Chain>());
    }

    #[test]
    fn lots_come_back_whole_while_the_pool_keeps_its_own() {
        let mut lots = vec![lot("a", "weth-usdc", -5, 5, 1000)];
        let mut chain: Chain = [((-5, 5), 1000 + 400)].into_iter().collect();
        assert_eq!(unpark_all(&mut chain, &mut lots), vec![("a".into(), 1000)]);
        assert_eq!(own_view(&chain, &lots), [((-5, 5), 400)].into_iter().collect::<Chain>(), "the pool's own position is untouched");
    }

    #[test]
    fn liquidity_withdrawn_elsewhere_is_not_a_stuck_lot() {
        // 700 left of the 1500 the two lots parked (withdrawn by hand, say)
        let mut lots = vec![lot("a", "weth-usdc", -5, 5, 1000), lot("b", "other", -5, 5, 500)];
        let mut chain: Chain = [((-5, 5), 700)].into_iter().collect();
        assert!(own_view(&chain, &lots).is_empty(), "nothing is the pool's own, and nothing negative");
        assert_eq!(unpark_all(&mut chain, &mut lots), vec![("a".into(), 700), ("b".into(), 0)]);
        assert!(lots.is_empty() && chain[&(-5, 5)] == 0);
    }

    #[test]
    fn entries_choose_how_their_lots_are_held() {
        let usdc = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
        let pool = "0x9ab7730b09ebd9ff2df70f06339bd289a1680a46";
        let mut s = json!({"parking": {"enabled": true, "range_pct": 0.5, "pools": [{"chain_id": 8453, "token": usdc, "pool": pool}]}});
        let w = whitelisted(&s, 8453, usdc).unwrap();
        assert_eq!((w.range_pct, w.recentre_after_min), (0.5, None));
        s["parking"]["pools"][0]["strategy"] = json!("recentre");
        s["parking"]["pools"][0]["range_pct"] = json!(0.2);
        let w = whitelisted(&s, 8453, usdc).unwrap();
        assert_eq!((w.range_pct, w.recentre_after_min), (0.2, Some(60.0)));
        assert!(entry_of(&s, &json!({"chain_id": 8453, "token": usdc, "pool": pool})).is_some());
        assert!(entry_of(&s, &json!({"chain_id": 8453, "token": usdc, "pool": "0x1111111111111111111111111111111111111111"})).is_none());
    }

    // -- decide ------------------------------------------------------------------------------------

    const USDC: &str = "usdc";
    const USDT: &str = "usdt";
    const WETH: &str = "weth";
    const DAI: &str = "dai";

    fn held(t: &str, usd: f64, pool: Option<&str>) -> Held {
        Held { token: t.into(), usd, park_pool: pool.map(|p| p.into()) }
    }

    /// weth-usdc with $3,000 of budget, out keeping both tokens, nothing held or parked; WETH parks
    /// in cbETH/WETH, USDC and USDT in USDC/USDT; USDT is a funding token
    fn sit() -> Situation {
        Situation {
            on: true,
            pool: "weth-usdc".into(),
            side: Side::Both,
            budget_usd: 3000.0,
            deployed_usd: 0.0,
            min_usd: 50.0,
            own: [held(WETH, 0.0, Some("cbeth-weth")), held(USDC, 0.0, Some("usdc-usdt"))],
            funding: vec![held(USDT, 0.0, Some("usdc-usdt"))],
            lots: vec![],
        }
    }

    fn parks(a: &[Act]) -> Vec<(String, i64)> {
        a.iter().filter_map(|x| if let Act::Park(t, u) = x { Some((t.clone(), u.round() as i64)) } else { None }).collect()
    }

    #[test]
    fn decide_out_with_both_tokens_parks_each() {
        let mut s = sit();
        s.own[0].usd = 1000.0;
        s.own[1].usd = 1500.0;
        assert_eq!(parks(&decide(&s)), vec![(WETH.into(), 1000), (USDC.into(), 1500)]);
    }

    #[test]
    fn decide_out_in_one_token_parks_only_that_one() {
        let mut s = sit();
        s.own[0].usd = 2900.0;
        s.own[1].usd = 900.0; // another pool's USDC in the shared wallet
        s.side = Side::Token0;
        assert_eq!(parks(&decide(&s)), vec![(WETH.into(), 2900)]);
        s.side = Side::Token1;
        assert_eq!(parks(&decide(&s)), vec![(USDC.into(), 900)]);
    }

    #[test]
    fn decide_never_takes_more_than_the_budget() {
        // the wallet holds $10,000 of the pool's tokens, most of it other pools'
        let mut s = sit();
        s.own[0].usd = 4000.0;
        s.own[1].usd = 6000.0;
        assert_eq!(parks(&decide(&s)), vec![(WETH.into(), 1200), (USDC.into(), 1800)]);
        // with $2,000 deployed and $500 parked, $500 is left
        s.deployed_usd = 2000.0;
        s.lots = vec![(DAI.into(), 500.0)];
        s.funding.push(held(DAI, 0.0, Some("dai-usdc")));
        assert_eq!(parks(&decide(&s)), vec![(WETH.into(), 200), (USDC.into(), 300)]);
        // the whole budget deployed or parked: nothing
        s.lots = vec![(DAI.into(), 1000.0)];
        assert_eq!(parks(&decide(&s)), vec![]);
    }

    #[test]
    fn decide_in_the_pool_parks_only_what_the_ranges_left() {
        let mut s = sit();
        s.deployed_usd = 2900.0;
        s.own[1].usd = 80.0; // leftovers
        assert_eq!(parks(&decide(&s)), vec![(USDC.into(), 80)]);
        s.deployed_usd = 2990.0; // the leftovers are within $10 of the budget: under the minimum
        assert!(decide(&s).is_empty());
        s.deployed_usd = 3100.0; // over budget: nothing, and no negative amounts
        s.own[0].usd = 500.0;
        assert!(decide(&s).is_empty());
    }

    #[test]
    fn decide_parks_the_funding_it_never_drew() {
        // weth-usdc out since before it entered: $8 of WETH, its $3,000 budget still in USDT
        let mut s = sit();
        s.own[0].usd = 8.0;
        s.funding[0].usd = 3140.0;
        assert_eq!(parks(&decide(&s)), vec![(USDT.into(), 2992)]);
        // less USDT than the shortfall: what there is
        s.funding[0].usd = 1000.0;
        assert_eq!(parks(&decide(&s)), vec![(USDT.into(), 1000)]);
        // once parked, not again (the lot counts against the budget, the token is parked)
        s.lots = vec![(USDT.into(), 1000.0)];
        s.funding[0].usd = 2140.0;
        assert!(decide(&s).is_empty());
    }

    #[test]
    fn decide_counts_own_tokens_before_funding() {
        let mut s = sit();
        s.own[0].usd = 1000.0; // parks
        s.own[1].usd = 30.0; // under the minimum: stays, but still counts as held
        s.funding[0].usd = 5000.0;
        assert_eq!(parks(&decide(&s)), vec![(WETH.into(), 1000), (USDT.into(), 1970)]);
        // an own token that is not whitelisted still counts as held
        s.own[0].park_pool = None;
        assert_eq!(parks(&decide(&s)), vec![(USDT.into(), 1970)]);
    }

    #[test]
    fn decide_skips_what_cannot_or_need_not_park() {
        let mut s = sit();
        s.own[0].usd = 1000.0;
        s.own[1].usd = 1000.0;
        // not whitelisted
        s.own[1].park_pool = None;
        assert_eq!(parks(&decide(&s)), vec![(WETH.into(), 1000)]);
        // its parking pool is the pool itself
        s.own[0].park_pool = Some("WETH-USDC".into());
        assert!(decide(&s).is_empty());
        // a funding token that is also one of its own is not counted twice
        let mut s = sit();
        s.own[1].usd = 500.0;
        s.funding = vec![held(USDC, 500.0, Some("usdc-usdt"))];
        assert_eq!(parks(&decide(&s)), vec![(USDC.into(), 500)]);
        // bad numbers are nothing
        let mut s = sit();
        s.own[0].usd = f64::NAN;
        s.own[1].usd = -5.0;
        s.funding[0].usd = f64::INFINITY;
        s.budget_usd = f64::NAN;
        assert!(decide(&s).is_empty());
    }

    #[test]
    fn decide_brings_lots_back_when_parking_stops() {
        let mut s = sit();
        s.lots = vec![(USDT.into(), 1000.0)];
        s.own[0].usd = 1000.0;
        s.on = false;
        assert_eq!(decide(&s), vec![Act::UnparkAll]);
        s.on = true;
        s.funding[0].park_pool = None; // USDT taken off the whitelist
        assert_eq!(decide(&s), vec![Act::UnparkAll]);
        s.funding.clear(); // no longer a funding token at all
        assert_eq!(decide(&s), vec![Act::UnparkAll]);
        // parking off with nothing parked: nothing at all
        let mut s = sit();
        s.on = false;
        s.own[0].usd = 1000.0;
        assert!(decide(&s).is_empty());
    }

    // -- reconcile ---------------------------------------------------------------------------------

    fn staged(stage: &str, l: u128, pre: u128, got: u128) -> Value {
        let mut x = lot("a", "weth-usdc", -5, 5, l);
        x["stage"] = json!(stage);
        x["pre"] = json!(pre.to_string());
        x["got"] = json!(got.to_string());
        x["other"] = json!("0xusdt");
        x["ts"] = json!(100);
        x
    }

    #[test]
    fn reconcile_a_park_cut_short() {
        // nothing pending
        assert_eq!(reconcile(&lot("a", "o", -5, 5, 10), &[], 10), Fix::Fine);
        // minted before the crash: the lot gets the liquidity the range gained
        let x = staged("parking", 0, 400, 30);
        match reconcile(&x, &[x.clone()], 1400) {
            Fix::Restore(l) => {
                assert_eq!(l["liquidity"], "1000");
                assert!(l.get("stage").is_none() && l.get("pre").is_none());
            }
            f => panic!("{f:?}"),
        }
        // a lot parked on the same range afterwards is its own, not this one's
        let mut y = lot("b", "other", -5, 5, 300);
        y["ts"] = json!(200);
        match reconcile(&x, &[x.clone(), y.clone()], 1700) {
            Fix::Restore(l) => assert_eq!(l["liquidity"], "1000"),
            f => panic!("{f:?}"),
        }
        // swapped but not minted: swap the other token back and forget the lot
        assert_eq!(reconcile(&x, &[x.clone()], 400), Fix::SwapBackAndDrop { from: "0xusdt".into(), to: "0xusdc".into(), amount: 30 });
        // not even swapped (got 0): just forget it
        let x = staged("parking", 0, 400, 0);
        assert_eq!(reconcile(&x, &[x.clone()], 400), Fix::SwapBackAndDrop { from: "0xusdt".into(), to: "0xusdc".into(), amount: 0 });
        // the pool's own strategy took liquidity from the range meanwhile: no phantom mint
        assert!(matches!(reconcile(&x, &[x.clone()], 100), Fix::SwapBackAndDrop { .. }));
    }

    #[test]
    fn reconcile_an_unpark_cut_short() {
        // the withdrawal never happened: the lot is still parked
        let x = staged("unparking", 1000, 1400, 0);
        match reconcile(&x, &[x.clone()], 1400) {
            Fix::Restore(l) => assert!(l["liquidity"] == "1000" && l.get("stage").is_none()),
            f => panic!("{f:?}"),
        }
        // it happened: the tokens are in the wallet, the lot goes
        assert_eq!(reconcile(&x, &[x.clone()], 400), Fix::SwapBackAndDrop { from: "0xusdt".into(), to: "0xusdc".into(), amount: 0 });
        // withdrawn, the swap back not done: it is done now
        let x = staged("burned", 1000, 1400, 77);
        assert_eq!(reconcile(&x, &[x.clone()], 400), Fix::SwapBackAndDrop { from: "0xusdt".into(), to: "0xusdc".into(), amount: 77 });
    }

    #[test]
    fn a_claim_moves_budget_from_its_origin_to_its_host_and_holds_no_range() {
        let claim = json!({"id": "c", "kind": "claim", "chain_id": 8453, "origin": "0xweth", "pool": "0xstable", "token": "0xusdt", "amount": "3000000000", "usd": 3000.0});
        let range = json!({"id": "r", "chain_id": 8453, "origin": "0xother", "pool": "0xstable", "tl": -35, "tu": 66, "liquidity": "600"});
        let lots = vec![claim.clone(), range];
        let usd = |x: &Value| x["usd"].as_f64().unwrap();
        assert_eq!(split_claims(&lots, 8453, "0xstable", &usd), (3000.0, 0.0), "the host's budget grows by what it holds for others");
        assert_eq!(split_claims(&lots, 8453, "0xweth", &usd), (0.0, 3000.0), "the origin's shrinks by what it parked");
        assert_eq!(split_claims(&lots, 10, "0xstable", &usd), (0.0, 0.0));
        // only the range lot is left out of the host's own positions: the claim's tokens are the host's to place
        let parked = by_range(&lots, 8453, "0xstable");
        assert_eq!(parked.len(), 1);
        assert_eq!(parked[&(-35, 66)], 600);
    }

    #[test]
    fn range_brackets_the_price() {
        let (a, b) = range(-276_300, 1, 0.5);
        assert!(a < -276_300 && -276_300 < b && b - a >= 99);
        let (a, b) = range(105, 10, 0.5);
        assert!(a % 10 == 0 && b % 10 == 0 && a <= 55 && b >= 155);
    }
}
