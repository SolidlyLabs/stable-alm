"""Walk-forward scoring of the directional grid.

    python score.py results/eth_c1.0.pkl [...]  ->  prints the tables, writes results/<name>_wf.csv

Each day's excess = the strategy's value change minus the 50/50 hold's, over the capital. Per family of
configs and train length: at the start of every 30-day test window, the config with the most excess in
the train window before it is booked for the window. The OOS excess is annualised with a 95% interval
from a 30-day block bootstrap, and each half of the period is shown on its own. "Median" is the median
config of the family over the whole period with no selection at all; "best IS" is the in-sample best
(what a backtest alone would show). PBO is the probability that the in-sample winner of half the
period ranks in the bottom half on the other half (CSCV over 12 blocks).
"""
import itertools
import os
import pickle
import sys

import numpy as np

CAP = 10_000.0
TEST = 30


def family(r):
    k, p = r["kind"], r["params"]
    if k == "hold":
        return "hold"
    if k == "blend":
        return "blend: turtle" if p["sig"] == "tur" else "blend: momentum"
    if k == "band":
        return "band (passive LP)"
    if p.get("vgate"):
        return "volatility exit"
    if not p.get("lp", True):
        return "trend switch, no LP"
    if p["up"] == "hold" and p["dn"] == "hold":
        return "LP when flat, hold winner"
    return "directional ranges"


def boot(x, block=30, n=2000, seed=1):
    rng = np.random.default_rng(seed)
    nb = max(1, len(x) // block)
    starts = np.arange(0, len(x) - block + 1)
    means = []
    for _ in range(n):
        idx = np.concatenate([np.arange(s, s + block) for s in rng.choice(starts, nb)])
        means.append(x[idx].mean())
    return np.percentile(means, [2.5, 97.5])


def pbo(E, blocks=12):
    """E: configs x days excess. CSCV over `blocks` contiguous blocks."""
    D = E.shape[1] // blocks * blocks
    B = E[:, :D].reshape(E.shape[0], blocks, -1).mean(axis=2)
    below = tot = 0
    for ins in itertools.combinations(range(blocks), blocks // 2):
        oos = [b for b in range(blocks) if b not in ins]
        i = B[:, list(ins)].mean(axis=1).argmax()
        o = B[:, oos].mean(axis=1)
        rank = (o < o[i]).mean()
        below += rank < 0.5
        tot += 1
    return below / tot


def main():
    for path in sys.argv[1:]:
        res = pickle.load(open(path, "rb"))
        ts = res[0]["ts"]
        V = np.array([r["value"] for r in res], float)
        Hd = np.array(res[0]["hold"], float)
        E = (np.diff(V, axis=1) - np.diff(Hd)) / CAP  # configs x days
        fam = np.array([family(r) for r in res])
        days = E.shape[1]
        print(f"\n== {os.path.basename(path)}: {len(res)} configs, {days} days ({days / 365:.1f} years)")
        print(f"{'family':<28} {'n':>4} {'train':>5} {'OOS %/yr':>9} {'95% interval':>18} {'1st half':>9} {'2nd half':>9} {'median':>8} {'best IS':>8} {'PBO':>5}")
        rows = []
        for fname in ["band (passive LP)", "trend switch, no LP", "LP when flat, hold winner", "directional ranges", "volatility exit",
                      "blend: momentum", "blend: turtle", "ALL"]:
            sel = np.ones(len(res), bool) if fname == "ALL" else fam == fname
            Ef = E[sel]
            if not len(Ef):
                continue
            med = np.median(Ef.mean(axis=1)) * 365 * 100
            best = Ef.mean(axis=1).max() * 365 * 100
            pb = pbo(Ef) if len(Ef) > 2 else float("nan")
            for train in (90, 180, 365):
                oos = []
                t = train
                while t + 1 <= days:
                    i = Ef[:, t - train:t].sum(axis=1).argmax()
                    oos.append(Ef[i, t:t + TEST])
                    t += TEST
                x = np.concatenate(oos)
                lo, hi = boot(x)
                h = len(x) // 2
                row = (fname, int(sel.sum()), train, x.mean() * 36500, lo * 36500, hi * 36500, x[:h].mean() * 36500, x[h:].mean() * 36500, med, best, pb)
                rows.append(row)
                print(f"{row[0]:<28} {row[1]:>4} {train:>5} {row[3]:>9.2f} {f'({row[4]:.1f} to {row[5]:.1f})':>18} {row[6]:>9.2f} {row[7]:>9.2f} {med:>8.2f} {best:>8.2f} {pb:>5.2f}")
        # the best fixed configs per family, and how they did each year
        print("\nbest fixed configs (in sample), excess %/yr by calendar year:")
        years = np.array([int(np.datetime64(int(t), "s").astype("datetime64[Y]").astype(int) + 1970) for t in ts[1:]])
        order = np.argsort(-E.mean(axis=1))[:12]
        for i in order:
            by = "  ".join(f"{y}: {E[i, years == y].mean() * 36500:6.1f}" for y in sorted(set(years)))
            print(f"  {E[i].mean() * 36500:7.2f}  {by}  {res[i]['summary']['label'][:100]}")
        import csv
        with open(path.replace(".pkl", "_wf.csv"), "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(["family", "n", "train", "oos_apr", "lo", "hi", "first_half", "second_half", "median", "best_is", "pbo"])
            w.writerows(rows)


if __name__ == "__main__":
    main()
