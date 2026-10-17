"""Walk-forward over a saved grid: which config would have been chosen, and what it really made.

Usage: python scripts/walk_forward.py [grid_name] [capital]
"""
import os
import sys

import pandas as pd

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.walkforward import pbo, summarize, walk_forward  # noqa: E402

REPORTS = os.path.join(os.path.dirname(__file__), "..", "reports")


def main():
    name = sys.argv[1] if len(sys.argv) > 1 else "grid"
    capital = float(sys.argv[2]) if len(sys.argv) > 2 else 15_000.0
    pnl = pd.read_parquet(os.path.join(REPORTS, f"{name}.parquet"))
    fam = {c: c.split("(")[0] + ("_noswap" if "swap=False" in c else "") for c in pnl.columns}
    # One rebalance's worth: swap half the book at 0.3 bp plus three transactions of gas.
    switch_cost = capital * 0.5 * 0.3e-4 + 3 * 0.02
    lines = [f"# Walk-forward on `{name}`", "", f"{len(pnl.columns)} configs, {len(pnl)} days "
             f"({pnl.index[0].date()} to {pnl.index[-1].date()}), capital ${capital:,.0f}.", ""]
    rows = []
    hold = [c for c in pnl.columns if c.startswith("hold_usdc")]
    for train, test in ((14, 7), (28, 7), (56, 14), (91, 28)):
        for smooth, excl, top in ((0, (), 1), (5, (), 1), (0, hold, 1), (0, hold, 3), (5, hold, 3)):
            wf = walk_forward(pnl, train, test, switch_cost, families=fam, smooth=smooth, exclude=excl, top_n=top)
            s = summarize(wf["oos"], capital)
            rows.append({"train_d": train, "test_d": test, "plateau": smooth, "lp_only": bool(excl), "top_n": top, **s,
                         "switches": int((wf["chosen"].config != wf["chosen"].config.shift()).sum() - 1)})
            if (train, test, smooth, bool(excl), top) == (56, 14, 0, True, 3):
                best_wf = wf
    res = pd.DataFrame(rows)
    lines += ["## Out-of-sample result per walk-forward setting", "", res.round(2).to_markdown(index=False), ""]
    # Per-family walk-forward: choose only within one family (is the family robust on its own?).
    fam_rows = []
    for f in sorted(set(fam.values())):
        cols = [c for c in pnl.columns if fam[c] == f]
        wf = walk_forward(pnl[cols], 56, 14, switch_cost)
        start = pnl.index[56]
        full = pnl.loc[start:, cols].sum().sort_values()
        fam_rows.append({"family": f, "configs": len(cols), **summarize(wf["oos"], capital),
                         "best_in_hindsight": float(full.iloc[-1]), "median_config": float(full.median())})
    lines += ["## Walk-forward within each family (56d train / 14d test)", "",
              pd.DataFrame(fam_rows).sort_values("pnl", ascending=False).round(2).to_markdown(index=False), ""]
    p = pbo(pnl.loc[:, pnl.abs().sum() > 0])
    lines += [f"Probability of backtest overfitting (CSCV, 10 blocks): **{p:.2f}** "
              "(share of splits where the in-sample best config lands in the bottom half out of sample).", ""]
    ch = best_wf["chosen"]
    picks = pd.Series([c for cfg in ch.config for c in cfg.split("|")])
    lines += ["## Configs chosen (56d train / 14d test, LP-only, top-3 ensemble), most frequent", "",
              picks.value_counts().head(15).to_frame("windows chosen").to_markdown(), ""]
    top = pnl.sum().sort_values(ascending=False).head(15)
    lines += ["## In-hindsight top configs (full period — biased, for reference only)", "",
              top.round(2).to_frame("pnl").to_markdown(), ""]
    out = os.path.join(REPORTS, f"{name}_walkforward.md")
    open(out, "w", encoding="utf-8").write("\n".join(lines))
    best_wf["oos"].to_frame("pnl").to_csv(os.path.join(REPORTS, f"{name}_wf_oos_daily.csv"))
    ch.to_csv(os.path.join(REPORTS, f"{name}_wf_chosen.csv"), index=False)
    print("\n".join(lines))


if __name__ == "__main__":
    main()
