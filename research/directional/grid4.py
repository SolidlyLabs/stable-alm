"""Multi-timeframe Combo grid on the long ETH path (dmtf.ComboTF): candle length x slow trend filter.

    python grid4.py [c ...]   ->  results/mtf_c<c>.pkl   (parking at 5%, as the app would run it)
"""
import itertools
import os
import pickle
import sys
import time
from concurrent.futures import ProcessPoolExecutor

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
import dmtf  # noqa: E402
import dsim  # noqa: E402
import dstrat  # noqa: E402
import grid  # noqa: E402

PARK = 0.05


def configs():
    out = [("hold", {}), ("band", {"k": 5, "delay": 4})]
    for tf, slow, n, hh, tl, k in itertools.product((1, 4, 24), (0, 720, 2160), (168, 336, 720), (24, 72), (0.5, 1.0), (3, 5)):
        if tf * 7 > n:
            continue
        out.append(("mtf", {"tf": tf, "slow": slow, "n": n, "hot_h": hh, "min_lp": 0.0, "trend_lp": tl, "k": k}))
    return out


def _one(job):
    kind, p, c = job
    if kind == "mtf":
        s = dmtf.ComboTF(grid._F, ts=grid._TS, **p)
    else:
        s = {"hold": dstrat.Hold, "band": dstrat.Band}[kind](grid._F, **p)
    r = dsim.run(grid._TS, grid._P, s, costs=dsim.Costs(c=c, park0=PARK, park1=PARK), h0=grid.H0)
    return {"kind": kind, "params": {**p, "park": PARK}, "c": c, "summary": dsim.summary(r, 10_000.0), "ts": r.ts,
            "value": r.value.astype(np.float32), "hold": r.hold.astype(np.float32), "deployed": r.deployed}


def main():
    cs = [float(c) for c in sys.argv[1:]] or [1.0]
    cf = configs()
    for c in cs:
        t = time.time()
        jobs = [(k, p, c) for k, p in cf]
        with ProcessPoolExecutor(grid.WORKERS, initializer=grid._init) as ex:
            res = list(ex.map(_one, jobs, chunksize=8))
        pickle.dump(res, open(os.path.join(grid.OUT, f"mtf_c{c}.pkl"), "wb"))
        print(f"c={c}: {len(res)} runs in {time.time() - t:.0f}s", flush=True)


if __name__ == "__main__":
    main()
