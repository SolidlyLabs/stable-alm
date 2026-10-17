"""Paired effect of one rule across the grid: daily alpha difference between configs that differ only
in that parameter, averaged over all such pairs; 95% interval from a 7-day block bootstrap of the days."""
import sys

import numpy as np

from score import boot, daily, load


def pairs(res, key, a, b):
    idx = {}
    for j, r in enumerate(res):
        if r["kind"] != "band":
            continue
        p = dict(r["params"])
        v = p.pop(key)
        idx.setdefault(tuple(sorted(p.items())), {})[v] = j
    return [(g[a], g[b]) for g in idx.values() if a in g and b in g]


def effect(pool, cap, key, a, b, where=None):
    res = load(pool, cap)
    d, _ = daily(res)
    pr = pairs(res, key, a, b)
    if where:
        pr = [(i, j) for i, j in pr if where(res[i]["params"])]
    diff = np.mean([d[j] - d[i] for i, j in pr], axis=0)
    lo, hi = boot(diff, cap)
    return diff.mean() * 365 / cap * 100, lo, hi, len(pr)


if __name__ == "__main__":
    for pool in sys.argv[1:]:
        for cap in (10_000, 100_000):
            try:
                g = effect(pool, cap, "gate", None, 4.0)
                n = effect(pool, cap, "k", 2.0, 0.5)
                w = effect(pool, cap, "k", 2.0, 5.0)
                dl = effect(pool, cap, "delay", 0, 48)
            except FileNotFoundError:
                continue
            f = lambda e: f"{e[0]:+6.1f} ({e[1]:+.1f} to {e[2]:+.1f}, n={e[3]})"  # noqa: E731
            print(f"{pool:<22} ${cap:>7,}  gate4 vs none {f(g)} | k0.5 vs k2 {f(n)} | k5 vs k2 {f(w)} | delay48 vs 0 {f(dl)}")
