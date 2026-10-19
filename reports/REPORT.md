# ALM for correlated pairs on Solidly V3 (Base): backtest report

Updated 2026-10-16.

## How these numbers were made

- **Pairs:** USDC/USDT, cbETH/WETH and USDC/USDbC.
- **Strategies:** 508 configs per pair, plus a wait-before-rebalance grid on 1-minute bars.
- **Price paths:** every swap since each pool started, replayed in order.
- **Fees: the new fee level is assumed for all of history.** Each hour pays at least the pool's
  post-Oct-13 rate: USDC/USDT 0.0987 bp, cbETH/WETH 0.44 bp, USDC/USDbC 0.06 bp. Historical volume is
  kept as it was.
- **Haircuts and costs:**
  - fees paid at 82% of what the processor attributes;
  - SOLID valued at 50%;
  - swaps 0.3 bp, through the aggregator;
  - gas $0.02 per transaction.
- **How results are scored:** walk-forward with 56-day training and 14-day test windows, so every
  figure is out-of-sample.
- **Exclusion:** the USDC/USDT May 19–24 2025 volume burst is left out.

Full tables: `compare_pairs.md`, `trend_analysis.md`.

## TL;DR

1. **The ladder wins on every pair under the new fees, and walk-forward confirms it.**
   - USDC/USDT: **2.9% APR** over 21 months, max drawdown −$30 on $15k.
   - cbETH/WETH: **5.9% APR** in WETH over 32 months, max drawdown −0.018 WETH on 6 WETH.
   - USDC/USDbC: 1.3% APR over 32 months, but **≈ 0% in the last 6 months**. USDbC is being retired,
     so this pair is not worth deploying.
   - PBO is 0.00–0.03 everywhere. The ladder family is a plateau: all 24 configs land close together.
2. **The yield comes almost entirely from volume, and volume is lumpy.** The recommended USDC/USDT
   ladder, under the new fee, made the following:

   | Window | APR |
   |---|---|
   | Last 30 days | 14% |
   | Last 60 days | 9.9% |
   | Last 180 days | 2.6% |

   May to July 2026 earned nearly nothing (≈ $0 of fees a month) and lost $10–40 a month to price
   drift. The weeks since mid-August paid $87, then $204 a month. Plan on **≈ 2.5–3% a year over a
   full cycle** (≈ $400–450 on $15k), with 10%+ only in busy months.
3. **Wait before rebalancing. Five minutes beats acting immediately on every pair, and longer is
   better still.** On 1-minute decision bars, walk-forward APR:

   | Pair | Rule | Immediate | 5 min | 15 min | 60 min |
   |---|---|---|---|---|---|
   | USDC/USDT | recentre on exit | −0.08% | 0.57% | 0.85% | **1.22%** |
   | USDC/USDT | 24h range | 1.50% | 1.67% | 1.70% | **1.71%** |
   | cbETH/WETH | recentre on exit | 1.02% | 2.07% | 3.11% | **3.50%** |
   | cbETH/WETH | 24h range | 3.11% | 3.16% | **3.29%** | 3.20% |
   | USDC/USDbC | recentre on exit | 0.32% | 0.54% | 0.64% | **0.92%** |

   Exits are usually noise. The price comes back, as the mean-reversion statistics below predict,
   and a rule that waits avoids paying for a round trip. The ladder re-solves on a 6-hour clock, so
   it gets this benefit by design.
4. **The 24h-range rule works but loses to the ladder.** The range is the last 24h low/high plus a
   directional buffer on exit. It makes 1.5–1.7% on USDC/USDT and 3.1–3.3% on cbETH, with very few
   actions: 30 rebalances in 21 months on USDC/USDT. That makes it a good low-maintenance fallback.
   The directional buffer does not help. Median PnL falls steadily as the buffer grows from 0 to
   2, 5 and 10 ticks on every pair (USDC/USDT +$613, +$573, +$531, +$490), and the same holds for
   the pad. The 24h low/high already covers where price goes next, and the extra width only dilutes
   fees. The best settings are no pad, no buffer and a 30-minute wait.
