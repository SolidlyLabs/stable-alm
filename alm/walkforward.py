"""Walk-forward selection over a grid of strategy configs, plus an overfitting check.

Every config is simulated once over the whole history, giving a daily PnL series. Walk-forward then
steps through time: on each train window it picks the config with the best (smoothed) train PnL and
books that config's PnL over the next test window. Only the test windows count, so every booked
day was chosen using data strictly before it. Switching to a different config costs one rebalance.

CSCV (Bailey et al., "The probability of backtest overfitting"): split days into S blocks, and for
every half/half split pick the in-sample best config and see where it ranks out of sample.
PBO = share of splits where the in-sample winner falls in the bottom half out of sample.
"""
from itertools import combinations

import numpy as np
import pandas as pd


def daily_pnl(results: list, field: str = "equity") -> pd.DataFrame:
    cols = {}
    for r in results:
        s = pd.Series(np.diff(getattr(r, field)), index=pd.to_datetime(r.day_ts[1:], unit="s").floor("D"))
        cols[r.label] = s.groupby(level=0).sum()
    return pd.DataFrame(cols).fillna(0.0)


def walk_forward(pnl: pd.DataFrame, train_days: int, test_days: int, switch_cost: float, families: dict = None,
                 smooth: int = 0, exclude: tuple = (), top_n: int = 1) -> dict:
    """families: optional {config: family}; smooth>0 scores a config by the mean train PnL of the
    `smooth` configs nearest to it in train-PnL rank within its family (plateau, not peak)."""
    days = pnl.index
    booked = []
    chosen = []
    prev = None
    start = train_days
    while start < len(days):
        train = pnl.iloc[start - train_days : start]
        test = pnl.iloc[start : start + test_days]
        score = train.sum().drop(list(exclude), errors="ignore")
        if smooth and families:
            fam = pd.Series(families)
            sm = {}
            for f, cfgs in fam.groupby(fam).groups.items():
                s = score.reindex(list(cfgs)).dropna().sort_values()
                roll = s.rolling(smooth, center=True, min_periods=1).mean()
                sm.update(roll.to_dict())
            score = pd.Series(sm)
        if top_n > 1:
            # Equal split across the top-N train configs; a switch costs a share of a rebalance.
            picks = list(score.sort_values(ascending=False).index[:top_n])
            pick = "|".join(sorted(picks))
            day_pnl = test[picks].mean(axis=1)
            if prev is not None:
                changed = len(set(picks) - set(prev.split("|")))
                day_pnl.iloc[0] -= switch_cost * changed / top_n
        else:
            pick = score.idxmax()
            day_pnl = test[pick].copy()
            if prev is not None and pick != prev:
                day_pnl.iloc[0] -= switch_cost
        booked.append(day_pnl)
        chosen.append({"from": test.index[0], "to": test.index[-1], "config": pick,
                       "train_pnl": float(train[pick.split("|")].sum().mean()),
                       "test_pnl": float(day_pnl.sum())})
        prev = pick
        start += test_days
    oos = pd.concat(booked)
    return {"oos": oos, "chosen": pd.DataFrame(chosen)}


def pbo(pnl: pd.DataFrame, blocks: int = 10) -> float:
    n = len(pnl) // blocks * blocks
    mat = pnl.iloc[-n:].to_numpy()
    parts = np.array_split(np.arange(n), blocks)
    worse = total = 0
    for is_blocks in combinations(range(blocks), blocks // 2):
        is_idx = np.concatenate([parts[i] for i in is_blocks])
        oos_idx = np.concatenate([parts[i] for i in range(blocks) if i not in is_blocks])
        best = mat[is_idx].sum(0).argmax()
        oos = mat[oos_idx].sum(0)
        rank = (oos < oos[best]).mean()
        worse += rank < 0.5
        total += 1
    return worse / total


def summarize(series: pd.Series, capital: float) -> dict:
    eq = series.cumsum()
    dd = (eq - eq.cummax()).min()
    days = len(series)
    return {
        "pnl": float(series.sum()),
        "apr_pct": float(series.sum() / capital * 365 / max(days, 1) * 100),
        "max_drawdown": float(dd),
        "worst_day": float(series.min()),
        "days": days,
        "positive_days_pct": float((series > 0).mean() * 100),
    }
