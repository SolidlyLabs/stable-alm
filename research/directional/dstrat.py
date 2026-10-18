"""Directional LP strategies for dsim: a regime (up, flat, down) from a trailing signal decides what the
capital does. Every width is in the pair's own daily volatility (trailing 30 days), so one parameter
set runs on any path.

Signals (all on closes up to the decision hour, in units of the hourly volatility, so a threshold
means the same in quiet and wild markets):
- mom(n):  log return over the last n hours / (sigma_h * sqrt(n))
- ema(n):  log(EMA(n/4) / EMA(n)) / (sigma_h * sqrt(n))
- brk(n):  +1 above the highest close of the previous n hours, -1 below the lowest, else 0
- tur(n):  the breakout's state: +1 from a new n-hour high until a new n-hour low (and the mirror)

Regimes switch with hysteresis: up when the signal rises above `th`, back to flat when it falls
below `off` (and the mirror for down). Breakout signals hold their regime until the opposite
breakout or until `hold_h` hours have passed without a new high (low).

What each regime does:
- flat:  a band of +-k daily sigmas around the price, re-centred after `delay` hours out of range.
- up:    'hold'  all in token0 (wait the move out in the winner);
         'above' a range just above the price, `wd` sigmas wide: all token0 now, sold into the rally
                 for fees (a take-profit ladder that earns), rolled up when the price leaves it;
         'skew'  a band with three quarters of its width above the price (mostly token0);
         'band'  nothing special (the flat band).
- down:  'hold' all in token1; 'below' a range just under the price (all token1, buying the dip);
         'skew'; 'band'.
- vgate: when the last day's volatility runs above vgate x the 30-day level, a big move is on: hold
  the token that won over the last `vh` hours until volatility falls back below 0.7 x vgate.
"""
import math

import numpy as np


def ema(a, n):
    out = np.empty_like(a)
    k = 2.0 / (n + 1)
    out[0] = a[0]
    for i in range(1, len(a)):
        out[i] = out[i - 1] + k * (a[i] - out[i - 1])
    return out


