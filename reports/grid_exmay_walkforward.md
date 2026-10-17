# Walk-forward on `grid_exmay`

250 configs, 631 days (2025-01-18 to 2026-10-16), capital $15,000.

## Out-of-sample result per walk-forward setting

|   train_d |   test_d |   plateau | lp_only   |   top_n |    pnl |   apr_pct |   max_drawdown |   worst_day |   days |   positive_days_pct |   switches |
|----------:|---------:|----------:|:----------|--------:|-------:|----------:|---------------:|------------:|-------:|--------------------:|-----------:|
|        14 |        7 |         0 | False     |       1 | 318.13 |      1.25 |         -76.47 |      -25.1  |    617 |               53.48 |         65 |
|        14 |        7 |         5 | False     |       1 | 350.21 |      1.38 |         -74.57 |      -25.1  |    617 |               53.97 |         65 |
|        14 |        7 |         0 | True      |       1 | 308.85 |      1.22 |        -100.64 |      -25.1  |    617 |               57.86 |         68 |
|        14 |        7 |         0 | True      |       3 | 330.92 |      1.31 |         -99.66 |      -25.1  |    617 |               57.54 |         74 |
|        14 |        7 |         5 | True      |       3 | 349.82 |      1.38 |        -102.36 |      -25.1  |    617 |               57.54 |         72 |
|        28 |        7 |         0 | False     |       1 | 394.94 |      1.59 |         -72.72 |      -25.1  |    603 |               55.89 |         54 |
|        28 |        7 |         5 | False     |       1 | 380.28 |      1.53 |         -81.75 |      -25.1  |    603 |               55.39 |         51 |
|        28 |        7 |         0 | True      |       1 | 402.43 |      1.62 |         -73.61 |      -25.1  |    603 |               59.87 |         55 |
|        28 |        7 |         0 | True      |       3 | 362.53 |      1.46 |         -81.01 |      -25.1  |    603 |               59.2  |         64 |
|        28 |        7 |         5 | True      |       3 | 352.3  |      1.42 |         -89.57 |      -25.1  |    603 |               59.2  |         64 |
|        56 |       14 |         0 | False     |       1 | 395.11 |      1.67 |         -67.28 |      -24.4  |    575 |               55.65 |         28 |
|        56 |       14 |         5 | False     |       1 | 434.3  |      1.84 |         -64.23 |      -24.4  |    575 |               56    |         26 |
|        56 |       14 |         0 | True      |       1 | 392.75 |      1.66 |         -71.98 |      -24.4  |    575 |               61.91 |         27 |
|        56 |       14 |         0 | True      |       3 | 391.84 |      1.66 |         -77.11 |      -22.67 |    575 |               62.09 |         32 |
|        56 |       14 |         5 | True      |       3 | 448.48 |      1.9  |         -78.74 |      -22.67 |    575 |               62.43 |         28 |
|        91 |       28 |         0 | False     |       1 | 325.69 |      1.47 |         -61.52 |      -25.51 |    540 |               53.15 |         11 |
|        91 |       28 |         5 | False     |       1 | 328.22 |      1.48 |         -61.52 |      -25.51 |    540 |               53.15 |         11 |
|        91 |       28 |         0 | True      |       1 | 375.46 |      1.69 |         -61.52 |      -25.51 |    540 |               65.19 |         13 |
|        91 |       28 |         0 | True      |       3 | 372.45 |      1.68 |         -64.22 |      -24.59 |    540 |               64.63 |         15 |
|        91 |       28 |         5 | True      |       3 | 359.56 |      1.62 |         -65.64 |      -24.59 |    540 |               64.63 |         15 |

## Walk-forward within each family (56d train / 14d test)

