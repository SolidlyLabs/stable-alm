# Fee capture on volatile pairs: can we build a profitable strategy, and does it carry over to our pools?

Date: 17 Oct 2026. Code and data (removed; it is in the git history up to tag v0.4.0): `research/volatile/` (simulator `vsim.py`, strategies `vstrat.py`, grid
`grid.py`, scoring `score.py` and `effects.py`, `fee_vs_lvr.py`, `elasticity.py`, pool surveys).
Goal as set: **fee capture, not emissions farming**. Income in every test here is swap fees only.

## Verdict

**No.** No volatile-pair strategy is profitable in a way that survives out of sample, and Solidly's only
volatile pool with real volume (WETH/USDC) is structurally unprofitable to LP in.

- On deep pools, fees per unit of at-the-money liquidity just about pay for what arbitrage takes from
  it (F/LVR ≈ 0.9–1.0). That is the competitive equilibrium, and range rules can't beat it reliably.
- Solidly's WETH/USDC pays only **0.55× its arbitrage loss**, in every one of 13 weeks.
- Our two live pools are the opposite case: fees exceed arbitrage loss thousands of times over. **Keep
  the ladder where it is.**

## Data

90 days (19 Jul – 17 Oct 2026) of every Swap/Mint/Burn:

| Pool | Role | Fee | Swaps |
|---|---|---|---|
| Uniswap V3 WETH/USDC, Base | design | 0.05% | 1.73M |
| Uniswap V3 WETH/USDC, Base | hold-out | 0.30% | 0.95M |
| Uniswap V3 cbBTC/USDC, Base | hold-out | 0.05% | 0.56M |
| Aerodrome Slipstream WETH/USDC CL50, Base | hold-out (thin, unstaked) | 0.05% | 58k |
| Solidly V3 WETH/USDC, Base | transfer target | dynamic (~0.1%) | 125k |

Aerodrome's main WETH/USDC CL100 pool logs about 480k events a day, too much for the shared RPC, so its
thin CL50 sibling stands in for Aerodrome.

A survey of every Solidly V3 pool on six chains (`survey_chains.csv`) found nothing to aim a volatile
strategy at:

- **Base:** 4 pools above $1k TVL. WETH/USDC holds $2k and earned $73 in fees in 60 days.
- **Arbitrum:** WETH/USDC earned $160 in 60 days.
- **Sonic:** wS/Anon is the only pool with meaningful fees ($2.2k in 60 days, on $5k TVL).

## Method

- **Simulator** (`vsim.py`): exact V3 position math for any decimals. Each swap pays amount_in × fee,
  shared with the pool's active liquidity at the swap's tick. Rebalances pay 5 bp on the swapped
  notional plus $0.02 gas per transaction. Decisions run on 5-minute bars, with no look-ahead.
- **Score: gain over REBAL.** This is fees − costs + position value change
  − value change of simply holding the tokens the position had at its last rebalance. It isolates what
  being an LP added, whatever the price did. Results vs a 50/50 hold are in the result files too.
- **Strategies:** every width is in multiples of the pool's own trailing 7-day daily volatility, snapped
  to its tick spacing, so one parameter set runs on any pool.
  - Full range.
  - Band ±k σ, re-centred after the price has been out of range for `delay` bars. An
    optional volatility exit leaves the pool while the last hour's variance is above 4× the 7-day level.
  - Our live ladder, as it is (fee history water-filling).
  - A forecast fee-vs-LVR ladder: expected fees per bucket from a random-walk price forecast, minus the
    loss-versus-rebalancing cost σ²·L·√P/4, optimised under the capital budget. It may leave capital
    out of the pool.
- **Grid:** 61 configs × 2 capital sizes ($10k, $100k) × 4 pools.
- **Scoring:** 28/7-day walk-forward, picking the best config on the previous 28 days. 95% intervals
  come from a 7-day block bootstrap. Rule effects are paired differences across every config pair that
  differs only in that rule.

## Results

### Walk-forward gain over REBAL (APR on capital)

| Pool | $10k | $100k | Median config |
|---|---|---|---|
| Uniswap WETH/USDC 0.05% (design) | −16.5% (−153 to +61) | −31.9% (−146 to +41) | −1.9% / −5.9% |
| Uniswap WETH/USDC 0.3% | −64.6% (−180 to −3) | −32.2% (−85 to +1) | −2.3% / −2.2% |
| Uniswap cbBTC/USDC 0.05% | −30.4% (−117 to +19) | −32.2% (−95 to +3) | −2.8% / −5.4% |
| Aerodrome WETH/USDC CL50 | −11.3% (−40 to +13) | −20.1% (−46 to −1) | −115% / −120% |

- **Selecting on recent results is worse than any fixed config.** It chases last month's lucky width.
- **The forecast fee-vs-LVR ladder** earned the highest fees (300–490% APR at $10k) and lost the most:
  −56% walk-forward on the design pool.
- **Our live ladder, applied unchanged to ETH/USDC,** was in range only 26% of the time. It puts
  liquidity where fees were paid yesterday, and on a volatile pair the price has left by then.

### The design pool's best fixed configs, booked on the hold-outs

| Config | Design | WETH/USDC 0.3% | cbBTC/USDC | Aerodrome CL50 |
|---|---|---|---|---|
| ±2σ, re-centre after 4 h | +34.9% | +8.9% | +14.0% | −23.5% |
| ±0.5σ, re-centre after 1 h, vol exit | +33.2% | −0.9% | −14.5% | −337.7% |
| ±1.5σ, re-centre after 4 h | +30.7% | −1.9% | +24.7% | −95.8% |
| ±0.5σ, re-centre after 15 min, vol exit | +26.9% | −1.1% | −23.1% | −332.2% |
| Full range | −6.0% | −1.1% | −1.6% | −2.4% |

