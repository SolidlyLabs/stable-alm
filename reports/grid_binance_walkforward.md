# Walk-forward on `grid_binance`

250 configs, 2114 days (2021-01-02 to 2026-10-16), capital $15,000.

## Out-of-sample result per walk-forward setting

|   train_d |   test_d |   plateau | lp_only   |   top_n |     pnl |   apr_pct |   max_drawdown |   worst_day |   days |   positive_days_pct |   switches |
|----------:|---------:|----------:|:----------|--------:|--------:|----------:|---------------:|------------:|-------:|--------------------:|-----------:|
|        14 |        7 |         0 | False     |       1 | 15629.1 |     18.11 |         -69.18 |      -69.18 |   2100 |               86.67 |        213 |
|        14 |        7 |         5 | False     |       1 | 15847.7 |     18.36 |         -69.18 |      -69.18 |   2100 |               86.76 |        207 |
|        14 |        7 |         0 | True      |       1 | 14427.8 |     16.72 |       -1213.66 |     -678.18 |   2100 |               87    |        213 |
|        14 |        7 |         0 | True      |       3 | 15176.5 |     17.59 |        -388.46 |     -210.57 |   2100 |               87.24 |        250 |
|        14 |        7 |         5 | True      |       3 | 15019.5 |     17.4  |        -733.25 |     -401.53 |   2100 |               87.19 |        244 |
|        28 |        7 |         0 | False     |       1 | 16012.4 |     18.68 |         -69.18 |      -69.18 |   2086 |               86.82 |        154 |
|        28 |        7 |         5 | False     |       1 | 16104.2 |     18.79 |         -69.18 |      -69.18 |   2086 |               87.15 |        155 |
|        28 |        7 |         0 | True      |       1 | 14799.3 |     17.26 |       -1214.23 |     -678.18 |   2086 |               86.86 |        154 |
|        28 |        7 |         0 | True      |       3 | 15543.1 |     18.13 |        -388.55 |     -210.57 |   2086 |               87.3  |        208 |
|        28 |        7 |         5 | True      |       3 | 15237.9 |     17.78 |        -733.35 |     -401.53 |   2086 |               87.54 |        202 |
|        56 |       14 |         0 | False     |       1 | 15749.4 |     18.62 |         -69.18 |      -69.18 |   2058 |               86.3  |         62 |
|        56 |       14 |         5 | False     |       1 | 15883.9 |     18.78 |         -69.18 |      -69.18 |   2058 |               86.49 |         64 |
|        56 |       14 |         0 | True      |       1 | 14537   |     17.19 |       -1220.27 |     -678.18 |   2058 |               86.49 |         62 |
|        56 |       14 |         0 | True      |       3 | 15361.6 |     18.16 |        -395.31 |     -210.57 |   2058 |               86.73 |         97 |
|        56 |       14 |         5 | True      |       3 | 15131.5 |     17.89 |        -733.16 |     -401.53 |   2058 |               86.64 |         92 |
|        91 |       28 |         0 | False     |       1 | 15671.8 |     18.85 |         -69.18 |      -69.18 |   2023 |               86.36 |         30 |
|        91 |       28 |         5 | False     |       1 | 15729.6 |     18.92 |         -69.18 |      -69.18 |   2023 |               86.46 |         28 |
|        91 |       28 |         0 | True      |       1 | 14443.9 |     17.37 |       -1228.44 |     -678.18 |   2023 |               86.55 |         30 |
|        91 |       28 |         0 | True      |       3 | 15132.2 |     18.2  |        -406.33 |     -210.57 |   2023 |               86.31 |         38 |
|        91 |       28 |         5 | True      |       3 | 14905.9 |     17.93 |        -733.06 |     -401.53 |   2023 |               86.65 |         36 |

## Walk-forward within each family (56d train / 14d test)

| family          |   configs |      pnl |   apr_pct |   max_drawdown |   worst_day |   days |   positive_days_pct |   best_in_hindsight |   median_config |
|:----------------|----------:|---------:|----------:|---------------:|------------:|-------:|--------------------:|--------------------:|----------------:|
| ladder          |        24 | 15614    |     18.46 |        -823.88 |     -413.99 |   2058 |               87.46 |            15801.8  |        13592.3  |
| volband         |        72 | 13468.3  |     15.92 |       -1344.43 |     -606.74 |   2058 |               87.56 |            14032.9  |        10029.3  |
| recenter        |        37 | 11737.5  |     13.88 |       -1507.67 |     -678.18 |   2058 |               83.97 |            12841.6  |         8546.56 |
| volband_noswap  |        72 | 10880.1  |     12.86 |       -1479.92 |     -654.16 |   2058 |               84.74 |            11611.9  |         9799.54 |
| recenter_noswap |        36 | 10767.4  |     12.73 |       -2060.35 |     -866.82 |   2058 |               85.47 |            11637    |         9967.17 |
| static          |         8 |  3230.54 |      3.82 |         -82.74 |      -82.74 |   2058 |               63.12 |             3411.11 |          259.61 |
| hold_usdc       |         1 |     0    |      0    |           0    |        0    |   2058 |                0    |                0    |            0    |

