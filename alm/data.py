"""Build the replay market from downloaded pool events and processor history.

The replay keeps the real price path fixed and asks, swap by swap: if our liquidity had been in the
pool instead of the wallet's real positions, what share of that swap's fee would it have earned?
That needs, per swap: the tick before and after, the pool's active liquidity at the tick *excluding*
the wallet's own historical positions, and the fee paid.

Fees: the pool fee is dynamic (0.04-2.4 bp), so each swap's fee is its input amount times that
hour's realised fee rate from the processor (feesUSD / volumeUSD).
Emissions: the processor's hourly emissionsUSD, spread uniformly over the hour; a position earns the
fraction of it equal to its share of in-range liquidity for the time it holds that share.
"""
import glob
import os

import numpy as np
import pandas as pd

from .paths import DATA  # noqa: E402
POOL = "0x9ab7730b09ebd9ff2df70f06339bd289a1680a46"
WALLET = os.environ.get("ALM_WALLET", "").lower()  # your LP wallet: its own positions are left out of "others' liquidity"


SEGMENT = 1_000_000


def segment_files(pool: str) -> list:
    """(segment start block, path) of the pool's event segments, oldest first."""
    out = []
    for f in glob.glob(os.path.join(DATA, "raw", pool.lower(), "events_*.parquet")):
        tag = os.path.basename(f)[7:-8]
        if tag.isdigit():
            out.append((int(tag), f))
    return sorted(out)


def wallet_events_path(pool: str, wallet: str) -> str:
    return os.path.join(DATA, "raw", pool.lower(), f"wallet_{wallet.lower()}.parquet")


def load_events(pool: str = POOL, since_block: int = None, wallet: str = None) -> pd.DataFrame:
    """All stored events, or only segments overlapping [since_block, now) plus the wallet's own
    Mint/Burn history (data/raw/<pool>/wallet_<addr>.parquet), which the replay needs in full."""
    files = [f for seg, f in segment_files(pool) if since_block is None or seg + SEGMENT > since_block]
    if wallet and since_block is not None and os.path.exists(wallet_events_path(pool, wallet)):
        files.append(wallet_events_path(pool, wallet))
    parts = [pd.read_parquet(f) for f in files if os.path.getsize(f) > 1000]
    if not parts:
        raise FileNotFoundError(f"no event data for pool {pool}; run the data bootstrap first")
    df = pd.concat(parts, ignore_index=True)
    if since_block is not None:
        df = df[(df.block >= since_block) | (df.kind != "swap")]
    df = df.drop_duplicates(["block", "log_index"])
    return df.sort_values(["block", "log_index"]).reset_index(drop=True)


def wallet_liquidity_at_tick(events: pd.DataFrame, wallet: str, swaps_idx: np.ndarray, ticks: np.ndarray) -> np.ndarray:
    """Liquidity the wallet itself had active at each swap's post tick."""
    lp = events[(events.kind != "swap") & (events.owner == wallet)]
    out = np.zeros(len(swaps_idx))
    if lp.empty:
        return out
    positions: dict = {}
    lp_rows = list(lp.itertuples())
    j = 0
    # Walk swaps in order, applying wallet mints/burns that precede each swap.
    for k, (ev_i, t) in enumerate(zip(swaps_idx, ticks)):
        while j < len(lp_rows) and lp_rows[j].Index < ev_i:
            r = lp_rows[j]
            key = (r.tick_lower, r.tick_upper)
            sign = 1 if r.kind == "mint" else -1
            positions[key] = positions.get(key, 0.0) + sign * float(r.amount)
            j += 1
        out[k] = sum(v for (tl, tu), v in positions.items() if tl <= t < tu and v > 0)
    return out


