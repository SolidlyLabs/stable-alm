"""Neighbourhood of the Combo winner: excess vs hold %/yr (worst calendar year) per n x hot_h."""
import pickle
import sys

import numpy as np

for c in ("0.55", "1.0", "2.0"):
    res = pickle.load(open(f"results/park_c{c}.pkl", "rb"))
    ts = res[0]["ts"]
    years = np.array([int(np.datetime64(int(t), "s").astype("datetime64[Y]").astype(int) + 1970) for t in ts[1:]])
    Hd = np.diff(np.array(res[0]["hold"], float))
    print(f"\nc={c}  (min_lp=0, trend_lp=1, k=5, protect, park 5%): total (worst year)  in pool")
    print("hot_h     " + "".join(f"{n:>22}" for n in (168, 336, 720)))
    for hh in (24, 72, 168, 336):
        row = f"{hh:>6}    "
        for n in (168, 336, 720):
            r = next(r for r in res if r["kind"] == "combo" and r["params"] == {"n": n, "hot_h": hh, "min_lp": 0.0, "trend_lp": 1.0, "k": 5,
                                                                              "protect": True, "park": 0.05})
            e = (np.diff(np.array(r["value"], float)) - Hd) / 1e4
            by = [e[years == y].mean() * 36500 for y in sorted(set(years))]
            row += f"{e.mean() * 36500:>9.1f} ({min(by):6.1f}) {r['summary']['deployed']:4.2f}"
        print(row)
