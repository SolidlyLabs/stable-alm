"""Directional strategies on real swaps: dstrat's hourly decisions run inside vsim (exact fees per swap,
the pool's real liquidity), on the 90 days of ETH pools in research/volatile/cache. The hourly signals
see the long Binance ETH history before the on-chain window (warm-up), then the pool's own closes.

    python onchain.py   ->  prints vs-hold and fee figures per pool and candidate
"""
import math
import os
import sys

import numpy as np
import pandas as pd

HERE = os.path.dirname(__file__)
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "volatile"))
import dstrat  # noqa: E402
import vsim  # noqa: E402

LN = math.log(1.0001)


class Adapter:
    """A dstrat strategy driven by vsim, deciding at hour closes. Prices in USD per ETH; the pool's price
    is converted (ETH pools here have token0 = WETH, numeraire token1)."""

    def __init__(self, make, label):
        self.make, self._label = make, label
        self.deploy_share, self.idle_w0 = 1.0, None

    def label(self):
        return self._label

    def prepare(self, m):
        self.m = m
        d = pd.read_parquet(os.path.join(HERE, "..", "..", "data", "raw", "binance_ethusdt_1h.parquet")).sort_values("ts")
        t0 = int(m.bar_end[0])
        pre = d[d.ts < t0]
        hb = np.where(m.bar_end % 3600 == 0)[0]  # bars that close an hour
        self.hour_of_bar = {int(b): len(pre) + k for k, b in enumerate(hb)}
        P = np.concatenate([pre.eth_usd.to_numpy(float), m.p[m.bar_idx[hb]]])
        self.f = dstrat.Features(P)

    def reset(self):
        self.s = self.make(self.f)
        self.s.reset()
        self.deploy_share, self.idle_w0 = 1.0, None

    def decide(self, st):
        h = self.hour_of_bar.get(int(st.b))
        if h is None:
            return None
        m = self.m
        rng = None
        if st.ranges:
            tl, tu, L = st.ranges[0]
            rng = (float(m.price_of_tick(tl)), float(m.price_of_tick(tu)), L)
        act = self.s.decide(h, st.p, rng)
        if act is None:
            return None
        if act[0] == "band":
            act = ("mix", 1.0, act[1], act[2], 0.5)
        if act[0] == "hold":
            self.deploy_share, self.idle_w0 = 0.0, act[1]
            return []
        f, lo, hi, w0 = act[1], act[2], act[3], act[4]
        self.deploy_share, self.idle_w0 = f, (w0 if f < 1 else None)
        if f <= 0:
            return []
        sp = m.pool.spacing
        dd = m.pool.d0 - m.pool.d1
        tick = lambda p: math.log(p / 10.0 ** dd) / LN
        tl = int(math.floor(tick(st.p * lo) / sp)) * sp
        tu = int(math.ceil(tick(st.p * hi) / sp)) * sp
        if tu <= tl:
            tu = tl + sp
        return [(tl, tu, 1.0)]


def candidates():
    B, R, Bl = dstrat.Band, dstrat.Regime, dstrat.Blend
    return [
        ("hold 50/50", lambda f: dstrat.Hold(f)),
        ("band +-3 sigma", lambda f: B(f, k=3, delay=4)),
        ("band +-5 sigma", lambda f: B(f, k=5, delay=4)),
        ("trend switch tur168, no LP", lambda f: R(f, sig="brk", n=168, hold_h=168, lp=False)),
        ("LP flat, hold winner brk168 k4", lambda f: R(f, sig="brk", n=168, hold_h=168, k=4)),
        ("blend tur168 min 50% k5", lambda f: Bl(f, sig="tur", n=168, th0=0.0, a=0.5, min_lp=0.5, k=5)),
        ("blend tur336 min 25% k5", lambda f: Bl(f, sig="tur", n=336, th0=0.0, a=0.75, min_lp=0.25, k=5)),
    ]


def main(pools=("uni-weth-usdc-5", "uni-weth-usdc-30", "aero-weth-usdc-cl50", "solidly-weth-usdc"), cands=None):
    cands = cands or candidates()
    for pool in pools:
        m = vsim.load_market(vsim.POOLS[pool])
        print(f"\n== {pool}: {len(m.ts):,} swaps, {(m.bar_end[-1] - m.bar_end[0]) / 86400:.0f} days")
        for name, mk in cands:
            a = Adapter(mk, name)
            r = vsim.run(m, a, capital=10_000.0)
            s = vsim.summary(r, 10_000.0)
            print(f"  {name:<34} vs hold {s['vs_hodl_apr']:8.2f}%  fees {s['fees_apr']:7.2f}%  cost {s['cost_apr']:6.2f}%  "
                  f"acts {s['actions']:4d}  in range {s['in_range']:.2f}  deployed {s['deployed']:.2f}", flush=True)


if __name__ == "__main__":
    main(*(sys.argv[1:2] and [tuple(sys.argv[1:])] or []))