5. **Volatility-adaptive width and timing lose on every pair (0.1–2.3% APR).** Widening and tightening
   on volatility (std, ATR, EWMA, Parkinson) churns: about 1,800–4,100 actions, and costs eat the
   extra fees. Of the timing modes, waiting *longer* when volatility is high (`vol_down`) is best in 8
   of 9 pair × skew cases, which is the same lesson as point 3.
6. **Treated as trading: the only edge is mean reversion. Never chase trends.**
   - Every pair mean-reverts at every horizon from 5 minutes to 1 day. Autocorrelation is −0.11 to
     −0.39. The variance ratio is 0.06–0.55 (below 1 means mean-reverting).
   - A move's direction predicts the *opposite* next move. The information coefficient is −0.1 to
     −0.5, where |IC| > 0.05 is usable.
   - Half-life back to the 7-day median: 1.7h on USDC/USDbC, 8.9h on USDC/USDT, 17.6h on cbETH/WETH.
   - In the adaptive family, skewing the range *against* the move (`meanrev`) ranked first on
     both USDC pairs, and was level with no skew on cbETH (2.26% vs 2.32%). Skewing *with* the move
     (`momentum`) ranked last on every pair, and on USDC/USDT a median momentum config lost money.
   - cbETH drifts +0.78 ticks a day against WETH (staking yield). Medians and 24h windows absorb
     that drift, so no separate rule is needed.
   - Exploiting mean reversion does not need a directional bet. Waiting before acting (point 3) and
     anchoring on a 1-day window (the ladder's lookback) already capture it.
7. **If the fee rises to 0.44 bp on USDC/USDT** (cbETH's level): the ladder's walk-forward APR rises from
   2.8% to **8.1%**. The other families scale less: recentre 5.8%, 24h range 3.6%, static 3.3%,
   adaptive 3.0%. The ranking is unchanged. This assumes volume does not react, and at 4.5× the
   fee it would. The recommended config's 60-day pace at 0.44 bp is 51% APR, which shows how
   strongly the result rests on that assumption. Treat it as an upper bound.

## Recommended system (now in `config/pools.json`)

| | USDC/USDT | cbETH/WETH |
|---|---|---|
| Strategy | `ladder(lookback=1d, span=3, every=6h, turnover=0.4, guard=15)` | `ladder(lookback=1d, span=6, every=6h, turnover=0.4, guard=15)` |
| Previous live setting | `lookback=3d, every=24h`: +$576 over 21 months, against **+$691** for the new one, which is also better in the last 60 and 180 days | — |
| Enabled | yes (dry run until `--execute`) | **no.** It needs WETH/cbETH in the wallet; flip `enabled` to use it |
| Swap cap / slippage | 3 bp / 2 bp | 5 bp / 3 bp |
| Rebalances per day | ≤ 5 | ≤ 5 |

Both use the 15-tick depeg guard. The executor:

- swaps only through the Solidly UI meta-aggregator. The live quote in the dry run was KyberSwap via
  Aerodrome CL, at −0.03 bp against the pool price;
- simulates every transaction before it sends it;
- uses exact-amount approvals;
- claims rewards weekly.

Today's dry run against the chain:

- **Burn:** simulated OK.
- **Swap:** 4,586 USDC → 4,592.69 USDT via KyberSwap.
- **Mint:** a single-tick [14,15) position.
- **Claim:** simulated OK, about 5.7 USDC + 1,782 SOLID + 5.7 USDT claimable.

Profit mix of the median ladder config over the full backtest:

| Pair | Fees | Emissions | Costs | Price / divergence |
|---|---|---|---|---|
| USDC/USDT | +$1,974 | +$96 | −$58 | −$239 |
| cbETH/WETH (WETH) | +0.767 | +0.143 | −0.066 | −0.140 |

The cbETH profit is fee income. It is not the staking drift, because the price leg is negative.

## Caveats specific to the new-fee scenario

- **Volume is held at its historical level.** A higher fee can push aggregator flow to other venues
  and lower volume. In the other direction, other LPs may add liquidity to chase the higher fee,
  which dilutes our share. Neither effect is modelled.
- **The floor is applied to all of history.** Absolute numbers are what the past would have paid at
  today's fee. Ranking between strategies is the robust output.
- **The ladder puts everything on 1–3 single ticks.** That is efficient while the peg holds, but a
  one-tick move takes the position out of range until the next 6-hour re-solve. This cost is in the
  backtest. The guard handles real depegs.


Pool `0x9ab7730b…0a46`, tick spacing 1, dynamic fee. Capital $15,000 USDC. Profit is measured
in USDC after the haircuts in the README: fees paid at 82% of what the processor attributes,
SOLID valued at 50%, swaps 0.3 bp, gas $0.02 per transaction.

## Earlier results (USDC/USDT, historical fees)

This section and everything after it were run on the fees the pool actually charged, before the
Oct 2026 change. They are kept for the depeg-guard and overfitting evidence.

### TL;DR (historical fees)

1. **The choice of strategy is not the main lever on this pool. The pool's fee is.** For most of
   2026 the dynamic fee sat at 0.05 bp, against 0.3–1 bp in 2025. At that level the whole pool paid
   LPs only a few dollars a day. Over the last 90 days every family made 0.5–2% APR out-of-sample.
   The fee setting is controlled by Solidly's own fee optimizer.
2. **Realistic out-of-sample result over 21 months: about 1.3–2.2% APR net.** This is walk-forward
   with 56-day training and 14-day test windows. It excludes the May 20–22 2025 volume-farming burst:
   $120M of volume and $10k of fees in three days, a one-off that dominated every in-hindsight ranking.
   - With the burst included, the tick ladder made +$1,848 (7.7% APR).
   - Without it, the ladder made +$511 (2.2%) at base costs and +$303 (1.3%) at stress costs.
     A static ±8-tick band made +$355 and +$300.
3. **Never swap-rebalance into a depeg, so every active strategy needs a guard.** On Binance price
   paths since 2021:
   - In March 2023, active strategies lost $500–2,600 on $15k. They sold the depegged coin at the
     bottom.
   - A static band around the peg made +$35 over the same window, because the peg came back.
   - The old `alm.ts` rule (recentre ±2% on exit) lost $800 in May 2022 and $1,200 in March 2023.
   - A 15-tick guard (freeze while spot is more than 0.15% from its 7-day median) cut the ladder's
     March 2023 loss from −$785 to −$16, and its max drawdown from −$826 to −$70.
4. **The overfitting checks pass.**
   - PBO is 0.00–0.04 on every grid.
   - The ladder family is a plateau, not a peak: all 24 configs are profitable and close together.
   - The in-hindsight champion (single-tick recentre, +$1,471) made only +$56 once the May 2025 burst
     was removed. It is a textbook overfit, and walk-forward avoided it.

## Recommended system

**A tick ladder with a depeg guard, a static fallback, and no swaps during a depeg.**

- **Normal regime.** Every 24h, re-solve the ladder on the last 3 days of swaps:
  `ladder(lookback=3d, span=3, every=24h, turnover=0.4)`.
  - Capital goes to single ticks in proportion to √(fees × competing liquidity) minus the competing
    liquidity.
  - It re-mints only when more than 40% of the allocation would move.
- **Depeg guard (G = 15 ticks).** If spot is more than 15 ticks (0.15%) from its 7-day median, freeze:
  no recentre, no swap. Resume when price comes back.
- **Fallback.** If the processor, RPC or volume data is stale, hold a static band of ±8 ticks around
  the 7-day median.
- **Execution (now built: the desktop app in `rs/`, see README).** Burn, swap through the Solidly UI
  aggregator, then mint. Then submit and confirm, and claim rewards weekly. (This report was made with
  the original Python backtester, which is removed; it is in the git history up to tag v0.4.0.)

## Validation of the simulator

The simulator was run on the wallet's real position history and compared with the processor's
per-position fee figures:

| Window | Simulated fees | Processor | Error |
|---|---|---|---|
| Oct 12–16 2026 (your campaign) | $13.31 | $13.05 | +2% |
| May 2025 – Oct 2026 (lifetime) | $59.24 | $48.68 | +22% (early thin-liquidity months) |

Emissions also line up: about $4.85 simulated against about 1,958 SOLID earned. The stress grid
uses a 0.67 fee haircut to cover the lifetime error.

## Results

### On-chain replay (Jan 2025 – Oct 2026, 1.18M swaps), walk-forward within each family

| Family | Base costs | Base, excl. May-25 burst | Stress (0.67 haircut, 1 bp swaps), excl. burst |
|---|---|---|---|
| ladder | **+$1,848** | **+$511** | **+$303** |
| volband, with swaps | +$1,156 | +$320 | +$111 |
| recenter, with swaps | +$445 | +$371 | +$227 |
| recenter, no swaps | +$539 | +$314 | +$161 |
| volband, no swaps | +$400 | +$187 | +$42 |
| static band | +$329 | +$355 | +$300 |
| old alm.ts (±2% recentre) | +$111 | +$87 | — |

Charts: `grid_equity.png`, `grid_exmay_equity.png`, `grid_stress_exmay_equity.png`.

### Your Oct 12–16 campaign under each strategy

Same start, same $15,401 of capital, same costs (`example_last_days.csv`):
- **Your actual result:** +$9.34, or about +$11.3 counting SOLID at half value. That ranks about 60th
  of 250.
- **Best config:** +$13.84, a median-anchored ATR band.
- **Static ±3 band:** +$13.07.
- **Ladder:** +$2.50 to +$10.
- **Old ±2% rule:** +$3.89.

Four days is noise. Any difference under about $5 means nothing.

### Binance price paths 2021–2026 (synthetic fees, relative comparison only)

The fee model here is calibrated on the real pool, with constant competing liquidity. That inflates
absolute APRs to 13–18%, so read this section only for the ranking and the depeg behaviour.

| Family | Walk-forward PnL | May 2022 (USDT wobble) | Mar 2023 (USDC to 0.87), median config |
|---|---|---|---|
| ladder | +$15,614 | +$113 | −$671 |
| volband | +$13,468 | −$132 | −$1,187 |
| recenter | +$11,738 | −$269 | −$1,386 |
| static | +$3,231 | −$14 | **+$31** |

### Depeg guard (Binance paths 2021–2026; on-chain excludes the May-25 burst)

Walk-forward within each (family, guard) group. Full table in `guard_*_guard_compare.md`.

| Group | Binance WF PnL | Max drawdown | May 2022 | Mar 2023 | On-chain WF PnL |
|---|---|---|---|---|---|
| **ladder, guard 15** | **+$16,419** | **−$70** | **+$136** | **−$16** | **+$548** |
| ladder, no guard | +$15,189 | −$826 | +$85 | −$785 | +$548 |
| recenter, guard 8 | +$13,612 | −$75 | +$114 | −$8 | +$282 |
| recenter, no guard | +$11,568 | −$974 | −$139 | −$938 | +$208 |
| volband, guard 8 | +$12,624 | −$72 | +$90 | −$4 | +$322 |
| volband, no guard | +$10,221 | −$1,011 | −$727 | −$997 | +$256 |
| static band | +$1,641 | −$93 | −$14 | +$31 | +$345 |

The guard costs nothing in normal markets: on-chain the ladder is unchanged at 15 and 30 ticks. It
cuts depeg losses by 95–99%. Every active strategy should run with it.

## Data and method

See `README.md`. Walk-forward books only out-of-sample days. Every config is simulated once, and a
switch between configs costs one rebalance. Four train/test lengths and five selection rules are
reported in `*_walkforward.md`, so no single setting is cherry-picked.

## Limits

- **Volume is held fixed** and only the price path is replayed. Deeper liquidity would attract more
  aggregator flow, so fees are conservative. On the other side, arbitrageurs would trade more against
  a large position, and that extra flow is not modelled.
- **Competing liquidity is observed, not reactive.** Other LPs might pull out or pile in after you
  move.
- **Emissions are assumed pro-rata to in-range liquidity × time.** The emissions check against SOLID
  received supports this but is loose.
- **Costs are flat.** Gas and swap costs are fixed numbers, and execution delay is ignored.
