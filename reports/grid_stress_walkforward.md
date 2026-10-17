# Walk-forward on `grid_stress`

250 configs, 637 days (2025-01-18 to 2026-10-16), capital $15,000.

## Out-of-sample result per walk-forward setting

|   train_d |   test_d |   plateau | lp_only   |   top_n |     pnl |   apr_pct |   max_drawdown |   worst_day |   days |   positive_days_pct |   switches |
|----------:|---------:|----------:|:----------|--------:|--------:|----------:|---------------:|------------:|-------:|--------------------:|-----------:|
|        14 |        7 |         0 | False     |       1 |  214.81 |      0.84 |        -120.57 |      -25.1  |    623 |               51.36 |         66 |
|        14 |        7 |         5 | False     |       1 |  233.97 |      0.91 |        -114.85 |      -25.1  |    623 |               51.69 |         62 |
|        14 |        7 |         0 | True      |       1 |  625.22 |      2.44 |        -104.85 |      -25.1  |    623 |               55.7  |         70 |
|        14 |        7 |         0 | True      |       3 |  575.87 |      2.25 |         -94.63 |      -25.1  |    623 |               56.34 |         73 |
|        14 |        7 |         5 | True      |       3 |  550.49 |      2.15 |        -113.35 |      -25.1  |    623 |               55.54 |         70 |
|        28 |        7 |         0 | False     |       1 |  255.4  |      1.02 |        -129.62 |      -25.1  |    609 |               51.72 |         53 |
|        28 |        7 |         5 | False     |       1 |  269.1  |      1.08 |        -127.12 |      -25.1  |    609 |               51.4  |         52 |
|        28 |        7 |         0 | True      |       1 |  669.78 |      2.68 |        -107.66 |      -25.1  |    609 |               56.49 |         54 |
|        28 |        7 |         0 | True      |       3 |  575.61 |      2.3  |        -127.65 |      -25.1  |    609 |               56.65 |         66 |
|        28 |        7 |         5 | True      |       3 |  586.78 |      2.34 |        -133.79 |      -25.1  |    609 |               56.65 |         63 |
|        56 |       14 |         0 | False     |       1 |  250.32 |      1.05 |         -83.47 |      -25.05 |    581 |               54.04 |         27 |
|        56 |       14 |         5 | False     |       1 |  276.99 |      1.16 |         -77.15 |      -25.05 |    581 |               53.7  |         25 |
|        56 |       14 |         0 | True      |       1 |  277.73 |      1.16 |         -83.47 |      -25.05 |    581 |               59.04 |         29 |
|        56 |       14 |         0 | True      |       3 |  290.56 |      1.22 |         -87.75 |      -24.87 |    581 |               60.93 |         29 |
|        56 |       14 |         5 | True      |       3 | 1348.48 |      5.65 |         -86.23 |      -24.87 |    581 |               59.21 |         27 |
|        91 |       28 |         0 | False     |       1 |  182.57 |      0.81 |         -78.83 |      -25.5  |    546 |               46.15 |         12 |
|        91 |       28 |         5 | False     |       1 |  193.59 |      0.86 |         -78.83 |      -25.5  |    546 |               46.34 |         11 |
|        91 |       28 |         0 | True      |       1 |  637.3  |      2.84 |         -78.83 |      -25.5  |    546 |               62.27 |         14 |
|        91 |       28 |         0 | True      |       3 |  558.45 |      2.49 |         -74.35 |      -24.59 |    546 |               61.54 |         15 |
|        91 |       28 |         5 | True      |       3 |  579.06 |      2.58 |         -74.35 |      -24.59 |    546 |               63    |         15 |

## Walk-forward within each family (56d train / 14d test)

