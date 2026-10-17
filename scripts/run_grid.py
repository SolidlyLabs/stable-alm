"""Simulate every config of a grid over one pool's on-chain history and save daily equity curves.

Usage:
  python scripts/run_grid.py --pool usdc-usdt --grid all --out all_usdc-usdt
  python scripts/run_grid.py --pool cbeth-weth --grid delay --bars 60 --out delay_cbeth-weth
  python scripts/run_grid.py --market binance --grid guard --start 2021-01-01 --out guard_binance
Output: reports/<out>.parquet (daily PnL per config, in the pool's numeraire), <out>_income.parquet,
<out>_summary.csv. Workers default to 3 to keep memory and CPU use modest.
"""
import argparse
import os
import sys
import time
from multiprocessing import Pool

import pandas as pd

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.data import POOLS, cached_market  # noqa: E402
from alm.grid import GRIDS  # noqa: E402
from alm.sim import Costs, run  # noqa: E402

M = None
A = None


def init(args):
    global M, A
    A = args
    if args.market == "binance":
        from alm.synthetic import SyntheticMarket, calibrate

        M = SyntheticMarket(start=args.start, **calibrate(cached_market()))
    else:
        M = cached_market(POOLS[args.pool]["pool"], bar_seconds=args.bars).slice_from(int(pd.Timestamp(args.start).timestamp()))


def work(i):
    s = GRIDS[A.grid]()[i]
    c = POOLS[A.pool]
    return run(M, s, capital_usdc=c["capital"], costs=Costs(fee_haircut=A.haircut, swap_bps=A.swap_bps, gas_usd=c["gas"]))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pool", default="usdc-usdt", choices=list(POOLS))
    ap.add_argument("--grid", default="all")
    ap.add_argument("--market", default="onchain", choices=["onchain", "binance"])
    ap.add_argument("--start", default="2024-01-01")
    ap.add_argument("--bars", type=int, default=300)
    ap.add_argument("--haircut", type=float, default=0.82)
    ap.add_argument("--swap-bps", type=float, default=0.3)
    ap.add_argument("--workers", type=int, default=2)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    if a.market == "onchain":  # build the cache once in the parent so workers only load it
        cached_market(POOLS[a.pool]["pool"], bar_seconds=a.bars)
    n = len(GRIDS[a.grid]())
    t = time.time()
    with Pool(a.workers, initializer=init, initargs=(a,)) as p:
        results = p.map(work, range(n), chunksize=1)
    out = os.path.join(os.path.dirname(__file__), "..", "reports")
    from alm.walkforward import daily_pnl

    daily_pnl(results).to_parquet(os.path.join(out, f"{a.out}.parquet"))
    daily_pnl(results, "income").to_parquet(os.path.join(out, f"{a.out}_income.parquet"))
    rows = [{"config": r.label, "family": r.name, "pnl": r.pnl, "fees": r.fees, "emissions": r.emissions, "costs": r.costs,
             "actions": r.actions, "time_in_range": r.time_in_range} for r in results]
    pd.DataFrame(rows).sort_values("pnl", ascending=False).to_csv(os.path.join(out, f"{a.out}_summary.csv"), index=False)
    print(f"{n} configs in {time.time() - t:.0f}s")


if __name__ == "__main__":
    main()
