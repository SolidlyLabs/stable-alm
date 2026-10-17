"""Hourly pool history from the Solidly data processor (via solidly.com's public proxy), plus a
daily on-chain read of the pool's fee, which Solidly V3 keeps in slot0 and can change.

Output:
  data/raw/<pool>/apr_hourly.parquet  volume, fees, emissions, TVL, active TVL per hour
  data/raw/<pool>/fee_daily.parquet   slot0 fee (pips) sampled once a day
"""
import json
import os
import sys
import urllib.request

import pandas as pd

from ..paths import DATA
from ..rpc import BASE_GENESIS_TS, base_block_ts, batch, call

API = "https://solidly.com/api/getPoolAprHistory/?chainId={chain}&poolId={pool}&interval=hour&limit=2400&offset={offset}"


def get(url: str):
    req = urllib.request.Request(url, headers={"User-Agent": "stable-alm/0.1"})
    return json.load(urllib.request.urlopen(req, timeout=120))


def apr_history(pool: str, chain: int = 8453) -> pd.DataFrame:
    rows, offset = [], 0
    while True:
        h = get(API.format(chain=chain, pool=pool, offset=offset))["history"]
        if not h:
            break
        rows += h
        offset += len(h)
        if len(h) < 2400:
            break
    df = pd.DataFrame(rows)
    df["timestamp"] = df["timestamp"].astype(int)
    for c in ("tvlUSD", "activeTvlUSD", "volumeUSD", "feesUSD", "emissionsUSD", "bribesUSD", "transactionCount"):
        df[c] = df[c].astype(float)
    return df.drop_duplicates("timestamp").sort_values("timestamp").reset_index(drop=True)


def fee_daily(pool: str, start_block: int, end_block: int) -> pd.DataFrame:
    blocks = list(range(start_block, end_block, 43_200)) + [end_block]
    rows = []
    for i in range(0, len(blocks), 40):
        part = blocks[i : i + 40]
        res = batch([("eth_call", [{"to": pool, "data": "0x3850c7bd"}, hex(b)]) for b in part])
        for b, r in zip(part, res):
            if isinstance(r, str) and len(r) >= 2 + 64 * 3:
                rows.append({"block": b, "ts": base_block_ts(b), "fee_pips": int(r[2 + 128 : 2 + 192], 16)})
    return pd.DataFrame(rows)


def main() -> None:
    pool = sys.argv[1].lower()
    start_block = int(sys.argv[2])
    out_dir = os.path.join(DATA, "raw", pool)
    os.makedirs(out_dir, exist_ok=True)
    apr = apr_history(pool)
    apr.to_parquet(os.path.join(out_dir, "apr_hourly.parquet"), index=False)
    print("apr hours", len(apr), pd.to_datetime(apr.timestamp.min(), unit="s"), "->", pd.to_datetime(apr.timestamp.max(), unit="s"))
    head = int(call("eth_blockNumber", []), 16)
    fees = fee_daily(pool, start_block, head)
    fees.to_parquet(os.path.join(out_dir, "fee_daily.parquet"), index=False)
    print("fee samples", len(fees), fees.fee_pips.value_counts().to_dict())
    assert BASE_GENESIS_TS


if __name__ == "__main__":
    main()
