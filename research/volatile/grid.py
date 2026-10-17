"""Run the fee-capture grid on one pool and save daily series for walk-forward scoring.

    python grid.py <pool-name> [capital ...]     ->  results/<pool>_<capital>.pkl
"""
import itertools
import os
import pickle
import sys
import time
from concurrent.futures import ProcessPoolExecutor

sys.path.insert(0, os.path.dirname(__file__))
import vsim  # noqa: E402
import vstrat  # noqa: E402

OUT = os.path.join(os.path.dirname(__file__), "results")
WORKERS = 2  # keep the PC responsive
_M = None


def configs():
    out = [("full", {})]
    for k, d, g in itertools.product([0.5, 1.0, 1.5, 2.0, 3.0, 5.0], [0, 3, 12, 48], [None, 4.0]):
        out.append(("band", {"k": k, "delay": d, "gate": g}))
    for lvr, ev, dl in itertools.product([1.0, 2.0, 4.0], [72, 288], [12, 48]):
        out.append(("feelvrf", {"lvr": lvr, "every": ev, "delay": dl}))
    return out


def make(kind, p):
    return {"full": vstrat.FullRange, "band": vstrat.Band, "feelvrf": vstrat.FeeLVRF}[kind](**p)


def _init(pool):
    global _M
    _M = vsim.load_market(vsim.POOLS[pool])


def _one(job):
    kind, p, cap = job
    t = time.time()
    r = vsim.run(_M, make(kind, p), capital=cap)
    s = vsim.summary(r, cap)
    return {"kind": kind, "params": p, "capital": cap, "summary": s, "days": r.days, "alpha": r.alpha,
            "equity": r.equity, "hodl": r.hodl, "secs": time.time() - t}


def main():
    pool = sys.argv[1]
    caps = [float(c) for c in sys.argv[2:]] or [10_000.0, 100_000.0]
    os.makedirs(OUT, exist_ok=True)
    jobs = [(k, p, c) for c in caps for k, p in configs()]
    res = []
    vsim.load_market(vsim.POOLS[pool])  # build the cache once, before the workers start
    with ProcessPoolExecutor(WORKERS, initializer=_init, initargs=(pool,)) as ex:
        for n, r in enumerate(ex.map(_one, jobs), 1):
            res.append(r)
            s = r["summary"]
            print(f"[{n}/{len(jobs)}] {int(r['capital']):>6} {s['label'][:70]:<70} alpha {s['alpha_apr']:7.1f}% fees {s['fees_apr']:7.1f}% "
                  f"cost {s['cost_apr']:5.1f}% acts {s['actions']:4d} ({r['secs']:.0f}s)", flush=True)
    for c in caps:
        pickle.dump([r for r in res if r["capital"] == c], open(os.path.join(OUT, f"{pool}_{int(c)}.pkl"), "wb"))


if __name__ == "__main__":
    main()
