"""Worked example: the wallet's Oct 12-16 2026 campaign vs what each strategy would have done.

Same start time (first deposit, 2026-10-12 14:57 UTC), same capital (15,401.49 USDC), same costs.
The wallet's real result (+$9.34 before SOLID, measured on-chain) is the bar to beat.
Usage: python scripts/example_last_days.py [config label ...]
"""
import os
import sys

import pandas as pd

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.data import cached_market  # noqa: E402
from alm.grid import grid  # noqa: E402
from alm.sim import Costs, run  # noqa: E402

START = "2026-10-12 14:57:45"
CAPITAL = 15_401.49
REAL = {"pnl": 9.34, "fees": 11.45, "note": "wallet's actual result, before ~1,784 SOLID (~$4)"}


def main():
    m = cached_market().slice_from(int(pd.Timestamp(START).timestamp()))
    wanted = set(sys.argv[1:])
    rows = [{"config": "YOUR ACTUAL POSITIONS", **REAL}]
    for s in grid():
        if wanted and s.label() not in wanted:
            continue
        r = run(m, s, capital_usdc=CAPITAL, costs=Costs())
        rows.append({"config": r.label, "pnl": r.pnl, "fees": r.fees, "emissions": r.emissions, "costs": r.costs,
                     "actions": r.actions, "time_in_range": r.time_in_range})
    df = pd.DataFrame(rows).sort_values("pnl", ascending=False)
    out = os.path.join(os.path.dirname(__file__), "..", "reports", "example_last_days.csv")
    df.to_csv(out, index=False)
    print(df.head(40).round(3).to_string(index=False))


if __name__ == "__main__":
    main()
