//! `--parity DIR`: compare the Rust runtime with the Python reference on fixtures written by
//! scripts/export_parity.py (recorded events, processor history, and Python's outputs).
//! Checks: tick math, ABI calldata, transaction signing, resize plans, and per pool the replay
//! market and the ladder's target ranges. Exit code 0 = everything matches.

use crate::data;
use crate::engine;
use crate::eth;
use crate::ladder::{self, Params};
use crate::tickmath;
use num_bigint::BigUint;
use serde_json::Value;
use std::path::Path;

fn read(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap_or_else(|_| panic!("missing {}", p.display()))).unwrap()
}

struct Check {
    fails: usize,
    n: usize,
}

impl Check {
    fn ok(&mut self, what: &str, good: bool, detail: String) {
        self.n += 1;
        if good {
            println!("  ok   {what}");
        } else {
            self.fails += 1;
            println!("  FAIL {what}: {detail}");
        }
    }
}

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * a.abs().max(b.abs()).max(1e-300)
}

pub fn run(dir: &str) -> i32 {
    let dir = Path::new(dir);
    let mut c = Check { fails: 0, n: 0 };
    let v = read(&dir.join("vectors.json"));

    println!("tick math");
    for t in v["ticks"].as_array().unwrap() {
        let tick = t["tick"].as_i64().unwrap();
        let got = tickmath::sqrt_ratio_at_tick(tick).to_string();
        c.ok(&format!("sqrt_ratio_at_tick({tick})"), got == t["sqrt"].as_str().unwrap(), got);
    }
    for a in v["amounts"].as_array().unwrap() {
        let p = |k: &str| BigUint::parse_bytes(a[k].as_str().unwrap().as_bytes(), 10).unwrap();
        let (x, y) = tickmath::amounts_at(&p("sp"), a["tl"].as_i64().unwrap(), a["tu"].as_i64().unwrap(), &p("liq"));
        c.ok("amounts_for_liquidity", x.to_string() == a["a0"].as_str().unwrap() && y.to_string() == a["a1"].as_str().unwrap(), format!("{x} {y}"));
        let l = tickmath::liquidity_for_amounts(&p("sp"), &tickmath::sqrt_ratio_at_tick(a["tl"].as_i64().unwrap()),
                                                &tickmath::sqrt_ratio_at_tick(a["tu"].as_i64().unwrap()), &p("x"), &p("y"));
        c.ok("liquidity_for_amounts", l.to_string() == a["l"].as_str().unwrap(), l.to_string());
    }

    println!("calldata and signing");
    for d in v["calldata"].as_array().unwrap() {
        let got = match d["fn"].as_str().unwrap() {
            "mint" => {
                let a = &d["args"];
                eth::calldata("mint(address,int24,int24,uint128,uint256,uint256,uint256)", &[eth::addr(a[0].as_str().unwrap()), eth::i(a[1].as_i64().unwrap()),
                    eth::i(a[2].as_i64().unwrap()), eth::u(eth::parse_uint(a[3].as_str().unwrap())), eth::u(eth::parse_uint(a[4].as_str().unwrap())),
                    eth::u(eth::parse_uint(a[5].as_str().unwrap())), eth::u(eth::parse_uint(a[6].as_str().unwrap()))])
            }
            "burn" => {
                let a = &d["args"];
                let u = |i: usize| eth::u(eth::parse_uint(a[i].as_str().unwrap()));
                eth::calldata("burnAndCollect(address,int24,int24,uint128,uint256,uint256,uint128,uint128,uint256)",
                    &[eth::addr(a[0].as_str().unwrap()), eth::i(a[1].as_i64().unwrap()), eth::i(a[2].as_i64().unwrap()), u(3), u(4), u(5), u(6), u(7), u(8)])
            }
            "claim" => {
                let a = &d["args"];
                let addrs = |k: &str| eth::Tok::Arr(a[k].as_array().unwrap().iter().map(|x| eth::addr(x.as_str().unwrap())).collect());
                let nums = |k: &str| eth::Tok::Arr(a[k].as_array().unwrap().iter().map(|x| eth::u(eth::parse_uint(x.as_str().unwrap()))).collect());
                let path = eth::Tok::Arr(a["path"].as_array().unwrap().iter().map(|h| {
                    let b = eth::unhex(h.as_str().unwrap()).unwrap();
                    let mut w = [0u8; 32];
                    w.copy_from_slice(&b);
                    eth::Tok::B32(w)
                }).collect());
                let flags = eth::Tok::Arr(a["flags"].as_array().unwrap().iter().map(|f| eth::Tok::Bool(f.as_bool().unwrap())).collect());
                eth::calldata("claimAll((address[],uint8[],address[],address[],uint256[],(bytes32[],bool[])))",
                    &[eth::Tok::Tup(vec![addrs("earners"), nums("types"), addrs("pools"), addrs("tokens"), nums("amounts"), eth::Tok::Tup(vec![path, flags])])])
            }
            _ => String::new(),
        };
        c.ok(&format!("calldata {}", d["fn"]), got == d["data"].as_str().unwrap(), got);
    }
    let s = &v["sign"];
    let acct = eth::Account::from_key(s["key"].as_str().unwrap()).unwrap();
    c.ok("address from key", acct.address.to_lowercase() == s["address"].as_str().unwrap().to_lowercase(), acct.address.clone());
    let tx = eth::Tx1559 {
        chain_id: s["chain_id"].as_u64().unwrap(),
        nonce: s["nonce"].as_u64().unwrap(),
        max_priority_fee: eth::parse_uint(s["tip"].as_str().unwrap()),
        max_fee: eth::parse_uint(s["max_fee"].as_str().unwrap()),
        gas: s["gas"].as_u64().unwrap(),
        to: s["to"].as_str().unwrap().into(),
        data: eth::unhex(s["data"].as_str().unwrap()).unwrap(),
    };
    let raw = format!("0x{}", eth::hex(&tx.sign(&acct)));
    c.ok("EIP-1559 signed transaction", raw == s["raw"].as_str().unwrap(), raw);

    println!("resize plans");
    for r in v["resize"].as_array().unwrap() {
        let cur: Vec<(i64, i64, i128, f64)> = r["current"].as_array().unwrap().iter()
            .map(|x| (x[0].as_i64().unwrap(), x[1].as_i64().unwrap(), x[2].as_i64().unwrap() as i128, x[3].as_f64().unwrap())).collect();
        let tgt: Vec<(i64, i64, f64)> = r["target"].as_array().unwrap().iter().map(|x| (x[0].as_i64().unwrap(), x[1].as_i64().unwrap(), x[2].as_f64().unwrap())).collect();
        let (b, a) = engine::resize_plan(&cur, &tgt, r["budget"].as_f64().unwrap(), r["tol"].as_f64().unwrap(), r["dust"].as_f64().unwrap());
        let bj: Vec<Value> = b.iter().map(|(x, y, z)| serde_json::json!([x, y, *z as i64])).collect();
        let aj: Vec<Value> = a.iter().map(|(x, y, z)| serde_json::json!([x, y, z])).collect();
        let ok = Value::Array(bj.clone()) == r["burns"] && aj.len() == r["adds"].as_array().unwrap().len()
            && aj.iter().zip(r["adds"].as_array().unwrap()).all(|(p, q)| p[0] == q[0] && p[1] == q[1] && close(p[2].as_f64().unwrap(), q[2].as_f64().unwrap(), 1e-12));
        c.ok("resize_plan", ok, format!("{bj:?} {aj:?}"));
    }

    for p in std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.path().is_dir()) {
        let pd = p.path();
        let meta = read(&pd.join("meta.json"));
        let exp = read(&pd.join("expected.json"));
        println!("pool {}", meta["name"].as_str().unwrap());
        let ev = data::read_jsonl(&pd.join("events.jsonl"));
        let apr_raw = read(&pd.join("apr.json"));
        let apr = apr_raw.as_object().unwrap().iter().map(|(k, v)| (k.parse::<i64>().unwrap(), (v[0].as_f64().unwrap(), v[1].as_f64().unwrap()))).collect();
        let (d0, d1) = (meta["d0"].as_i64().unwrap() as i32, meta["d1"].as_i64().unwrap() as i32);
        let m = match data::build_market(ev, meta["wallet"].as_str().unwrap(), &apr, d0, d1, meta["numeraire"].as_i64().unwrap()) {
            Ok(m) => m,
            Err(e) => {
                c.ok("build market", false, e);
                continue;
            }
        };
        let e = &exp["market"];
        c.ok("swap count", m.ts.len() as u64 == e["n"].as_u64().unwrap(), format!("{} vs {}", m.ts.len(), e["n"]));
        c.ok("bar count", m.bar_end.len() as u64 == e["bars"].as_u64().unwrap(), format!("{} vs {}", m.bar_end.len(), e["bars"]));
        let ticks_ok = m.tick.iter().zip(e["tick"].as_array().unwrap()).all(|(a, b)| *a == b.as_i64().unwrap());
        c.ok("ticks (outliers pinned)", ticks_ok && m.tick.len() == e["tick"].as_array().unwrap().len(), String::new());
        let fs: f64 = m.fee.iter().sum();
        c.ok("fee sum", close(fs, e["fee_sum"].as_f64().unwrap(), 1e-9), format!("{fs} vs {}", e["fee_sum"]));
        let ls: f64 = m.l_others.iter().sum();
        c.ok("others' liquidity sum", close(ls, e["l_others_sum"].as_f64().unwrap(), 1e-9), format!("{ls} vs {}", e["l_others_sum"]));
        c.ok("last bar tick", *m.bar_tick.last().unwrap() == e["bar_tick_last"].as_i64().unwrap(), format!("{}", m.bar_tick.last().unwrap()));
        let params = Params::from(meta["strategy"].as_object().unwrap()).unwrap();
        let gd = ladder::guard_dev(&m, &params);
        let eg = exp["guard_dev"].as_f64();
        c.ok("guard deviation", match (gd, eg) { (Some(a), Some(b)) => close(a, b, 1e-12), (None, None) => true, (Some(a), None) => a.is_nan(), _ => false },
             format!("{gd:?} vs {eg:?}"));
        let fx = &exp["flvr"];
        match (fx["since"].as_i64(), fx["f"].as_f64(), fx["lvr"].as_f64()) {
            (Some(since), Some(ef), Some(el)) => {
                let ev = data::read_jsonl(&pd.join("events.jsonl"));
                let got = data::fee_vs_lvr(&ev, &apr, d0, d1, meta["numeraire"].as_i64().unwrap(), since).unwrap_or_default();
                let counts = got.swaps as u64 == fx["swaps"].as_u64().unwrap_or(0) && got.hours as u64 == fx["hours"].as_u64().unwrap_or(0);
                let ratio_ok = match (got.ratio(), fx["ratio"].as_f64()) { (Some(a), Some(b)) => close(a, b, 1e-9), (None, None) => true, _ => false };
                c.ok("F/LVR, trailing 7 days", counts && close(got.f, ef, 1e-9) && close(got.lvr, el, 1e-9) && ratio_ok,
                     format!("ratio {:?} (F {}, LVR {}, {} swaps, {} h) vs {}", got.ratio(), got.f, got.lvr, got.swaps, got.hours, fx));
            }
            _ => c.ok("F/LVR, trailing 7 days", false, "no reference in expected.json; rerun scripts/export_parity.py".into()),
        }
        for case in exp["targets"].as_array().unwrap() {
            let raw = ladder::decide(&m, &params, case["budget0"].as_f64().unwrap(), case["sp"].as_f64().unwrap(), case["tick"].as_i64().unwrap());
            let got = engine::cap_ranges(engine::snap_ranges(raw, meta["spacing"].as_i64().unwrap()), meta["max_ranges"].as_u64().unwrap() as usize);
            let want = case["ranges"].as_array().unwrap();
            let ok = got.len() == want.len()
                && got.iter().zip(want).all(|(g, w)| g.0 == w[0].as_i64().unwrap() && g.1 == w[1].as_i64().unwrap() && close(g.2, w[2].as_f64().unwrap(), 1e-9));
            c.ok(&format!("ladder target, budget {}", case["budget0"]), ok, format!("{got:?} vs {want:?}"));
        }
    }
    println!("{} checks, {} failed", c.n, c.fails);
    if c.fails == 0 { 0 } else { 1 }
}
