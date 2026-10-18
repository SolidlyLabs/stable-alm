import time, pandas as pd, numpy as np
import dsim, dstrat
d = pd.read_parquet("../../data/raw/binance_ethusdt_1h.parquet").sort_values("ts")
ts, P = d.ts.to_numpy(np.int64), d.eth_usd.to_numpy(float)
f = dstrat.Features(P)
cfgs = [dstrat.Band(f, k=3, delay=4),
        dstrat.Blend(f, sig="mom", n=96, th0=0.5, a=0.5, min_lp=0.25, k=3),
        dstrat.Blend(f, sig="mom", n=168, th0=1.0, a=0.5, min_lp=0.0, k=3),
        dstrat.Blend(f, sig="ema", n=168, th0=0.5, a=0.5, min_lp=0.25, k=3, lean=0.5),
        dstrat.Blend(f, sig="mom", n=168, th0=1.0, a=1.0, min_lp=0.5, k=5, protect=False)]
for s in cfgs:
    r = dsim.run(ts, P, s, costs=dsim.Costs(c=1.0), h0=720)
    x = dsim.summary(r, 10000)
    print(f"{x['label'][:90]:<90} vsHold {x['vs_hold_apr']:7.2f}% fees {x['fees_apr']:6.2f}% cost {x['cost_apr']:5.2f}% acts {x['actions']:5d} dep {x['deployed']:.2f} act {x['active']:.2f}")
