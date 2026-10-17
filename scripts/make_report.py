"""Equity chart: walk-forward out-of-sample PnL per family vs the old rule and holding USDC.

Usage: python scripts/make_report.py [grid_name] [capital]
"""
import os
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
import pandas as pd  # noqa: E402

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.walkforward import walk_forward  # noqa: E402

REPORTS = os.path.join(os.path.dirname(__file__), "..", "reports")


def main():
    name = sys.argv[1] if len(sys.argv) > 1 else "grid"
    capital = float(sys.argv[2]) if len(sys.argv) > 2 else 15_000.0
    pnl = pd.read_parquet(os.path.join(REPORTS, f"{name}.parquet"))
    switch_cost = capital * 0.5 * 0.3e-4 + 3 * 0.02
    fam = {c: c.split("(")[0] + ("_noswap" if "swap=False" in c else "") for c in pnl.columns}
    hold = [c for c in pnl.columns if c.startswith("hold_usdc")]
    curves = {}
    for f, color in (("ladder", "#1f6feb"), ("volband", "#8250df"), ("recenter", "#bf8700"), ("static", "#2da44e")):
        cols = [c for c in pnl.columns if fam[c] == f]
        curves[f"{f} family, walk-forward"] = (walk_forward(pnl[cols], 56, 14, switch_cost)["oos"], color, "-")
    allwf = walk_forward(pnl, 56, 14, switch_cost, exclude=hold)["oos"]
    curves["all 250 configs, walk-forward"] = (allwf, "#57606a", "-")
    start = allwf.index[0]
    old = [c for c in pnl.columns if c.startswith("recenter(k=200")][0]
    curves["old alm.ts rule (+-2%, recentre)"] = (pnl.loc[start:, old], "#cf222e", "--")
    fig, ax = plt.subplots(figsize=(11, 5.8))
    for label, (s, color, ls) in curves.items():
        s = s.loc[start:]
        ax.plot(s.cumsum(), color=color, ls=ls, lw=2.2 if "ladder" in label else 1.4,
                label=f"{label}: {s.sum():+,.0f} USDC")
    ax.axhline(0, color="black", lw=0.6, label="hold USDC")
    ax.set_ylabel(f"Cumulative net profit, USDC (on ${capital:,.0f})")
    ax.set_title(f"Out-of-sample (56d train / 14d test) — {name}")
    ax.legend(loc="upper left", fontsize=9)
    ax.grid(alpha=0.25)
    fig.tight_layout()
    out = os.path.join(REPORTS, f"{name}_equity.png")
    fig.savefig(out, dpi=130)
    print(out)


if __name__ == "__main__":
    main()
