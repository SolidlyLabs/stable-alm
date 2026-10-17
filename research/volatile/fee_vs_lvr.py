"""Is liquidity at the current price worth providing? Per unit of liquidity, week by week:

    F   = sum over swaps of fee_i / L_i       (what one more unit of in-range liquidity would earn)
    LVR = sum over bars of r^2 * sqrt(P) / 4   (expected loss to arbitrage of that unit; 1/sqrt(P) when
                                                token0 is the numeraire), from 5-minute and 1-hour returns

F / LVR > 1: at-the-money liquidity earns more than it loses to arbitrage, before swap and gas costs.
The ratio does not depend on how much liquidity we add when volume scales with depth (Solidly), and is
the marginal case for a deep pool. Solidly pools use the processor's hourly fee rate.

    python fee_vs_lvr.py <pool-name> ...
"""
import os
import sys

import numpy as np
import pandas as pd

sys.path.insert(0, os.path.dirname(__file__))
import vsim  # noqa: E402


def weekly(m: vsim.Market, fee=None):
    fee = m.fee if fee is None else fee
    wk = (m.ts - m.ts[0]) // (7 * 86400)
    f = pd.Series(fee / np.maximum(m.L, 1e-18)).groupby(wk).sum()
    root = m.sp if m.pool.numeraire == 1 else 1 / m.sp
    bw = (m.bar_end - m.ts[0]) // (7 * 86400)
    rb = root[m.bar_idx]
    lvr5 = pd.Series(m.bar_r2 * rb / 4).groupby(bw).sum()
    lp = m.bar_lp
    h = np.arange(0, len(lp), 12)
    r1 = np.diff(lp[h], prepend=lp[0])
    lvr60 = pd.Series(r1 * r1 * rb[h] / 4).groupby(bw[h]).sum()
    out = pd.DataFrame({"F": f, "lvr5": lvr5, "lvr60": lvr60}).dropna()
    out["ratio5"], out["ratio60"] = out.F / out.lvr5, out.F / out.lvr60
    return out


if __name__ == "__main__":
    for name in sys.argv[1:]:
        m = vsim.load_market(vsim.POOLS[name])
        w = weekly(m)
        tot = w[["F", "lvr5", "lvr60"]].sum()
        print(f"{name:<22} weeks {len(w):2d}  F/LVR(5m) {tot.F / tot.lvr5:5.2f}  F/LVR(1h) {tot.F / tot.lvr60:5.2f}  "
              f"weeks above 1 (1h): {(w.ratio60 > 1).mean():.0%}   weekly 1h ratios: " + " ".join(f"{x:.2f}" for x in w.ratio60))
