"""Range strategies. Each precomputes its indicators over the market's bars in prepare() so that
decide() at bar b only reads values computed from bars <= b (no lookahead).

Range notation: half-width k around centre c is [c - k, c + k + 1), so k = 0 is the single tick c.
"""
import numpy as np
import pandas as pd

from .sim import Target


class Strategy:
    name = "base"

    def __init__(self, **params):
        self.params = params
        self.__dict__.update(params)

    def prepare(self, m) -> None:
        self._prepare_guard(m)

    def _prepare_guard(self, m) -> None:
        """Depeg guard: weekly median of the bar tick. While spot is more than `guard` ticks away
        from it, the strategy freezes — no recentre, no swap — and waits for the peg to return.
        Rebalancing into a depeg turns a temporary deviation into a realised loss."""
        g = self.params.get("guard")
        self.guard_dev = None
        if g:
            week = max(1, int(7 * 86400 // m.bar_seconds))
            med = pd.Series(m.bar_tick.astype(float)).rolling(week, min_periods=min(12, week)).median().to_numpy()
            # Before the window has enough bars the guard has no reference and stays off (no backfill
            # from later bars).
            self.guard_dev = np.nan_to_num(np.abs(m.bar_tick - med), nan=0.0)

    def frozen(self, st) -> bool:
        return self.guard_dev is not None and self.guard_dev[st.bar] > self.params["guard"]

    def reset(self) -> None:
        self.cur = None  # (centre, k)

    def decide(self, st):
        raise NotImplementedError

    def label(self) -> str:
        return self.name + "(" + ",".join(f"{k}={v}" for k, v in self.params.items()) + ")"


class HoldUSDC(Strategy):
    """Benchmark: do nothing."""

    name = "hold_usdc"

    def decide(self, st):
        return None


def _band(c: int, k: int) -> tuple:
    return (c - k, c + k + 1, 1.0)


class StaticBand(Strategy):
    """Mint once around the starting tick (or a fixed anchor) and never touch it."""

    name = "static"

    def decide(self, st):
        if self.cur is None:
            c = self.params.get("anchor", st.tick)
            self.cur = (c, self.k)
            return Target([_band(c, self.k)], swap=True)
        return None


class Recenter(Strategy):
    """Out of range for `dwell` bars (and `cooldown` since the last action) -> recentre on spot.

    swap=True swaps to the new range's ratio (a classic rebalance); swap=False re-mints with the
    inventory as is and parks the leftover in a limit range of width `limit` (no swap).
    The old vote-optimizer alm.ts rule is Recenter(k=200, dwell=0, cooldown=0, swap=True).
    """

    name = "recenter"

    def decide(self, st):
        if self.cur is None:
            self.cur = (st.tick, self.k)
            return Target([_band(st.tick, self.k)], swap=True)
        if self.frozen(st):
            return None
        if st.bars_out_of_range >= max(self.dwell, 1) and st.bar - st.last_action_bar >= self.cooldown:
            self.cur = (st.tick, self.k)
            return Target([_band(st.tick, self.k)], swap=self.swap, limit_width=self.params.get("limit", 0))
        return None


class VolBand(Strategy):
    """Width from an indicator, centred on spot or on a median/SMA anchor.

    k = clamp(ceil(z * sigma), kmin, kmax), sigma = rolling std of the bar tick over `lookback`
    bars ('std', Bollinger-like) or mean high-low range over `lookback` bars ('atr').
    centre = spot tick ('spot'), rolling median ('median') or rolling mean ('sma') over `anchor_lb`.
    Every `every` bars the target is recomputed; the position is changed only if the centre moved
    by >= `move` ticks or k changed, or if the position has been out of range for `dwell` bars.
    swap=False re-mints around the new centre with existing inventory and parks the leftover in a
    limit range (`limit` ticks wide) next to spot â€” so widening/tightening never needs a swap.
    """

    name = "volband"

    def prepare(self, m) -> None:
        self._prepare_guard(m)
        t = pd.Series(m.bar_tick.astype(float))
        lb = self.lookback
        if self.params.get("vol", "std") == "atr":
            sig = pd.Series(m.bar_high - m.bar_low, dtype=float).rolling(lb, min_periods=max(lb // 4, 2)).mean()
        else:
            sig = t.rolling(lb, min_periods=max(lb // 4, 2)).std()
        # warm-up bars use the volatility seen so far, never a later value
        early = t.expanding(min_periods=2).std() if self.params.get("vol", "std") != "atr" else \
            pd.Series(m.bar_high - m.bar_low, dtype=float).expanding(min_periods=1).mean()
        self.sig = sig.fillna(early).fillna(0.0).to_numpy()
        a = self.params.get("anchor", "spot")
        alb = self.params.get("anchor_lb", lb)
        if a == "median":
            self.anchor_arr = t.rolling(alb, min_periods=1).median().round().to_numpy()
        elif a == "sma":
            self.anchor_arr = t.rolling(alb, min_periods=1).mean().round().to_numpy()
        else:
            self.anchor_arr = t.to_numpy()

    def _target(self, st) -> tuple:
        k = int(np.clip(np.ceil(self.z * self.sig[st.bar]), self.kmin, self.kmax))
        return int(self.anchor_arr[st.bar]), k

    def decide(self, st):
        if self.cur is None:
            self.cur = self._target(st)
            return Target([_band(*self.cur)], swap=True)
        if self.frozen(st):
            return None
        out = st.bars_out_of_range >= self.params.get("dwell", 10**9)
        if st.bar % self.every and not out:
            return None
        if st.bar - st.last_action_bar < self.params.get("cooldown", 0):
            return None
        c, k = self._target(st)
        if out and not (c - k <= st.tick <= c + k):
            c = st.tick  # anchor lags the market: follow spot rather than stay out of range
        cc, ck = self.cur
        if abs(c - cc) < self.move and k == ck and not out:
            return None
        self.cur = (c, k)
        return Target([_band(c, k)], swap=self.swap, limit_width=self.params.get("limit", 0))


class Range24h(Strategy):
    """Range = where the price traded over the last `lookback_s` seconds (default 24h), plus a
    directional buffer on the side the price is leaving through.

    Rebalance only after the price has been out of range for `dwell_s` seconds (0 = at the first
    decision bar out of range), so a quick excursion that comes back costs nothing.
    New range: [low24h - pad, high24h + pad + 1), widened to contain spot; then extended by
    `buffer` ticks in the direction of the exit (price above the old range -> extra room above).
    `min_width` keeps the range from collapsing on a flat day.
    """

    name = "range24h"

    def prepare(self, m) -> None:
        self._prepare_guard(m)
        n = max(1, int(round(self.params.get("lookback_s", 86400) / m.bar_seconds)))
        self.lo24 = pd.Series(m.bar_low).rolling(n, min_periods=1).min().to_numpy()
        self.hi24 = pd.Series(m.bar_high).rolling(n, min_periods=1).max().to_numpy()
        self.dwell_bars = int(np.ceil(self.params.get("dwell_s", 0) / m.bar_seconds))
        self.cooldown_bars = int(np.ceil(self.params.get("cooldown_s", 0) / m.bar_seconds))

    def _range(self, st, direction: int) -> tuple:
        pad, buf, mw = self.params.get("pad", 0), self.params.get("buffer", 0), self.params.get("min_width", 1)
        lo = int(min(self.lo24[st.bar], st.tick)) - pad
        hi = int(max(self.hi24[st.bar], st.tick)) + pad + 1
        if direction > 0:
            hi += buf
        elif direction < 0:
            lo -= buf
        while hi - lo < mw:
            lo, hi = lo - 1, hi + 1 if (hi - lo) % 2 else hi
        return lo, hi

    def decide(self, st):
        if self.cur is None:
            lo, hi = self._range(st, 0)
            self.cur = (lo, hi)
            return Target([(lo, hi, 1.0)], swap=True)
        if self.frozen(st):
            return None
        if st.bars_out_of_range >= max(self.dwell_bars, 1) and st.bar - st.last_action_bar >= self.cooldown_bars:
            lo_old, hi_old = self.cur
            direction = 1 if st.tick >= hi_old else -1
            lo, hi = self._range(st, direction)
            self.cur = (lo, hi)
            return Target([(lo, hi, 1.0)], swap=self.params.get("swap", True), limit_width=self.params.get("limit", 0))
        return None


class Adaptive(Strategy):
    """Volatility-adaptive range, adaptive timing, and an optional directional skew ("LP as trading").

    Width: k = clamp(ceil(z * sigma), kmin, kmax), sigma in ticks per sqrt(day) from one of
      'std' (rolling std of 5-min tick changes), 'atr' (mean bar high-low), 'ewma' (exponentially
      weighted std of changes), 'parkinson' (high-low range estimator), each scaled to the horizon.
    Timing, relative to the 7-day median sigma (regime ratio r = sigma / sigma_ref):
      dwell (wait out of range before acting) = dwell_s * r**a and recheck period = recheck_s / r**a,
      with a = +1 ('vol_up': more patience when volatile — excursions revert), -1 ('vol_down': act
      faster when volatile), 0 ('fixed').
    Skew (ticks added to the centre, capped at skew_max) from a trend signal over skew_lb:
      'momentum'  : past move / sigma            -> lean the range the way price is going
      'meanrev'   : -(spot - median) / sigma     -> lean toward the mean (fade the move)
      'ema'       : (EMA_fast - EMA_slow) / sigma -> trend-following crossover
    A range leaning above spot holds more token0 and sells it as price rises, i.e. a long position
    in token0 that takes profit gradually; leaning below is the mirror.
    """

    name = "adaptive"

    def prepare(self, m) -> None:
        self._prepare_guard(m)
        bs = m.bar_seconds
        day = 86400 / bs
        t = pd.Series(m.bar_tick.astype(float))
        d = t.diff().fillna(0.0)
        lb = max(2, int(self.params.get("vol_lb_s", 86400) / bs))
        algo = self.params.get("vol", "std")
        if algo == "atr":
            per_bar = pd.Series(m.bar_high - m.bar_low, dtype=float).rolling(lb, min_periods=2).mean()
        elif algo == "ewma":
            per_bar = np.sqrt((d**2).ewm(span=lb, min_periods=2).mean())
        elif algo == "parkinson":
            hl = pd.Series(m.bar_high - m.bar_low + 1.0, dtype=float)
            per_bar = np.sqrt((hl**2).rolling(lb, min_periods=2).mean() / (4 * np.log(2)))
        else:
            per_bar = d.rolling(lb, min_periods=2).std()
        sig = (per_bar * np.sqrt(day)).fillna(1.0).clip(lower=0.25)  # warm-up: a default, not a later value
        self.sig = sig.to_numpy()
        self.sig_ref = sig.rolling(int(7 * day), min_periods=1).median().to_numpy()
        slb = max(2, int(self.params.get("skew_lb_s", 86400) / bs))
        mode = self.params.get("skew", "none")
        if mode == "momentum":
            sigl = (t - t.shift(slb)).fillna(0.0)
        elif mode == "meanrev":
            sigl = -(t - t.rolling(slb, min_periods=1).median())
        elif mode == "ema":
            sigl = t.ewm(span=max(2, slb // 4)).mean() - t.ewm(span=slb).mean()
        else:
            sigl = t * 0.0
        self.skew_raw = (sigl / sig).fillna(0.0).to_numpy()
        a = {"vol_up": 1.0, "vol_down": -1.0}.get(self.params.get("timing", "fixed"), 0.0)
        r = np.clip(self.sig / np.maximum(self.sig_ref, 1e-9), 0.25, 4.0) ** a
        self.dwell_bars = np.ceil(self.params.get("dwell_s", 900) * r / bs).astype(int)
        self.recheck_bars = np.maximum(1, (self.params.get("recheck_s", 3600) / r / bs)).astype(int)

    def _target(self, st) -> tuple:
        b = st.bar
        k = int(np.clip(np.ceil(self.z * self.sig[b]), self.kmin, self.kmax))
        cap = self.params.get("skew_max", 0)
        skew = int(np.clip(round(self.params.get("skew_gain", 1.0) * self.skew_raw[b]), -cap, cap)) if cap else 0
        c = st.tick + skew
        lo, hi = c - k, c + k + 1
        # Keep spot inside the range so the position earns now (the skew only leans it).
        lo, hi = min(lo, st.tick), max(hi, st.tick + 1)
        return lo, hi

    def reset(self) -> None:
        self.cur = None
        self.last_check = -10**9

    def decide(self, st):
        b = st.bar
        if self.cur is None:
            self.cur = self._target(st)
            self.last_check = b
            return Target([(*self.cur, 1.0)], swap=True)
        if self.frozen(st):
            return None
        out = st.bars_out_of_range >= max(1, self.dwell_bars[b])
        due = b - self.last_check >= self.recheck_bars[b]
        if not (out or due):
            return None
        self.last_check = b
        lo, hi = self._target(st)
        clo, chi = self.cur
        move = self.params.get("move", 2)
        if not out and abs((lo + hi) - (clo + chi)) / 2 < move and abs((hi - lo) - (chi - clo)) < 2 * move:
            return None
        self.cur = (lo, hi)
        return Target([(lo, hi, 1.0)], swap=True)


class Ladder(Strategy):
    """Water-filling across single ticks (research S4).

    Every `every` bars, over the trailing `lookback` seconds: R_i = pool fees paid while the price
    sat in tick i, O_i = mean others-liquidity there. Our income from l_i in tick i is
    R_i * l_i / (O_i + l_i); with equal capital cost per unit of liquidity near the peg the optimum
    is l_i = max(0, sqrt(R_i * O_i / lam) - O_i), lam found by bisection so sum(l_i) fits the budget.
    Ticks are restricted to +-`span` around the median tick. Re-minted (with a swap) only when the
    allocation changed by more than `turnover` (fraction of capital moved).
    """

    name = "ladder"

    def reset(self) -> None:
        self.cur = None
        self.w = None

    def _solve(self, st):
        m = st.market
        i1 = st.i
        i0 = int(np.searchsorted(m.ts, m.ts[i1] - self.lookback))
        ticks = m.tick[i0 : i1 + 1]
        if len(ticks) < 50:
            return None
        c = int(np.median(ticks))
        lo, hi = c - self.span, c + self.span + 1
        sel = (ticks >= lo) & (ticks < hi)
        idx = ticks[sel] - lo
        n = hi - lo
        r = np.bincount(idx, weights=m.fee[i0 : i1 + 1][sel], minlength=n)
        cnt = np.bincount(idx, minlength=n)
        o = np.bincount(idx, weights=m.l_others[i0 : i1 + 1][sel], minlength=n) / np.maximum(cnt, 1)
        o = np.maximum(o, 1.0)
        if r.sum() <= 0:
            return None
        budget = self.budget_l  # liquidity units the capital buys per tick (approx, near peg)

        def alloc(lam):
            return np.maximum(0.0, np.sqrt(r * o / lam) - o)

        a, b = 1e-30, 1e10
        for _ in range(200):
            mid = np.sqrt(a * b)
            if alloc(mid).sum() > budget:
                a = mid
            else:
                b = mid
        liq = alloc(b)
        if liq.sum() <= 0:
            return None
        w = liq / liq.sum()
        return lo, w

    def decide(self, st):
        if self.cur is None:
            # Capital buys ~capital / (sqrt(1.0001)-1) / 1.0 liquidity units in one tick near peg.
            x, y = st.x, st.y
            # Capital (whole token0 units) -> liquidity of one tick at the current price:
            # L = v * sqrt(P) / (sqrt(1.0001) - 1), in raw units, then scaled like the market's
            # liquidity. With equal decimals and P ~ 1 this is v / (sqrt(1.0001) - 1).
            m = st.market
            px = getattr(m, "px_scale", 1.0)
            ls = getattr(m, "liq_scale", 10.0 ** getattr(m, "decimals", 6))
            v = x + y / (st.sp**2 * px)
            self.budget_l = v * 10.0 ** getattr(m, "decimals", 6) * st.sp / (np.sqrt(1.0001) - 1) / ls
        if self.cur is not None and (st.bar % self.every or self.frozen(st)):
            return None
        sol = self._solve(st)
        if sol is None:
            if self.cur is None:
                self.cur = (st.tick, None)
                return Target([(st.tick - 2, st.tick + 3, 1.0)], swap=True)
            return None
        lo, w = sol
        if self.w is not None and self.cur[0] == lo and np.abs(w - self.w).sum() / 2 < self.turnover:
            return None
        self.cur, self.w = (lo, None), w
        ranges = [(lo + j, lo + j + 1, float(w[j])) for j in range(len(w)) if w[j] > 1e-3]
        return Target(ranges, swap=True)


class ReplayPositions(Strategy):
    """Replays a wallet's real mints/burns (for validating the simulator against reality)."""

    name = "replay"

    def __init__(self, schedule: list, **params):
        super().__init__(**params)
        self.schedule = schedule  # list of (ts, [(tl, tu, liquidity), ...]) target states

    def reset(self) -> None:
        self.j = 0

    def decide(self, st):
        m = st.market
        if self.j < len(self.schedule) and m.bar_end[st.bar] >= self.schedule[self.j][0]:
            ranges = self.schedule[self.j][1]
            self.j += 1
            st.ranges = [r for r in ranges]
            st.x = st.y = 0.0
            return None
        return None
