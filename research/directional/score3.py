"""Scores the parking grid (grid3.py): walk-forward per family and park APR, plus what the protocol sees.

    python score3.py results/park_c1.0.pkl [...]

Columns as in score.py (OOS = walk-forward re-picking with a 180-day train window). "fees" is the median
config's fee APR and "in pool" its average share of capital in the main pool: what the protocol gains
in liquidity and fees. "park" is the yield idle capital earned in pegged pools (also liquidity there).
"""
import os
import pickle
import sys

import numpy as np

import score

CAP = score.CAP


def family(r):
    k = r["kind"]
    if k == "combo":
        return "combo (wait out the move)"
    if k == "blend":
        return "blend: turtle"
    return {"hold": "hold (+park)", "band": "band (passive LP)"}[k]


def main():
    for path in sys.argv[1:]:
        res = pickle.load(open(path, "rb"))
        ts = res[0]["ts"]
        V = np.array([r["value"] for r in res], float)
        Hd = np.array(res[0]["hold"], float)
        E = (np.diff(V, axis=1) - np.diff(Hd)) / CAP
        fam = np.array([family(r) for r in res])
        park = np.array([r["params"]["park"] for r in res])
        days = E.shape[1]
        years = np.array([int(np.datetime64(int(t), "s").astype("datetime64[Y]").astype(int) + 1970) for t in ts[1:]])
        print(f"\n== {os.path.basename(path)}: {len(res)} runs, {days / 365:.1f} years")
        print(f"{'family':<27}{'park':>5}{'n':>5}{'OOS %/yr':>9}{'95% interval':>17}{'median':>8}{'best IS':>8}{'PBO':>6}{'fees':>7}{'in pool':>8}{'park':>6}")
        for fname in ["hold (+park)", "band (passive LP)", "blend: turtle", "combo (wait out the move)"]:
            for pk in sorted(set(park)):
                sel = (fam == fname) & (park == pk)
                Ef = E[sel]
                if not len(Ef):
                    continue
                m = Ef.mean(axis=1)
                imed = np.where(sel)[0][np.argsort(m)[len(m) // 2]]
                sm = res[imed]["summary"]
                oos, t = [], 180
                while t + 1 <= days:
                    oos.append(Ef[Ef[:, t - 180:t].sum(axis=1).argmax(), t:t + score.TEST])
                    t += score.TEST
                x = np.concatenate(oos)
                lo, hi = score.boot(x)
                pb = score.pbo(Ef) if len(Ef) > 2 else float("nan")
                print(f"{fname:<27}{pk * 100:>4.0f}%{len(Ef):>5}{x.mean() * 36500:>9.2f}{f'({lo * 36500:.1f} to {hi * 36500:.1f})':>17}"
                      f"{np.median(m) * 36500:>8.2f}{m.max() * 36500:>8.2f}{pb:>6.2f}{sm['fees_apr']:>7.1f}{sm['deployed']:>8.2f}{sm['park_apr']:>6.1f}")
        # configs that beat hold+park every year while keeping at least half the capital in the pool
        print("\nconfigs >= 50% in the pool on average, beating hold every calendar year (excess %/yr; park 5%):")
        rows = []
        for i, r in enumerate(res):
            if r["params"]["park"] != 0.05 or r["summary"]["deployed"] < 0.5:
                continue
            by = [E[i, years == y].mean() * 36500 for y in sorted(set(years))]
            if min(by) > 0:
                rows.append((E[i].mean() * 36500, by, r))
        for tot, by, r in sorted(rows, key=lambda z: -min(z[1]))[:12]:
            s = r["summary"]
            print(f"  {tot:7.2f}  " + "  ".join(f"{b:6.1f}" for b in by) +
                  f"  in pool {s['deployed']:.2f} fees {s['fees_apr']:5.1f} acts {s['actions']:4d}  {s['label'][:90]}")


if __name__ == "__main__":
    main()
