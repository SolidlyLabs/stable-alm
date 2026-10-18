"""Event-driven LP simulator.

A strategy is asked at every decision bar what ranges it wants. When it changes them, the
simulator burns everything, optionally swaps toward the ratio the new ranges need, and mints.
Between actions the position is fixed, and fee/emission income is computed swap by swap:

    our_fee_i = fee_i * ours_i / (others_i + ours_i)

where ours_i is our liquidity active at the swap's tick (averaged over the pre- and post-swap
tick, since a swap that crosses our edge only half-touches it). Volume is held fixed, which is
conservative: deeper liquidity would attract more aggregator flow in reality.

Accounting is in USDC (token0) at the pool price. Rewards are paid by the rewarder, not into the
position, so they accumulate as cash and are not compounded. Haircuts turn processor-reported
income into what is really received: fees are paid at ~82% of the processor's attribution
(measured against this wallet's lifetime payouts), and SOLID emissions are valued at a fraction of
the processor's USD figure because the token has almost no liquidity to sell into.
"""
from dataclasses import dataclass, field

import numpy as np

from .pool_math import amounts_for_liquidity, liquidity_for_amounts, ratio_for_range, sqrt_at


@dataclass
class Costs:
    swap_bps: float = 0.3  # aggregator fee + impact on swapped notional (observed ~0.01-0.1 bp)
    gas_usd: float = 0.02  # per tx on Base (observed ~$0.005)
    fee_haircut: float = 0.82
    emission_haircut: float = 0.5


@dataclass
class Target:
    """Desired position. ranges: list of (tick_lower, tick_upper, value_weight).

    swap=True: swap inventory to the ratio the ranges need, then mint each with its value weight.
    swap=False: mint ranges[0] with as much liquidity as the inventory allows, and put the leftover
    single-sided into a 'limit' range of width limit_width next to the current tick,
    so no swap is ever made.
    """

    ranges: list
    swap: bool = True
    limit_width: int = 0


@dataclass
class State:
    i: int  # index of the current swap (the market's latest state)
    bar: int
    tick: int
    sp: float
    ranges: list  # list of (tl, tu, liquidity)
    x: float  # idle token0
    y: float  # idle token1
    last_action_bar: int
    bars_out_of_range: int
    market: object = None

    def in_range(self) -> bool:
        return any(tl <= self.tick < tu and liq > 0 for tl, tu, liq in self.ranges)


@dataclass
class Result:
    name: str
    params: dict
    day_ts: np.ndarray
    equity: np.ndarray  # USDC value of position + idle + cash rewards - costs, at day ends
    income: np.ndarray  # cumulative received rewards - costs at the same marks (no mark-to-market)
    fees: float
    emissions: float
    costs: float
    actions: int
    time_in_range: float
    extra: dict = field(default_factory=dict)
    label: str = ""

    @property
    def pnl(self) -> float:
        return float(self.equity[-1] - self.equity[0])


def _liq_in(ranges: list, ticks: np.ndarray) -> np.ndarray:
    out = np.zeros(len(ticks))
    for tl, tu, liq in ranges:
        if liq > 0:
            out += liq * ((ticks >= tl) & (ticks < tu))
    return out


