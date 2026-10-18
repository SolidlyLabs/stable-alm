"""Fee-only LP simulator for volatile pairs (any decimals), for fee-capture strategy research.

Income is swap fees only (no emissions): each swap pays amount_in * fee, shared pro rata between
our liquidity and the pool's active liquidity at the swap's tick (pre/post average, as in alm.sim).
Prices are whole-token prices in the numeraire (the stable side); liquidity is in whole-token units
(raw / 10**((d0+d1)/2)), so the V3 formulas hold for mixed decimals.

Scores:
- equity: position + idle tokens + fees - costs, in the numeraire;
- vs_hodl: equity - value of the starting 50/50 inventory held untouched;
- alpha (vs REBAL): fees - costs + sum over holding periods of (position value change - value change of
  simply holding the tokens the position had at the period start). This is what being an LP added,
  independent of the directional exposure the strategy chose.
"""
import glob
import math
import os
from dataclasses import dataclass, field

import numpy as np
import pandas as pd

ROOT = os.path.join(os.path.dirname(__file__), "..", "..")
BAR = 300


@dataclass
class Pool:
    name: str
    address: str
    d0: int
    d1: int
    fee: float  # fraction, e.g. 0.0005
    spacing: int
    numeraire: int  # 1 = token1 is the stable (value in token1); 0 = token0 is
    data_dir: str = None
    hourly_fee: bool = False  # Solidly: dynamic fee, taken per hour from the processor (fees / volume)


POOLS = {
    "uni-weth-usdc-5": Pool("uni-weth-usdc-5", "0xd0b53d9277642d899df5c87a3966a349a798f224", 18, 6, 0.0005, 10, 1),
    "uni-weth-usdc-30": Pool("uni-weth-usdc-30", "0x6c561b446416e1a00e8e93e221854d6ea4171372", 18, 6, 0.003, 60, 1),
    "uni-cbbtc-usdc-5": Pool("uni-cbbtc-usdc-5", "0xfbb6eed8e7aa03b138556eedaf5d271a5e1e43ef", 6, 8, 0.0005, 10, 0),
    "aero-weth-usdc-cl50": Pool("aero-weth-usdc-cl50", "0xaad23a67f2ac693abbe543489aeb3f24f561d517", 18, 6, 0.0005, 50, 1),
    "solidly-weth-usdc": Pool("solidly-weth-usdc", "0x551a0e3d267bea87048f08cc94cc6035ad99221b", 18, 6, 0.001, 10, 1, hourly_fee=True),
    "solidly-usdc-usdt": Pool("solidly-usdc-usdt", "0x9ab7730b09ebd9ff2df70f06339bd289a1680a46", 6, 6, 0.0001, 1, 0, hourly_fee=True),
    "solidly-cbeth-weth": Pool("solidly-cbeth-weth", "0x74ad64bac8df96af2b63ea50c3d8840b79aa7a3b", 18, 18, 0.0001, 1, 1, hourly_fee=True),
}


CACHE = os.path.join(os.path.dirname(__file__), "cache")
ARR = ("ts", "tick", "tick_pre", "p", "sp", "L", "fee", "vol", "dt", "bar_end", "bar_idx", "bar_tick", "bar_lp", "bar_r2")


def load_market(pool: Pool) -> "Market":
    """Market from the array cache (built once from the events), so worker processes stay small."""
    f = os.path.join(CACHE, pool.name + ".npz")
    if not os.path.exists(f):
        m = Market(pool)
        os.makedirs(CACHE, exist_ok=True)
        np.savez(f, **{k: getattr(m, k) for k in ARR})
        return m
    m = object.__new__(Market)
    m.pool = pool
    z = np.load(f)
    for k in ARR:
        setattr(m, k, z[k])
    return m


