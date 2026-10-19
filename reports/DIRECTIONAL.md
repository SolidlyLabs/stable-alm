# LP as trading: wait out the move, LP after it

Date: 18 Oct 2026. Code (removed; it is in the git history up to tag v0.4.0): `research/directional/` (simulator `dsim.py`, strategies `dstrat.py`, grids
`grid.py` / `grid2.py` / `grid3.py`, scoring `score.py` / `score3.py`, neighbourhood `neigh.py`, real-swap
check `onchain.py`, same-window check `window.py`). Goal as set: maximise what the LP has at the end, not
time in range; good for the protocol too (fees and liquidity growing).

## Verdict

**One rule works, on volatile pools that pay at least their arbitrage loss (F/LVR ≥ 1):**

> Farm a wide band (±5 daily sigmas) all the time, except right after a new 2-week high or low: then
> take everything out and hold the token the move favours (up: the volatile token; down: the stable),
> for 24 hours after the last new extreme. Re-enter the band when the move stalls.
> Capital waiting outside is parked in a pegged pool (stable in USDC/USDT, ETH in cbETH/WETH or
> wstETH/WETH, wS in wS/stS) until the main pool needs it again.

That is Combo(n=336, hot_h=24, min_lp=0, trend_lp=1, k=5) in `dstrat.py`.

| Fee/LVR of the pool | Passive band vs hold | Combo vs hold | Worst calendar year | Capital in the pool |
|---|---|---|---|---|
| 0.55 (Solidly WETH/USDC) | −23.5%/yr | +4.6%/yr | +1.8 | 72% |
| 1.0 (deep Uniswap) | −15.6%/yr | +13.5%/yr | +11.6 | 72% |
| 2.0 | +2.2%/yr | +33.2%/yr | +24.1 | 72% |

ETH/USD hourly, Jan 2024 – Oct 2026 (2.7 years), synthetic fees, 5 bps swaps, gas, parking at 5% a year.
"vs hold" is against an unparked 50/50 hold.

It is positive in every calendar year at every fee level. It keeps about three quarters of the capital
in the pool, and its fee income is close to the passive band's: 20% vs 17% a year at F/LVR 1. So it is
good for the LP and the protocol alike. The time it spends out is the time the pool pays least relative to
what arbitrage takes.

**On real swaps** (19 Jul – 17 Oct 2026; ETH +31% but choppy; no parking):

| Pool | Passive ±5σ | Combo n336 hot24 | Combo n720 hot24 |
|---|---|---|---|
| Uniswap WETH/USDC 0.05% | −2.4% | **+54.0%** | +29.6% |
| Uniswap WETH/USDC 0.30% | −7.4% | **+39.7%** | +14.0% |
| Solidly WETH/USDC | −44.9% | **+16.7%** | −29.4% |

The same quarter on Binance prices with synthetic fees gives +21% (F/LVR 0.55) and +28% (F/LVR 1) for the
n336 setting, in line with the real swaps. One quarter is one sample. The 2.7-year table is the evidence;
this check shows that the real-swap mechanics do not break it.

Aerodrome CL50 is excluded. Its liquidity emptied several times (Jul 20, Jul 28, Aug 6, Aug 17), and the
price ran to absurd ticks; on Aug 7 it sat 17% under the market for hours. The simulator traded at those
prices. The app swaps through the aggregator at the market price, so those losses are not real. The
Aero numbers in `reports/VOLATILE.md` may carry the same artefact.

## What was tried, and what lost

- **Passive band (control):** −16%/yr vs hold at F/LVR 1, −24% at 0.55, about 0 at 2. LPing a volatile
  pair through big moves gives the arbitrageurs the move.
- **Pure trend switch** (all in the winner after a weekly breakout, no LP): the best fixed config is
  +51%/yr over 2.7 years, but walk-forward re-picking is unstable (95% interval ±60). On the real-swap
  quarter it lost 36–54%/yr: a whipsaw. It is a directional bet, not LPing, and gives the protocol
  nothing.
- **Directional ranges** (a range above the price in an uptrend to sell into it, below it in a downtrend,
  or a skewed band): lose to both the band and the switch. A range that sells into a rally is a covered
  call. It caps the upside the strategy was trying to catch.
- **Volatility exit** (out when the last day's volatility jumps): loses. By the time volatility shows,
  the move has happened.
- **Blend** (pool share falls as the trend strengthens): sits between band and switch. Each 25% kept in
  the pool costs 1.5–3 points a year at F/LVR ≤ 1.
- **Combo, longer hot windows** (out for a week or two after each extreme): earns more on paper
  (+48%/yr), but with 1–25% in the pool it is the trend switch again.

Neighbourhood of the winner at F/LVR 1 (park 5%): excess %/yr (worst calendar year), share in the pool.

| hot_h \ n | 168 h | 336 h | 720 h |
|---|---|---|---|
| 24 h | 26.1 (4.0) 0.61 | **13.5 (11.6) 0.72** | 9.9 (5.3) 0.83 |
| 72 h | 31.1 (2.4) 0.31 | 13.4 (−0.3) 0.50 | 9.3 (5.5) 0.69 |
| 168 h | 48.4 (35.9) 0.07 | 27.5 (9.2) 0.25 | 18.9 (−9.7) 0.51 |

The surface is smooth, with no lone spike, and the trade-off is plain: the longer it stays out, the more it
trades and the less it farms. For the "good for both" brief, the row hot_h = 24 is the zone. n = 336 is
the steadiest year to year, and n = 720 keeps more in the pool (83%).

## Parking

Idle capital earns a pegged pool's APR while it waits (`Costs.park0/park1` in `dsim.py`; moving in or out
costs 1 bp and one gas). Because the Combo waits outside only about a quarter of the time, parking at 5%
adds about 1–2 points a year. It also keeps that capital inside the protocol as liquidity in the pegged
pool. For a strategy that waits more (blend, trend switch), parking at 5% adds 5–10 points.

