"""Cross-pair comparison of strategy families (walk-forward, out-of-sample) + the wait-before-rebalance test.

Usage: python scripts/compare_pairs.py
Reads reports/full_<pair>.parquet and reports/delay_<pair>.parquet; writes reports/compare_pairs.md.
"""
import os
import re
import sys

import pandas as pd

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.data import POOLS  # noqa: E402
from alm.walkforward import pbo, summarize, walk_forward  # noqa: E402

R = os.path.join(os.path.dirname(__file__), "..", "reports")
UNIT = {"usdc-usdt": "USDC", "usdc-usdbc": "USDC", "cbeth-weth": "WETH"}
# One-off burst that dominated every ranking on USDC/USDT; excluded (see REPORT.md).
EXCLUDE = {"usdc-usdt": ("2025-05-19", "2025-05-24")}


def family(c: str) -> str:
    f = c.split("(")[0]
    g = "+guard" if "guard=" in c else ""
    if f == "adaptive":
        skew = re.search(r"skew=(\w+)", c).group(1)
        timing = re.search(r"timing=(\w+)", c).group(1)
        return f"adaptive[{skew},{timing}]"
    if f == "range24h":
        w = re.search(r"dwell_s=(\d+)", c).group(1)
        return f"range24h[wait {w}s]{g}"
    if f in ("recenter", "volband"):
        return f + ("_noswap" if "swap=False" in c else "") + g
    return f + g


def load(name: str, kind: str) -> pd.DataFrame:
    p = pd.read_parquet(os.path.join(R, f"{kind}_{name}.parquet"))
    if name in EXCLUDE:
        a, b = EXCLUDE[name]
        p = p[(p.index < a) | (p.index > b)]
    return p


def fam_table(p: pd.DataFrame, name: str, fam_fn) -> pd.DataFrame:
    cap = POOLS[name]["capital"]
    sc = cap * 0.5 * 0.3e-4 + 3 * POOLS[name]["gas"]
    fam = {c: fam_fn(c) for c in p.columns}
    rows = []
    for f in sorted(set(fam.values())):
        cols = [c for c in p.columns if fam[c] == f]
        oos = walk_forward(p[cols], 56, 14, sc)["oos"]
        s = summarize(oos, cap)
        rows.append({"family": f, "configs": len(cols), f"wf_pnl_{UNIT[name]}": s["pnl"], "apr_pct": s["apr_pct"],
                     "max_dd": s["max_drawdown"], "median_cfg": float(p[cols].iloc[56:].sum().median()),
                     "best_cfg_hindsight": float(p[cols].iloc[56:].sum().max())})
    return pd.DataFrame(rows).sort_values("apr_pct", ascending=False)


def main():
    lines = ["# Strategy families across pairs (walk-forward 56d train / 14d test, out-of-sample)", "",
             "New-fee scenario: each hour pays at least the pool's post-Oct-13 fee level (USDC/USDT 0.0987 bp, "
             "cbETH/WETH 0.44 bp, USDC/USDbC 0.06 bp), volume unchanged.", ""]
    best = []
    for name in POOLS:
        path = os.path.join(R, f"full_{name}.parquet")
        if not os.path.exists(path):
            continue
        p = load(name, "full")
        t = fam_table(p, name, family)
        overall = walk_forward(p.drop(columns=[c for c in p.columns if c.startswith("hold_")]), 56, 14,
                               POOLS[name]["capital"] * 1.5e-5)["oos"]
        so = summarize(overall, POOLS[name]["capital"])
        lines += [f"## {name} ({len(p.columns)} configs, {len(p)} days, capital {POOLS[name]['capital']:g} {UNIT[name]})", "",
                  t.round(4).to_markdown(index=False), "",
                  f"Walk-forward across all LP configs: {so['pnl']:.4f} {UNIT[name]} ({so['apr_pct']:.2f}% APR), "
                  f"max drawdown {so['max_drawdown']:.4f}. PBO {pbo(p.loc[:, p.abs().sum() > 0]):.2f}.", ""]
        top = t.iloc[0]
        best.append({"pair": name, "best_family": top.family, "apr_pct": top.apr_pct, "all_configs_wf_apr": so["apr_pct"]})
        dpath = os.path.join(R, f"delay_{name}.parquet")
        if os.path.exists(dpath):
            d = load(name, "delay")
            def wait_of(c: str) -> str:
                if c.startswith("recenter"):
                    return "recenter wait %s min" % re.search(r"dwell=(\d+)", c).group(1)
                return "range24h wait %d min" % (int(re.search(r"dwell_s=(\d+)", c).group(1)) // 60)
            dt = fam_table(d, name, wait_of)
            lines += ["Wait before rebalancing (1-minute decision bars):", "", dt.round(4).to_markdown(index=False), ""]
    lines = lines[:4] + ["## Best family per pair", "", pd.DataFrame(best).round(2).to_markdown(index=False), ""] + lines[4:]
    open(os.path.join(R, "compare_pairs.md"), "w", encoding="utf-8").write("\n".join(lines))
    print("\n".join(lines))


if __name__ == "__main__":
    main()
