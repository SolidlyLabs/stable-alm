"""Does Solidly pool volume follow the pool's in-range liquidity? (aggregators route by depth)
Daily volume vs daily mean active TVL, log-log, per pool; plus the same on weekly sums."""
import os
import sys

import numpy as np
import pandas as pd

ROOT = os.path.join(os.path.dirname(__file__), "..", "..")
for name, pool in [("USDC/USDT", "0x9ab7730b09ebd9ff2df70f06339bd289a1680a46"), ("cbETH/WETH", "0x74ad64bac8df96af2b63ea50c3d8840b79aa7a3b"),
                   ("USDC/USDbC", "0x7a8d53466144e1f75de2e70ec908e18f771b49fb"), ("WETH/USDC", "0x551a0e3d267bea87048f08cc94cc6035ad99221b")]:
    f = os.path.join(ROOT, "data", "raw", pool, "apr_hourly.parquet")
    if not os.path.exists(f):
        print(name, "no data"); continue
    h = pd.read_parquet(f)
    h["day"] = h.timestamp // 86400
    d = h.groupby("day").agg(vol=("volumeUSD", "sum"), act=("activeTvlUSD", "mean"), tvl=("tvlUSD", "mean"))
    d = d[(d.vol > 0) & (d.act > 100)]
    for lab, x in (("daily", d), ("weekly", d.groupby(d.index // 7).agg(vol=("vol", "sum"), act=("act", "mean")))):
        lx, ly = np.log(x.act.to_numpy()), np.log(x.vol.to_numpy())
        b = np.polyfit(lx, ly, 1)[0] if len(x) > 5 else float("nan")
        # Changes: does a jump in liquidity go with a jump in volume? (removes slow trends)
        dl, dv = np.diff(lx), np.diff(ly)
        bd = np.polyfit(dl, dv, 1)[0] if len(dl) > 5 else float("nan")
        print(f"{name:<11} {lab:<6} n={len(x):4d}  elasticity levels {b:+.2f}  changes {bd:+.2f}  corr(changes) {np.corrcoef(dl, dv)[0,1]:+.2f}")
