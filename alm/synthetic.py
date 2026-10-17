"""A Market built from Binance USDCUSDT minute candles instead of pool swaps.

The on-chain pool only has ~21 months of history and no depeg. Binance has 2018-now, including the
March 2023 USDC depeg (to ~0.87) and long stretches of drift, so it stresses width and rebalance
rules on far more price paths. It cannot say what fees a pool earned, so the fee flow is calibrated
from the real pool:

- one pseudo-swap per minute at the minute's close tick (pre-tick = open);
- pool fee per minute = calibrated median fee/minute x (Binance quote volume / its rolling 30-day
  median, clipped to [0.2, 5]) — busier minutes pay more, as on-chain;
- competing liquidity at the current tick = calibrated median of the pool's others-liquidity;
- emissions per second = calibrated mean.

Treat results as a robustness test of the *rules*, not a forecast of income.
"""
import os

import numpy as np
import pandas as pd

from .data import DATA


class SyntheticMarket:
    def __init__(self, fee_per_min: float, l_others: float, emis_per_sec: float, start: str = None, end: str = None,
                 bar_seconds: int = 300):
        k = pd.read_parquet(os.path.join(DATA, "raw", "binance_usdcusdt_1m.parquet"))
        if start:
            k = k[k.open_time >= pd.Timestamp(start).timestamp() * 1000]
        if end:
            k = k[k.open_time < pd.Timestamp(end).timestamp() * 1000]
        k = k.reset_index(drop=True)
        ln = np.log(1.0001)
        self.ts = (k.open_time.to_numpy() // 1000 + 60).astype(np.int64)
        self.tick = np.floor(np.log(k.close.to_numpy()) / ln).astype(np.int64)
        self.tick_pre = np.floor(np.log(k.open.to_numpy()) / ln).astype(np.int64)
        self.sp = np.sqrt(k.close.to_numpy())
        qv = k.quote_volume.astype(float)
        act = (qv / qv.rolling(43200, min_periods=1440).median().bfill()).clip(0.2, 5).fillna(1.0).to_numpy()
        self.fee = fee_per_min * act
        self.amount_in = self.fee.copy()
        self.l_others = np.full(len(k), l_others)
        self.l_wallet = np.zeros(len(k))
        self.emis_rate = np.full(len(k), emis_per_sec)
        self.dt = np.diff(np.concatenate([self.ts, [self.ts[-1]]])).astype(float)
        t0 = (self.ts[0] // bar_seconds + 1) * bar_seconds
        self.bar_end = np.arange(t0, self.ts[-1], bar_seconds)
        self.bar_idx = np.searchsorted(self.ts, self.bar_end, side="right") - 1
        self.bar_tick = self.tick[self.bar_idx]
        grp = np.searchsorted(self.bar_end, self.ts, side="left")
        hi = pd.Series(np.floor(np.log(k.high.to_numpy()) / ln)).groupby(grp).max()
        lo = pd.Series(np.floor(np.log(k.low.to_numpy()) / ln)).groupby(grp).min()
        n = len(self.bar_end)
        self.bar_high = hi.reindex(range(n)).ffill().bfill().to_numpy()
        self.bar_low = lo.reindex(range(n)).ffill().bfill().to_numpy()
        self.bar_seconds = bar_seconds
        self.apr = None


def calibrate(market, since: str = "2025-01-17") -> dict:
    """Median fee/minute, median others-liquidity and mean emissions/s from the on-chain market."""
    t0 = pd.Timestamp(since).timestamp()
    sel = market.ts >= t0
    minutes = pd.Series(market.fee[sel]).groupby(market.ts[sel] // 60).sum()
    span_min = (market.ts[sel][-1] - market.ts[sel][0]) / 60
    return {
        "fee_per_min": float(minutes.sum() / span_min),
        "l_others": float(np.median(market.l_others[sel])),
        "emis_per_sec": float((market.emis_rate[sel] * market.dt[sel]).sum() / (market.ts[sel][-1] - market.ts[sel][0])),
    }
