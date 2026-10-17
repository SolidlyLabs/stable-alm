# Walk-forward on `grid`

250 configs, 637 days (2025-01-18 to 2026-10-16), capital $15,000.

## Out-of-sample result per walk-forward setting

|   train_d |   test_d |   plateau | lp_only   |   top_n |     pnl |   apr_pct |   max_drawdown |   worst_day |   days |   positive_days_pct |   switches |
|----------:|---------:|----------:|:----------|--------:|--------:|----------:|---------------:|------------:|-------:|--------------------:|-----------:|
|        14 |        7 |         0 | False     |       1 |  334.02 |      1.3  |         -98.99 |      -25.1  |    623 |               53.93 |         63 |
|        14 |        7 |         5 | False     |       1 |  361.93 |      1.41 |         -96.06 |      -25.1  |    623 |               52.81 |         64 |
|        14 |        7 |         0 | True      |       1 |  824.01 |      3.22 |         -96.37 |      -25.1  |    623 |               58.11 |         68 |
|        14 |        7 |         0 | True      |       3 |  792.66 |      3.1  |         -88.62 |      -25.1  |    623 |               58.11 |         73 |
|        14 |        7 |         5 | True      |       3 |  789.92 |      3.09 |         -86.1  |      -25.1  |    623 |               56.98 |         73 |
|        28 |        7 |         0 | False     |       1 |  372.01 |      1.49 |         -99.52 |      -25.1  |    609 |               54.19 |         51 |
|        28 |        7 |         5 | False     |       1 |  367.12 |      1.47 |        -106.3  |      -25.1  |    609 |               53.86 |         55 |
|        28 |        7 |         0 | True      |       1 |  874.16 |      3.49 |         -84.54 |      -25.1  |    609 |               58.78 |         54 |
|        28 |        7 |         0 | True      |       3 |  781.23 |      3.12 |         -90.37 |      -25.1  |    609 |               58.95 |         65 |
|        28 |        7 |         5 | True      |       3 |  755.61 |      3.02 |         -99.5  |      -25.1  |    609 |               59.44 |         64 |
|        56 |       14 |         0 | False     |       1 |  424.74 |      1.78 |         -64.06 |      -22.05 |    581 |               54.56 |         22 |
|        56 |       14 |         5 | False     |       1 |  434.33 |      1.82 |         -82.75 |      -25.79 |    581 |               54.73 |         23 |
|        56 |       14 |         0 | True      |       1 |  442.56 |      1.85 |         -66.36 |      -22.05 |    581 |               59.72 |         23 |
|        56 |       14 |         0 | True      |       3 |  423.41 |      1.77 |         -81.84 |      -22.18 |    581 |               60.59 |         29 |
|        56 |       14 |         5 | True      |       3 | 1712.67 |      7.17 |         -85.38 |      -24.87 |    581 |               60.24 |         28 |
|        91 |       28 |         0 | False     |       1 |  347.97 |      1.55 |         -63.87 |      -25.51 |    546 |               49.45 |          9 |
|        91 |       28 |         5 | False     |       1 |  332.17 |      1.48 |         -63.87 |      -25.51 |    546 |               49.45 |          9 |
|        91 |       28 |         0 | True      |       1 |  891.22 |      3.97 |         -63.87 |      -25.51 |    546 |               64.47 |         12 |
|        91 |       28 |         0 | True      |       3 |  785.96 |      3.5  |         -64.98 |      -24.59 |    546 |               64.84 |         15 |
|        91 |       28 |         5 | True      |       3 |  787.6  |      3.51 |         -65.64 |      -24.59 |    546 |               64.84 |         15 |

## Walk-forward within each family (56d train / 14d test)

