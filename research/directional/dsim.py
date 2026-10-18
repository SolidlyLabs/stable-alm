"""LP as trading: an hourly simulator for directional liquidity strategies on a long price history.

The question: is it worth stepping out of the range when a big move comes, waiting it out in the
token that wins, and providing liquidity again after the move? Or, gentler, placing the range on the
side the price is heading so the position sells into the move? The score is the value at the end,
not fees or time in range.

Model (one pair, token0 the volatile asset, token1 the stable numeraire, price P = token1 per token0):
- A position is one V3 range [pa, pb) with liquidity L. Its token amounts follow the exact V3 formulas,
  so impermanent loss is exact on the hourly path.
- Fees: a deep pool's fees per unit of in-range liquidity are about what arbitrage takes from it (loss
  versus rebalancing, LVR = sigma^2 * L * sqrt(P) / 4 per unit time). Each hour pays
  c * r^2 * L * sqrt(P) / 4, r the hour's log return, for the share of the hour the price spent in the
  range (both ends in: all of it; one end: half). c is the pool's fee/LVR ratio, measured in
  reports/VOLATILE.md: about 1.0 for deep Uniswap WETH/USDC, 0.55 for Solidly WETH/USDC. This is the
  long-history stand-in for real swaps; the winners are re-checked on real swaps with vsim.
- Actions pay swap_bps on the value swapped plus gas per transaction.
- Decisions are made at the close of each hour on data up to that close (no look-ahead).

Actions a strategy returns from decide():
  None                         keep everything as is
  ("band", lo_mult, hi_mult)   one range [P*lo_mult, P*hi_mult): all capital, swapping to its ratio
  ("hold", w0)                 no position: hold share w0 of the value in token0, the rest in token1
  ("mix", f, lo_mult, hi_mult, w0)
                               share f of the value in the range [P*lo_mult, P*hi_mult), the rest held
                               idle with share w0 of it in token0 (part of the capital waits for the
                               upside, the rest farms)
"""
import math
from dataclasses import dataclass

import numpy as np


@dataclass
class Costs:
    swap_bps: float = 5.0
    gas: float = 0.02
    c: float = 1.0  # fee / LVR ratio of the pool
    # parking: idle capital sits in a pegged pool until the main pool needs it again (token0 in e.g.
    # cbETH/WETH or wS/stS, token1 in USDC/USDT), earning that pool's APR (as a fraction a year).
    # Moving in or out of the parking pool costs park_bps of the value moved plus one gas.
    park0: float = 0.0
    park1: float = 0.0
    park_bps: float = 1.0


def amounts(L, p, pa, pb):
    sp, sa, sb = math.sqrt(p), math.sqrt(pa), math.sqrt(pb)
    if sp <= sa:
        return L * (1 / sa - 1 / sb), 0.0
    if sp >= sb:
        return 0.0, L * (sb - sa)
    return L * (1 / sp - 1 / sb), L * (sp - sa)


def liquidity_for(V, p, pa, pb):
    """Liquidity a value V (token1) buys on [pa, pb) at price p, and the token amounts per unit."""
    a, b = amounts(1.0, p, pa, pb)
    v1 = a * p + b
    return (V / v1 if v1 > 0 else 0.0), a, b


@dataclass
class Result:
    label: str
    ts: np.ndarray       # daily marks
    value: np.ndarray    # strategy value at the marks
    hold: np.ndarray     # 50/50 hold value at the marks
    fees: float
    costs: float
    actions: int
    in_pool: float       # share of hours with a position
    in_range: float      # share of hours the position was in range
    token0: float        # average share of value in token0 (directional exposure)
    deployed: float      # average share of value in the pool (what the protocol sees as liquidity)
    active: float        # average share of value in the pool and in range (liquidity that earns and deepens the book)
    park: float = 0.0    # yield earned by idle capital parked in pegged pools


