"""Guarded vs unguarded families: walk-forward within each (family, guard) group.

Usage: python scripts/guard_compare.py <grid_name> [exclude_may25]
"""
import os
import re
import sys

import pandas as pd

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.walkforward import summarize, walk_forward  # noqa: E402

REPORTS = os.path.join(os.path.dirname(__file__), "..", "reports")
DEPEGS = {"may22": ("2022-05-08", "2022-05-20"), "mar23": ("2023-03-09", "2023-03-20")}


def group(c: str) -> str:
    g = re.search(r"guard=(\d+)", c)
    return c.split("(")[0] + ("_noswap" if "swap=False" in c else "") + "|guard=" + (g.group(1) if g else "none")


def main():
    name = sys.argv[1]
    p = pd.read_parquet(os.path.join(REPORTS, f"{name}.parquet"))
    if len(sys.argv) > 2:
        p = p[(p.index < "2025-05-19") | (p.index > "2025-05-24")]
    sc = 15000 * 0.5 * 0.3e-4 + 0.06
    g = {c: group(c) for c in p.columns}
    rows = []
    for f in sorted(set(g.values())):
        cols = [c for c in p.columns if g[c] == f]
        oos = walk_forward(p[cols], 56, 14, sc)["oos"]
        dep = {k: float(oos.loc[a:b].sum()) for k, (a, b) in DEPEGS.items() if oos.index[0] <= pd.Timestamp(a)}
        rows.append({"group": f, **{k: v for k, v in summarize(oos, 15000).items() if k in ("pnl", "apr_pct", "max_drawdown", "worst_day")},
                     "median_config": float(p[cols].iloc[56:].sum().median()), **dep})
    df = pd.DataFrame(rows).sort_values("pnl", ascending=False)
    out = os.path.join(REPORTS, f"{name}_guard_compare.md")
    open(out, "w", encoding="utf-8").write(df.round(1).to_markdown(index=False))
    print(df.round(1).to_string(index=False))


if __name__ == "__main__":
    main()
