"""Binance spot USDCUSDT 1-minute klines, all history, resumable.

The pegged pair trades on CEXes far longer than any on-chain pool exists, so this gives years of
price paths (including the March 2023 USDC depeg) to stress range strategies on.
Output: data/raw/binance_usdcusdt_1m.parquet
"""
import json
import os
import time
import urllib.error
import urllib.request

import pandas as pd

OUT = os.path.join(os.path.dirname(__file__), "..", "data", "raw", "binance_usdcusdt_1m.parquet")
URLS = ["https://api.binance.com/api/v3/klines", "https://data-api.binance.vision/api/v3/klines"]
START_MS = 1544572800000  # 2018-12-12, before USDCUSDT listing


def get(params: str):
    for url in URLS:
        try:
            req = urllib.request.Request(f"{url}?{params}", headers={"User-Agent": "stable-alm/0.1"})
            return json.load(urllib.request.urlopen(req, timeout=60))
        except urllib.error.HTTPError as e:
            if e.code == 429:
                time.sleep(30)
            last = e
        except urllib.error.URLError as e:
            last = e
    raise RuntimeError(f"binance unavailable: {last}")


def main() -> None:
    frames = []
    start = START_MS
    if os.path.exists(OUT):
        old = pd.read_parquet(OUT)
        frames.append(old)
        start = int(old.open_time.max()) + 60_000
    now = int(time.time() * 1000)
    rows = []
    while start < now:
        k = get(f"symbol=USDCUSDT&interval=1m&limit=1000&startTime={start}")
        if not k:
            start += 1000 * 60_000
            continue
        rows += [[r[0], float(r[1]), float(r[2]), float(r[3]), float(r[4]), float(r[5]), float(r[7]), int(r[8])] for r in k]
        start = k[-1][0] + 60_000
        if len(rows) >= 200_000:
            frames.append(pd.DataFrame(rows, columns=["open_time", "open", "high", "low", "close", "volume", "quote_volume", "trades"]))
            rows = []
            pd.concat(frames).drop_duplicates("open_time").to_parquet(OUT, index=False)
            print("saved through", pd.to_datetime(start, unit="ms"), flush=True)
    if rows:
        frames.append(pd.DataFrame(rows, columns=["open_time", "open", "high", "low", "close", "volume", "quote_volume", "trades"]))
    df = pd.concat(frames).drop_duplicates("open_time").sort_values("open_time")
    df.to_parquet(OUT, index=False)
    print("done", len(df), pd.to_datetime(df.open_time.min(), unit="ms"), "->", pd.to_datetime(df.open_time.max(), unit="ms"))


if __name__ == "__main__":
    main()
