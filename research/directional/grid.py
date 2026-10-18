"""The directional grid on the long ETH path: every config's daily value series, for walk-forward scoring.

    python grid.py [c ...]    ->  results/eth_c<c>.pkl   (c: the pool's fee/LVR ratio, default 1.0)
"""
import itertools
import os
import pickle
import sys
import time
from concurrent.futures import ProcessPoolExecutor

import numpy as np
import pandas as pd

sys.path.insert(0, os.path.dirname(__file__))
import dsim  # noqa: E402
import dstrat  # noqa: E402

HERE = os.path.dirname(__file__)
OUT = os.path.join(HERE, "results")
WORKERS = 2  # keep the PC responsive
H0 = 720  # a month of warm-up for the volatility
_F = _TS = _P = None


def load():
    d = pd.read_parquet(os.path.join(HERE, "..", "..", "data", "raw", "binance_ethusdt_1h.parquet")).sort_values("ts")
    return d.ts.to_numpy(np.int64), d.eth_usd.to_numpy(float)


def configs():
    out = [("hold", {}), *[("band", {"k": k, "delay": d}) for k in (1, 2, 3, 5, 8) for d in (0, 4, 24)]]
    modes = [("hold", "hold", False), ("hold", "hold", True), ("above", "hold", True), ("skew", "skew", True), ("hold", "below", True),
             ("above", "below", True), ("band", "hold", True), ("hold", "band", True)]
    for sig, n, th, off in itertools.product(("mom", "ema"), (24, 48, 96, 168, 336), (1.0, 1.5, 2.0), (0.0, 0.5)):
        for (up, dn, lp), k in itertools.product(modes, (2, 4)):
            if not lp and k != 2:
                continue
            out.append(("regime", {"sig": sig, "n": n, "th": th, "off": off, "up": up, "dn": dn, "lp": lp, "k": k}))
    for n, hh in itertools.product((24, 48, 96, 168, 336), (24, 72, 168)):
        for (up, dn, lp), k in itertools.product(modes, (2, 4)):
            if not lp and k != 2:
                continue
            out.append(("regime", {"sig": "brk", "n": n, "hold_h": hh, "up": up, "dn": dn, "lp": lp, "k": k}))
    for vg, vh, k in itertools.product((1.5, 2.0, 3.0), (6, 24, 72), (2, 4)):
        out.append(("regime", {"sig": "mom", "n": 48, "th": 99.0, "off": 0.0, "vgate": vg, "vh": vh, "k": k}))
    return out


def _init():
    global _F, _TS, _P
    _TS, _P = load()
    _F = dstrat.Features(_P)


def _one(job):
    kind, p, c = job
    s = {"hold": dstrat.Hold, "band": dstrat.Band, "regime": dstrat.Regime}[kind](_F, **p)
    r = dsim.run(_TS, _P, s, costs=dsim.Costs(c=c), h0=H0)
    return {"kind": kind, "params": p, "c": c, "summary": dsim.summary(r, 10_000.0), "ts": r.ts, "value": r.value.astype(np.float32),
            "hold": r.hold.astype(np.float32), "token0": r.token0, "in_pool": r.in_pool}


def main():
    cs = [float(c) for c in sys.argv[1:]] or [1.0]
    os.makedirs(OUT, exist_ok=True)
    cf = configs()
    for c in cs:
        t = time.time()
        jobs = [(k, p, c) for k, p in cf]
        with ProcessPoolExecutor(WORKERS, initializer=_init) as ex:
            res = list(ex.map(_one, jobs, chunksize=20))
        pickle.dump(res, open(os.path.join(OUT, f"eth_c{c}.pkl"), "wb"))
        best = sorted(res, key=lambda r: -r["summary"]["vs_hold_apr"])[:5]
        print(f"c={c}: {len(res)} configs in {time.time() - t:.0f}s; best in sample:", flush=True)
        for r in best:
            print(f"   {r['summary']['vs_hold_apr']:7.2f}%  {r['summary']['label'][:110]}", flush=True)


if __name__ == "__main__":
    main()
