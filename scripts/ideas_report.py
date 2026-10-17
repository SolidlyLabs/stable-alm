"""Score the novel-ideas grid (alm/grid.py: ideas_grid) against the live ladder family.

Usage: python scripts/ideas_report.py [grid prefix, default 'ideas']
Reads reports/<prefix>_<pair>.parquet for every pair; writes reports/<prefix>_report.md.

Overfitting controls:
- every family is scored only by walk-forward (configs chosen on past data, booked on the next window),
  at three train/test settings;
- the gain over the baseline family is the out-of-sample daily difference, with a 95% interval from a
  14-day block bootstrap, and it must hold in both halves of the period;
- the ideas were designed on USDC/USDT, so cbETH/WETH and USDC/USDbC are hold-out pairs;
- PBO (CSCV) over the whole grid; the median config of each family is shown too (no selection at all).
"""
import os
import re
import sys

import numpy as np
import pandas as pd

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.data import POOLS  # noqa: E402
from alm.walkforward import pbo, summarize, walk_forward  # noqa: E402

R = os.path.join(os.path.dirname(__file__), "..", "reports")
EXCLUDE = {"usdc-usdt": ("2025-05-19", "2025-05-24")}
UNIT = {"usdc-usdt": "USDC", "usdc-usdbc": "USDC", "cbeth-weth": "WETH"}
BASE = "ladder (baseline)"


def family(c: str) -> str:
    if c.startswith("ladder("):
        return BASE
    if c.startswith("dual("):
        return "dual fast/slow"
    if "forecast=" in c:
        return "forecast mix"
    if "decay_hl_s" in c and "emis=True" in c and "core_frac" in c:
        return "combo decay+emis+core"
    if "tag=fast" in c:
        return "fast re-solve (1-2h)"
    for key, name in (("swapfree", "swap-free"), ("core_frac", "core band"), ("exit_s", "exit re-solve"),
                      ("emis=True", "emission-aware"), ("center=blend", "blended centre"), ("decay_hl_s", "recency decay")):
        if key in c:
            return name
    return "other"


def boot_ci(diff: pd.Series, block: int = 14, n: int = 2000, seed: int = 7) -> tuple:
    """95% interval of the mean daily difference, by moving-block bootstrap."""
    rng = np.random.default_rng(seed)
    x = diff.to_numpy()
    nb = int(np.ceil(len(x) / block))
    starts = rng.integers(0, max(1, len(x) - block + 1), size=(n, nb))
    means = np.array([np.concatenate([x[s : s + block] for s in row])[: len(x)].mean() for row in starts])
    return float(np.percentile(means, 2.5)), float(np.percentile(means, 97.5))


def main():
    prefix = sys.argv[1] if len(sys.argv) > 1 else "ideas"
    lines = [f"# Novel ideas vs the live ladder (`{prefix}` grid)", "",
             "Walk-forward (configs picked on the train window only, booked on the next test window), "
             "new-fee scenario, 15-tick depeg guard everywhere. Gains are out-of-sample APR points over the "
             "baseline ladder family, with a 95% block-bootstrap interval.", ""]
    for name in POOLS:
        path = os.path.join(R, f"{prefix}_{name}.parquet")
        if not os.path.exists(path):
            continue
        p = pd.read_parquet(path)
        if name in EXCLUDE:
            a, b = EXCLUDE[name]
            p = p[(p.index < a) | (p.index > b)]
        cap = POOLS[name]["capital"]
        sc = cap * 0.5 * 0.3e-4 + 3 * POOLS[name]["gas"]
        fam = {c: family(c) for c in p.columns}
        oos = {}
        rows = []
        for f in sorted(set(fam.values()), key=lambda f: (f != BASE, f)):
            cols = [c for c in p.columns if fam[c] == f]
            row = {"family": f, "configs": len(cols)}
            for tr, te in ((28, 7), (56, 14), (91, 28)):
                wf = walk_forward(p[cols], tr, te, sc)["oos"]
                row[f"wf {tr}/{te} APR%"] = summarize(wf, cap)["apr_pct"]
                if (tr, te) == (56, 14):
                    oos[f] = wf
                    s = summarize(wf, cap)
                    row["max_dd"] = s["max_drawdown"]
            row["median cfg APR%"] = float(p[cols].iloc[56:].sum().median() / cap * 365 / len(p.iloc[56:]) * 100)
            rows.append(row)
        t = pd.DataFrame(rows)
        base = oos[BASE]
        gains = []
        for f, s in oos.items():
            if f == BASE:
                continue
            d = (s - base.reindex(s.index).fillna(0.0)).dropna()
            k = 365 / cap * 100
            lo, hi = boot_ci(d)
            half = len(d) // 2
            gains.append({"family": f, "gain APR pts": d.mean() * k, "95% CI low": lo * k, "95% CI high": hi * k,
                          "1st half": d.iloc[:half].mean() * k, "2nd half": d.iloc[half:].mean() * k,
                          "days better %": float((d > 0).mean() * 100)})
        g = pd.DataFrame(gains).sort_values("gain APR pts", ascending=False)
        allwf = summarize(walk_forward(p, 56, 14, sc)["oos"], cap)
        basewf = summarize(oos[BASE], cap)
        lines += [f"## {name} ({len(p.columns)} configs, {len(p)} days, capital {cap:g} {UNIT[name]})", "",
                  t.round(3).to_markdown(index=False), "", "Gain over the baseline family (56/14 walk-forward):", "",
                  g.round(3).to_markdown(index=False), "",
                  f"Walk-forward choosing across the whole grid: {allwf['apr_pct']:.2f}% APR vs baseline family "
                  f"{basewf['apr_pct']:.2f}%. PBO over the grid: {pbo(p.loc[:, p.abs().sum() > 0]):.2f}.", ""]
    out = os.path.join(R, f"{prefix}_report.md")
    open(out, "w", encoding="utf-8").write("\n".join(lines))
    print("\n".join(lines))


if __name__ == "__main__":
    main()
