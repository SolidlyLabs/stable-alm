"""Screen every Solidly V3 pool on Base: 60-day fees and emissions per $ of in-range (active) TVL,
plus the price volatility an LP position would carry. Output: research/volatile/survey.csv"""
import os
import sys
import time

import numpy as np
import pandas as pd

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))
from alm import market_data  # noqa: E402
from alm.download.processor import apr_history  # noqa: E402

WALLET = os.environ.get("ALM_WALLET", "0x0000000000000000000000000000000000000000")  # any address: the API needs one
OUT = os.path.join(os.path.dirname(__file__), "survey.csv")

ps = [p for p in market_data.pools(WALLET) if p["tvl_usd"] >= 1000]
print(len(ps), "pools with TVL >= $1k")
rows = []
since = time.time() - 60 * 86400
for p in ps:
    try:
        h = apr_history(p["pool"])
    except Exception as e:  # noqa: BLE001
        print("skip", p["symbol"], e)
        continue
    h = h[h.timestamp >= since]
    if len(h) < 24 * 7:
        continue
    act = h.activeTvlUSD.clip(lower=1)
    days = len(h) / 24
    k = 365 / days
    r = {
        "symbol": p["symbol"], "pool": p["pool"], "spacing": p["tick_spacing"], "fee_pips": p["fee_pips"],
        "pegged": p["pegged"], "tvl": p["tvl_usd"], "active_mean": h.activeTvlUSD.mean(),
        "vol_60d": h.volumeUSD.sum(), "fees_60d": h.feesUSD.sum(), "emis_60d": h.emissionsUSD.sum(),
        "bribes_60d": h.bribesUSD.sum(), "days": days,
        # APR on in-range TVL, hour by hour: what liquidity that is in range earns.
        "fee_apr_active": (h.feesUSD.sum() / (h.activeTvlUSD.mean() or 1)) * k * 100,
        "emis_apr_active": (h.emissionsUSD.sum() / (h.activeTvlUSD.mean() or 1)) * k * 100,
        "emis_last7_usd": h[h.timestamp >= time.time() - 7 * 86400].emissionsUSD.sum(),
        "fees_last7_usd": h[h.timestamp >= time.time() - 7 * 86400].feesUSD.sum(),
        "t0": p["token0"]["symbol"], "t1": p["token1"]["symbol"],
        "d_t0": p["token0"]["address"], "d_t1": p["token1"]["address"],
    }
    rows.append(r)
    print(f"{r['symbol']:<22} tvl {r['tvl']:>11,.0f} act {r['active_mean']:>10,.0f} fees {r['fees_60d']:>9,.0f} "
          f"emis {r['emis_60d']:>9,.0f} feeAPR {r['fee_apr_active']:>7.1f}% emisAPR {r['emis_apr_active']:>7.1f}%")
pd.DataFrame(rows).sort_values("emis_60d", ascending=False).to_csv(OUT, index=False)
