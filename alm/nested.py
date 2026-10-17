"""Overlapping (nested) ranges instead of the ladder's separate single-tick ranges.

V3 liquidity adds up per tick, so a set of overlapping ranges earns exactly what single-tick ranges
with the same per-tick liquidity earn; only the shape of the per-tick profile matters. Two families:

- NestedLadder: the ladder's own target, rebuilt as nested ranges ("layer cake": each layer spans
  every tick whose weight is at least that layer's level). Same profile, fewer positions that each
  cover the price: a check that the decomposition changes nothing but the number of transactions.
- Pyramid: ranges centred on the ladder's centre (median tick of the lookback), half-widths `ks`,
  layer k holding value in proportion to (2k+1)**a. All layers earn while the price sits at the
  centre; as it drifts, the narrow layers drop out first and the wide ones keep earning until the
  next re-centre. a = 0: equal value per layer (most peaked); a = 1: equal liquidity per layer.
  Re-centres every `every` bars when the centre moved (and the depeg guard isn't holding).
"""
import numpy as np

from .sim import Target
from .strategies import Ladder


def layer_cake(lo: int, w) -> list:
    """Per-tick weights on [lo, lo+len(w)) -> nested ranges (tl, tu, value) with the same per-tick value."""
    w = np.asarray(w, dtype=float)
    out = []
    prev = 0.0
    for level in sorted(set(float(x) for x in w if x > 0)):
        mask = w >= level - 1e-15
        j = 0
        while j < len(w):
            if mask[j]:
                k = j
                while k < len(w) and mask[k]:
                    k += 1
                out.append((lo + j, lo + k, (level - prev) * (k - j)))
                j = k
            else:
                j += 1
        prev = level
    return out


class NestedLadder(Ladder):
    name = "nested"

    def decide(self, st):
        t = super().decide(st)
        if t is None:
            return None
        lo = min(a for a, _, _ in t.ranges)
        hi = max(b for _, b, _ in t.ranges)
        w = np.zeros(hi - lo)
        for a, _, x in t.ranges:
            w[a - lo] += x
        return Target(layer_cake(lo, w), swap=True)


class Pyramid(Ladder):
    name = "pyramid"

    def __init__(self, ks=(0, 1, 2, 3), a=0.0, **params):
        super().__init__(ks=tuple(ks), a=a, **params)

    def reset(self) -> None:
        super().reset()
        self.centre = None

    def decide(self, st):
        if self.centre is not None and (st.bar % self.every or self.frozen(st)):
            return None
        if self.centre is None:
            m = st.market
            px = getattr(m, "px_scale", 1.0)
            ls = getattr(m, "liq_scale", 10.0 ** getattr(m, "decimals", 6))
            v = st.x + st.y / (st.sp**2 * px)
            self.budget_l = v * 10.0 ** getattr(m, "decimals", 6) * st.sp / (np.sqrt(1.0001) - 1) / ls
        sol = self._solve(st)
        c = st.tick if sol is None else sol[0] + self.span
        if c == self.centre:
            return None
        self.centre = c
        ranges = [(c - k, c + k + 1, float((2 * k + 1) ** self.a)) for k in self.ks]
        return Target(ranges, swap=True)