class Market:
    """Swap-level replay arrays plus 5-minute decision bars."""

    def __init__(self, pool: str = POOL, wallet: str = WALLET, bar_seconds: int = 300, decimals: int = 6,
                 numeraire: int = 0, usd_per_numeraire=None, fee_floor_bps: float = 0.0, events: pd.DataFrame = None,
                 decimals1: int = None):
        """decimals: token0's decimals; decimals1: token1's (default: the same). Prices and liquidity
        stay in raw on-chain units; px_scale = 10**(d0-d1) turns a raw price into a whole-token price
        and liq_scale = 10**((d0+d1)/2) turns raw liquidity into whole-token liquidity, so mixed-decimal
        pairs (WETH 18 / USDC 6) are valued correctly. numeraire: 0 = value everything
        in token0, 1 = in token1. usd_per_numeraire: optional hourly Series (index = hour ts) to turn
        the processor's USD emissions into numeraire units; None means the numeraire is a USD stable."""
        ev = load_events(pool) if events is None else events
        d0, d1 = decimals, (decimals if decimals1 is None else decimals1)
        self.decimals, self.decimals1, self.numeraire = d0, d1, numeraire
        self.px_scale = 10.0 ** (d0 - d1)
        self.liq_scale = 10.0 ** ((d0 + d1) / 2)
        sw = ev[ev.kind == "swap"]
        idx = sw.index.to_numpy()
        self.ts = sw.ts.to_numpy(dtype=np.int64)
        raw_tick = sw.tick.to_numpy(dtype=np.int64)
        raw_sp = np.array([int(s) for s in sw.sqrt_price_x96], dtype=float) / 2**96
        # A near-empty pool lets a swap run the price to absurd ticks (down to -887272). That is
        # not a market price, and any real liquidity would have absorbed the swap, so swaps more
        # than 20 ticks from the rolling median are pinned to the median.
        med = pd.Series(raw_tick).rolling(51, center=True, min_periods=1).median().round().to_numpy()
        bad = np.abs(raw_tick - med) > 20
        self.tick = np.where(bad, med, raw_tick).astype(np.int64)
        self.sp = np.where(bad, np.power(1.0001, (self.tick + 0.5) / 2), raw_sp)
        self.n_outliers = int(bad.sum())
        l_active = np.array([float(x) for x in sw.liquidity])
        self.tick_pre = np.concatenate([[self.tick[0]], self.tick[:-1]])
        a0 = sw.amount0.astype(float).to_numpy() / 10.0**d0
        a1 = sw.amount1.astype(float).to_numpy() / 10.0**d1
        p = self.sp**2 * self.px_scale  # whole token1 per whole token0
        # Swap input in numeraire units (the fee rate below is dimensionless).
        in0 = np.where(a0 > 0, a0, 0.0)
        in1 = np.where(a1 > 0, a1, 0.0)
        self.amount_in = in0 + in1 / p if numeraire == 0 else in0 * p + in1
        l_wallet = wallet_liquidity_at_tick(ev, wallet, idx, self.tick)
        # Raw liquidity is in sqrt(wei0*wei1); / 10**((d0+d1)/2) gives whole-token liquidity
        # (= / 10**decimals for equal decimals, the units pool_math uses).
        self.l_wallet = l_wallet / self.liq_scale
        self.l_others = np.clip(l_active - l_wallet, 0.0, None) / self.liq_scale

        apr_path = os.path.join(DATA, "raw", pool, "apr_hourly.parquet")
        apr = pd.read_parquet(apr_path) if os.path.exists(apr_path) else pd.DataFrame(
            {"timestamp": [], "volumeUSD": [], "feesUSD": [], "emissionsUSD": []})
        hour = (self.ts // 3600) * 3600
        apr = apr.set_index("timestamp")
        # Dimensionless fee rate per hour = processor fees / processor volume (both USD).
        vol_usd = apr["volumeUSD"].reindex(np.unique(hour))
        rate_h = (apr["feesUSD"].reindex(np.unique(hour)) / vol_usd.replace(0, np.nan)).fillna(0.0).clip(0, 0.01)
        # "New higher fees" scenario: every hour pays at least the pool's new fee level, volume unchanged.
        rate_h = rate_h.clip(lower=fee_floor_bps / 1e4)
        self.fee = self.amount_in * rate_h.reindex(hour).to_numpy()
        # Emission accrual: USD per second for the hour each inter-swap interval starts in.
        emis_h = apr["emissionsUSD"].reindex(np.unique(hour)).fillna(0.0)
        if usd_per_numeraire is not None:
            emis_h = emis_h / usd_per_numeraire.reindex(emis_h.index).ffill().bfill()
        self.emis_rate = emis_h.reindex(hour).to_numpy() / 3600.0
        self.dt = np.diff(np.concatenate([self.ts, [self.ts[-1]]])).astype(float)
        self.apr = apr

        # Decision bars: index of the last swap at or before each bar end; tick carried forward.
        t0 = (self.ts[0] // bar_seconds + 1) * bar_seconds
        self.bar_end = np.arange(t0, self.ts[-1], bar_seconds)
        self.bar_idx = np.searchsorted(self.ts, self.bar_end, side="right") - 1
        self.bar_tick = self.tick[self.bar_idx]
        # Per-bar high/low over the swaps inside the bar (carried when no swaps).
        hi = pd.Series(self.tick).groupby(np.searchsorted(self.bar_end, self.ts, side="left")).max()
        lo = pd.Series(self.tick).groupby(np.searchsorted(self.bar_end, self.ts, side="left")).min()
        n = len(self.bar_end)
        self.bar_high = hi.reindex(range(n)).ffill().fillna(self.tick[0]).to_numpy()
        self.bar_low = lo.reindex(range(n)).ffill().fillna(self.tick[0]).to_numpy()
        self.bar_seconds = bar_seconds

    def slice_from(self, start_ts: int) -> "Market":
        """View of the market from start_ts on (shares arrays)."""
        m = object.__new__(Market)
        i = int(np.searchsorted(self.ts, start_ts))
        b = int(np.searchsorted(self.bar_end, start_ts))
        for k in ("ts", "tick", "sp", "tick_pre", "amount_in", "l_wallet", "l_others", "fee", "emis_rate", "dt"):
            setattr(m, k, getattr(self, k)[i:])
        m.bar_end = self.bar_end[b:]
        m.bar_idx = self.bar_idx[b:] - i
        m.bar_tick, m.bar_high, m.bar_low = self.bar_tick[b:], self.bar_high[b:], self.bar_low[b:]
        m.bar_seconds, m.apr = self.bar_seconds, self.apr
        m.decimals, m.numeraire = getattr(self, "decimals", 6), getattr(self, "numeraire", 0)
        m.decimals1 = getattr(self, "decimals1", m.decimals)
        m.px_scale = getattr(self, "px_scale", 1.0)
        m.liq_scale = getattr(self, "liq_scale", 10.0**m.decimals)
        return m


ARRAYS = ("ts", "tick", "sp", "tick_pre", "amount_in", "l_wallet", "l_others", "fee", "emis_rate", "dt",
          "bar_end", "bar_idx", "bar_tick", "bar_high", "bar_low")


# Backtest pools: equal-decimal correlated pairs on Solidly V3 Base.
POOLS = {
    "usdc-usdt": {"pool": POOL, "decimals": 6, "numeraire": 0, "capital": 15_000.0, "gas": 0.02, "fee_floor_bps": 0.0987},
    "cbeth-weth": {"pool": "0x74ad64bac8df96af2b63ea50c3d8840b79aa7a3b", "decimals": 18, "numeraire": 1,
                   "capital": 6.0, "gas": 0.02 / 2500, "eth": True, "fee_floor_bps": 0.44},
    "usdc-usdbc": {"pool": "0x7a8d53466144e1f75de2e70ec908e18f771b49fb", "decimals": 6, "numeraire": 0,
                   "capital": 15_000.0, "gas": 0.02, "fee_floor_bps": 0.06},
}
# Fee floors = each pool's realised fee level since the Oct 13 2026 fee increase (user's scenario:
# "assume the new higher fees will be used"). Set FEE_FLOOR_OVERRIDE (bp) to test another level.


def eth_usd_hourly() -> pd.Series:
    df = pd.read_parquet(os.path.join(DATA, "raw", "binance_ethusdt_1h.parquet"))
    return df.set_index("ts")["eth_usd"]


def cached_market(pool: str = POOL, rebuild: bool = False, bar_seconds: int = 300, fee_floor_bps: float = None) -> Market:
    """Market arrays cached as .npz, so worker processes skip the raw-event parsing."""
    cfg = next((c for c in POOLS.values() if c["pool"] == pool), {"decimals": 6, "numeraire": 0})
    if fee_floor_bps is None:
        env = os.environ.get("FEE_FLOOR_OVERRIDE")
        fee_floor_bps = float(env) if env else cfg.get("fee_floor_bps", 0.0)
    suffix = ("" if bar_seconds == 300 else f"_{bar_seconds}s") + (f"_floor{fee_floor_bps:g}" if fee_floor_bps else "")
    path = os.path.join(DATA, f"market_{pool}{suffix}.npz")
    if rebuild or not os.path.exists(path):
        m = Market(pool, bar_seconds=bar_seconds, decimals=cfg["decimals"], numeraire=cfg["numeraire"],
                   usd_per_numeraire=eth_usd_hourly() if cfg.get("eth") else None, fee_floor_bps=fee_floor_bps)
        np.savez(path, bar_seconds=m.bar_seconds, decimals=m.decimals, numeraire=m.numeraire,
                 **{k: getattr(m, k) for k in ARRAYS})
        return m
    z = np.load(path)
    m = object.__new__(Market)
    for k in ARRAYS:
        setattr(m, k, z[k])
    m.bar_seconds = int(z["bar_seconds"])
    m.decimals = int(z["decimals"]) if "decimals" in z else 6
    m.numeraire = int(z["numeraire"]) if "numeraire" in z else 0
    m.decimals1, m.px_scale, m.liq_scale = m.decimals, 1.0, 10.0**m.decimals  # backtest pools: equal decimals
    m.apr = None
    return m


def live_market(pool: str, wallet: str, decimals: int, numeraire: int = 0, days: int = 10, decimals1: int = None) -> Market:
    """Market over the last `days` for the live executor: real fees (no scenario floor), recent
    segments only plus the wallet's full Mint/Burn history."""
    from .rpc import call

    head = int(call("eth_blockNumber", []), 16)
    since = head - days * 43_200
    ev = load_events(pool.lower(), since_block=since, wallet=wallet)
    return Market(pool.lower(), wallet=wallet.lower(), decimals=decimals, numeraire=numeraire, events=ev, decimals1=decimals1)