def run(ts, P, strat, capital=10_000.0, costs=None, h0=0, h1=None) -> Result:
    """Run `strat` over hours [h0, h1) of the price path P (token1 per token0)."""
    costs = costs or Costs()
    h1 = len(P) if h1 is None else h1
    p = P[h0]
    x, y = capital / 2 / p, capital / 2  # idle tokens
    hx, hy = x, y
    rng = None  # (pa, pb, L)
    fees = cost = park = 0.0
    parking = costs.park0 > 0 or costs.park1 > 0
    actions = 0
    n_pool = n_in = 0
    exp0 = dep = act_in = 0.0
    marks, vals, holds = [], [], []
    next_mark = (int(ts[h0]) // 86400 + 1) * 86400
    strat.reset()
    for h in range(h0, h1):
        p = P[h]
        if h > h0 and rng is not None:
            pa, pb, L = rng
            q = P[h - 1]
            r = math.log(p / q)
            share = 0.5 * ((pa <= q < pb) + (pa <= p < pb))
            if share:
                fees += costs.c * r * r * L * math.sqrt(math.sqrt(p * q)) / 4 * share
                n_in += 1
            n_pool += 1
        if h > h0 and parking:
            g0, g1 = x * costs.park0 / 8760, y * costs.park1 / 8760
            x, y = x + g0, y + g1
            park += g0 * p + g1
        px, py = amounts(rng[2], p, rng[0], rng[1]) if rng else (0.0, 0.0)
        X, Y = x + px, y + py
        V = X * p + Y
        idle0 = x * p + y
        exp0 += X * p / V if V > 0 else 0.0
        if rng is not None and V > 0:
            dep += (px * p + py) / V
            act_in += (px * p + py) / V if rng[0] <= p < rng[1] else 0.0
        act = strat.decide(h, p, rng)
        if act is not None and act[0] == "band":
            act = ("mix", 1.0, act[1], act[2], 0.5)
        if act is not None:
            n_tx = 1 if rng else 0
            if act[0] == "mix":
                f, pa, pb, w0 = act[1], p * act[2], p * act[3], act[4]
                L, a, b = liquidity_for(V * f, p, pa, pb)
                need0 = a * L + w0 * V * (1 - f) / p
                swap = abs(X - need0) * p
                c = swap * costs.swap_bps / 1e4 + (costs.gas if swap > 1e-9 else 0.0) + (n_tx + 1) * costs.gas
                Vn = V - c
                L, a, b = liquidity_for(Vn * f, p, pa, pb)
                rng = (pa, pb, L) if f > 0 else None
                x, y = w0 * Vn * (1 - f) / p, (1 - w0) * Vn * (1 - f)
            else:
                w0 = act[1]
                swap = abs(X * p - w0 * V)
                c = swap * costs.swap_bps / 1e4 + (costs.gas if swap > 1e-9 else 0.0) + n_tx * costs.gas
                Vn = V - c
                x, y = w0 * Vn / p, (1 - w0) * Vn
                rng = None
            if parking:
                moved = abs(x * p + y - idle0)
                if moved > 1e-9:
                    pc = moved * costs.park_bps / 1e4 + costs.gas
                    x, y = (x * (1 - pc / (x * p + y)), y * (1 - pc / (x * p + y))) if x * p + y > pc else (x, y)
                    c += pc
            cost += c
            actions += 1
        if ts[h] >= next_mark or h == h1 - 1:
            px, py = amounts(rng[2], p, rng[0], rng[1]) if rng else (0.0, 0.0)
            marks.append(int(ts[h]))
            vals.append((x + px) * p + y + py + fees)
            holds.append(hx * p + hy)
            next_mark += 86400
    n = h1 - h0
    return Result(strat.label(), np.array(marks), np.array(vals), np.array(holds), fees, cost, actions,
                  n_pool / n, n_in / max(n_pool, 1), exp0 / n, dep / n, act_in / n, park)


def summary(r: Result, capital: float) -> dict:
    d = (r.ts[-1] - r.ts[0]) / 86400 if len(r.ts) > 1 else 1
    k = 365 / max(d, 1e-9) / capital * 100
    return {"label": r.label, "days": round(d, 1), "vs_hold_apr": (r.value[-1] - r.hold[-1]) * k, "pnl_apr": (r.value[-1] - capital) * k,
            "fees_apr": r.fees * k, "park_apr": r.park * k, "cost_apr": r.costs * k, "actions": r.actions, "in_pool": r.in_pool, "in_range": r.in_range, "token0": r.token0,
            "deployed": r.deployed, "active": r.active}
