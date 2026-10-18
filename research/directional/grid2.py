"""Partial allocation (Blend): how much of the capital farms and how much waits in the trend's winner.

    python grid2.py [c ...]   ->  results/blend_c<c>.pkl
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


def configs():
    out = [("hold", {}), *[("band", {"k": k, "delay": 4}) for k in (3, 5)]]
    for sig, n, th0, a, mn, k, pr, ln in itertools.product(("mom", "ema"), (96, 168, 336), (0.5, 1.0), (0.5, 1.0, 3.0), (0.0, 0.5), (3, 5),
                                                         (True, False), (0.0, 0.5)):
        out.append(("blend", {"sig": sig, "n": n, "th0": th0, "a": a, "min_lp": mn, "k": k, "protect": pr, "lean": ln}))
    for n, mn, k, pr, ln, a in itertools.product((96, 168, 336), (0.0, 0.25, 0.5, 0.75), (3, 5), (True, False), (0.0, 0.5), (1.0,)):
        out.append(("blend", {"sig": "tur", "n": n, "th0": 0.0, "a": 1.0 - mn, "min_lp": mn, "k": k, "protect": pr, "lean": ln}))
    return out


def _one(job):
    kind, p, c = job
    s = {"hold": dstrat.Hold, "band": dstrat.Band, "blend": dstrat.Blend}[kind](grid._F, **p)
    r = dsim.run(grid._TS, grid._P, s, costs=dsim.Costs(c=c), h0=grid.H0)
    return {"kind": kind, "params": p, "c": c, "summary": dsim.summary(r, 10_000.0), "ts": r.ts, "value": r.value.astype(np.float32),
            "hold": r.hold.astype(np.float32), "token0": r.token0, "in_pool": r.in_pool, "deployed": r.deployed, "active": r.active}


def main():
    cs = [float(c) for c in sys.argv[1:]] or [1.0]
    cf = configs()
    for c in cs:
        t = time.time()
        with ProcessPoolExecutor(grid.WORKERS, initializer=grid._init) as ex:
            res = list(ex.map(_one, [(k, p, c) for k, p in cf], chunksize=20))
        pickle.dump(res, open(os.path.join(grid.OUT, f"blend_c{c}.pkl"), "wb"))
        print(f"c={c}: {len(res)} configs in {time.time() - t:.0f}s", flush=True)


if __name__ == "__main__":
    main()
