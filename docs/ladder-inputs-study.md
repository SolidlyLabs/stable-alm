# Ladder inputs: design and backtest study

## Design

Each ladder setting now has an input with the same name: `hours`, `span`, `every_hours`, `turnover`, `guard` and `width`. When an input is connected, its value at each decision replaces the setting. The value is clamped to the setting's limits, and while the input is still warming up the setting applies. With nothing connected, the ladder behaves exactly as before.

- **`hours`.** The median is taken at the decision, over the window the input gives in whole 5-minute bars. The block keeps the closing prices for this, and its lookback grows to the 168 h maximum so any window is available.
- **`span`, `turnover`, `width`.** These feed the solve (`graph::ladder_at` → `ladder::Params`), both in the live engine (`run_custom`) and in the backtest.
- **`every_hours`, `guard`.** These feed the re-solve schedule and the depeg guard in `decide`.
- The live log shows `ladder inputs: window … h` whenever the inputs move a setting away from the block's own.

Example: a 6 h window by default that lengthens to 24 h while the price is more than 6 ticks from its 24 h median.

```
price ─► median 24h ─► distance_gt(6) ─► switch(a=24, b=6) ─► ladder.hours
```

## Study

- Harness: `cargo test --release strategy_study -- --ignored`, set up with the environment variables SPEC, POOLS, PERIODS and OUT.
- Pools: usdc-usdt and cbeth-weth on Base.
- Periods: three 90-day periods (the last 90 days, 90–180 days ago and 180–270 days ago), $10k each.
- Costs and limits as on the live app: swap costs, gas, and the daily cap of 5 rebalances.
- Baseline: your current settings (24 h, span 3, every 1 h, turnover 0.4, guard 15).
- Result measure: PnL in basis points of capital, compared with the baseline.
- 55 variants in two rounds.

### Fixed windows

Change in PnL against the 24 h baseline, in bp of capital, for each 90-day period (most recent first):

| window | cbeth-weth | usdc-usdt | sum | periods better |
|---|---|---|---|---|
| 4 h | +61 / +171 / +107 | +41 / 0 / 0 | +381 | 4/6 |
| 6 h | +68 / +151 / +102 | +37 / −8 / +2 | +351 | 5/6 |
| 8 h | +64 / +102 / +96 | +35 / +2 / +3 | +303 | **6/6** |
| 12 h | +53 / +106 / +86 | +34 / 0 / +3 | +282 | 5/6 |
| 16 h | +38 / +51 / +39 | +33 / −1 / +4 | +165 | 5/6 |
| 24 h (current) | 0 | 0 | 0 | — |
| 48 h | −87 / −149 / −102 | −48 / +25 / −4 | −365 | 1/6 |
| 72 h | −124 / −232 / −154 | −67 / +38 / −8 | −547 | 1/6 |

Shorter windows keep the range closer to the price: time in range rises from 80–91% to 88–98%. The extra fees easily pay for twice as many rebalances. The one exception is usdc-usdt 90–180 days ago, a slow drift where the longer windows did better.

### Indicator-driven window

Seven conditions were tested, alone and combined:

- distance from the 24 h median (> 3 or > 6 ticks);
- volatility ratio, Parkinson 6 h / 72 h > 1.5 or 2 h / 24 h > 2;
- efficiency ratio 6 h > 0.4;
- out of range;
- |z-score 24 h| > 2.

- **Short window while the signal holds, long otherwise** (6/24 h and 4/48 h): worse than the fixed windows every time, and the 4/48 h pairs are worse than the baseline (−250 to −400 bp). Shortening the window during a move makes the ladder chase the move: it re-centres right at the extreme and then has to come back.
- **Long window while the signal holds, short otherwise:** this is the useful direction. Holding the centre through a jump and following quickly the rest of the time is the one design that beat its own fixed short window.

| variant | sum vs current | periods better | vs its fixed short window |
|---|---|---|---|
| 24 h on distance > 6, else 4 h | +390 | 5/6 | +9 vs 4 h |
| 12 h on distance > 6, else 6 h | +371 | 5/6 | +20 vs 6 h |
| 24 h on distance > 6, else 6 h | +368 | 5/6 | +17 vs 6 h |
| 12 h on distance > 3, else 6 h | +355 | 5/6 | +4 vs 6 h |
| 24 h on ER > 0.4 and distance > 3, else 6 h | +350 | 5/6 | −1 vs 6 h |
| volatility, z-score or out of range as the switch | +188 to +330 | 4–6/6 | −20 to −160 |

### Other settings from inputs (on a 6 h window)

- **span 2 or 4, or span 6/3 on volatility:** within ±10 bp of 6 h alone. The span doesn't matter much.
- **turnover 0.2:** best on cbeth-weth (+40 over 6 h), but about −35 on usdc-usdt and 300 actions in 90 days. Not robust.
- **turnover 0.6:** −125. It moves too little.
- **re-solve every 15 min while distance > 3, else every hour:** +6 over 6 h. Negligible.

## Conclusions

1. **The big lever is the window length, not the indicators.** Going from 24 h to 4–8 h adds about 50–170 bp per 90 days on cbeth-weth and about 35–40 bp on usdc-usdt in the recent period.
   - 8 h is the most consistent choice: better in every one of the 6 runs.
   - It costs about 1.5–2× the rebalances. These runs include gas and swap costs, and the daily cap of 5 is already binding.
2. **An indicator adds at most about 10–20 bp over a fixed short window**, and only one family does:
   - the signal is distance from the 24 h median;
   - it must lengthen the window during a jump, not shorten it.

   That is within the noise of six runs, so I wouldn't count on it yet.
3. **Don't drive the window with volatility, z-score or out-of-range signals.** They trail the fixed short window, and shortening the window on any signal loses money.

**Recommendation:** for usdc-usdt, set `hours` to 8 (or 6). If you want the adaptive version, use "24 h while distance > 6 ticks from the 24 h median, else 6 h". The inputs make that possible now, but its edge over a plain 6–8 h window has not been shown to be reliable.

The raw results are in the session scratchpad (`study/full.json`, `study/r2.json`). The strategy generator is `study/make_spec.py`.