def _income(m, i0: int, i1: int, ranges: list, t0: float = None, t1: float = None) -> tuple:
    """Earnings of fixed ranges held from time t0 to t1, where i0 / i1 are the last swaps at or
    before t0 / t1. Fees: the swaps (i0, i1]. Emissions and in-range time: the state after each
    swap k in [i0, i1] counts from max(ts[k], t0) to min(ts[k+1], t1), so nothing accrues to these
    ranges after t1 (when the strategy may have changed them). Without t0 / t1 the interval runs
    over the swaps (i0, i1] as before."""
    if not ranges:
        return 0.0, 0.0, 0.0, 0.0
    fees = 0.0
    if i1 > i0:
        sl = slice(i0 + 1, i1 + 1)
        ours_post, ours_pre = _liq_in(ranges, m.tick[sl]), _liq_in(ranges, m.tick_pre[sl])
        ours = 0.5 * (ours_post + ours_pre)
        share = np.where(ours > 0, ours / (m.l_others[sl] + ours + 1e-12), 0.0)
        fees = float(np.dot(m.fee[sl], share))
    if t0 is None or t1 is None:
        if i1 <= i0:
            return fees, 0.0, 0.0, 0.0
        ks = np.arange(i0 + 1, i1 + 1)
        dt = m.dt[ks]
    else:
        if t1 <= t0:
            return fees, 0.0, 0.0, 0.0
        ks = np.arange(i0, i1 + 1)
        nxt = np.append(m.ts[ks[:-1] + 1], t1).astype(float)
        dt = np.clip(np.minimum(nxt, t1) - np.maximum(m.ts[ks].astype(float), t0), 0.0, None)
    held = _liq_in(ranges, m.tick[ks])
    share_t = np.where(held > 0, held / (m.l_others[ks] + held + 1e-12), 0.0)
    emis = float(np.dot(m.emis_rate[ks] * dt, share_t))
    t_in = float(np.dot(dt, held > 0))
    return fees, emis, t_in, float(dt.sum())


def _position_amounts(ranges: list, sp: float):
    x = y = 0.0
    for tl, tu, liq in ranges:
        a, b = amounts_for_liquidity(liq, sp, tl, tu)
        x += a
        y += b
    return x, y


