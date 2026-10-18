# Strategy library audit (October 2026)

Which built-in strategies earn their place. Every built-in was run on a stable, a correlated and two
volatile Solidly pools on Base, and the ones that were kept are the ones that are best, or close to
the best, somewhere.

## Method

- Pools: USDC/USDT (stable, $10,000), cbETH/WETH (correlated, $10,000), WETH/USDC ($2,000) and
  WETH/AERO ($1,000). The two volatile pools are thin (fees cover about 0.6 of what arbitrage
  takes, F/LVR); no volatile Solidly pool on Base had F/LVR at or above 1.
- First pass: each strategy as shipped, quarter by quarter over two years (USDC/USDT from its first
  swap, May 2025).
- Before anything was removed, every strategy got its best chance: a walk-forward tune (Strategy lab
  › Auto, two years in eight quarters, each quarter's settings tuned with 30 runs on the half year
  before it, objective: return over holding). Each strategy is scored by the better of its re-tuned
  and its shipped settings, both only on quarters they were not tuned on. The simple-rule
  strategies were converted to node strategies first, so the optimizer could tune them.

## Result: % a year over holding, mean of 6 out-of-sample quarters (quarters ahead of holding)

| Strategy | USDC/USDT | cbETH/WETH | WETH/USDC | WETH/AERO | Kept |
|---|---:|---:|---:|---:|:---:|
| Ladder (blocks) | +4.8 (5/6) | +11.1 (6/6) | -185.5 (0/6) | -218.1 (0/6) | yes |
| Bollinger with a volatility freeze | +4.2 (5/6) | +8.0 (5/6) | -73.8 (0/6) | -150.5 (0/6) |  |
| Bollinger band range | +4.1 (5/6) | +8.0 (6/6) | -78.9 (0/6) | -112.0 (0/6) | yes |
| Keltner channel | +3.7 (5/6) | +6.8 (6/6) | -128.5 (0/6) | -166.1 (0/6) |  |
| ATR-sized range | +2.4 (4/6) | +7.1 (6/6) | -163.7 (0/6) | -193.7 (0/6) |  |
| Ichimoku base line | +3.2 (4/6) | +5.9 (6/6) | -180.0 (0/6) | -162.1 (0/6) |  |
| Donchian channel | +3.7 (5/6) | +5.2 (6/6) | -101.5 (0/6) | -143.8 (0/6) |  |
| Momentum pause (MACD) | +1.2 (3/6) | +7.4 (6/6) | -152.5 (0/6) | -190.9 (0/6) |  |
| Tight range | +1.8 (4/6) | +6.4 (4/6) | -312.8 (0/6) | -186.1 (0/6) |  |
| Trend average | +3.2 (5/6) | +4.6 (6/6) | -246.1 (0/6) | -164.6 (0/6) |  |
| Calm markets only (ADX) | +1.7 (5/6) | +5.4 (5/6) | -17.5 (2/6) | +0.0 (0/6) |  |
| Balanced | +2.2 (4/6) | +4.4 (4/6) | -293.1 (0/6) | -150.8 (0/6) |  |
| Mean-reversion tilt | +2.9 (4/6) | +3.4 (4/6) | -256.7 (0/6) | -149.6 (0/6) |  |
| Trend lean | +2.0 (4/6) | +4.2 (5/6) | -275.6 (0/6) | -1.4 (3/6) |  |
| Patient | +2.7 (4/6) | +2.7 (6/6) | -222.6 (0/6) | -92.0 (0/6) |  |
| Peg keeper | +2.4 (5/6) | +2.4 (5/6) | -68.8 (1/6) | -73.1 (0/6) |  |
| Wait out the move, with the trend | +1.8 (4/6) | +2.3 (6/6) | +11.9 (3/6) | +51.8 (5/6) |  |
| Wait out the move | +0.3 (3/6) | +1.5 (5/6) | +35.9 (4/6) | +75.1 (6/6) | yes |
| Wait out the move, watching WETH | -0.4 (2/6) | +1.1 (6/6) | +33.6 (5/6) | +74.9 (5/6) | yes |

The ladder is "Ladder (blocks)": the app's ladder as a node, identical to it in backtests (same
return, fees and moves on both pegged pools).

- Pegged and correlated pools: the ladder is best on both, ahead of every indicator range in most
  quarters. The Bollinger band range is the best of the indicator ranges on both, with a small worst
  quarter; the volatility freeze added to it changes nothing measurable, the other ranges are behind
  it on both pools.
- Volatile pools: every strategy that keeps a range in the pool loses to holding in every quarter,
  re-tuned or not (the best of them, re-tuned, still -69% to -74% a year). Only the "wait out the
  move" strategies beat holding; the daily-trend filter made the plain one worse on both.
- The optimizer's own verdict for every kept strategy is to keep its shipped settings: re-tuning
  did not give a steady edge on the quarter after.

Kept: the ladder (the app's own type, and as a block), the Bollinger band range, "Wait out the move"
and "Wait out the move, watching WETH". Removed: the simple rules (tight, balanced, patient, follow
the trend: follow cannot be a node strategy, since a range cannot depend on the position, and the
others are a fixed width the optimizer explores anyway), the ATR, Keltner, Donchian, EMA, peg,
volatility freeze, mean-reversion, ADX, Ichimoku, trend lean and MACD ranges, and the daily-trend
wait-out. Strategies saved under your own names are untouched, simple rules included.

## Logic changes tried on the survivors

| Variant | USDC/USDT | cbETH/WETH | WETH/USDC | WETH/AERO |
|---|---:|---:|---:|---:|
| Ladder, re-solved when the price leaves it | +4.4 (5/6) | +10.4 (5/6) | – | – |
| Ladder with a depeg hold | +5.3 (5/6) | +10.6 (6/6) | – | – |
| Ladder, depeg hold and re-solve when out | +4.9 (5/6) | +10.2 (5/6) | – | – |
| Wait out, a ladder inside | – | – | -34.6 (2/6) | -42.9 (2/6) |
| Wait out (WETH), a ladder inside | – | – | -30.9 (3/6) | +26.6 (3/6) |
| Directional only (never in the pool) | – | – | +33.0 (4/6) | +127.1 (6/6) |

- Re-solving the ladder as soon as the price leaves its ticks is worse on both pools: holding still
  while out of range, as the live ladder does, is the better rule (the median-centred ladder is
  usually where the price returns).
- A depeg hold (no action while the price is more than 15 ticks from its 7-day median) is noise:
  +0.5 on one pool, -0.5 on the other.
- A ladder inside "wait out the move" is much worse than its ±5σ band.
- "Directional only" never provides liquidity: it holds the token the two-week breakout favours.
  It matches the best wait-out strategy on WETH/USDC and beats it on WETH/AERO (+127% vs +75% a
  year, 6 of 6 quarters). On these thin pools the time spent in the pool costs money: the wait-out
  strategies earn from which token they hold, not from fees. It is a trading strategy rather than
  liquidity provision, on two pools and six quarters, so it is not in the library.