| family          |   configs |     pnl |   apr_pct |   max_drawdown |   worst_day |   days |   positive_days_pct |   best_in_hindsight |   median_config |
|:----------------|----------:|--------:|----------:|---------------:|------------:|-------:|--------------------:|--------------------:|----------------:|
| ladder          |        24 | 1372.05 |      5.75 |         -83.07 |      -22    |    581 |               57.31 |             1364.64 |         1250.93 |
| volband         |        72 |  760.37 |      3.18 |        -140.37 |      -26.63 |    581 |               58.52 |              617.29 |          -42.06 |
| recenter_noswap |        36 |  555.42 |      2.33 |        -194.71 |      -24.23 |    581 |               63.17 |              451.16 |          128.47 |
| recenter        |        37 |  277.52 |      1.16 |         -88.02 |      -19.25 |    581 |               59.55 |             1033.49 |          236.76 |
| static          |         8 |  272.93 |      1.14 |         -47.6  |      -26.03 |    581 |               56.28 |              552.46 |          304.48 |
| volband_noswap  |        72 |  243.72 |      1.02 |        -188.7  |      -28.32 |    581 |               59.72 |              264.19 |          155.09 |
| hold_usdc       |         1 |    0    |      0    |           0    |        0    |    581 |                0    |                0    |            0    |

Probability of backtest overfitting (CSCV, 10 blocks): **0.04** (share of splits where the in-sample best config lands in the bottom half out of sample).

## Configs chosen (56d train / 14d test, LP-only, top-3 ensemble), most frequent

|                                                        |   windows chosen |
|:-------------------------------------------------------|-----------------:|
| static(k=8)                                            |               12 |
| static(k=12)                                           |               11 |
| static(k=5)                                            |                9 |
| recenter(k=200,dwell=1,cooldown=0,swap=True)           |                8 |
| static(k=20)                                           |                7 |
| recenter(k=8,dwell=288,cooldown=0,swap=True)           |                6 |
| static(k=1)                                            |                5 |
| static(k=0)                                            |                5 |
| ladder(lookback=86400,span=6,every=288,turnover=0.4)   |                5 |
| recenter(k=0,dwell=288,cooldown=0,swap=True)           |                4 |
| recenter(k=0,dwell=72,cooldown=0,swap=True)            |                4 |
| ladder(lookback=86400,span=3,every=72,turnover=0.4)    |                4 |
| recenter(k=8,dwell=288,cooldown=0,swap=False,limit=9)  |                3 |
| ladder(lookback=604800,span=3,every=288,turnover=0.15) |                3 |
| ladder(lookback=86400,span=6,every=72,turnover=0.4)    |                3 |

## In-hindsight top configs (full period — biased, for reference only)

|                                                        |     pnl |
|:-------------------------------------------------------|--------:|
| ladder(lookback=86400,span=6,every=288,turnover=0.4)   | 1361.08 |
| ladder(lookback=86400,span=3,every=288,turnover=0.4)   | 1356.76 |
| ladder(lookback=86400,span=3,every=72,turnover=0.4)    | 1274.71 |
| ladder(lookback=86400,span=6,every=72,turnover=0.4)    | 1270.33 |
| ladder(lookback=259200,span=3,every=72,turnover=0.15)  | 1267.99 |
| ladder(lookback=604800,span=3,every=72,turnover=0.15)  | 1263.14 |
| ladder(lookback=86400,span=3,every=72,turnover=0.15)   | 1260    |
| ladder(lookback=86400,span=6,every=288,turnover=0.15)  | 1258.25 |
| ladder(lookback=86400,span=3,every=288,turnover=0.15)  | 1256.23 |
| ladder(lookback=604800,span=6,every=72,turnover=0.15)  | 1249.09 |
| ladder(lookback=259200,span=3,every=288,turnover=0.15) | 1244.47 |
| ladder(lookback=86400,span=6,every=72,turnover=0.15)   | 1244.29 |
| ladder(lookback=604800,span=6,every=288,turnover=0.15) | 1235.68 |
| ladder(lookback=259200,span=6,every=288,turnover=0.15) | 1232.63 |
| ladder(lookback=259200,span=6,every=72,turnover=0.15)  | 1229.87 |
