# Novel ideas vs the live ladder (`ideas` grid)

Walk-forward (configs picked on the train window only, booked on the next test window), new-fee scenario, 15-tick depeg guard everywhere. Gains are out-of-sample APR points over the baseline ladder family, with a 95% block-bootstrap interval.

## usdc-usdt (132 configs, 631 days, capital 15000 USDC)

| family               |   configs |   wf 28/7 APR% |   wf 56/14 APR% |   max_dd |   wf 91/28 APR% |   median cfg APR% |
|:---------------------|----------:|---------------:|----------------:|---------:|----------------:|------------------:|
| ladder (baseline)    |        24 |          2.701 |           2.883 |  -30.411 |           2.676 |             2.469 |
| blended centre       |         8 |          2.704 |           2.697 |  -46.618 |           2.971 |             2.594 |
| core band            |         8 |          2.845 |           3.022 |  -54.022 |           3.208 |             2.998 |
| emission-aware       |        16 |          2.778 |           2.961 |  -30.4   |           2.946 |             2.557 |
| exit re-solve        |        12 |          2.698 |           2.833 |  -57.225 |           2.972 |             2.75  |
| fast re-solve (1-2h) |        24 |          2.803 |           2.95  |  -59.886 |           3.09  |             2.935 |
| recency decay        |        24 |          2.729 |           2.996 |  -39.283 |           2.954 |             2.819 |
| swap-free            |        16 |          1.68  |           1.97  |  -34.036 |           1.745 |             1.43  |

Gain over the baseline family (56/14 walk-forward):

| family               |   gain APR pts |   95% CI low |   95% CI high |   1st half |   2nd half |   days better % |
|:---------------------|---------------:|-------------:|--------------:|-----------:|-----------:|----------------:|
| core band            |          0.139 |       -0.145 |         0.406 |      0.355 |     -0.077 |          42.087 |
| recency decay        |          0.113 |       -0.064 |         0.366 |      0.203 |      0.023 |          46.783 |
| emission-aware       |          0.078 |       -0.032 |         0.212 |      0.113 |      0.043 |          44     |
| fast re-solve (1-2h) |          0.067 |       -0.262 |         0.409 |      0.158 |     -0.024 |          45.739 |
| exit re-solve        |         -0.05  |       -0.315 |         0.163 |      0.156 |     -0.256 |          39.304 |
| blended centre       |         -0.186 |       -0.642 |         0.16  |     -0.19  |     -0.183 |          43.478 |
| swap-free            |         -0.913 |       -1.762 |        -0.3   |     -0.569 |     -1.257 |          36     |

Walk-forward choosing across the whole grid: 2.96% APR vs baseline family 2.88%. PBO over the grid: 0.34.

## cbeth-weth (132 configs, 988 days, capital 6 WETH)

| family               |   configs |   wf 28/7 APR% |   wf 56/14 APR% |   max_dd |   wf 91/28 APR% |   median cfg APR% |
|:---------------------|----------:|---------------:|----------------:|---------:|----------------:|------------------:|
| ladder (baseline)    |        24 |          5.957 |           5.898 |   -0.018 |           5.896 |             4.504 |
| blended centre       |         8 |          5.715 |           5.858 |   -0.018 |           6.047 |             5.356 |
| core band            |         8 |          5.644 |           5.636 |   -0.018 |           5.762 |             5.553 |
| emission-aware       |        16 |          5.894 |           5.93  |   -0.018 |           6.017 |             5.079 |
| exit re-solve        |        12 |          5.62  |           5.672 |   -0.018 |           5.877 |             5.411 |
| fast re-solve (1-2h) |        24 |          5.887 |           5.921 |   -0.018 |           6.231 |             5.581 |
| recency decay        |        24 |          6.142 |           6.29  |   -0.018 |           6.468 |             5.825 |
| swap-free            |        16 |          2.975 |           3.154 |   -0.019 |           2.822 |             2.441 |

Gain over the baseline family (56/14 walk-forward):

| family               |   gain APR pts |   95% CI low |   95% CI high |   1st half |   2nd half |   days better % |
|:---------------------|---------------:|-------------:|--------------:|-----------:|-----------:|----------------:|
| recency decay        |          0.392 |        0.159 |         0.688 |     -0.116 |      0.901 |          54.828 |
| emission-aware       |          0.032 |       -0.07  |         0.176 |      0.074 |     -0.01  |          51.609 |
| fast re-solve (1-2h) |          0.023 |       -0.309 |         0.363 |     -0.496 |      0.542 |          47.532 |
| blended centre       |         -0.04  |       -0.239 |         0.217 |     -0.052 |     -0.028 |          55.579 |
| exit re-solve        |         -0.226 |       -0.487 |         0.02  |      0.013 |     -0.464 |          43.026 |
| core band            |         -0.262 |       -0.523 |         0.031 |      0.126 |     -0.651 |          40.129 |
| swap-free            |         -2.744 |       -3.656 |        -1.86  |     -0.364 |     -5.125 |          30.472 |

Walk-forward choosing across the whole grid: 6.23% APR vs baseline family 5.90%. PBO over the grid: 0.25.

## usdc-usdbc (132 configs, 988 days, capital 15000 USDC)

| family               |   configs |   wf 28/7 APR% |   wf 56/14 APR% |   max_dd |   wf 91/28 APR% |   median cfg APR% |
|:---------------------|----------:|---------------:|----------------:|---------:|----------------:|------------------:|
| ladder (baseline)    |        24 |          1.662 |           1.279 |   -6.552 |           1.02  |             1.206 |
| blended centre       |         8 |          1.51  |           1.049 |  -19.958 |           0.878 |             1.023 |
| core band            |         8 |          1.513 |           1.07  |  -20.802 |           0.88  |             1.068 |
| emission-aware       |        16 |          1.618 |           1.207 |  -10.459 |           0.985 |             1.112 |
| exit re-solve        |        12 |          1.454 |           1.058 |  -23.932 |           0.896 |             1.038 |
| fast re-solve (1-2h) |        24 |          1.422 |           1.014 |  -23.602 |           0.898 |             1.035 |
| recency decay        |        24 |          1.419 |           1.097 |  -15.291 |           0.957 |             0.881 |
| swap-free            |        16 |          1.613 |           1.232 |   -7.129 |           0.976 |             1.152 |

Gain over the baseline family (56/14 walk-forward):

| family               |   gain APR pts |   95% CI low |   95% CI high |   1st half |   2nd half |   days better % |
|:---------------------|---------------:|-------------:|--------------:|-----------:|-----------:|----------------:|
| swap-free            |         -0.047 |       -0.104 |         0.009 |     -0.015 |     -0.079 |          44.206 |
| emission-aware       |         -0.072 |       -0.121 |        -0.035 |     -0.094 |     -0.05  |          48.069 |
| recency decay        |         -0.182 |       -0.252 |        -0.069 |     -0.28  |     -0.084 |          45.279 |
| core band            |         -0.209 |       -0.255 |        -0.091 |     -0.325 |     -0.093 |          39.27  |
| exit re-solve        |         -0.221 |       -0.27  |        -0.098 |     -0.315 |     -0.126 |          41.416 |
| blended centre       |         -0.23  |       -0.292 |        -0.117 |     -0.327 |     -0.133 |          43.026 |
| fast re-solve (1-2h) |         -0.265 |       -0.334 |        -0.099 |     -0.382 |     -0.148 |          41.309 |

Walk-forward choosing across the whole grid: 1.21% APR vs baseline family 1.28%. PBO over the grid: 0.02.
