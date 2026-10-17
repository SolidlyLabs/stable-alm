"""Is there a predictable direction to exploit? Statistics on each pair's tick path.

For 5-minute bars of the pool tick:
- autocorrelation of tick changes at 5m / 1h / 1d horizons (negative = mean reversion);
- variance ratio VR(q) = Var(q-bar change) / (q * Var(1-bar change)); < 1 mean-reverting, > 1 trending;
- predictive power (information coefficient = rank correlation of signal vs the next move) of
  momentum (past move) and of distance from the rolling median, for several horizons;
- mean-reversion half-life from an AR(1) fit on the tick minus its 7-day median;
- drift: average ticks per day (cbETH accrues staking yield against WETH).
Output: reports/trend_analysis.md
"""
import os
import sys

import numpy as np
import pandas as pd

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.data import POOLS, cached_market  # noqa: E402

H = {"5m": 1, "1h": 12, "6h": 72, "1d": 288}


def analyse(name: str) -> dict:
    m = cached_market(POOLS[name]["pool"])
    t = pd.Series(m.bar_tick.astype(float), index=pd.to_datetime(m.bar_end, unit="s"))
    # Only periods with activity: drop flat stretches longer than a day (dead pool).
    active = t.diff().abs().rolling(288, min_periods=1).sum() > 0
    t = t[active]
    d1 = t.diff().dropna()
    out = {"pair": name, "days": round((t.index[-1] - t.index[0]).days), "drift_ticks_per_day": float((t.iloc[-1] - t.iloc[0]) /
           max((t.index[-1] - t.index[0]).days, 1))}
    for k, q in H.items():
        dq = t.diff(q).dropna()
        out[f"acf_{k}"] = float(dq.autocorr(lag=q)) if len(dq) > 2 * q else np.nan
        if q > 1:
            out[f"VR_{k}"] = float(t.diff(q).var() / (q * d1.var()))
    rows = []
    for lb in ("1h", "6h", "1d"):
        for fwd in ("1h", "6h", "1d"):
            past = t.diff(H[lb])
            dev = t - t.rolling(H[lb], min_periods=1).median()
            fut = t.shift(-H[fwd]) - t
            df = pd.DataFrame({"mom": past, "dev": dev, "fut": fut}).dropna()
            df = df[df.fut != 0]
            rows.append({"lookback": lb, "horizon": fwd, "IC_momentum": df.mom.corr(df.fut, method="spearman"),
                         "IC_distance_from_median": df.dev.corr(df.fut, method="spearman"), "n": len(df)})
    x = (t - t.rolling(2016, min_periods=288).median()).dropna()
    phi = np.corrcoef(x.iloc[:-1].to_numpy(), x.iloc[1:].to_numpy())[0, 1]
    out["halflife_hours"] = float(np.log(0.5) / np.log(phi) * 5 / 60) if 0 < phi < 1 else np.nan
    return out, pd.DataFrame(rows)


def main():
    lines = ["# Trend / mean-reversion analysis (5-minute pool ticks)", ""]
    summary = []
    for name in POOLS:
        s, ic = analyse(name)
        summary.append(s)
        lines += [f"## {name}", "", "Predictive power (Spearman IC; |IC| < 0.02 is noise, > 0.05 is usable):", "",
                  ic.round(3).to_markdown(index=False), ""]
    lines = lines[:2] + ["## Summary", "", pd.DataFrame(summary).round(3).to_markdown(index=False), "",
                         "acf < 0 and VR < 1 mean the price mean-reverts at that horizon; > 0 / > 1 means it trends.",
                         ""] + lines[2:]
    out = os.path.join(os.path.dirname(__file__), "..", "reports", "trend_analysis.md")
    open(out, "w", encoding="utf-8").write("\n".join(lines))
    print("\n".join(lines))


if __name__ == "__main__":
    main()
