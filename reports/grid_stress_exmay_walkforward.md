# Walk-forward on `grid_stress_exmay`

250 configs, 631 days (2025-01-18 to 2026-10-16), capital $15,000.

## Out-of-sample result per walk-forward setting

|   train_d |   test_d |   plateau | lp_only   |   top_n |    pnl |   apr_pct |   max_drawdown |   worst_day |   days |   positive_days_pct |   switches |
|----------:|---------:|----------:|:----------|--------:|-------:|----------:|---------------:|------------:|-------:|--------------------:|-----------:|
|        14 |        7 |         0 | False     |       1 | 213.64 |      0.84 |         -93.61 |      -25.1  |    617 |               50.57 |         64 |
|        14 |        7 |         5 | False     |       1 | 191.95 |      0.76 |        -108.45 |      -25.66 |    617 |               51.22 |         65 |
|        14 |        7 |         0 | True      |       1 | 196.43 |      0.77 |        -121.95 |      -25.1  |    617 |               54.94 |         66 |
|        14 |        7 |         0 | True      |       3 | 190.06 |      0.75 |        -120.9  |      -25.1  |    617 |               56.56 |         74 |
|        14 |        7 |         5 | True      |       3 | 199.46 |      0.79 |        -130.71 |      -25.66 |    617 |               56.08 |         71 |
|        28 |        7 |         0 | False     |       1 | 292.64 |      1.18 |         -63.28 |      -25.1  |    603 |               52.24 |         54 |
|        28 |        7 |         5 | False     |       1 | 307.9  |      1.24 |         -66.61 |      -25.1  |    603 |               52.57 |         50 |
|        28 |        7 |         0 | True      |       1 | 270.37 |      1.09 |         -90.28 |      -25.1  |    603 |               56.55 |         55 |
|        28 |        7 |         0 | True      |       3 | 261.98 |      1.06 |        -105.42 |      -25.1  |    603 |               57.05 |         65 |
|        28 |        7 |         5 | True      |       3 | 245.07 |      0.99 |        -116.73 |      -25.1  |    603 |               57.21 |         62 |
|        56 |       14 |         0 | False     |       1 | 202.66 |      0.86 |         -84.49 |      -24.77 |    575 |               52.17 |         27 |
|        56 |       14 |         5 | False     |       1 | 243.34 |      1.03 |         -86.4  |      -24.77 |    575 |               54.61 |         26 |
|        56 |       14 |         0 | True      |       1 | 199.92 |      0.85 |         -90.91 |      -24.77 |    575 |               58.26 |         27 |
|        56 |       14 |         0 | True      |       3 | 250.28 |      1.06 |         -80.67 |      -24.59 |    575 |               58.96 |         30 |
|        56 |       14 |         5 | True      |       3 | 288.33 |      1.22 |         -88.92 |      -24.59 |    575 |               59.13 |         27 |
|        91 |       28 |         0 | False     |       1 | 210.58 |      0.95 |         -73.44 |      -25.5  |    540 |               48.7  |         12 |
|        91 |       28 |         5 | False     |       1 | 212.13 |      0.96 |         -71.88 |      -25.5  |    540 |               50.19 |         12 |
|        91 |       28 |         0 | True      |       1 | 259.6  |      1.17 |         -73.44 |      -25.5  |    540 |               61.67 |         13 |
|        91 |       28 |         0 | True      |       3 | 227.15 |      1.02 |         -70.09 |      -24.59 |    540 |               62.04 |         16 |
|        91 |       28 |         5 | True      |       3 | 243.06 |      1.1  |         -70.09 |      -28.49 |    540 |               62.59 |         16 |

## Walk-forward within each family (56d train / 14d test)