| family          |   configs |     pnl |   apr_pct |   max_drawdown |   worst_day |   days |   positive_days_pct |   best_in_hindsight |   median_config |
|:----------------|----------:|--------:|----------:|---------------:|------------:|-------:|--------------------:|--------------------:|----------------:|
| ladder          |        24 | 1847.56 |      7.74 |         -57.37 |      -22.05 |    581 |               61.79 |             1879.42 |         1668.2  |
| volband         |        72 | 1155.8  |      4.84 |         -88.77 |      -23.51 |    581 |               63.51 |             1087.62 |          544.09 |
| recenter_noswap |        36 |  539.48 |      2.26 |        -107.88 |      -23.89 |    581 |               66.27 |              567.43 |          257.18 |
| recenter        |        37 |  445.12 |      1.86 |         -81.6  |      -17.48 |    581 |               63.34 |             1528.22 |          532.91 |
| volband_noswap  |        72 |  399.81 |      1.67 |        -148.66 |      -23.31 |    581 |               60.59 |              373.16 |          272.62 |
| static          |         8 |  328.54 |      1.38 |         -47.75 |      -26.03 |    581 |               57.14 |              668.96 |          365.24 |
| hold_usdc       |         1 |    0    |      0    |           0    |        0    |    581 |                0    |                0    |            0    |

Probability of backtest overfitting (CSCV, 10 blocks): **0.01** (share of splits where the in-sample best config lands in the bottom half out of sample).

## Configs chosen (56d train / 14d test, LP-only, top-3 ensemble), most frequent

|                                                      |   windows chosen |
|:-----------------------------------------------------|-----------------:|
| static(k=8)                                          |               12 |
| ladder(lookback=86400,span=3,every=72,turnover=0.4)  |               11 |
| static(k=5)                                          |                9 |
| static(k=12)                                         |                8 |
| ladder(lookback=86400,span=6,every=72,turnover=0.4)  |                7 |
| recenter(k=0,dwell=72,cooldown=0,swap=True)          |                6 |
| static(k=20)                                         |                6 |
| recenter(k=200,dwell=1,cooldown=0,swap=True)         |                6 |
| static(k=0)                                          |                5 |
| ladder(lookback=259200,span=6,every=72,turnover=0.4) |                4 |
| static(k=1)                                          |                4 |
| recenter(k=0,dwell=288,cooldown=0,swap=True)         |                4 |
| recenter(k=8,dwell=288,cooldown=0,swap=True)         |                3 |
| ladder(lookback=86400,span=6,every=72,turnover=0.15) |                3 |
| ladder(lookback=86400,span=6,every=288,turnover=0.4) |                3 |

## In-hindsight top configs (full period — biased, for reference only)

|                                                        |     pnl |
|:-------------------------------------------------------|--------:|
| ladder(lookback=86400,span=6,every=288,turnover=0.4)   | 1877.75 |
| ladder(lookback=86400,span=3,every=288,turnover=0.4)   | 1869.74 |
| ladder(lookback=86400,span=3,every=72,turnover=0.15)   | 1829.45 |
| ladder(lookback=86400,span=6,every=72,turnover=0.15)   | 1817.69 |
| ladder(lookback=86400,span=3,every=72,turnover=0.4)    | 1794.9  |
| ladder(lookback=86400,span=6,every=72,turnover=0.4)    | 1789.42 |
| ladder(lookback=86400,span=6,every=288,turnover=0.15)  | 1783.98 |
| ladder(lookback=86400,span=3,every=288,turnover=0.15)  | 1776.45 |
| ladder(lookback=259200,span=3,every=72,turnover=0.15)  | 1720.11 |
| ladder(lookback=259200,span=6,every=72,turnover=0.15)  | 1680.06 |
| ladder(lookback=259200,span=3,every=288,turnover=0.15) | 1671.55 |
| ladder(lookback=259200,span=6,every=288,turnover=0.15) | 1661.23 |
| ladder(lookback=604800,span=3,every=72,turnover=0.15)  | 1660.53 |
| ladder(lookback=604800,span=6,every=72,turnover=0.15)  | 1644.3  |
| ladder(lookback=259200,span=6,every=72,turnover=0.4)   | 1621.21 |
