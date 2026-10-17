"""Every Solidly V3 pool on every chain the UI serves, ranked by 60-day fees + emissions, with the
yield per $ of in-range TVL. Output: research/volatile/survey_chains.csv"""
import os
import sys
import time

import pandas as pd

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))
from alm import market_data  # noqa: E402
from alm.download.processor import apr_history  # noqa: E402

WALLET = os.environ.get("ALM_WALLET", "0x0000000000000000000000000000000000000000")  # any address: the API needs one
OUT = os.path.join(os.path.dirname(__file__), "survey_chains.csv")
rows = []
since = time.time() - 60 * 86400
for cid in (8453, 1, 42161, 10, 146, 250):
    market_data.CHAIN_ID = cid
    market_data._cache.clear()
    try:
        ps = market_data.pools(WALLET)
    except Exception as e:  # noqa: BLE001
        print(cid, "init failed:", e)
        continue
    ps = [p for p in ps if p["tvl_usd"] >= 200]
    print(f"chain {cid}: {len(ps)} pools with TVL >= $200")
    for p in ps[:40]:
        try:
            h = apr_history(p["pool"], chain=cid)
        except Exception as e:  # noqa: BLE001
            print("  skip", p["symbol"], e)
            continue
        h = h[h.timestamp >= since]
        if len(h) < 24 * 7:
            continue
        k = 365 / (len(h) / 24)
        act = h.activeTvlUSD.mean() or 1
        r = {"chain": cid, "symbol": p["symbol"], "pool": p["pool"], "spacing": p["tick_spacing"], "pegged": p["pegged"],
             "tvl": p["tvl_usd"], "active": act, "volume_60d": h.volumeUSD.sum(), "fees_60d": h.feesUSD.sum(),
             "emis_60d": h.emissionsUSD.sum(), "bribes_60d": h.bribesUSD.sum(),
             "fee_apr_active": h.feesUSD.sum() / act * k * 100, "emis_apr_active": h.emissionsUSD.sum() / act * k * 100}
        rows.append(r)
        print(f"  {r['symbol']:<26} tvl {r['tvl']:>12,.0f} act {act:>11,.0f} vol {r['volume_60d']:>13,.0f} "
              f"fees {r['fees_60d']:>9,.0f} emis {r['emis_60d']:>9,.0f}  fee {r['fee_apr_active']:6.1f}% emis {r['emis_apr_active']:6.1f}%")
df = pd.DataFrame(rows)
df["income_60d"] = df.fees_60d + df.emis_60d
df.sort_values("income_60d", ascending=False).to_csv(OUT, index=False)
