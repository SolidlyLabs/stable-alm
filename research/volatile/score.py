"""Walk-forward scoring of grid results.

For each (pool, capital): daily alpha (vs REBAL) per config from the cumulative series. Every `test`
days, pick the config (optionally within a family) with the highest alpha over the previous `train`
days and book its alpha over the next `test` days. Reported as APR on capital, with a 95% interval from a
7-day block bootstrap of the booked daily alphas.

    python score.py <pool> <capital> [train test]
"""
import os
import pickle
import sys

import numpy as np

HERE = os.path.dirname(__file__)


def load(pool, cap):
    return pickle.load(open(os.path.join(HERE, "results", f"{pool}_{int(cap)}.pkl"), "rb"))


def daily(res):
    """configs x days matrix of daily alpha; common day grid = the shortest series."""
    n = min(len(r["alpha"]) for r in res)
    a = np.array([r["alpha"][:n] for r in res])
    return np.diff(a, axis=1), res[0]["days"][1:n]


def walk(d, train=28, test=7, family=None, labels=None):
    nd = d.shape[1]
    idx = np.arange(d.shape[0]) if family is None else np.array([i for i, l in enumerate(labels) if l.startswith(family)])
    booked, picks = [], []
    t = train
    while t < nd:
        score = d[idx, t - train : t].sum(axis=1)
        j = idx[int(np.argmax(score))]
        booked.append(d[j, t : t + test])
        picks.append(j)
        t += test
    return np.concatenate(booked) if booked else np.array([]), picks


def boot(x, cap, block=7, n=2000, seed=1):
    if len(x) < block:
        return float("nan"), float("nan")
    rng = np.random.default_rng(seed)
    nb = len(x) // block
    blocks = np.array([x[i * block : (i + 1) * block] for i in range(nb)])
    means = [blocks[rng.integers(0, nb, nb)].mean() for _ in range(n)]
    return tuple(np.percentile(means, [2.5, 97.5]) * 365 / cap * 100)


def report(pool, cap, train=28, test=7):
    res = load(pool, cap)
    labels = [r["summary"]["label"] for r in res]
    d, days = daily(res)
    out = {"pool": pool, "capital": cap, "days": d.shape[1]}
    for fam in (None, "band", "feelvrf"):
        x, picks = walk(d, train, test, fam, labels)
        lo, hi = boot(x, cap)
        out[fam or "all"] = {"apr": x.mean() * 365 / cap * 100 if len(x) else float("nan"), "lo": lo, "hi": hi,
                             "picks": [labels[j] for j in picks]}
    full = d.sum(axis=1) / d.shape[1] * 365 / cap * 100
    order = np.argsort(-full)
    out["top"] = [(labels[j], float(full[j])) for j in order[:8]]
    out["median"] = float(np.median(full))
    out["by_label"] = dict(zip(labels, map(float, full)))
    return out


if __name__ == "__main__":
    pool, cap = sys.argv[1], float(sys.argv[2])
    tr, te = (int(sys.argv[3]), int(sys.argv[4])) if len(sys.argv) > 4 else (28, 7)
    r = report(pool, cap, tr, te)
    print(f"{pool} ${cap:,.0f}  {r['days']} days  median config {r['median']:.1f}%")
    for k in ("all", "band", "feelvrf"):
        v = r[k]
        print(f"  walk-forward {k:<8} {v['apr']:7.1f}%  (95% {v['lo']:.1f} to {v['hi']:.1f})  picks: {sorted(set(v['picks']))[:4]}")
    print("  top whole-period configs:")
    for l, a in r["top"]:
        print(f"    {a:7.1f}%  {l}")
