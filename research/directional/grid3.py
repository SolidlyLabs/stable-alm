"""Parking and the move-aware Combo: idle capital earns a pegged pool's APR while it waits.

    python grid3.py [c ...]   ->  results/park_c<c>.pkl   (each config at park APRs 0, 5% and 10%)
"""
import itertools
import os
import pickle
import sys
import time
from concurrent.futures import ProcessPoolExecutor

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
import dsim  # noqa: E402
import dstrat  # noqa: E402
import grid  # noqa: E402

PARKS = (0.0, 0.05, 0.10)


def configs():
    out = [("hold", {}), *[("band", {"k": k, "delay": 4}) for k in (3, 5)]]
    for n, mn, k, pr in itertools.product((168, 336, 720), (0.0, 0.25, 0.5, 0.75), (3, 5), (True, False)):
        out.append(("blend", {"sig": "tur", "n": n, "th0": 0.0, "a": 1.0 - mn, "min_lp": mn, "k": k, "protect": pr}))
    for n, hh, mn, tl, k, pr in itertools.product((168, 336, 720), (24, 72, 168, 336), (0.0, 0.25, 0.5), (0.5, 0.75, 1.0), (3, 5), (True, False)):
        if tl < mn:
            continue
        out.append(("combo", {"n": n, "hot_h": hh, "min_lp": mn, "trend_lp": tl, "k": k, "protect": pr}))
    return out


def _one(job):
    kind, p, c, park = job
    s = {"hold": dstrat.Hold, "band": dstrat.Band, "blend": dstrat.Blend, "combo": dstrat.Combo}[kind](grid._F, **p)
    r = dsim.run(grid._TS, grid._P, s, costs=dsim.Costs(c=c, park0=park, park1=park), h0=grid.H0)
    return {"kind": kind, "params": {**p, "park": park}, "c": c, "summary": dsim.summary(r, 10_000.0), "ts": r.ts,
            "value": r.value.astype(np.float32), "hold": r.hold.astype(np.float32), "token0": r.token0, "in_pool": r.in_pool,
            "deployed": r.deployed, "active": r.active}


def main():
    cs = [float(c) for c in sys.argv[1:]] or [1.0]
    cf = configs()
    for c in cs:
        t = time.time()
        jobs = [(k, p, c, pk) for k, p in cf for pk in PARKS]
        with ProcessPoolExecutor(grid.WORKERS, initializer=grid._init) as ex:
            res = list(ex.map(_one, jobs, chunksize=20))
        pickle.dump(res, open(os.path.join(grid.OUT, f"park_c{c}.pkl"), "wb"))
        print(f"c={c}: {len(res)} runs in {time.time() - t:.0f}s", flush=True)


if __name__ == "__main__":
    main()