| family          |   configs |    pnl |   apr_pct |   max_drawdown |   worst_day |   days |   positive_days_pct |   best_in_hindsight |   median_config |
|:----------------|----------:|-------:|----------:|---------------:|------------:|-------:|--------------------:|--------------------:|----------------:|
| ladder          |        24 | 510.6  |      2.16 |         -44.05 |      -22.05 |    575 |               63.65 |              519.85 |          452.09 |
| recenter        |        37 | 370.92 |      1.57 |         -75.82 |      -17.48 |    575 |               61.22 |              265.66 |           75.06 |
| static          |         8 | 354.99 |      1.5  |         -47.75 |      -26.03 |    575 |               57.57 |              340.77 |          286.27 |
| volband         |        72 | 319.52 |      1.35 |         -97.55 |      -22.84 |    575 |               63.3  |              323.21 |           16.7  |
| recenter_noswap |        36 | 313.69 |      1.33 |         -93.27 |      -23.89 |    575 |               66.43 |              300.18 |           34.41 |
| volband_noswap  |        72 | 186.66 |      0.79 |        -146.26 |      -23.31 |    575 |               61.39 |              183.12 |           56.56 |
| hold_usdc       |         1 |   0    |      0    |           0    |        0    |    575 |                0    |                0    |            0    |

Probability of backtest overfitting (CSCV, 10 blocks): **0.01** (share of splits where the in-sample best config lands in the bottom half out of sample).

## Configs chosen (56d train / 14d test, LP-only, top-3 ensemble), most frequent

|                                                       |   windows chosen |
|:------------------------------------------------------|-----------------:|
| ladder(lookback=86400,span=3,every=72,turnover=0.4)   |               14 |
| ladder(lookback=86400,span=6,every=72,turnover=0.4)   |               11 |
| static(k=8)                                           |                8 |
| static(k=12)                                          |                8 |
| static(k=20)                                          |                7 |
| recenter(k=200,dwell=1,cooldown=0,swap=True)          |                7 |
| ladder(lookback=86400,span=3,every=72,turnover=0.15)  |                6 |
| static(k=0)                                           |                5 |
| static(k=5)                                           |                5 |
| ladder(lookback=604800,span=6,every=72,turnover=0.15) |                4 |
| static(k=1)                                           |                4 |
| ladder(lookback=259200,span=6,every=72,turnover=0.4)  |                3 |
| ladder(lookback=86400,span=6,every=288,turnover=0.4)  |                3 |
| ladder(lookback=604800,span=3,every=72,turnover=0.15) |                3 |
| ladder(lookback=86400,span=6,every=72,turnover=0.15)  |                3 |

## In-hindsight top configs (full period — biased, for reference only)

|                                                        |    pnl |
|:-------------------------------------------------------|-------:|
| ladder(lookback=86400,span=3,every=72,turnover=0.4)    | 513.83 |
| ladder(lookback=86400,span=6,every=72,turnover=0.4)    | 508.58 |
| ladder(lookback=86400,span=6,every=288,turnover=0.4)   | 485.07 |
| ladder(lookback=86400,span=3,every=288,turnover=0.4)   | 477.89 |
| ladder(lookback=86400,span=3,every=72,turnover=0.15)   | 477.84 |
| ladder(lookback=86400,span=6,every=288,turnover=0.15)  | 467.95 |
| ladder(lookback=86400,span=6,every=72,turnover=0.15)   | 466.28 |
| ladder(lookback=86400,span=3,every=288,turnover=0.15)  | 461.28 |
| ladder(lookback=259200,span=6,every=288,turnover=0.4)  | 452.05 |
| ladder(lookback=259200,span=3,every=288,turnover=0.4)  | 447.81 |
| ladder(lookback=259200,span=3,every=72,turnover=0.4)   | 441.02 |
| ladder(lookback=259200,span=6,every=72,turnover=0.4)   | 439.91 |
| ladder(lookback=259200,span=6,every=288,turnover=0.15) | 439.81 |
| ladder(lookback=259200,span=6,every=72,turnover=0.15)  | 438.44 |
| ladder(lookback=259200,span=3,every=72,turnover=0.15)  | 437.67 |
