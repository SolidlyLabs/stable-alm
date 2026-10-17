"""Ladder variants for the 'novel ideas' round (reports/IDEAS.md). Each switch is one idea; with
every switch off, LadderX is the plain Ladder.

  decay_hl_s  Recency weighting. Inside the lookback, each swap's fees and the others' liquidity it
              saw are weighted 0.5 ** (age / decay_hl_s), so the solve leans on the last hours.
  exit_s      Re-solve as soon as all our liquidity has been out of range for exit_s seconds, rather
              than waiting for the next scheduled solve (the schedule still runs).
  swapfree    Inventory-aware ladder: never swap. Ticks above spot can only hold token0 and ticks
              below only token1, so each side is water-filled with the inventory that side can use.
              On a mean-reverting pair this keeps what the pool sold us on the side where the price
              should come back to buy it, instead of swapping back at the worse price.
  core_frac   Two-layer book: core_frac of the value sits in one wide band (+-core_k ticks around the
              7-day median) and is only moved when that median shifts by >= core_move ticks; the
              rest runs the ladder. The core earns when price leaves the ladder's ticks.
  center      'median' (1-day window median, the original) or 'blend': 1-day and 7-day medians
              averaged, so the ladder leans toward where the pair mean-reverts to.
  forecast    Model-based placement (0..1 = weight of the forecast vs the trailing fee histogram).
              Over the last fc_lookback_s, every bar's deviation from its 1-day median is paired with
              where the price went over the next `every` bars (relative to that median). Bars whose
              deviation matches today's (+-1 tick) give the forecast distribution of where the price
              will sit until the next solve; the tick fee weight is that probability times the recent
              fee rate. This uses the measured mean reversion directly instead of assuming tomorrow's
              fees land where yesterday's did.
  prior       Tail insurance. The window is widened by `tail` ticks each side and every tick gets a
              baseline weight of prior * (mean tick fee), on top of what it earned. Others' liquidity
              is thin, so a small amount just outside the usual range catches a large share of the
              fees paid while the price is outside it (18% of the possible fees in the baseline).
  emis        Emission-aware placement: a tick's value is fees * fee_haircut + emissions paid while
              the price sat there * emission_haircut (emissions are time-based, fees volume-based).
"""
import numpy as np
import pandas as pd

from .pool_math import amounts_for_liquidity, ratio_for_range
from .sim import Target
from .strategies import Strategy