Realistic rates now (24-hour realised APR from the dashboard; parking $10k dilutes them):

| Parking pool | TVL | APR 24h | After adding $10k |
|---|---|---|---|
| USDC/USDT, Base | $48k | 7.0% | ~5.8% |
| cbETH/WETH, Base | $17k | 25% | ~16% |
| wstETH/WETH, Optimism | $14k | 14% | ~8% |
| wS/stS, Sonic | $0.2k | 0.5% | ~0 |

For Sonic wS/Anon the wS leg has nowhere useful to park today: wS/stS has $184 TVL and $15 of daily
volume.

## Which pools qualify (F/LVR from the dashboard, last 7 days)

| Pool | F/LVR | Fit |
|---|---|---|
| wS/Anon, Sonic | 4.4 | yes: the farm zone, where the rule should do best (not yet tested; no long Anon history) |
| SOLID/WETH, Ethereum | 1.7 | borderline, $537 TVL |
| WETH/USDC, Arbitrum | 0.99 | borderline |
| WETH/USDC, Base | 0.67 | no: about break-even at best |
| wS/WETH, wS/USDC.e, USDC/WETH (Ethereum, Optimism) | 0.1–0.5 | no |

Stable and pegged pairs (F/LVR in the hundreds or thousands) need none of this. The ladder stays.

## How it would go into the app

1. **A strategy block** "Wait out the move": inputs are the channel length (n hours), the hot window, and
   the band width in sigmas. Output is the pool share and the side to wait in. It is built on the
   existing graph nodes (price channel, band), so it backtests in the Strategy lab like any other.
2. **A gate on F/LVR:** the block runs only while the pool's 7-day F/LVR is ≥ 1. Below that, it holds
   or parks (the pool does not pay for the arbitrage it takes).
3. **Parking with a separate ledger:** opt-in per strategy, with a whitelist of pegged pools per token
   (USDC → USDC/USDT; WETH → cbETH/WETH; wS → wS/stS). Parked capital is tracked as lots tagged with
   the pool it came from. When the main pool needs it, only those lots come back, and any pre-existing
   position in the parking pool stays untouched. Each move costs gas and a pegged swap, so a park is
   worth it only when the expected wait × APR clears the round-trip cost (a day at 6% on $5k is
   $0.80; Base gas is cents, so it clears).

## Other timeframes and other tokens

The app now runs any indicator on longer candles (15 min to 1 day, built in memory from the stored
5-minute bars, closed candles only) and can watch another token's price (its USD price from Solidly
pools with at least $500 of active liquidity, through a paired token when there is no stable pool;
a `stale` output is true while there is no reliable price). Two uses were tested.

**Longer candles for the breakout itself** (research/directional/grid4.py, score4.py; 2.7 years of
ETH, parking at 5%): reading the 2-week breakout on 4-hour or daily closes instead of hourly ones
costs more than the false breakouts it filters. The reference config (n = 336 h, hot 24 h, k = 5)
at F/LVR 1, excess over hold per year: hourly +13.5, 4-hour +8.4, daily −4.8. The confirmation lag
gives the first part of every move to arbitrage.

**A slow trend filter** (a move is waited out only when a slower trend agrees; against it the
capital stays in the pool): on 2.7 years of ETH a 30-day filter lowers the reference config
(+5.7 vs +13.5 at F/LVR 1) but makes the family steadier under re-tuning. With walk-forward
re-picking it scores +12.9 (−16 to +39) vs −4.6 (−41 to +27) unfiltered, with a lower PBO
(0.12 vs 0.39) and more capital in the pool (81% vs 72%). The intervals overlap: a lead, not a result.

**In the app's own backtester** (real Base swaps, 90 days, $1,000; excess over holding):

| Strategy | WETH/USDC | AERO/WETH |
|---|---|---|
| Wait out the move | +5.3% | −5.3% |
| … with the trend (2-week trend on daily candles) | +6.3% | −1.9% |
| … watching WETH (WETH's 7-day highs and lows count as a move too) | +7.9% | +5.4% |

The WETH watch helps most on the pool of a token that moves with ETH but trades thinly: WETH's own
breakout arrives before AERO/WETH's. Both variants are in the library as examples ("Wait out the
move, with the trend" and "Wait out the move, watching WETH"). One quarter on two pools is too
little to prefer them over the plain one.

**Not possible yet:** BTC on Base. cbBTC's only Solidly pool with WETH has $68 of active liquidity,
under the $500 rule, so it is listed as not watchable. A price from another DEX or an oracle would
be needed for that.

## Limits

- One asset (ETH) over 2.7 years. The fee model is synthetic (fees = F/LVR × LVR), with F/LVR held
  constant; real F/LVR moves, and during big moves it may differ from its average. The real-swap check
  covers one quarter. BTC and older ETH history would test robustness (that needs a download).
- The parameters were picked on the same 2.7 years they are scored on. The smooth neighbourhood and
  the every-year result guard against overfitting, but they do not replace a hold-out period.
- Fee dilution by our own size and volume elasticity are not in the synthetic run (they are in the
  real-swap run and in the app's backtester).
