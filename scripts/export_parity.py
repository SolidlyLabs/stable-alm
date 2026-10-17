"""Write fixtures for the Rust runtime's parity check (`StableALM --parity DIR`).

The Python code is the reference: tick math, ABI calldata, EIP-1559 signing (eth_account), resize
plans, and per pool the replay market and the ladder's target ranges on recorded events.

    python scripts/export_parity.py [--data data] [--out rs/parity]
"""
import argparse
import json
import os
import random
import sys

import numpy as np
import pandas as pd

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from eth_abi import encode  # noqa: E402
from eth_account import Account  # noqa: E402
from eth_utils import keccak  # noqa: E402

import alm.data as almdata  # noqa: E402
from alm.executor.engine import cap_ranges, make_strategy, resize_plan, snap_ranges  # noqa: E402
from alm.executor.tickmath import amounts_for_liquidity, liquidity_for_amounts, sqrt_ratio_at_tick  # noqa: E402
from alm.sim import State  # noqa: E402

WALLET = os.environ.get("ALM_WALLET", "").lower()  # the wallet whose LP events are in data/raw/<pool>/wallet_<address>.parquet
POOLS = [
    {"name": "usdc-usdt", "pool": "0x9ab7730b09ebd9ff2df70f06339bd289a1680a46", "d0": 6, "d1": 6, "numeraire": 0,
     "spacing": 1, "max_ranges": 5, "budgets": [100.0, 2500.0, 80000.0],
     "strategy": {"type": "ladder", "lookback": 86400, "span": 3, "every": 72, "turnover": 0.4, "guard": 15}},
    {"name": "cbeth-weth", "pool": "0x74ad64bac8df96af2b63ea50c3d8840b79aa7a3b", "d0": 18, "d1": 18, "numeraire": 1,
     "spacing": 1, "max_ranges": 5, "budgets": [0.05, 1.0, 40.0],
     "strategy": {"type": "ladder", "lookback": 86400, "span": 6, "every": 72, "turnover": 0.4, "guard": 15}},
]


def calldata(sig: str, types: list, args: list) -> str:
    return "0x" + (keccak(text=sig)[:4] + encode(types, args)).hex()


def vectors() -> dict:
    rnd = random.Random(7)
    ticks = [0, 1, -1, 2, -2, 7, -7, 100, -100, 1326, 1327, 20000, -20000, 276324, -276324, 887272, -887272]
    ticks += [rnd.randint(-887272, 887272) for _ in range(20)]
    out = {"ticks": [{"tick": t, "sqrt": str(sqrt_ratio_at_tick(t))} for t in ticks], "amounts": []}
    for _ in range(30):
        c = rnd.randint(-5000, 5000)
        tl, tu = c - rnd.randint(1, 40), c + rnd.randint(1, 40)
        sp = sqrt_ratio_at_tick(c + rnd.randint(-60, 60))
        liq = rnd.randint(1, 10**24)
        a0, a1 = amounts_for_liquidity(sp, sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu), liq)
        x, y = rnd.randint(0, 10**22), rnd.randint(0, 10**22)
        l = liquidity_for_amounts(sp, sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu), x, y)
        out["amounts"].append({"sp": str(sp), "tl": tl, "tu": tu, "liq": str(liq), "a0": str(a0), "a1": str(a1),
                               "x": str(x), "y": str(y), "l": str(l)})

    pool, me = POOLS[0]["pool"], WALLET
    mint = [pool, -3, 4, 123456789012345678, 10**20, 5 * 10**19, 1_900_000_000]
    burn = [pool, 1326, 1327, 98765432109876, 1, 2, 2**128 - 1, 2**128 - 1, 1_900_000_000]
    sigm = "mint(address,int24,int24,uint128,uint256,uint256,uint256)"
    sigb = "burnAndCollect(address,int24,int24,uint128,uint256,uint256,uint128,uint128,uint256)"
    claim = {"earners": [me, me], "types": [0, 1], "pools": [pool, POOLS[1]["pool"]],
             "tokens": ["0x777cf5ba9c291a1a8f57ff14836f6f9dc5c0f9dd", "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"],
             "amounts": [12345678901234567890, 42], "path": ["0x" + "ab" * 32, "0x" + "01" * 32], "flags": [True, False]}
    sigc = "claimAll((address[],uint8[],address[],address[],uint256[],(bytes32[],bool[])))"
    ctypes = ["(address[],uint8[],address[],address[],uint256[],(bytes32[],bool[]))"]
    cargs = [(claim["earners"], claim["types"], claim["pools"], claim["tokens"], claim["amounts"],
              ([bytes.fromhex(h[2:]) for h in claim["path"]], claim["flags"]))]
    s = lambda v: [str(x) if isinstance(x, int) and not isinstance(x, bool) and abs(x) > 2**53 else x for x in v]  # noqa: E731
    out["calldata"] = [
        {"fn": "mint", "args": [str(x) if i >= 3 else x for i, x in enumerate(mint)],
         "data": calldata(sigm, sigm[5:-1].split(","), mint)},
        {"fn": "burn", "args": [str(x) if i >= 3 else x for i, x in enumerate(burn)],
         "data": calldata(sigb, sigb[15:-1].split(","), burn)},
        {"fn": "claim", "args": {**claim, "types": [str(t) for t in claim["types"]], "amounts": [str(a) for a in claim["amounts"]]},
         "data": calldata(sigc, ctypes, cargs)},
    ]
    del s

    key = "0x" + os.urandom(32).hex()  # throwaway key for the signing vector, new on every export (never funded)
    acct = Account.from_key(key)
    data = out["calldata"][0]["data"]
    tx = {"chainId": 8453, "nonce": 17, "to": acct.address, "data": data, "value": 0, "gas": 412345,
          "maxFeePerGas": 12_345_678, "maxPriorityFeePerGas": 1_000_000, "type": 2}
    signed = acct.sign_transaction(tx)
    raw = getattr(signed, "raw_transaction", None) or signed.rawTransaction
    out["sign"] = {"key": key, "address": acct.address, "chain_id": 8453, "nonce": 17, "tip": "1000000",
                   "max_fee": "12345678", "gas": 412345, "to": acct.address.lower(), "data": data,
                   "raw": "0x" + raw.hex().removeprefix("0x")}

    cases = [
        ([(-1, 0, 10**12, 400.0), (0, 1, 2 * 10**12, 600.0)], [(-1, 0, 0.4), (0, 1, 0.6)], 1000.0, 10.0, 5.0),
        ([(-1, 0, 10**12, 400.0), (0, 1, 2 * 10**12, 600.0)], [(-1, 0, 0.2), (0, 1, 0.5), (1, 2, 0.3)], 1000.0, 10.0, 5.0),
        ([(-1, 0, 10**12, 400.0), (0, 1, 2 * 10**12, 600.0)], [(0, 1, 1.0)], 1500.0, 10.0, 5.0),
        ([(-1, 0, 10**12, 400.0), (-1, 0, 10**11, 40.0)], [(-1, 0, 1.0)], 300.0, 3.0, 5.0),
        ([(3, 4, 777777777, 2.0)], [(3, 4, 1.0)], 3.0, 0.1, 5.0),
        ([(5, 6, 999999999999, 123.456)], [(5, 6, 0.3), (6, 7, 0.7)], 200.0, 2.0, 0.002),
    ]
    out["resize"] = []
    for cur, tgt, budget, tol, dust in cases:
        b, a = resize_plan(cur, tgt, budget, tol, dust)
        out["resize"].append({"current": [list(c) for c in cur], "target": [list(t) for t in tgt], "budget": budget,
                              "tol": tol, "dust": dust, "burns": [[x, y, int(z)] for x, y, z in b],
                              "adds": [[x, y, z] for x, y, z in a]})
    return out