class LadderX(Strategy):
    name = "ladderx"

    def prepare(self, m) -> None:
        self._prepare_guard(m)
        day = int(86400 / m.bar_seconds)
        t = pd.Series(m.bar_tick.astype(float))
        self.med7 = t.rolling(7 * day, min_periods=1).median().round().to_numpy().astype(int)
        self.exit_bars = int(np.ceil(self.params.get("exit_s", 0) / m.bar_seconds)) if self.params.get("exit_s") else 0
        self.med1 = t.rolling(day, min_periods=1).median().round().to_numpy().astype(int)
        self.bar_tick = m.bar_tick.astype(int)

    def reset(self) -> None:
        self.cur = None
        self.w = None
        self.core = None

    # -- the solve -----------------------------------------------------------------------------
    def _stats(self, st, include_spot: bool = False):
        """Per-tick fees R and mean others' liquidity O around the centre, over the lookback."""
        m = st.market
        i1 = st.i
        i0 = int(np.searchsorted(m.ts, m.ts[i1] - self.lookback))
        ticks = m.tick[i0 : i1 + 1]
        if len(ticks) < 50:
            return None
        c = int(np.median(ticks))
        if self.params.get("center") == "blend":
            c = int(round((c + self.med7[st.bar]) / 2))
        tail = int(self.params.get("tail", 0)) if self.params.get("prior") else 0
        lo, hi = c - self.span - tail, c + self.span + 1 + tail
        if include_spot:  # swap-free sides and exit re-solves need the current tick in the window
            lo, hi = min(lo, st.tick - 1), max(hi, st.tick + 2)
        sel = (ticks >= lo) & (ticks < hi)
        idx = ticks[sel] - lo
        n = hi - lo
        hl = self.params.get("decay_hl_s")
        if hl:
            age = m.ts[i1] - m.ts[i0 : i1 + 1][sel]
            wgt = np.power(0.5, age / hl)
        else:
            wgt = np.ones(int(sel.sum()))
        r = np.bincount(idx, weights=m.fee[i0 : i1 + 1][sel] * wgt, minlength=n)
        if self.params.get("emis"):  # haircuts as in sim.Costs: fees 82%, SOLID 50%
            e = m.emis_rate[i0 : i1 + 1][sel] * m.dt[i0 : i1 + 1][sel]
            r = 0.82 * r + 0.5 * np.bincount(idx, weights=e * wgt, minlength=n)
        ws = np.bincount(idx, weights=wgt, minlength=n)
        o = np.bincount(idx, weights=m.l_others[i0 : i1 + 1][sel] * wgt, minlength=n) / np.maximum(ws, 1e-12)
        if self.params.get("prior") and r.sum() > 0:
            seen = ws > 0
            o = np.where(seen, o, np.median(o[seen]) if seen.any() else 1.0)  # unseen ticks: typical depth
            r = r + self.params["prior"] * r.sum() / n
        o = np.maximum(o, 1.0)
        if r.sum() <= 0:
            return None
        a = self.params.get("forecast", 0.0)
        if a:
            f = self._forecast(st, lo, n)
            if f is not None:
                r = (1 - a) * r / r.sum() + a * f
        return lo, r, o

    def _forecast(self, st, lo: int, n: int):
        """Probability of each tick in [lo, lo+n) over the next `every` bars, given today's
        deviation from the 1-day median (empirical, from the trailing fc_lookback_s)."""
        m = st.market
        b = st.bar
        h = int(self.every)
        lb = int(self.params.get("fc_lookback_s", 7 * 86400) / m.bar_seconds)
        s0 = max(0, b - lb)
        s1 = b - h  # the last start whose whole future window is already observed
        if s1 - s0 < 288:
            return None
        dev = self.bar_tick[s0:s1] - self.med1[s0:s1]
        now_dev = self.bar_tick[b] - self.med1[b]
        starts = np.nonzero(np.abs(dev - now_dev) <= 1)[0] + s0
        if len(starts) < 24:
            return None
        if len(starts) > 400:  # thin evenly to bound the cost
            starts = starts[np.linspace(0, len(starts) - 1, 400).astype(int)]
        fut = self.bar_tick[starts[:, None] + np.arange(1, h + 1)[None, :]] - self.med1[starts][:, None]
        idx = (fut + self.med1[b] - lo).ravel()
        idx = idx[(idx >= 0) & (idx < n)]
        if not len(idx):
            return None
        f = np.bincount(idx, minlength=n).astype(float)
        return f / f.sum()

    @staticmethod
    def _fill(r, o, budget):
        """Water-filling: l_i = max(0, sqrt(R_i O_i / lam) - O_i) with sum l = budget."""
        if budget <= 0 or r.sum() <= 0:
            return np.zeros_like(r)

        def alloc(lam):
            return np.maximum(0.0, np.sqrt(r * o / lam) - o)

        a, b = 1e-30, 1e10
        for _ in range(200):
            mid = np.sqrt(a * b)
            if alloc(mid).sum() > budget:
                a = mid
            else:
                b = mid
        return alloc(b)

    def _inventory(self, st):
        """Token0 and token1 we would hold after burning everything (position + idle)."""
        x, y = st.x, st.y
        for tl, tu, liq in st.ranges:
            a, b = amounts_for_liquidity(liq, st.sp, tl, tu)
            x, y = x + a, y + b
        return x, y

    def _weights(self, st, lo, r, o):
        """Liquidity per tick (in budget units) -> value weights over ticks [lo, lo+n)."""
        budget = self.budget_l
        if not self.params.get("swapfree"):
            return self._fill(r, o, budget)
        n = len(r)
        ticks = lo + np.arange(n)
        x, y = self._inventory(st)
        v = x + y / st.sp**2
        fx = x / v if v > 0 else 0.5
        cur = int(np.searchsorted(ticks, st.tick))  # index of the current tick
        l = np.zeros(n)
        # Current tick first, from the unconstrained solve, capped by what each token allows.
        lc = self._fill(r, o, budget)[cur] if 0 <= cur < n else 0.0
        rc = ratio_for_range(st.sp, st.tick, st.tick + 1)
        if 0 <= cur < n:
            caps = [lc]
            if rc > 0:
                caps.append(fx * budget / rc)
            if rc < 1:
                caps.append((1 - fx) * budget / (1 - rc))
            lc = max(0.0, min(caps))
            l[cur] = lc
        bx = max(0.0, fx * budget - rc * lc)  # left for ticks above (token0 only)
        by = max(0.0, (1 - fx) * budget - (1 - rc) * lc)  # left for ticks below (token1 only)
        above, below = ticks > st.tick, ticks < st.tick
        la = self._fill(r[above], o[above], bx)
        lb = self._fill(r[below], o[below], by)
        # A side that saw no fees in the window still parks its token next to spot (a limit
        # order that fills when the price comes back), rather than leaving it idle.
        if bx > 0 and la.sum() <= 0 and above.any():
            la[0] = bx
        if by > 0 and lb.sum() <= 0 and below.any():
            lb[-1] = by
        l[above], l[below] = la, lb
        return l

    def _core_range(self, st):
        k = self.params.get("core_k", 10)
        c = self.med7[st.bar]
        if self.core is None or abs(c - self.core) >= self.params.get("core_move", max(2, k // 2)):
            self.core = c
        return self.core - k, self.core + k + 1

    # -- decisions -----------------------------------------------------------------------------
    def decide(self, st):
        if self.cur is None:
            m = st.market
            ls = getattr(m, "liq_scale", 10.0 ** getattr(m, "decimals", 6))
            v = st.x + st.y / (st.sp**2 * getattr(m, "px_scale", 1.0))
            self.budget_l = v * 10.0 ** getattr(m, "decimals", 6) * st.sp / (np.sqrt(1.0001) - 1) / ls
        if self.cur is not None:
            if self.frozen(st):
                return None
            scheduled = st.bar % self.every == 0
            exited = self.exit_bars and st.bars_out_of_range >= self.exit_bars
            if not (scheduled or exited):
                return None
        else:
            exited = False
        s = self._stats(st, include_spot=bool(self.params.get("swapfree")) or bool(exited))
        if s is None:
            if self.cur is None:
                self.cur = (st.tick, None)
                return Target([(st.tick - 2, st.tick + 3, 1.0)], swap=True)
            return None
        lo, r, o = s
        liq = self._weights(st, lo, r, o)
        if liq.sum() <= 0:
            return None
        # Value weights: near the peg one unit of liquidity in one tick costs the same value
        # whichever token it is held in, so liquidity shares are value shares.
        w = liq / liq.sum()
        same_lo = self.cur is not None and self.cur[0] == lo and self.w is not None and len(self.w) == len(w)
        if not exited and same_lo and np.abs(w - self.w).sum() / 2 < self.turnover:
            return None
        self.cur, self.w = (lo, None), w
        cf = self.params.get("core_frac", 0.0)
        ranges = [(lo + j, lo + j + 1, float(w[j]) * (1 - cf)) for j in range(len(w)) if w[j] > 1e-3]
        if cf:
            a, b = self._core_range(st)
            ranges.append((a, b, cf))
        return Target(ranges, swap=True)


class Dual(Strategy):
    """Two ladders sharing the capital: `fast` (weight w) and `slow` (1 - w). Each keeps its own
    schedule and turnover rule; when either changes its allocation, the merged book is re-minted.
    The slow half does not chase every move, so less of the book locks in each swing."""

    name = "dual"

    def __init__(self, fast: dict, slow: dict, w: float = 0.5, **params):
        super().__init__(fast=fast, slow=slow, w=w, **params)
        g = {"guard": params["guard"]} if params.get("guard") else {}
        self.kids = [(LadderX(**fast, **g), w), (LadderX(**slow, **g), 1 - w)]

    def prepare(self, m) -> None:
        self._prepare_guard(m)
        for k, _ in self.kids:
            k.prepare(m)

    def reset(self) -> None:
        self.cur = None
        self.last = [None, None]
        for k, _ in self.kids:
            k.reset()

    def decide(self, st):
        from dataclasses import replace

        changed = False
        for j, (k, w) in enumerate(self.kids):
            sub = replace(st, x=st.x * w, y=st.y * w) if k.cur is None else st
            t = k.decide(sub)
            if t is not None:
                self.last[j] = t
                changed = True
        if not changed or any(t is None for t in self.last):
            return None
        self.cur = True
        ranges = []
        for (k, w), t in zip(self.kids, self.last):
            tw = sum(r[2] for r in t.ranges) or 1.0
            ranges += [(a, b, c / tw * w) for a, b, c in t.ranges]
        return Target(ranges, swap=True)
