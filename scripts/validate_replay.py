"""Replay the wallet's real positions through the simulator and compare the fees it predicts with
what the Solidly processor attributes to each position (getPositionEarnings)."""
import os
import sys

import pandas as pd

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.data import WALLET, cached_market, load_events  # noqa: E402
from alm.sim import Costs, run  # noqa: E402
from alm.strategies import ReplayPositions  # noqa: E402

ev = load_events()
lp = ev[(ev.kind != "swap") & (ev.owner == WALLET)]
pos, sched = {}, []
for r in lp.itertuples():
    key = (int(r.tick_lower), int(r.tick_upper))
    pos[key] = pos.get(key, 0.0) + (1 if r.kind == "mint" else -1) * float(r.amount) / 1e6
    sched.append((int(r.ts), [(a, b, v) for (a, b), v in pos.items() if v > 1e-9]))
start = int(pd.Timestamp(sys.argv[1] if len(sys.argv) > 1 else "2026-10-12 14:50").timestamp())
sched = [s for s in sched if s[0] >= start - 1]
m = cached_market(fee_floor_bps=0.0).slice_from(start)
r = run(m, ReplayPositions(sched), capital_usdc=0.0, costs=Costs(fee_haircut=1.0, emission_haircut=1.0, gas_usd=0))
print("replay from", pd.to_datetime(start, unit="s"), "positions changes", len(sched))
print("simulated fees (gross, processor basis):", round(r.fees, 3), "emissions USD:", round(r.emissions, 3), "time in range:", round(r.time_in_range, 3))
