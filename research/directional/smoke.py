import time, pandas as pd, numpy as np
import dsim, dstrat
d = pd.read_parquet("../../data/raw/binance_ethusdt_1h.parquet").sort_values("ts")
ts, P = d.ts.to_numpy(np.int64), d.eth_usd.to_numpy(float)
f = dstrat.Features(P)
h0 = 720
cfgs = [dstrat.Hold(f), dstrat.Band(f, k=2, delay=4), dstrat.Band(f, k=5, delay=12),
        dstrat.Regime(f, sig="mom", n=48, th=1.5, off=0.5, up="hold", dn="hold", lp=False),
        dstrat.Regime(f, sig="mom", n=48, th=1.5, off=0.5, up="hold", dn="hold"),
        dstrat.Regime(f, sig="mom", n=168, th=1.5, off=0.5, up="above", dn="hold"),
        dstrat.Regime(f, sig="brk", n=168, up="hold", dn="hold"),
        dstrat.Regime(f, sig="ema", n=96, th=1.0, off=0.3, up="skew", dn="skew"),
        dstrat.Regime(f, sig="mom", n=48, th=99, off=0, vgate=2.0, vh=24)]
for s in cfgs:
    t = time.time()
    r = dsim.run(ts, P, s, costs=dsim.Costs(c=1.0), h0=h0)
    x = dsim.summary(r, 10000)
    print(f"{x['label'][:85]:<85} vsHold {x['vs_hold_apr']:7.2f}% pnl {x['pnl_apr']:7.2f}% fees {x['fees_apr']:6.2f}% cost {x['cost_apr']:5.2f}% acts {x['actions']:5d} pool {x['in_pool']:.2f} t0 {x['token0']:.2f} ({time.time()-t:.1f}s)")
