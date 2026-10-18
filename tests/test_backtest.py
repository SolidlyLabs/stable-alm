"""Offline tests for the backtester: no lookahead, capital reconciles, income stops with the position."""
import os
import sys
import unittest

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from alm.data import Market  # noqa: E402
from alm.sim import Costs, Target, run  # noqa: E402
from alm.strategies import Recenter, Strategy, VolBand  # noqa: E402


def market(ts, ticks, bar_seconds=300, fee=0.0, emis=0.0, others=1e6):
    """A Market from swap times and ticks (equal decimals, token0 numeraire)."""
    m = object.__new__(Market)
    m.ts = np.asarray(ts, dtype=np.int64)
    m.tick = np.asarray(ticks, dtype=np.int64)
    m.sp = np.power(1.0001, (m.tick + 0.5) / 2)
    m.tick_pre = np.concatenate([[m.tick[0]], m.tick[:-1]])
    n = len(m.ts)
    m.amount_in = np.ones(n)
    m.l_wallet = np.zeros(n)
    m.l_others = np.full(n, float(others))
    m.fee = np.full(n, float(fee))
    m.emis_rate = np.full(n, float(emis))
    m.dt = np.diff(np.concatenate([m.ts, [m.ts[-1]]])).astype(float)
    t0 = (m.ts[0] // bar_seconds + 1) * bar_seconds
    m.bar_end = np.arange(t0, m.ts[-1], bar_seconds)
    m.bar_idx = np.searchsorted(m.ts, m.bar_end, side="right") - 1
    m.bar_tick = m.tick[m.bar_idx]
    m.bar_high = m.bar_low = m.bar_tick.astype(float)
    m.bar_seconds, m.apr = bar_seconds, None
    m.decimals = m.decimals1 = 6
    m.numeraire, m.px_scale, m.liq_scale = 0, 1.0, 1e6
    return m


def walk(n, seed):
    rng = np.random.default_rng(seed)
    ts = np.cumsum(rng.integers(20, 400, n))
    ticks = np.cumsum(rng.integers(-2, 3, n))
    return ts, ticks


class Recorder(Strategy):
    """Wraps a strategy and records (bar, target ranges) of every decision."""

    def __init__(self, inner):
        super().__init__()
        self.inner, self.name, self.log = inner, inner.name, []

    def prepare(self, m):
        self.inner.prepare(m)

    def reset(self):
        self.inner.reset()
        self.log = []

    def decide(self, st):
        t = self.inner.decide(st)
        if t is not None:
            self.log.append((st.bar, [tuple(r[:2]) for r in t.ranges]))
        return t


class Scripted(Strategy):
    """Fixed targets at given bars."""

    name = "scripted"

    def __init__(self, plan):
        super().__init__()
        self.plan = plan

    def reset(self):
        pass

    def decide(self, st):
        return self.plan.get(st.bar)


class NoLookahead(unittest.TestCase):
    def _decisions(self, m, strat):
        r = Recorder(strat)
        run(m, r, capital_usdc=10_000.0, costs=Costs(gas_usd=0.0, swap_bps=0.0))
        return r.log

    def test_future_prices_do_not_change_past_decisions(self):
        ts, ticks = walk(3000, 1)
        cut = len(ts) // 2
        future = ticks.copy()
        future[cut:] += np.cumsum(np.random.default_rng(2).integers(-6, 7, len(ts) - cut))
        t_cut = ts[cut]
        for make in (lambda: Recenter(k=5, dwell=2, cooldown=3, swap=True, guard=8),
                     lambda: VolBand(vol="std", lookback=48, z=2, kmin=1, kmax=20, anchor="median", anchor_lb=48,
                                     every=12, move=2, dwell=6, swap=True, guard=8)):
            a = self._decisions(market(ts, ticks), make())
            b = self._decisions(market(ts, future), make())
            m = market(ts, ticks)
            before = lambda log: [d for d in log if m.bar_end[d[0]] < t_cut]
            self.assertTrue(before(a), "strategy made no decisions before the cut")
            self.assertEqual(before(a), before(b))

    def test_indicators_use_only_past_bars(self):
        # Every precomputed series at bar b must be the same whatever happens after b, warm-up included.
        ts, ticks = walk(3000, 4)
        for b in (3, 40, 400):
            m1 = market(ts, ticks)
            later = ticks.copy()
            later[m1.bar_idx[b] + 1:] += 50
            m2 = market(ts, later)
            for make, attr in ((lambda: Recenter(k=5, dwell=1, cooldown=0, swap=True, guard=8), "guard_dev"),
                               (lambda: VolBand(vol="std", lookback=48, z=2, kmin=1, kmax=20, anchor="median",
                                                anchor_lb=48, every=12, move=2, dwell=6, swap=True), "sig")):
                s1, s2 = make(), make()
                s1.prepare(m1)
                s2.prepare(m2)
                np.testing.assert_array_equal(getattr(s1, attr)[:b + 1], getattr(s2, attr)[:b + 1], err_msg=f"{attr} at bar {b}")

    def test_slice_keeps_the_predecessor_swap(self):
        ts, ticks = walk(500, 3)
        m = market(ts, ticks)
        for start in (int(ts[10]) + 1, int(m.bar_end[7]) - 1, int(m.bar_end[7])):
            s = m.slice_from(start)
            self.assertGreaterEqual(int(s.bar_idx.min()), 0)
            self.assertTrue(np.all(s.ts[s.bar_idx] <= s.bar_end))
            self.assertTrue(np.all(s.tick[s.bar_idx] == m.tick[m.bar_idx[len(m.bar_end) - len(s.bar_end):]]))


class Accounting(unittest.TestCase):
    def test_capital_reconciles_after_every_action(self):
        # Flat price, no income: what is left is exactly capital minus the costs, counted once.
        ts = np.arange(0, 30_000, 60)
        m = market(ts, np.zeros(len(ts), dtype=int))
        plan = {0: Target([(-10, 11, 1.0)]), 5: Target([(-3, 4, 1.0)]), 20: Target([(-30, 31, 0.5), (-2, 3, 0.5)])}
        res = run(m, Scripted(plan), capital_usdc=10_000.0, costs=Costs(swap_bps=25.0, gas_usd=0.5))
        self.assertEqual(res.actions, 3)
        self.assertGreater(res.costs, 1.5)  # gas alone is 3 burns/mints... plus a real swap cost
        self.assertAlmostEqual(res.equity[-1], 10_000.0 - res.costs, places=6)

    def test_emissions_stop_when_the_position_leaves(self):
        # Swaps at 0 and 1000, then nothing until 10000. In range from the first decision (t=300)
        # until the move out of range at bar 3 (t=1200): 900 s of emissions, not the 9000 s gap.
        ts = [0, 1000, 10_000, 10_100]
        m = market(ts, [0, 0, 0, 0], emis=1.0, others=0.0)
        plan = {0: Target([(-5, 6, 1.0)]), 3: Target([(100, 120, 1.0)])}
        res = run(m, Scripted(plan), capital_usdc=1_000.0, costs=Costs(gas_usd=0.0, swap_bps=0.0, emission_haircut=1.0))
        self.assertEqual(res.actions, 2)
        self.assertAlmostEqual(res.emissions, 900.0, places=3)

    def test_emissions_accrue_through_quiet_bars(self):
        # Held the whole time: emissions cover first decision to last bar end, swaps or not.
        ts = [0, 1000, 10_000, 10_100]
        m = market(ts, [0, 0, 0, 0], emis=1.0, others=0.0)
        res = run(m, Scripted({0: Target([(-5, 6, 1.0)])}), capital_usdc=1_000.0,
                  costs=Costs(gas_usd=0.0, swap_bps=0.0, emission_haircut=1.0))
        self.assertAlmostEqual(res.emissions, float(m.bar_end[-1] - m.bar_end[0]), places=3)


if __name__ == "__main__":
    unittest.main()
