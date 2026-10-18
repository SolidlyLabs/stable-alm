"""Scores the multi-timeframe grid (grid4.py) by candle length and slow filter: walk-forward re-picking
inside each group, the group's median config, and the n=336/hot 24h/trend_lp 1/k 5 config itself.

    python score4.py results/mtf_c1.0.pkl [...]
"""
import os
import pickle
import sys

import numpy as np

import score

CAP = score.CAP


def main():
    for path in sys.argv[1:]:
        res = pickle.load(open(path, "rb"))
        V = np.array([r["value"] for r in res], float)
        Hd = np.array(res[0]["hold"], float)
        E = (np.diff(V, axis=1) - np.diff(Hd)) / CAP
        days = E.shape[1]
        print(f"\n== {os.path.basename(path)}: {len(res)} runs, {days / 365:.1f} years (park 5%)")
        print(f"{'candles':>8}{'slow':>6}{'n':>5}{'OOS %/yr':>9}{'95% interval':>17}{'median':>8}{'best IS':>8}{'PBO':>6}{'ref':>8}{'in pool':>8}")
        for k in ("hold", "band"):
            i = next(j for j, r in enumerate(res) if r["kind"] == k)
            print(f"{k:>14}{'':>5}{E[i].mean() * 36500:>9.2f}")
        groups = sorted({(r["params"]["tf"], r["params"]["slow"]) for r in res if r["kind"] == "mtf"})
        for tf, slow in groups:
            sel = np.array([r["kind"] == "mtf" and r["params"]["tf"] == tf and r["params"]["slow"] == slow for r in res])
            Ef = E[sel]
            m = Ef.mean(axis=1)
            oos, t = [], 180
            while t + 1 <= days:
                oos.append(Ef[Ef[:, t - 180:t].sum(axis=1).argmax(), t:t + score.TEST])
                t += score.TEST
            x = np.concatenate(oos)
            lo, hi = score.boot(x)
            ref = [i for i, r in enumerate(res) if r["kind"] == "mtf" and r["params"] == {"tf": tf, "slow": slow, "n": 336, "hot_h": 24, "min_lp": 0.0,
                                                                                          "trend_lp": 1.0, "k": 5, "park": 0.05}]
            rv = E[ref[0]].mean() * 36500 if ref else float("nan")
            dep = res[ref[0]]["summary"]["deployed"] if ref else float("nan")
            print(f"{str(tf) + 'h':>8}{(str(slow // 24) + 'd') if slow else '-':>6}{len(Ef):>5}{x.mean() * 36500:>9.2f}{f'({lo * 36500:.1f} to {hi * 36500:.1f})':>17}"
                  f"{np.median(m) * 36500:>8.2f}{m.max() * 36500:>8.2f}{score.pbo(Ef):>6.2f}{rv:>8.2f}{dep:>8.2f}")


if __name__ == "__main__":
    main()
