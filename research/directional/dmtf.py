"""Multi-timeframe "wait out the move": Combo with its breakout read on longer candles, and an optional slow
trend that a move must agree with.

- tf:   the candle length in hours. The n-hour channel and the turtle trend are built from candle closes
        only, and a new extreme counts when a candle closes beyond it (as the app's "candles" setting
        does: a value holds from one close to the next). tf=1 is the plain Combo.
- slow: a turtle trend over `slow` hours of daily closes. A move against it is not waited out: the
        capital stays in the pool at trend_lp (a counter-trend spike tends to snap back).
"""
import math

import numpy as np

import dstrat


def candles(ts, P, tf):
    """The last completed tf-hour close at every hour (an hour's own close when it ends a candle)."""
    if tf == 1:
        return P.copy()
    end = ((ts + 3600) % (tf * 3600)) == 0  # this hour's close is the candle's close
    out = np.full(len(P), np.nan)
    last = np.nan
    for h in range(len(P)):
        if end[h]:
            last = P[h]
        out[h] = last
    return out


def turtle(C, n, step):
    """Channel of the previous n hours of candle closes (sampled every `step` hours) and the turtle state."""
    hi = np.full(len(C), np.nan)
    lo = np.full(len(C), np.nan)
    st = np.zeros(len(C))
    s = 0.0
    for h in range(n, len(C)):
        w = C[h - n:h:1] if step == 1 else C[h - n:h]
        w = w[w == w]
        if not len(w) or C[h] != C[h]:
            st[h] = s
            continue
        hi[h], lo[h] = w.max(), w.min()
        if C[h] > hi[h]:
            s = 1.0
        elif C[h] < lo[h]:
            s = -1.0
        st[h] = s
    return hi, lo, st


class ComboTF(dstrat.Combo):
    name = "combo_tf"

    def __init__(self, f, ts=None, tf=1, slow=0, **kw):
        dstrat.Strategy.__init__(self, f, **kw, tf=tf, slow=slow)
        kw = {"n": 336, "hot_h": 24, "min_lp": 0.0, "trend_lp": 1.0, "k": 5.0, "delay": 4, "protect": True, **kw}
        for k, v in kw.items():
            setattr(self, k, v)
        self.tf, self.slow = tf, slow
        key = ("mtf", tf, self.n)
        if key not in f._cache:
            C = candles(ts, f.P, tf)
            f._cache[key] = (C, *turtle(C, self.n, tf))
        self.C, self.hi, self.lo, self.st = f._cache[key]
        self.sst = None
        if slow:
            k2 = ("mtf_slow", slow)
            if k2 not in f._cache:
                D = candles(ts, f.P, 24)
                f._cache[k2] = turtle(D, slow, 24)[2]
            self.sst = f._cache[k2]

    def decide(self, h, p, rng):
        sd = self.f.sigma_day(h)
        st = self.st[h]
        c = self.C[h]
        if sd is None or self.hi[h] != self.hi[h] or c != c:
            return None
        if (st > 0 and c > self.hi[h]) or (st < 0 and c < self.lo[h]):
            self.last = h
        hot = st != 0 and self.last is not None and h - self.last <= self.hot_h
        if st < 0 and not self.protect:
            hot, st = False, 0
        if hot and self.sst is not None and self.sst[h] != st:
            hot = False  # against the slow trend: farm through it
        mid = (self.hi[h] + self.lo[h]) / 2
        half = (self.hi[h] - self.lo[h]) / 4
        calm = abs(p - mid) <= (half if self.cur is None or self.cur[0] < 1.0 else 1.5 * half)
        if hot:
            fr = self.min_lp
        elif st != 0 and not calm:
            fr = self.trend_lp
        else:
            fr = 1.0
        w0 = 0.5 if fr >= 1.0 else (1.0 if st > 0 else 0.0)
        tag = (fr, w0)
        if self.cur is not None and tag != self.cur and not hot and h - self.since < 24:
            tag = self.cur
        if tag != self.cur:
            self.since = h
        fr, w0 = tag
        if tag == self.cur:
            if fr == 0 or rng is None:
                return None
            if rng[0] <= p < rng[1]:
                self.out = None
                return None
            self.out = h if self.out is None else self.out
            if h - self.out < self.delay:
                return None
        self.out = None
        self.cur = tag
        w = math.exp(self.k * sd)
        return ("mix", fr, 1 / w, w, w0)