def run(market, strategy, capital_usdc: float = 15_000.0, costs: Costs = None, start_ratio_usdc: float = 1.0) -> Result:
    """Simulate `strategy` from the market's first bar with `capital_usdc` of USDC."""
    costs = costs or Costs()
    m = market
    st = State(i=int(m.bar_idx[0]), bar=0, tick=int(m.tick[m.bar_idx[0]]), sp=float(m.sp[m.bar_idx[0]]),
               ranges=[], x=capital_usdc * start_ratio_usdc, y=0.0, last_action_bar=-10**9, bars_out_of_range=0, market=m)
    st.y = capital_usdc * (1 - start_ratio_usdc) * st.sp ** 2
    num1 = getattr(m, "numeraire", 0) == 1
    if num1:  # capital is in token1 units and starts as token1
        st.x, st.y = 0.0, capital_usdc
    fees = emis = cost = 0.0
    t_in = t_tot = 0.0
    actions = 0
    day_ts, equity = [], []
    last_i = st.i
    last_t = float(m.bar_end[0])
    next_day = (int(m.bar_end[0]) // 86400 + 1) * 86400
    cash = 0.0  # rewards received minus costs paid

    def mark() -> float:
        px, py = _position_amounts(st.ranges, st.sp)
        v0 = (px + st.x) + (py + st.y) / st.sp**2
        return (v0 * st.sp**2 if num1 else v0) + cash

    income = [0.0]
    day_ts.append(int(m.bar_end[0]))
    equity.append(capital_usdc)
    strategy.prepare(m)
    strategy.reset()
    for b in range(len(m.bar_end)):
        i = int(m.bar_idx[b])
        t = float(m.bar_end[b])
        if t > last_t:
            # what the ranges held since the last decision earned, up to this decision
            f, e, a, tt = _income(m, last_i, i, st.ranges, last_t, t)
            fees += f
            emis += e
            cash += f * costs.fee_haircut + e * costs.emission_haircut
            t_in += a
            t_tot += tt
        last_i, last_t = i, t
        st.i, st.bar, st.tick, st.sp = i, b, int(m.tick[i]), float(m.sp[i])
        st.bars_out_of_range = 0 if st.in_range() else st.bars_out_of_range + 1
        target = strategy.decide(st)
        if target is not None:
            c, paid = _execute(st, target, costs)
            if num1:  # _execute prices costs in token0
                c *= st.sp**2
                paid *= st.sp**2
            cost += c
            # Swap cost already left the inventory inside _execute; only gas is paid from cash.
            cash -= paid
            actions += 1
            st.last_action_bar = b
            # Restart the out-of-range clock after any action: a one-sided swap-free position can
            # legitimately sit out of range waiting for price, and must not re-trigger every bar.
            st.bars_out_of_range = 0
        if m.bar_end[b] >= next_day:
            day_ts.append(int(next_day))
            equity.append(mark())
            income.append(cash)
            next_day += 86400
    day_ts.append(int(m.bar_end[-1]))
    equity.append(mark())
    income.append(cash)
    return Result(
        name=strategy.name, params=strategy.params, day_ts=np.array(day_ts), equity=np.array(equity),
        income=np.array(income),
        fees=fees * costs.fee_haircut, emissions=emis * costs.emission_haircut, costs=cost, actions=actions,
        time_in_range=t_in / t_tot if t_tot else 0.0, label=strategy.label(),
    )


def _execute(st: State, target: Target, costs: Costs) -> tuple:
    """Burn all, (optionally) swap, mint target. Returns (total cost, part paid from cash), in
    the numeraire: the swap cost is taken from the swapped inventory, gas from cash."""
    sp, p = st.sp, st.sp**2
    px, py = _position_amounts(st.ranges, sp)
    x, y = st.x + px, st.y + py
    n_tx = (1 if st.ranges else 0) + len(target.ranges)
    cost = 0.0
    new = []
    if target.swap:
        v = x + y / p
        w = np.array([r[2] for r in target.ranges], dtype=float)
        w = w / w.sum()
        f0 = sum(wi * ratio_for_range(sp, tl, tu) for (tl, tu, _), wi in zip(target.ranges, w))
        want_x = f0 * v
        swapped = abs(want_x - x)  # notional in USDC
        if swapped > 1e-6:
            c = swapped * costs.swap_bps / 1e4
            cost += c
            v -= c
            want_x = f0 * v
            x, y = want_x, (v - want_x) * p
            n_tx += 1
        for (tl, tu, _), wi in zip(target.ranges, w):
            rx = ratio_for_range(sp, tl, tu) * wi * v
            ry = (wi * v - rx) * p
            liq = liquidity_for_amounts(rx, ry, sp, tl, tu)
            ax, ay = amounts_for_liquidity(liq, sp, tl, tu)
            x, y = x - ax, y - ay
            new.append((tl, tu, liq))
    else:
        tl, tu, _ = target.ranges[0]
        liq = liquidity_for_amounts(x, y, sp, tl, tu)
        ax, ay = amounts_for_liquidity(liq, sp, tl, tu)
        x, y = x - ax, y - ay
        new.append((tl, tu, liq))
        lw = target.limit_width
        if lw > 0:
            t = st.tick
            if x > 1e-6:  # token0 left: range entirely above price sells it as price rises
                lo = t + 1
                liq = liquidity_for_amounts(x, 0.0, sp, lo, lo + lw)
                ax, _ = amounts_for_liquidity(liq, sp, lo, lo + lw)
                x -= ax
                new.append((lo, lo + lw, liq))
                n_tx += 1
            if y > 1e-6:
                hi = t
                liq = liquidity_for_amounts(0.0, y, sp, hi - lw, hi)
                _, ay = amounts_for_liquidity(liq, sp, hi - lw, hi)
                y -= ay
                new.append((hi - lw, hi, liq))
                n_tx += 1
    st.ranges = new
    st.x, st.y = max(x, 0.0), max(y, 0.0)
    gas = n_tx * costs.gas_usd
    return cost + gas, gas


def fixed_ranges_value(ranges: list, sp: float) -> float:
    x, y = _position_amounts(ranges, sp)
    return x + y / sp**2


__all__ = ["Costs", "Target", "State", "Result", "run", "sqrt_at", "fixed_ranges_value"]