Probability of backtest overfitting (CSCV, 10 blocks): **0.00** (share of splits where the in-sample best config lands in the bottom half out of sample).

## Configs chosen (56d train / 14d test, LP-only, top-3 ensemble), most frequent

|                                                                                                                                   |   windows chosen |
|:----------------------------------------------------------------------------------------------------------------------------------|-----------------:|
| ladder(lookback=86400,span=6,every=72,turnover=0.15)                                                                              |               97 |
| ladder(lookback=86400,span=3,every=72,turnover=0.15)                                                                              |               91 |
| ladder(lookback=86400,span=6,every=72,turnover=0.4)                                                                               |               45 |
| ladder(lookback=86400,span=3,every=72,turnover=0.4)                                                                               |               31 |
| recenter(k=1,dwell=12,cooldown=72,swap=True)                                                                                      |               19 |
| recenter(k=1,dwell=12,cooldown=0,swap=True)                                                                                       |                8 |
| recenter(k=200,dwell=1,cooldown=0,swap=True)                                                                                      |                8 |
| static(k=1)                                                                                                                       |                8 |
| static(k=0)                                                                                                                       |                8 |
| ladder(lookback=259200,span=6,every=288,turnover=0.4)                                                                             |                7 |
| ladder(lookback=604800,span=6,every=72,turnover=0.15)                                                                             |                6 |
| ladder(lookback=604800,span=6,every=288,turnover=0.4)                                                                             |                6 |
| ladder(lookback=604800,span=6,every=288,turnover=0.15)                                                                            |                6 |
| ladder(lookback=604800,span=6,every=72,turnover=0.4)                                                                              |                6 |
| volband(vol=atr,lookback=288,z=2,kmin=1,kmax=20,anchor=spot,anchor_lb=288,every=12,move=2,dwell=72,cooldown=72,swap=True,limit=3) |                5 |

## In-hindsight top configs (full period — biased, for reference only)

|                                                                                                                                     |     pnl |
|:------------------------------------------------------------------------------------------------------------------------------------|--------:|
| ladder(lookback=86400,span=6,every=72,turnover=0.15)                                                                                | 15792.2 |
| ladder(lookback=86400,span=3,every=72,turnover=0.15)                                                                                | 15790.5 |
| ladder(lookback=86400,span=6,every=72,turnover=0.4)                                                                                 | 15388.9 |
| ladder(lookback=86400,span=3,every=72,turnover=0.4)                                                                                 | 15387   |
| ladder(lookback=86400,span=6,every=288,turnover=0.15)                                                                               | 14900   |
| ladder(lookback=86400,span=6,every=288,turnover=0.4)                                                                                | 14817.9 |
| ladder(lookback=86400,span=3,every=288,turnover=0.15)                                                                               | 14722.3 |
| ladder(lookback=86400,span=3,every=288,turnover=0.4)                                                                                | 14636.8 |
| ladder(lookback=259200,span=6,every=72,turnover=0.15)                                                                               | 14080.8 |
| volband(vol=std,lookback=288,z=1,kmin=0,kmax=20,anchor=median,anchor_lb=288,every=12,move=2,dwell=72,cooldown=72,swap=True,limit=3) | 14041.8 |
| ladder(lookback=259200,span=6,every=288,turnover=0.15)                                                                              | 13965.7 |
| volband(vol=std,lookback=288,z=1,kmin=1,kmax=20,anchor=median,anchor_lb=288,every=12,move=2,dwell=72,cooldown=72,swap=True,limit=3) | 13962.6 |
| ladder(lookback=259200,span=3,every=72,turnover=0.15)                                                                               | 13929.3 |
| ladder(lookback=259200,span=3,every=288,turnover=0.15)                                                                              | 13741.3 |
| ladder(lookback=259200,span=6,every=288,turnover=0.4)                                                                               | 13685.2 |
