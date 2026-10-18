"""Cross-check: the on-chain candidates on the Binance path over the same 90 days, and the ETH move in it.

    python window.py <t0> <t1>   (unix seconds; default the last 90 days of the Binance file)
"""
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
import dsim  # noqa: E402
import dstrat  # noqa: E402
import grid  # noqa: E402
import onchain  # noqa: E402


def main():
    ts, P = grid.load()
    t1 = int(sys.argv[2]) if len(sys.argv) > 2 else int(ts[-1])
    t0 = int(sys.argv[1]) if len(sys.argv) > 1 else t1 - 90 * 86400
    h0, h1 = int(np.searchsorted(ts, t0)), int(np.searchsorted(ts, t1))
    print(f"{np.datetime64(t0, 's')} .. {np.datetime64(t1, 's')}: ETH {P[h0]:.0f} -> {P[h1 - 1]:.0f} "
          f"({(P[h1 - 1] / P[h0] - 1) * 100:+.1f}%), low {P[h0:h1].min():.0f}, high {P[h0:h1].max():.0f}")
    f = dstrat.Features(P)
    for c in (0.55, 1.0):
        print(f"c={c}")
        for label, make in onchain.candidates():
            r = dsim.run(ts, P, make(f), costs=dsim.Costs(c=c), h0=h0, h1=h1)
            s = dsim.summary(r, 10_000.0)
            print(f"  {label:<34} vs hold {s['vs_hold_apr']:8.2f}%  fees {s['fees_apr']:7.2f}%  acts {s['actions']:3d}  deployed {s['deployed']:.2f}")


if __name__ == "__main__":
    main()
