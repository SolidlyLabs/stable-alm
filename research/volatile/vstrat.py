"""Pool-agnostic fee-capture strategies for vsim. Widths are in multiples of the pool's own daily
volatility (trailing 7 days) and snapped to its tick spacing, so one parameter set runs on any pool.

- FullRange: one position over (almost) all prices, never touched.
- Band(k, delay, gate): +-k daily sigmas around the price; re-centred once the price has been out of
  range for `delay` bars (rebalance on exit). gate: leave the pool while the last hour's
  volatility exceeds `gate` x the 7-day level, come back when it calms.
- FeeLVR(lookback, span, every, bucket, lvr): the live ladder generalised. Over the trailing
  `lookback`, per price bucket of `bucket` spacings within +-span sigmas: R = fees the pool earned
  there, O = mean liquidity there, c = expected loss to arbitrage (LVR = sigma^2/4 * L * sqrt(P) per
  unit time, times the time the price spent there, times `lvr`). Our liquidity per bucket maximises
  sum R*l/(O+l) - c*l under the capital budget: l = max(0, sqrt(R*O/(c + lam*v)) - O). When fees
  don't cover the expected loss anywhere, part or all of the capital stays out of the pool.
"""
import math

import numpy as np

LN = math.log(1.0001)


def trailing_mean(a, n, min_n):
    c = np.cumsum(np.concatenate([[0.0], a]))
    idx = np.arange(1, len(a) + 1)
    lo = np.maximum(0, idx - n)
    cnt = idx - lo
    out = (c[idx] - c[lo]) / cnt
    out[cnt < min_n] = np.nan
    return out