def roll_std(r, n):
    c1 = np.cumsum(np.concatenate([[0.0], r]))
    c2 = np.cumsum(np.concatenate([[0.0], r * r]))
    i = np.arange(1, len(r) + 1)
    lo = np.maximum(0, i - n)
    cnt = i - lo
    m = (c1[i] - c1[lo]) / cnt
    v = (c2[i] - c2[lo]) / cnt - m * m
    out = np.sqrt(np.maximum(v, 0))
    out[cnt < n // 2] = np.nan
    return out


class Features:
    """Signals for one path, computed once and shared by every strategy on it."""

    def __init__(self, P):
        self.P = P
        self.lp = np.log(P)
        self.r = np.diff(self.lp, prepend=self.lp[0])
        self.sh = roll_std(self.r, 720)  # hourly sigma, 30 days
        self.s24 = roll_std(self.r, 24)
        self._cache = {}

    def sigma_day(self, h):
        s = self.sh[h]
        return s * math.sqrt(24) if s == s and s > 0 else None

    def sig(self, kind, n):
        key = (kind, n)
        if key in self._cache:
            return self._cache[key]
        lp, sh = self.lp, self.sh
        if kind == "mom":
            v = np.full(len(lp), np.nan)
            v[n:] = (lp[n:] - lp[:-n]) / (sh[n:] * math.sqrt(n))
        elif kind == "ema":
            f, s = ema(lp, max(2, n // 4)), ema(lp, n)
            v = (f - s) / (sh * math.sqrt(n))
        elif kind == "brk":
            P = self.P
            v = np.zeros(len(P))
            for h in range(n, len(P)):
                w = P[h - n:h]
                v[h] = 1.0 if P[h] > w.max() else -1.0 if P[h] < w.min() else 0.0
        elif kind == "tur":
            # persistent breakout (turtle): +1 from a new n-hour high until a new n-hour low, and back
            P = self.P
            v = np.zeros(len(P))
            st = 0.0
            for h in range(n, len(P)):
                w = P[h - n:h]
                if P[h] > w.max():
                    st = 1.0
                elif P[h] < w.min():
                    st = -1.0
                v[h] = st
        else:
            raise ValueError(kind)
        self._cache[key] = v
        return v


class Strategy:
    name = "s"

    def __init__(self, f: Features, **params):
        self.f = f
        self.params = params
        for k, v in params.items():
            setattr(self, k, v)

    def label(self):
        return self.name + "(" + ",".join(f"{k}={v}" for k, v in self.params.items()) + ")"

    def reset(self):
        pass


class Hold(Strategy):
    name = "hold"

    def decide(self, h, p, rng):
        return None


class Band(Strategy):
    """The passive control: +-k daily sigmas, re-centred after `delay` hours out of range."""
    name = "band"

    def __init__(self, f, k=2.0, delay=4):
        super().__init__(f, k=k, delay=delay)

    def reset(self):
        self.out = None

    def decide(self, h, p, rng):
        sd = self.f.sigma_day(h)
        if sd is None:
            return None
        if rng is not None:
            if rng[0] <= p < rng[1]:
                self.out = None
                return None
            self.out = h if self.out is None else self.out
            if h - self.out < self.delay:
                return None
        self.out = None
        w = math.exp(self.k * sd)
        return ("band", 1 / w, w)


class Regime(Strategy):
    name = "regime"

    def __init__(self, f, sig="mom", n=48, th=1.5, off=0.5, up="hold", dn="hold", k=2.0, wd=1.0, delay=4, vgate=None, vh=24,
                 hold_h=72, lp=True):
        super().__init__(f, sig=sig, n=n, th=th, off=off, up=up, dn=dn, k=k, wd=wd, delay=delay, vgate=vgate, vh=vh, hold_h=hold_h, lp=lp)
        self.s = f.sig(sig, n)

    def reset(self):
        self.reg = 0
        self.out = None
        self.cur = None  # what is in place: ("flat"|"up"|"down"|"move", detail)
        self.last_new = None
        self.hot = False

    def regime(self, h):
        s = self.s[h]
        if s != s:
            return 0
        if self.sig == "brk":
            if s != 0:
                self.reg = int(s)
                self.last_new = h
            elif self.reg and h - self.last_new > self.hold_h:
                self.reg = 0
            return self.reg
        if self.reg == 0:
            self.reg = 1 if s > self.th else -1 if s < -self.th else 0
        elif self.reg == 1 and s < self.off:
            self.reg = -1 if s < -self.th else 0
        elif self.reg == -1 and s > -self.off:
            self.reg = 1 if s > self.th else 0
        return self.reg

    def _band(self, h, p, rng, lo_w, hi_w, tag):
        """Place (or keep) a range; re-place after `delay` hours out of it."""
        if self.cur == tag and rng is not None:
            if rng[0] <= p < rng[1]:
                self.out = None
                return None
            self.out = h if self.out is None else self.out
            if h - self.out < self.delay:
                return None
        self.out = None
        self.cur = tag
        return ("band", lo_w, hi_w)

    def decide(self, h, p, rng):
        sd = self.f.sigma_day(h)
        if sd is None:
            return None
        reg = self.regime(h)
        if self.vgate:
            ratio = self.f.s24[h] / self.f.sh[h] if self.f.sh[h] > 0 else 0
            if not self.hot and ratio > self.vgate:
                self.hot = True
            elif self.hot and ratio < 0.7 * self.vgate:
                self.hot = False
            if self.hot:
                lp = self.f.lp
                w = 1.0 if lp[h] > lp[max(0, h - self.vh)] else 0.0
                tag = ("move", w)
                if self.cur != tag:
                    self.cur = tag
                    return ("hold", w)
                return None
        w = math.exp(self.k * sd)
        wd = math.exp(self.wd * sd)
        if reg == 0:
            if not self.lp:
                if self.cur != ("hold", 0.5):
                    self.cur = ("hold", 0.5)
                    return ("hold", 0.5)
                return None
            return self._band(h, p, rng, 1 / w, w, "flat")
        mode = self.up if reg == 1 else self.dn
        if mode == "hold":
            tag = ("hold", 1.0 if reg == 1 else 0.0)
            if self.cur != tag:
                self.cur = tag
                return tag
            return None
        if mode == "band":
            return self._band(h, p, rng, 1 / w, w, "flat")
        if mode == "skew":
            a, b = (w ** 0.5, w ** 1.5) if reg == 1 else (w ** 1.5, w ** 0.5)
            return self._band(h, p, rng, 1 / a, b, ("skew", reg))
        if mode == "above":
            # rolled up only when the price has left it upward; below it, it is all token0 anyway
            if self.cur == ("above",) and rng is not None and p < rng[0]:
                return None
            return self._band(h, p, rng, 1.0005, 1.0005 * wd, ("above",))
        if mode == "below":
            if self.cur == ("below",) and rng is not None and p >= rng[1]:
                return None
            return self._band(h, p, rng, 0.9995 / wd, 0.9995, ("below",))
        raise ValueError(mode)


class Blend(Strategy):
    """Part of the capital farms, part waits for the move. The share in the pool falls as the trend signal
    strengthens: f = 1 - a * (|s| - th0), floored at min_lp and stepped in quarters (so small wiggles do
    not churn). The part taken out is held in the token the signal favours (up: token0; down: token1,
    or with protect=False only up moves take capital out). The pool part is a band of +-k daily sigmas,
    re-centred after `delay` hours out of range, optionally leaning toward the trend (lean, in quarters of
    the width moved to the trend's side).
    """
    name = "blend"

    def __init__(self, f, sig="mom", n=96, th0=0.5, a=0.5, min_lp=0.25, k=3.0, delay=4, protect=True, lean=0.0, step=0.25):
        super().__init__(f, sig=sig, n=n, th0=th0, a=a, min_lp=min_lp, k=k, delay=delay, protect=protect, lean=lean, step=step)
        self.s = f.sig(sig, n)

    def reset(self):
        self.cur = None
        self.out = None

    def decide(self, h, p, rng):
        sd = self.f.sigma_day(h)
        s = self.s[h]
        if sd is None or s != s:
            return None
        d = 1 if s > 0 else -1
        x = max(0.0, abs(s) - self.th0)
        if d < 0 and not self.protect:
            x = 0.0
        fr = max(self.min_lp, 1.0 - self.a * x)
        fr = round(fr / self.step) * self.step
        w0 = 1.0 if d > 0 else 0.0
        if fr >= 1.0:
            w0, d_lean = 0.5, 0
        else:
            d_lean = d
        tag = (fr, w0, d_lean)
        if tag == self.cur and rng is not None:
            if rng[0] <= p < rng[1]:
                self.out = None
                return None
            self.out = h if self.out is None else self.out
            if h - self.out < self.delay:
                return None
        elif tag == self.cur and fr == 0:
            return None
        self.out = None
        self.cur = tag
        w = math.exp(self.k * sd)
        lo, hi = 1 / w, w
        if self.lean and d_lean:
            shift = math.exp(self.lean * self.k * sd * d_lean)
            lo, hi = lo * shift, hi * shift
            lo, hi = min(lo, 0.999), max(hi, 1.001)
        return ("mix", fr, lo, hi, w0)


class Combo(Strategy):
    """Wait out the move, LP after it. A turtle breakout over n hours sets the trend and its winner. While
    the move is live (a new n-hour extreme in the trend's direction within the last hot_h hours), only
    min_lp of the capital farms and the rest waits in the winner. Once the move stalls, trend_lp farms
    while the trend still stands, and all of it once the price is back inside the n-hour channel's
    middle half (calm). The pool part is a band of +-k daily sigmas, re-centred after `delay` hours out.
    With parking on (dsim Costs.park0/park1) the waiting part earns a pegged pool's APR meanwhile.
    """
    name = "combo"

    def __init__(self, f, n=336, hot_h=72, min_lp=0.25, trend_lp=0.5, k=5.0, delay=4, protect=True):
        super().__init__(f, n=n, hot_h=hot_h, min_lp=min_lp, trend_lp=trend_lp, k=k, delay=delay, protect=protect)
        P = f.P
        self.st = f.sig("tur", n)
        hi = np.full(len(P), np.nan)
        lo = np.full(len(P), np.nan)
        for h in range(n, len(P)):
            w = P[h - n:h]
            hi[h], lo[h] = w.max(), w.min()
        self.hi, self.lo = hi, lo

    def reset(self):
        self.cur = None
        self.out = None
        self.last = None
        self.since = -10**9

    def decide(self, h, p, rng):
        sd = self.f.sigma_day(h)
        st = self.st[h]
        if sd is None or self.hi[h] != self.hi[h]:
            return None
        if (st > 0 and p > self.hi[h]) or (st < 0 and p < self.lo[h]):
            self.last = h
        hot = st != 0 and self.last is not None and h - self.last <= self.hot_h
        if st < 0 and not self.protect:
            hot, st = False, 0
        mid = (self.hi[h] + self.lo[h]) / 2
        half = (self.hi[h] - self.lo[h]) / 4
        calm = abs(p - mid) <= (half if self.cur is None or self.cur[0] < 1.0 else 1.5 * half)  # in at the middle half, out past 3/4
        if hot:
            fr = self.min_lp
        elif st != 0 and not calm:
            fr = self.trend_lp
        else:
            fr = 1.0
        w0 = 0.5 if fr >= 1.0 else (1.0 if st > 0 else 0.0)
        tag = (fr, w0)
        if self.cur is not None and tag != self.cur and not hot and h - self.since < 24:
            tag = self.cur  # a calm or trend change waits a day in the current state (a new move does not)
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