| family          |   configs |    pnl |   apr_pct |   max_drawdown |   worst_day |   days |   positive_days_pct |   best_in_hindsight |   median_config |
|:----------------|----------:|-------:|----------:|---------------:|------------:|-------:|--------------------:|--------------------:|----------------:|
| ladder          |        24 | 303.31 |      1.28 |         -67.19 |      -22    |    575 |               59.48 |              278.64 |          236.55 |
| static          |         8 | 299.5  |      1.27 |         -47.96 |      -26.03 |    575 |               56    |              286.97 |          241.54 |
| recenter        |        37 | 226.96 |      0.96 |         -77.13 |      -17.87 |    575 |               59.65 |              176.08 |         -143.23 |
| recenter_noswap |        36 | 161.16 |      0.68 |         -96.83 |      -23.89 |    575 |               64.17 |              234.36 |          -15.18 |
| volband         |        72 | 110.54 |      0.47 |        -142.52 |      -26.64 |    575 |               58.61 |              154.46 |         -447.93 |
| volband_noswap  |        72 |  41.63 |      0.18 |        -201    |      -28.26 |    575 |               60.17 |               94.32 |          -23.2  |
| hold_usdc       |         1 |   0    |      0    |           0    |        0    |    575 |                0    |                0    |            0    |

Probability of backtest overfitting (CSCV, 10 blocks): **0.02** (share of splits where the in-sample best config lands in the bottom half out of sample).

## Configs chosen (56d train / 14d test, LP-only, top-3 ensemble), most frequent

|                                                                                                                                        |   windows chosen |
|:---------------------------------------------------------------------------------------------------------------------------------------|-----------------:|
| static(k=8)                                                                                                                            |               12 |
| static(k=12)                                                                                                                           |               12 |
| recenter(k=200,dwell=1,cooldown=0,swap=True)                                                                                           |                8 |
| static(k=20)                                                                                                                           |                8 |
| recenter(k=8,dwell=288,cooldown=0,swap=True)                                                                                           |                8 |
| ladder(lookback=86400,span=3,every=72,turnover=0.4)                                                                                    |                7 |
| static(k=5)                                                                                                                            |                7 |
| static(k=0)                                                                                                                            |                5 |
| ladder(lookback=86400,span=6,every=72,turnover=0.4)                                                                                    |                5 |
| static(k=1)                                                                                                                            |                5 |
| volband(vol=atr,lookback=2016,z=2,kmin=0,kmax=20,anchor=median,anchor_lb=2016,every=12,move=2,dwell=72,cooldown=72,swap=False,limit=3) |                3 |
| ladder(lookback=259200,span=6,every=72,turnover=0.4)                                                                                   |                3 |
| ladder(lookback=604800,span=3,every=288,turnover=0.15)                                                                                 |                3 |
| ladder(lookback=604800,span=3,every=288,turnover=0.4)                                                                                  |                3 |
| ladder(lookback=86400,span=3,every=72,turnover=0.15)                                                                                   |                3 |

## In-hindsight top configs (full period — biased, for reference only)

|                                                        |    pnl |
|:-------------------------------------------------------|-------:|
| static(k=8)                                            | 284.91 |
| static(k=12)                                           | 283.74 |
| ladder(lookback=259200,span=6,every=288,turnover=0.4)  | 267.41 |
| ladder(lookback=259200,span=3,every=288,turnover=0.4)  | 264.5  |
| static(k=5)                                            | 261.74 |
| static(k=20)                                           | 259.53 |
| ladder(lookback=259200,span=3,every=72,turnover=0.4)   | 250.58 |
| ladder(lookback=259200,span=6,every=72,turnover=0.4)   | 248.95 |
| ladder(lookback=604800,span=6,every=288,turnover=0.15) | 247.61 |
| ladder(lookback=604800,span=6,every=72,turnover=0.15)  | 245.63 |
| ladder(lookback=259200,span=6,every=288,turnover=0.15) | 238.92 |
| ladder(lookback=259200,span=3,every=288,turnover=0.15) | 236.89 |
| ladder(lookback=86400,span=3,every=72,turnover=0.4)    | 233.39 |
| recenter(k=8,dwell=288,cooldown=0,swap=False,limit=9)  | 232.31 |
| ladder(lookback=604800,span=6,every=288,turnover=0.4)  | 229.83 |