class Market:
    def __init__(self, pool: Pool, events: pd.DataFrame = None, since_ts: int = None):
        self.pool = pool
        if events is None:
            d = pool.data_dir or os.path.join(ROOT, "data", "raw", pool.address)
            files = sorted(glob.glob(os.path.join(d, "events_*.parquet")))
            files = [f for f in files if int(os.path.basename(f)[7:-8]) >= 48_000_000]  # the 90-day research window
            parts = [pd.read_parquet(f) for f in files if os.path.getsize(f) > 1000]
            events = pd.concat(parts, ignore_index=True).drop_duplicates(["block", "log_index"])
        sw = events[events.kind == "swap"].sort_values(["block", "log_index"])
        if since_ts:
            sw = sw[sw.ts >= since_ts]
        self.ts = sw.ts.to_numpy(np.int64)
        self.tick = sw.tick.to_numpy(np.int64)
        self.tick_pre = np.concatenate([[self.tick[0]], self.tick[:-1]])
        dd = pool.d0 - pool.d1
        # Whole-token price of token0 in token1, and sqrt.
        self.p = np.power(1.0001, self.tick.astype(float)) * 10.0**dd
        self.sp = np.sqrt(self.p)
        lsc = 10.0 ** ((pool.d0 + pool.d1) / 2)
        self.L = sw.liquidity.astype(float).to_numpy() / lsc
        a0 = sw.amount0.astype(float).to_numpy() / 10.0**pool.d0
        a1 = sw.amount1.astype(float).to_numpy() / 10.0**pool.d1
        in0, in1 = np.clip(a0, 0, None), np.clip(a1, 0, None)
        # Fee in numeraire units.
        if pool.numeraire == 1:
            self.fee = (in0 * self.p + in1) * pool.fee
        else:
            self.fee = (in0 + in1 / self.p) * pool.fee
        self.vol = self.fee / pool.fee
        if pool.hourly_fee:
            d = pool.data_dir or os.path.join(ROOT, "data", "raw", pool.address)
            apr = pd.read_parquet(os.path.join(d, "apr_hourly.parquet")).drop_duplicates("timestamp").set_index("timestamp")
            rate = (apr["feesUSD"] / apr["volumeUSD"].replace(0, np.nan)).clip(0, 0.01)
            hour = (self.ts // 3600) * 3600
            r = rate.reindex(hour).to_numpy()
            r = np.where(np.isnan(r), pool.fee, r)
            self.fee = self.vol * r
        self.dt = np.diff(np.concatenate([self.ts, [self.ts[-1]]])).astype(float)
        t0 = (self.ts[0] // BAR + 1) * BAR
        self.bar_end = np.arange(t0, self.ts[-1], BAR)
        self.bar_idx = np.searchsorted(self.ts, self.bar_end, side="right") - 1
        self.bar_tick = self.tick[self.bar_idx]
        self.bar_lp = np.log(self.p[self.bar_idx])
        r = np.diff(self.bar_lp, prepend=self.bar_lp[0])
        self.bar_r2 = r * r

    def price_of_tick(self, t):
        return 1.0001 ** np.asarray(t, dtype=float) * 10.0 ** (self.pool.d0 - self.pool.d1)

    def value(self, x, y, p):
        """Value of x token0 + y token1 in the numeraire at price p (token1 per token0)."""
        return x * p + y if self.pool.numeraire == 1 else x + y / p


def amounts(L, p, pa, pb):
    """Whole-token amounts of liquidity L on [pa, pb) at price p."""
    sp, sa, sb = math.sqrt(p), math.sqrt(pa), math.sqrt(pb)
    if sp <= sa:
        return L * (1 / sa - 1 / sb), 0.0
    if sp >= sb:
        return 0.0, L * (sb - sa)
    return L * (1 / sp - 1 / sb), L * (sp - sa)


@dataclass
class Costs:
    swap_bps: float = 5.0  # aggregator cost + impact on swapped notional
    gas: float = 0.02  # per transaction, in the numeraire (USD)


@dataclass
class St:
    b: int
    i: int
    tick: int
    p: float
    ranges: list  # (tl, tu, L)
    x: float
    y: float
    m: object = None
    last_action: int = -10**9
    out_since: int = None


@dataclass
class Res:
    label: str
    days: np.ndarray
    equity: np.ndarray
    hodl: np.ndarray
    alpha: np.ndarray  # cumulative alpha vs REBAL at day marks
    fees: float
    costs: float
    actions: int
    in_range: float
    deployed: float  # time-average share of capital in positions
    extra: dict = field(default_factory=dict)


def _pos_amounts(m, ranges, p):
    x = y = 0.0
    for tl, tu, L in ranges:
        a, b = amounts(L, p, m.price_of_tick(tl), m.price_of_tick(tu))
        x += a
        y += b
    return x, y


def _income(m, i0, i1, ranges):
    if i1 <= i0 or not ranges:
        return 0.0, 0.0
    sl = slice(i0 + 1, i1 + 1)
    tp, tq = m.tick[sl], m.tick_pre[sl]
    o_post = np.zeros(i1 - i0)
    o_pre = np.zeros(i1 - i0)
    for tl, tu, L in ranges:
        o_post += L * ((tp >= tl) & (tp < tu))
        o_pre += L * ((tq >= tl) & (tq < tu))
    ours = 0.5 * (o_post + o_pre)
    share = np.where(ours > 0, ours / (m.L[sl] + ours + 1e-18), 0.0)
    return float(np.dot(m.fee[sl], share)), float(np.dot(m.dt[sl], o_post > 0))


def mint_plan(m, targets, V, p):
    """Liquidity per target range (tl, tu, weight) for total value V at price p, plus the token mix."""
    out, x, y = [], 0.0, 0.0
    w = np.array([t[2] for t in targets], float)
    w = w / w.sum()
    for (tl, tu, _), wi in zip(targets, w):
        a, b = amounts(1.0, p, m.price_of_tick(tl), m.price_of_tick(tu))
        v1 = m.value(a, b, p)
        L = wi * V / v1 if v1 > 0 else 0.0
        out.append((tl, tu, L))
        x += a * L
        y += b * L
    return out, x, y


def run(m: Market, strat, capital: float = 10_000.0, costs: Costs = None, start_bar: int = 0, end_bar: int = None) -> Res:
    costs = costs or Costs()
    end_bar = len(m.bar_end) if end_bar is None else end_bar
    b0 = start_bar
    i = int(m.bar_idx[b0])
    p = float(m.p[i])
    # Start 50/50 in value.
    if m.pool.numeraire == 1:
        x, y = capital / 2 / p, capital / 2
    else:
        x, y = capital / 2, capital / 2 * p
    hx, hy = x, y
    st = St(b=b0, i=i, tick=int(m.tick[i]), p=p, ranges=[], x=x, y=y, m=m)
    fees = cost = 0.0
    alpha = 0.0
    seg_pos_v0 = 0.0  # value of the position at the segment start (at the then price)
    seg_tok = (0.0, 0.0)  # its tokens at the segment start
    t_in = t_tot = dep = 0.0
    actions = 0
    days, eq, ho, al = [], [], [], []
    last_i = i
    next_day = (int(m.bar_end[b0]) // 86400 + 1) * 86400
    strat.prepare(m)
    strat.reset()

    def equity(pp):
        px, py = _pos_amounts(m, st.ranges, pp)
        return m.value(px + st.x, py + st.y, pp) + fees - cost

    def seg_alpha(pp):
        px, py = _pos_amounts(m, st.ranges, pp)
        return m.value(px, py, pp) - m.value(seg_tok[0], seg_tok[1], pp)

    for b in range(b0, end_bar):
        i = int(m.bar_idx[b])
        if i > last_i:
            f, a = _income(m, last_i, i, st.ranges)
            fees += f
            t_in += a
            dt = float(m.ts[i] - m.ts[last_i])
            t_tot += dt
            if st.ranges:
                px, py = _pos_amounts(m, st.ranges, st.p)
                pv = m.value(px, py, st.p)
                dep += dt * pv / max(pv + m.value(st.x, st.y, st.p), 1e-9)
            last_i = i
        st.b, st.i, st.tick, st.p = b, i, int(m.tick[i]), float(m.p[i])
        inr = any(tl <= st.tick < tu for tl, tu, _ in st.ranges)
        if inr or not st.ranges:
            st.out_since = None
        elif st.out_since is None:
            st.out_since = b
        tgt = strat.decide(st)
        if tgt is not None:
            pp = st.p
            alpha += seg_alpha(pp)
            px, py = _pos_amounts(m, st.ranges, pp)
            X, Y = st.x + px, st.y + py
            n_tx = (1 if st.ranges else 0)
            c = 0.0
            if tgt:  # list of (tl, tu, weight, [share of capital]) ; share defaults to 1
                share = getattr(strat, "deploy_share", 1.0)
                V = m.value(X, Y, pp) * share
                ranges, nx, ny = mint_plan(m, tgt, V, pp)
                # Idle after minting; a negative side is bought with the other token at the pool price.
                dx, dy = X - nx, Y - ny
                swap_val = 0.0
                if dx < 0:
                    swap_val = m.value(-dx, 0.0, pp)
                    dy -= -dx * pp
                    dx = 0.0
                elif dy < 0:
                    swap_val = m.value(0.0, -dy, pp)
                    dx -= -dy / pp
                    dy = 0.0
                c += swap_val * costs.swap_bps / 1e4 + (costs.gas if swap_val > 0 else 0.0)
                n_tx += len(ranges)
                st.x, st.y = max(dx, 0.0), max(dy, 0.0)
                st.ranges = ranges
            else:
                st.x, st.y, st.ranges = X, Y, []
            # idle tokens held in a chosen mix (directional strategies wait part of the capital in a token)
            w0 = getattr(strat, "idle_w0", None)
            if w0 is not None:
                Vi = m.value(st.x, st.y, pp)
                want_x = w0 * Vi / pp if m.pool.numeraire == 1 else w0 * Vi
                sv = abs(m.value(st.x - want_x, 0.0, pp))
                if sv > 1e-9:
                    ci = sv * costs.swap_bps / 1e4 + costs.gas
                    c += ci  # booked in `cost` (equity subtracts it), not taken from the tokens
                    st.x = w0 * Vi / pp if m.pool.numeraire == 1 else w0 * Vi
                    st.y = (1 - w0) * Vi if m.pool.numeraire == 1 else (1 - w0) * Vi * pp
            c += n_tx * costs.gas
            cost += c
            alpha -= c
            seg_tok = _pos_amounts(m, st.ranges, pp)
            actions += 1
            st.last_action = b
            st.out_since = None
        if m.bar_end[b] >= next_day or b == end_bar - 1:
            days.append(int(m.bar_end[b]))
            eq.append(equity(st.p))
            ho.append(m.value(hx, hy, st.p))
            al.append(alpha + seg_alpha(st.p) + fees)
            next_day += 86400
    return Res(strat.label(), np.array(days), np.array(eq), np.array(ho), np.array(al), fees, cost, actions,
               t_in / t_tot if t_tot else 0.0, dep / t_tot if t_tot else 0.0)


def summary(r: Res, capital: float) -> dict:
    d = (r.days[-1] - r.days[0]) / 86400 if len(r.days) > 1 else 1
    k = 365 / max(d, 1e-9) / capital * 100
    return {"label": r.label, "days": round(d, 1), "alpha_apr": r.alpha[-1] * k, "fees_apr": r.fees * k,
            "cost_apr": r.costs * k, "vs_hodl_apr": (r.equity[-1] - r.hodl[-1]) * k,
            "pnl_apr": (r.equity[-1] - capital) * k, "actions": r.actions, "in_range": r.in_range, "deployed": r.deployed}
