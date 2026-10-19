# Novel stable-pair ideas vs the live ladder

Date: 17 Oct 2026. Code (removed; it is in the git history up to tag v0.4.0): `alm/ladder_x.py` (LadderX, Dual), grids `ideas` and `ideas2` in `alm/grid.py`, scoring
`scripts/ideas_report.py`. Full tables: `reports/ideas_report.md`, `reports/ideas2_report.md`.

**Verdict: keep the live ladder unchanged.** None of the ten ideas improves on it on all three pairs.
The two that looked best on USDC/USDT both fail on at least one of the pairs held out from design.

## How overfitting was controlled

- **Design pair:** the ideas were designed on USDC/USDT. cbETH/WETH and USDC/USDbC were held out and
  looked at only after their grids finished.
- **Walk-forward scoring:** each idea is a family of configs. Configs are picked on the train window
  and booked on the next test window, at 28/7, 56/14 and 91/28 days. The baseline is the same
  procedure over the 24-config ladder family.
- **Gain:** the out-of-sample daily difference from the baseline, with a 95% interval from a 14-day
  block bootstrap. Each half of the period is also checked on its own.
- **Other checks:** the median config of each family (no selection at all) and PBO over the grid.
- **Common settings:** new-fee scenario, 15-tick depeg guard everywhere, May 19–24 2025 USDC/USDT
  burst excluded.
- **Acceptance rule:** the gain must be positive on all three pairs, its interval must clear zero
  somewhere, and it must hold in both halves.

## Ideas tested

| Idea | What it changes | Why it might help |
|---|---|---|
| Recency decay | Fee history is weighted with a 6–24 h half-life instead of a flat 24 h | Volume clusters in time |
| Fast re-solve | Re-solve every 1–2 h | React to new tick activity |
| Exit re-solve | Re-solve early once the price has stayed outside the ladder | Cut the 18% of fees lost while out of range |
| Core band | 5–20% of capital in a wide band | Insurance against moves |
| Swap-free | Fit the weights to the tokens already held, no swap | Remove swap cost and divergence |
| Emission-aware | Weight SOLID emissions (at 50%) into tick income | Emissions follow in-range liquidity, not volume |
| Blended centre | Centre on the 1-day / 7-day median blend | Steadier centre |
| Forecast mix | Mix in the empirical distribution of future ticks for the current deviation | Use mean reversion |
| Dual fast/slow | Two ladders (1 d / 3 d) merged | Diversify the horizon |
| Combo | Decay 6 h + emissions + 10% core | Stack the three best single ideas |
| Fee tail (smoke test only) | Thin liquidity a few ticks outside the usual range | Others' liquidity there is thin |

## Results: out-of-sample gain over the ladder, APR points, 56/14 walk-forward

| Idea | USDC/USDT (design) | cbETH/WETH (hold-out) | USDC/USDbC (hold-out) | Passes? |
|---|---|---|---|---|
| Combo decay+emis+core | +0.16 (−0.10 to +0.43; 2nd half −0.01) | +0.18 (−0.03 to +0.44) | **−0.21** (−0.28 to −0.09) | no |
| Recency decay | +0.11 (−0.06 to +0.37) | **+0.39** (+0.16 to +0.69; 1st half −0.12) | **−0.18** (−0.25 to −0.07) | no |
| Core band | +0.14 (−0.15 to +0.41) | −0.26 | −0.21 | no |
| Emission-aware | +0.08 (−0.03 to +0.21) | +0.03 | −0.07 (−0.12 to −0.04) | no |
| Fast re-solve | +0.07 | +0.02 | −0.27 | no |
| Forecast mix | +0.06 | −0.01 | −0.20 | no |
| Exit re-solve | −0.05 | −0.23 | −0.22 | no |
| Blended centre | −0.19 | −0.04 | −0.23 | no |
| Dual fast/slow | −0.31 (significantly worse) | −0.85 (significantly worse) | −0.27 | no |
| Swap-free | −0.91 (significantly worse) | −2.74 (significantly worse) | −0.05 | no |

Fee tail, on USDC/USDT since 25 May 2025, a single run:
- the baseline earned $680;
- the tail variants earned $638–670;
- time in range rose from 83% to 90% and divergence improved by about $25, but costs rose by $40–70.

Walk-forward APR of the baseline ladder for reference:

| Pair | APR | Unit |
|---|---|---|
| USDC/USDT | 2.88% | USDC |
| cbETH/WETH | 5.90% | WETH |
| USDC/USDbC | 1.28% | USDC |

Choosing freely across a whole grid gives:

| Grid | USDC/USDT | cbETH/WETH | USDC/USDbC | PBO |
|---|---|---|---|---|
| Round 1 | 2.96% | 6.23% | 1.21% | 0.34 / 0.25 / 0.02 |
| Round 2 | 3.00% | 6.05% | 1.19% | 0.13 / 0.02 / 0.00 |

So any edge is a fraction of a point and isn't stable across pairs.

## What this says

- **The ladder already does what these ideas aim for.** Its 24 h lookback, 6 h schedule and 40%
  turnover threshold put it near the sweet spot. Every idea that reacts faster (decay, fast re-solve,
  exit re-solve, dual) gains in some periods and loses in others. Net, the extra rebalancing and swap
  costs cancel the extra fees.
- **The remaining gaps aren't cheaply recoverable:**
  - **Out of range:** $237 of fees were missed while out of range since May 2025. Covering those ticks
    costs more in rebalancing than they pay.
  - **In range:** in-range dilution was $219 (oracle $1,107 vs our $888). Our median share of the
    active tick is already 66%.
  - **Divergence:** −$281, about $15 a month. Divergence is the cost of rebalancing into a
    mean-reverting price. Slower ladders cut it but give up more fees.
- **Recency decay** is the only idea with a significant gain anywhere (cbETH/WETH). It fails the
  first-half check there, and it loses significantly on USDC/USDbC. It isn't robust enough to deploy.
- **Swap-free and dual** are clearly worse and are ruled out.

## Recommendation

Keep `Ladder(lookback=86400, span=3, every=72, turnover=0.4, guard=15)` live. If there's interest,
re-test recency decay (12–24 h half-life) on USDC/USDT in about three months, with new
out-of-sample data, before considering it again.
