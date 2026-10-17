"""Read-only live planner: what the chosen strategy would do to the wallet's position right now.

1. Pulls new pool events since the last download and rebuilds the replay cache.
2. Reconstructs the wallet's open positions from its Mint/Burn history, plus idle USDC/USDT.
3. Warms the strategy up on recent history, then asks it for a target at the latest bar,
   starting from the wallet's real position.
4. Prints the plan: burns, swap, mints, and expected cost. Nothing is signed or sent.

Usage: python scripts/plan.py [ladder|static|recenter] [wallet]
"""
import os
import sys

import numpy as np
import pandas as pd

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.data import POOL, WALLET, cached_market, load_events  # noqa: E402
from alm.pool_math import amounts_for_liquidity  # noqa: E402
from alm.rpc import call  # noqa: E402
from alm.sim import State  # noqa: E402
from alm.strategies import Ladder, Recenter, StaticBand  # noqa: E402

USDC = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
USDT = "0xfde4c96c8593536e31f229ea8f37b2ada2699bb2"
STRATEGIES = {
    # Robust picks from the walk-forward (see reports/REPORT.md).
    "ladder": lambda: Ladder(lookback=3 * 86400, span=3, every=288, turnover=0.4, guard=15),
    "static": lambda: StaticBand(k=8),
    "recenter": lambda: Recenter(k=3, dwell=288, cooldown=0, swap=False, limit=4, guard=15),
}


def balance(token: str, wallet: str) -> float:
    r = call("eth_call", [{"to": token, "data": "0x70a08231" + "0" * 24 + wallet[2:]}, "latest"])
    return int(r, 16) / 1e6


def wallet_positions(wallet: str) -> list:
    ev = load_events()
    lp = ev[(ev.kind != "swap") & (ev.owner == wallet)]
    pos = {}
    for r in lp.itertuples():
        key = (int(r.tick_lower), int(r.tick_upper))
        pos[key] = pos.get(key, 0.0) + (1 if r.kind == "mint" else -1) * float(r.amount) / 1e6
    return [(a, b, v) for (a, b), v in pos.items() if v > 1e-6]


def main():
    name = sys.argv[1] if len(sys.argv) > 1 else "ladder"
    wallet = (sys.argv[2] if len(sys.argv) > 2 else WALLET).lower()
    here = os.path.dirname(__file__)
    os.system(f'python "{os.path.join(here, "download_pool_events.py")}" {POOL} 52000000')
    m = cached_market(rebuild=True).slice_from(int(pd.Timestamp.utcnow().timestamp()) - 10 * 86400)
    strat = STRATEGIES[name]()
    strat.prepare(m)
    strat.reset()
    b = len(m.bar_end) - 1
    i = int(m.bar_idx[b])
    sp = float(m.sp[i])
    ranges = wallet_positions(wallet)
    x, y = balance(USDC, wallet), balance(USDT, wallet)
    st = State(i=i, bar=b, tick=int(m.tick[i]), sp=sp, ranges=ranges, x=x, y=y, last_action_bar=-10**9,
               bars_out_of_range=0 if any(a <= m.tick[i] < c for a, c, _ in ranges) else 10**6, market=m)
    if name == "ladder":
        strat.cur = None  # force a fresh solve against today's market
        strat.every = 1
    elif ranges:
        # Judge the wallet's existing position by the strategy's own rules instead of a fresh start.
        a, c, _ = max(ranges, key=lambda r: r[2])
        strat.cur = ((a + c - 1) // 2, (c - a - 1) // 2)
    if strat.frozen(st):
        print(f"DEPEG GUARD: tick {st.tick} is more than {strat.params['guard']} ticks from its 7-day median; "
              "holding, no swaps.")
        return
    target = strat.decide(st)
    px = py = 0.0
    print(f"pool tick {st.tick}  price {sp * sp:.6f} USDT/USDC  wallet idle {x:,.2f} USDC  {y:,.2f} USDT")
    for a, c, liq in ranges:
        ax, ay = amounts_for_liquidity(liq, sp, a, c)
        px, py = px + ax, py + ay
        state = "IN RANGE" if a <= st.tick < c else "out of range"
        print(f"  open position [{a}, {c})  {ax:,.2f} USDC + {ay:,.2f} USDT  {state}")
    total = x + px + (y + py) / (sp * sp)
    print(f"total {total:,.2f} USDC-equivalent")
    if target is None:
        print(f"\n{strat.label()}: keep the current position (no action).")
        return
    print(f"\n{strat.label()} plan (swap={'yes' if target.swap else 'no'}):")
    print("  1. burn all open positions and collect")
    w = np.array([r[2] for r in target.ranges])
    w = w / w.sum()
    for (a, c, _), wi in zip(target.ranges, w):
        print(f"  mint [{a}, {c})  {wi * 100:5.1f}% of capital  (~{wi * total:,.0f} USDC-equivalent)")
    print("Dry run only: no transaction was built or sent.")


if __name__ == "__main__":
    main()