None is positive on all four pools. ±2σ with a slow re-centre comes closest (3 of 4), but its design-pool
neighbours differ from it by ±50 points, so that is noise, not an edge.

### Rule effects (paired across the grid, $10k)

| Rule | Design | WETH/USDC 0.3% | cbBTC/USDC | Aerodrome CL50 |
|---|---|---|---|---|
| Volatility exit vs none | +5.0 (−34 to +65) | +13.3 (−36 to +80) | −1.9 (−23 to +29) | **−72.6** (−97 to −43) |
| ±0.5σ vs ±2σ | +4.9 | −1.8 | **−23.7** | **−175.6** |
| ±5σ vs ±2σ | −8.1 | −2.0 | −6.3 | **+79.0** |
| Re-centre after 4 h vs at once | +7.3 | −3.4 | +5.9 | +43.2 |

Bold effects are those whose interval excludes zero. Every one of them is a loss for narrow ranges on
the thinner pools; no rule helps everywhere.

### Why: fees vs loss to arbitrage, per unit of at-the-money liquidity (`fee_vs_lvr.py`)

F = Σ fee_i / L_i (what one more unit of in-range liquidity earns). LVR = Σ r²·√P/4 over 1-hour returns
(what arbitrage takes from it). The ratio is the gross margin of LPing at the money, before costs.

| Pool | F/LVR | Weeks above 1 |
|---|---|---|
| Uniswap WETH/USDC 0.05% | 1.01 | 69% |
| Uniswap WETH/USDC 0.3% | 0.95 | 69% |
| Uniswap cbBTC/USDC 0.05% | 0.92 | 31% |
| **Solidly WETH/USDC** | **0.55** | **0%** (0.22–0.83) |
| Solidly cbETH/WETH (live) | ≈ 14,000 | 100% (lowest week 1.9) |
| Solidly USDC/USDT (live) | ≈ 12,000 | 94% (lowest week 0.7) |

- **Deep volatile pools sit at break-even.** LPs keep adding until the margin is gone, so a range rule
  can only win by luck or by being faster than the arbitrageurs.
- **Solidly's WETH/USDC volume is mostly arbitrage.** A 0.1% pool with $2k of liquidity next to deep
  0.05% pools gets traded mainly when it is mispriced. Every dollar in it loses about half of what it
  "earns" in fees, whatever the strategy.
- **Aerodrome CL50 is a thin pool.** Its F/LVR swings from 0.7 to 15,000 because in some weeks almost no
  liquidity was in range. That is the thin-pool case: small money takes all of a tiny fee flow, but in
  a thin pool the price path is the arbitrage against your own liquidity. The narrow configs lost
  96–338% APR there in the simulation.

## What carries over to our pools

1. **Solidly volume follows our liquidity** (`elasticity.py`, daily and weekly volume vs mean active
   TVL, 1.4–2.7 years of processor history):

   | Pool | Elasticity, changes | Correlation, weekly |
   |---|---|---|
   | USDC/USDT | 0.8–1.1 | +0.46 |
   | cbETH/WETH | 0.9 | +0.61 |
   | USDC/USDbC | 0.6–0.9 | +0.78 |
   | WETH/USDC | 1.4–1.7 | +0.76 |

   Aggregators route by depth, so fee yield per dollar of in-range liquidity stays roughly constant as
   liquidity is added. Our backtests hold volume fixed, so they understate what more capital earns in
   our pools. Correlation isn't proof (LPs also add when volume is high), but it supports running more
   capital in USDC/USDT and cbETH/WETH before the yield dilutes.

2. **F/LVR is a good pool screen and a good safety signal.**
   - **Screen:** a pool is worth a fee-capture position only if its weekly F/LVR (1-hour returns) is
     well above 1. Ours are above 1,000 in normal weeks. For candidates, show the figure in Discover
     pairs.
   - **Safety:** if a live pool's trailing-week F/LVR falls toward 2, its volume has turned into
     arbitrage flow and the position should shrink. It is the general form of our depeg guard, and the
     "out of pool when income doesn't cover expected loss" gate.

3. **A volatility exit doesn't transfer.**
   - The volatility exit doesn't help robustly on volatile pairs: +13 to −73 points.
   - On our pairs, volatility is too low for it to fire outside depegs, which the depeg guard already
     covers.

## Recommendation

- **Don't deploy a volatile strategy.** In particular, don't LP Solidly WETH/USDC.
- **Keep the ladder on USDC/USDT and cbETH/WETH.**
- **Next steps worth doing:**
  - add the weekly F/LVR to the dashboard as a monitor, and to Discover pairs as a screen;
  - consider more capital in the live pools, given the volume elasticity. Size up in steps and check
    the realised fee yield per in-range dollar after each one.

## Limits

- **90 days only.** The 95% intervals on daily LP alpha are ±50–100 APR points, so small edges can't be
  seen either way.
- **The volatile simulator holds volume fixed** and doesn't let our liquidity move the price. This
  understates income in Solidly-style pools, where volume follows depth, and is badly wrong in thin
  pools, where we would be the price-setting liquidity.
- **Fee share uses the active liquidity at the swap's ending tick,** averaged with the starting tick.
  Swaps that cross many ticks are approximated.
- **Swap cost is 5 bp flat;** real aggregator costs for ETH/USDC on Base are often lower. The busiest
  band configs pay 9–19 APR points in costs, so at 1 bp they would recover roughly 7–15 points. That is
  well inside the intervals above and doesn't change the verdict (not re-run).
- **Execution is at 5-minute bar ends,** with no MEV, no JIT competition beyond what is in the recorded
  liquidity, and no gas spikes.