def clean(row: dict) -> dict:
    out = {}
    for k, v in row.items():
        if k == "tx" or v is None or (isinstance(v, float) and np.isnan(v)):
            continue
        out[k] = int(v) if isinstance(v, (np.integer,)) or (isinstance(v, float) and k in ("tick", "tick_lower", "tick_upper")) else v
    return out


def fee_vs_lvr(df: pd.DataFrame, rate: dict, d0: int, d1: int, numeraire: int, since: int) -> dict:
    """Reference for the runtime's F/LVR monitor (research/volatile/fee_vs_lvr.py, 1-hour returns).

    F = sum of fee_i / L_i over swaps (amount in x the hour's processor fee rate, 0 for hours it lacks; swaps
    with no active liquidity skipped); LVR = sum of r^2 * sqrt(P) / 4 over 1-hour bars ending on the hour
    (1/sqrt(P) when token0 is the numeraire), the price at a bar being the last swap's tick.
    """
    sw = df[(df.kind == "swap") & (df.ts >= since)].sort_values(["block", "log_index"])
    ts = sw.ts.to_numpy(np.int64)
    p = np.power(1.0001, sw.tick.to_numpy(float)) * 10.0 ** (d0 - d1)
    L = sw.liquidity.astype(float).to_numpy() / 10.0 ** ((d0 + d1) / 2)
    in0 = np.clip(sw.amount0.astype(float).to_numpy() / 10.0**d0, 0, None)
    in1 = np.clip(sw.amount1.astype(float).to_numpy() / 10.0**d1, 0, None)
    amount_in = in0 + in1 / p if numeraire == 0 else in0 * p + in1
    r_h = np.array([rate.get(int(h), 0.0) for h in (ts // 3600) * 3600])
    ok = L > 0
    f = float((amount_in[ok] * r_h[ok] / L[ok]).sum())
    bar_end = np.arange((ts[0] // 3600 + 1) * 3600, ts[-1], 3600)
    pb = p[np.searchsorted(ts, bar_end, side="right") - 1]
    lp = np.log(pb)
    r = np.diff(lp, prepend=lp[:1])
    root = np.sqrt(pb) if numeraire == 1 else 1 / np.sqrt(pb)
    lvr = float((r * r * root / 4).sum())
    return {"since": int(since), "f": f, "lvr": lvr, "ratio": f / lvr if lvr > 0 else None,
            "swaps": int(len(sw)), "hours": int(len(bar_end))}


def export_pool(cfg: dict, data_dir: str, out_dir: str) -> None:
    pool = cfg["pool"]
    raw = os.path.join(data_dir, "raw", pool)
    almdata.DATA = data_dir
    segs = sorted(f for f in os.listdir(raw) if f.startswith("events_"))
    segs.sort(key=lambda f: int(f[len("events_"):-len(".parquet")]))
    head = int(pd.read_parquet(os.path.join(raw, segs[-1]), columns=["block"]).block.max())
    since = head - 10 * 43_200
    parts = []
    for f in segs:  # keep the window's swaps and every Mint/Burn, one segment at a time
        s = pd.read_parquet(os.path.join(raw, f))
        if "block" in s.columns:  # segments with no events are stored without columns
            parts.append(s[(s.block >= since) | (s.kind != "swap")])
        del s
    ev = pd.concat(parts, ignore_index=True)
    wpath = os.path.join(raw, f"wallet_{WALLET}.parquet")
    df = pd.concat([ev, pd.read_parquet(wpath)], ignore_index=True)
    df = df[(df.block >= since) | (df.kind != "swap")].drop_duplicates(["block", "log_index"])
    df = df.sort_values(["block", "log_index"]).reset_index(drop=True)

    m = almdata.Market(pool, wallet=WALLET, decimals=cfg["d0"], numeraire=cfg["numeraire"], events=df, decimals1=cfg["d1"])
    pd_dir = os.path.join(out_dir, cfg["name"])
    os.makedirs(pd_dir, exist_ok=True)
    with open(os.path.join(pd_dir, "events.jsonl"), "w") as f:
        for r in df.to_dict("records"):
            f.write(json.dumps(clean(r)) + "\n")
    apr = pd.read_parquet(os.path.join(raw, "apr_hourly.parquet")).drop_duplicates("timestamp").set_index("timestamp")
    rate = (apr["feesUSD"] / apr["volumeUSD"].replace(0, np.nan)).fillna(0.0).clip(0, 0.01)
    emis = apr["emissionsUSD"].fillna(0.0)
    rate_by_hour = {int(t): float(rate[t]) for t in apr.index}
    json.dump({str(int(t)): [float(rate[t]), float(emis[t])] for t in apr.index}, open(os.path.join(pd_dir, "apr.json"), "w"))
    json.dump({k: cfg[k] for k in ("name", "pool", "d0", "d1", "numeraire", "spacing", "max_ranges", "strategy")} | {"wallet": WALLET},
              open(os.path.join(pd_dir, "meta.json"), "w"), indent=1)

    strat = make_strategy(cfg["strategy"])
    strat.prepare(m)
    gd = strat.guard_dev
    bidx = len(m.bar_end) - 1
    tick = int(m.tick[-1])
    sp = float(1.0001 ** (tick + 0.37)) ** 0.5
    targets = []
    for budget in cfg["budgets"]:
        strat = make_strategy(cfg["strategy"])
        strat.prepare(m)
        strat.reset()
        strat.every = 1
        st = State(i=int(m.bar_idx[bidx]), bar=bidx, tick=tick, sp=sp, ranges=[], x=budget, y=0.0,
                   last_action_bar=-10**9, bars_out_of_range=10**6, market=m)
        t = strat.decide(st)
        ranges = cap_ranges(snap_ranges(t.ranges, cfg["spacing"]), cfg["max_ranges"])
        targets.append({"budget0": budget, "sp": sp, "tick": tick, "ranges": [[a, c, float(w)] for a, c, w in ranges]})
    exp = {
        "market": {"n": int(len(m.ts)), "bars": int(len(m.bar_end)), "tick": [int(x) for x in m.tick],
                   "fee_sum": float(m.fee.sum()), "l_others_sum": float(m.l_others.sum()),
                   "bar_tick_last": int(m.bar_tick[-1]), "outliers": int(m.n_outliers)},
        "guard_dev": None if gd is None else float(gd[bidx]),
        "targets": targets,
        "flvr": fee_vs_lvr(df, rate_by_hour, cfg["d0"], cfg["d1"], cfg["numeraire"], int(df[df.kind == "swap"].ts.max()) - 7 * 86400),
    }
    json.dump(exp, open(os.path.join(pd_dir, "expected.json"), "w"))
    print(f"{cfg['name']}: {len(df)} events, {len(m.ts)} swaps, {len(m.bar_end)} bars, {m.n_outliers} outliers pinned, "
          f"F/LVR (7d) {exp['flvr']['ratio']}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", default="data")
    ap.add_argument("--out", default="rs/parity")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    json.dump(vectors(), open(os.path.join(a.out, "vectors.json"), "w"), indent=1)
    for cfg in POOLS:
        export_pool(cfg, a.data, a.out)


if __name__ == "__main__":
    main()