class Base:
    name = "base"

    def __init__(self, **params):
        self.params = params
        for k, v in params.items():
            setattr(self, k, v)
        self.deploy_share = 1.0

    def label(self):
        return self.name + "(" + ",".join(f"{k}={v}" for k, v in self.params.items()) + ")"

    def prepare(self, m):
        self.m = m
        self.var7 = trailing_mean(m.bar_r2, 2016, 288)
        self.var1h = trailing_mean(m.bar_r2, 12, 12)

    def reset(self):
        self.centre = None

    def sigma_day(self, b):
        v = self.var7[b]
        return math.sqrt(v * 288) if v == v and v > 0 else None

    def snap(self, t, down=True):
        s = self.m.pool.spacing
        return (t // s) * s if down else -((-t) // s) * s


class FullRange(Base):
    name = "full"

    def decide(self, st):
        if self.centre is not None:
            return None
        self.centre = 0
        s = self.m.pool.spacing
        return [(self.snap(-887000), self.snap(887000, False), 1.0)]


class Band(Base):
    name = "band"

    def __init__(self, k=2.0, delay=0, gate=None):
        super().__init__(k=k, delay=delay, gate=gate)

    def decide(self, st):
        sd = self.sigma_day(st.b)
        if sd is None:
            return None
        if self.gate:
            hot = self.var1h[st.b] > self.gate * self.var7[st.b]
            if hot and st.ranges:
                self.centre = None
                return []
            if hot:
                return None
        if st.ranges and (st.out_since is None or st.b - st.out_since < self.delay):
            return None
        half = max(self.m.pool.spacing, int(self.k * sd / LN))
        tl, tu = self.snap(st.tick - half), self.snap(st.tick + half, False)
        if tu <= tl:
            tu = tl + self.m.pool.spacing
        self.centre = st.tick
        return [(tl, tu, 1.0)]


class FeeLVR(Base):
    name = "feelvr"

    def __init__(self, lookback=86400, span=2.0, every=72, bucket=1, lvr=1.0, turnover=0.3, delay=12):
        super().__init__(lookback=lookback, span=span, every=every, bucket=bucket, lvr=lvr, turnover=turnover, delay=delay)

    def reset(self):
        self.w = None
        self.lo = None
        self.next_b = -1

    def _solve(self, st):
        m = self.m
        sd = self.sigma_day(st.b)
        if sd is None:
            return None
        i1 = st.i
        i0 = int(np.searchsorted(m.ts, m.ts[i1] - self.lookback))
        if i1 - i0 < 50:
            return None
        W = m.pool.spacing * self.bucket
        half = max(1, int(self.span * sd / LN / W + 0.5))
        c0 = (st.tick // W) * W
        lo = c0 - half * W
        n = 2 * half + 1
        t = m.tick[i0 : i1 + 1]
        k = (t - lo) // W
        sel = (k >= 0) & (k < n)
        k = k[sel]
        R = np.bincount(k, weights=m.fee[i0 : i1 + 1][sel], minlength=n)
        cnt = np.bincount(k, minlength=n)
        O = np.bincount(k, weights=m.L[i0 : i1 + 1][sel], minlength=n) / np.maximum(cnt, 1)
        tau = np.bincount(k, weights=m.dt[i0 : i1 + 1][sel], minlength=n)
        if R.sum() <= 0:
            return None
        var_s = self.var7[st.b] / BAR_S
        edges = lo + W * np.arange(n + 1)
        pe = m.price_of_tick(edges)
        pm = np.sqrt(pe[:-1] * pe[1:])
        root = np.sqrt(pm) if m.pool.numeraire == 1 else 1 / np.sqrt(pm)
        c = self.lvr * var_s / 4 * root * tau
        # Capital per unit of liquidity in each bucket at today's price.
        p = st.p
        sp = math.sqrt(p)
        sa, sb = np.sqrt(pe[:-1]), np.sqrt(pe[1:])
        x1 = np.where(sp <= sa, 1 / sa - 1 / sb, np.where(sp >= sb, 0.0, 1 / sp - 1 / sb))
        y1 = np.where(sp <= sa, 0.0, np.where(sp >= sb, sb - sa, sp - sa))
        v = x1 * p + y1 if m.pool.numeraire == 1 else x1 + y1 / p
        O = np.maximum(O, 1e-12)
        V = m.value(st.x, st.y, p) + sum(m.value(*_amts(m, r, p), p) for r in st.ranges)

        def alloc(lam):
            return np.maximum(0.0, np.sqrt(R * O / np.maximum(c + lam * v, 1e-300)) - O)

        if (c > 0).all() and float(np.dot(alloc(0.0), v)) <= V:
            l = alloc(0.0)
        else:
            a, b = 1e-30, 1e10
            for _ in range(200):
                mid = math.sqrt(a * b)
                if float(np.dot(alloc(mid), v)) > V:
                    a = mid
                else:
                    b = mid
            l = alloc(b)
        val = l * v
        if val.sum() <= V * 1e-4:
            return lo, None, 0.0
        return lo, val / val.sum(), float(val.sum() / V), W

    def decide(self, st):
        due = st.b >= self.next_b
        stuck = st.ranges and st.out_since is not None and st.b - st.out_since >= self.delay
        if not (due or stuck or (not st.ranges and self.w is None)):
            return None
        sol = self._solve(st)
        if sol is None:
            return None
        self.next_b = st.b + self.every
        if sol[1] is None:
            self.w = None
            return [] if st.ranges else None
        lo, w, share, W = sol
        if self.w is not None and self.lo == lo and not stuck and st.ranges and np.abs(w - self.w).sum() / 2 < self.turnover:
            return None
        self.lo, self.w = lo, w
        self.deploy_share = min(1.0, share)
        return [(lo + j * W, lo + (j + 1) * W, float(w[j])) for j in range(len(w)) if w[j] > 1e-3]


BAR_S = 300.0


def _amts(m, r, p):
    from vsim import amounts

    tl, tu, L = r
    return amounts(L, p, m.price_of_tick(tl), m.price_of_tick(tu))


def _phi(z):
    return 0.5 * (1 + np.vectorize(math.erf)(z / math.sqrt(2)))


class FeeLVRF(Base):
    """Forecast version: expected fees per bucket = pool fee rate x horizon x P(price in bucket over the
    horizon) x our share; cost = lvr x sigma^2/4 x L x sqrt(P) x the same time. The occupation
    probability comes from a driftless random walk with the trailing 7-day volatility. Others'
    liquidity per bucket = median active liquidity seen there in the last `o_days` days (nearest
    measured bucket where the price never went)."""

    name = "feelvrf"

    def __init__(self, every=72, span=3.0, bucket=1, lvr=0.5, lookback=86400, turnover=0.3, delay=12, o_days=7, max_ranges=0):
        super().__init__(every=every, span=span, bucket=bucket, lvr=lvr, lookback=lookback, turnover=turnover,
                         delay=delay, o_days=o_days, max_ranges=max_ranges)

    def reset(self):
        self.w = None
        self.lo = None
        self.next_b = -1

    def _solve(self, st):
        m = self.m
        var_b = self.var7[st.b]
        if not var_b == var_b or var_b <= 0:
            return None
        var_s = var_b / BAR_S
        i1 = st.i
        i0 = int(np.searchsorted(m.ts, m.ts[i1] - self.lookback))
        if i1 - i0 < 50:
            return None
        f = m.fee[i0 : i1 + 1].sum() / max(float(m.ts[i1] - m.ts[i0]), 1.0)  # numeraire per second
        H = self.every * BAR_S
        W = m.pool.spacing * self.bucket
        sdH = math.sqrt(var_s * H)
        half = max(1, int(self.span * sdH / LN / W + 0.5))
        c0 = (st.tick // W) * W
        lo = c0 - half * W
        n = 2 * half + 1
        edges = lo + W * np.arange(n + 1)
        x0 = st.tick * LN
        ze = edges * LN - x0
        pi = np.zeros(n)
        for j in range(8):
            s = math.sqrt(var_s * H * (j + 0.5) / 8)
            cdf = _phi(ze / s)
            pi += np.diff(cdf)
        pi /= 8
        # Others' liquidity per bucket.
        j0 = int(np.searchsorted(m.ts, m.ts[i1] - self.o_days * 86400))
        t = m.tick[j0 : i1 + 1]
        k = (t - lo) // W
        sel = (k >= 0) & (k < n)
        O = np.full(n, np.nan)
        if sel.any():
            ks, Ls = k[sel], m.L[j0 : i1 + 1][sel]
            for q in np.unique(ks):
                O[q] = np.median(Ls[ks == q])
        if np.isnan(O).all():
            O[:] = np.median(m.L[j0 : i1 + 1])
        idx = np.arange(n)
        good = ~np.isnan(O)
        O = np.interp(idx, idx[good], O[good])
        O = np.maximum(O, 1e-12)
        pe = m.price_of_tick(edges)
        pm = np.sqrt(pe[:-1] * pe[1:])
        root = np.sqrt(pm) if m.pool.numeraire == 1 else 1 / np.sqrt(pm)
        A = f * H * pi
        c = self.lvr * var_s / 4 * root * H * pi
        p = st.p
        sp = math.sqrt(p)
        sa, sb = np.sqrt(pe[:-1]), np.sqrt(pe[1:])
        x1 = np.where(sp <= sa, 1 / sa - 1 / sb, np.where(sp >= sb, 0.0, 1 / sp - 1 / sb))
        y1 = np.where(sp <= sa, 0.0, np.where(sp >= sb, sb - sa, sp - sa))
        v = x1 * p + y1 if m.pool.numeraire == 1 else x1 + y1 / p
        V = m.value(st.x, st.y, p) + sum(m.value(*_amts(m, r, p), p) for r in st.ranges)

        def alloc(lam):
            return np.maximum(0.0, np.sqrt(A * O / np.maximum(c + lam * v, 1e-300)) - O)

        if (c > 0).all() and float(np.dot(alloc(0.0), v)) <= V:
            l = alloc(0.0)
        else:
            a, b = 1e-30, 1e10
            for _ in range(200):
                mid = math.sqrt(a * b)
                if float(np.dot(alloc(mid), v)) > V:
                    a = mid
                else:
                    b = mid
            l = alloc(b)
        val = l * v
        if val.sum() <= V * 1e-3:
            return lo, None, 0.0, W
        w = val / val.sum()
        if self.max_ranges:
            keep = np.argsort(-w)[: self.max_ranges]
            mask = np.zeros(n, bool)
            mask[keep] = True
            share = float(val[mask].sum() / V)
            w = np.where(mask, w, 0.0)
            w = w / w.sum()
            return lo, w, share, W
        return lo, w, float(val.sum() / V), W

    def decide(self, st):
        due = st.b >= self.next_b
        stuck = bool(st.ranges) and st.out_since is not None and st.b - st.out_since >= self.delay
        if not (due or stuck):
            return None
        sol = self._solve(st)
        if sol is None:
            return None
        self.next_b = st.b + self.every
        lo, w, share, W = sol
        if w is None:
            self.w = None
            return [] if st.ranges else None
        if (self.w is not None and self.lo == lo and len(w) == len(self.w) and not stuck and st.ranges
                and np.abs(w - self.w).sum() / 2 < self.turnover and abs(share - self.deploy_share) < self.turnover):
            return None
        self.lo, self.w = lo, w
        self.deploy_share = min(1.0, share)
        return [(lo + j * W, lo + (j + 1) * W, float(w[j])) for j in range(len(w)) if w[j] > 1e-3]
