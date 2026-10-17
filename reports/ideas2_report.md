# Novel ideas vs the live ladder (`ideas2` grid)

Walk-forward (configs picked on the train window only, booked on the next test window), new-fee scenario, 15-tick depeg guard everywhere. Gains are out-of-sample APR points over the baseline ladder family, with a 95% block-bootstrap interval.

## usdc-usdt (48 configs, 631 days, capital 15000 USDC)

| family                |   configs |   wf 28/7 APR% |   wf 56/14 APR% |   max_dd |   wf 91/28 APR% |   median cfg APR% |
|:----------------------|----------:|---------------:|----------------:|---------:|----------------:|------------------:|
| ladder (baseline)     |        24 |          2.701 |           2.883 |  -30.411 |           2.676 |             2.469 |
| combo decay+emis+core |         8 |          2.824 |           3.038 |  -51.347 |           3.11  |             2.93  |
| dual fast/slow        |         8 |          2.454 |           2.578 |  -59.849 |           2.777 |             2.623 |
| forecast mix          |         8 |          2.731 |           2.943 |  -55.677 |           2.944 |             2.838 |

Gain over the baseline family (56/14 walk-forward):

| family                |   gain APR pts |   95% CI low |   95% CI high |   1st half |   2nd half |   days better % |
|:----------------------|---------------:|-------------:|--------------:|-----------:|-----------:|----------------:|
| combo decay+emis+core |          0.155 |       -0.101 |         0.429 |      0.324 |     -0.014 |          46.087 |
| forecast mix          |          0.06  |       -0.217 |         0.307 |      0.186 |     -0.065 |          44.174 |
| dual fast/slow        |         -0.305 |       -0.512 |        -0.101 |     -0.148 |     -0.462 |          36.174 |

Walk-forward choosing across the whole grid: 3.00% APR vs baseline family 2.88%. PBO over the grid: 0.13.

## cbeth-weth (48 configs, 988 days, capital 6 WETH)

| family                |   configs |   wf 28/7 APR% |   wf 56/14 APR% |   max_dd |   wf 91/28 APR% |   median cfg APR% |
|:----------------------|----------:|---------------:|----------------:|---------:|----------------:|------------------:|
| ladder (baseline)     |        24 |          5.957 |           5.898 |   -0.018 |           5.896 |             4.504 |
| combo decay+emis+core |         8 |          6.119 |           6.08  |   -0.018 |           6.136 |             5.777 |
| dual fast/slow        |         8 |          4.906 |           5.049 |   -0.019 |           5.124 |             4.914 |
| forecast mix          |         8 |          5.83  |           5.888 |   -0.018 |           5.968 |             5.64  |

Gain over the baseline family (56/14 walk-forward):

| family                |   gain APR pts |   95% CI low |   95% CI high |   1st half |   2nd half |   days better % |
|:----------------------|---------------:|-------------:|--------------:|-----------:|-----------:|----------------:|
| combo decay+emis+core |          0.182 |       -0.027 |         0.442 |      0.11  |      0.254 |          50.644 |
| forecast mix          |         -0.01  |       -0.193 |         0.246 |      0.019 |     -0.039 |          49.785 |
| dual fast/slow        |         -0.849 |       -1.163 |        -0.539 |     -0.423 |     -1.275 |          28.863 |

Walk-forward choosing across the whole grid: 6.05% APR vs baseline family 5.90%. PBO over the grid: 0.02.

## usdc-usdbc (48 configs, 988 days, capital 15000 USDC)

| family                |   configs |   wf 28/7 APR% |   wf 56/14 APR% |   max_dd |   wf 91/28 APR% |   median cfg APR% |
|:----------------------|----------:|---------------:|----------------:|---------:|----------------:|------------------:|
| ladder (baseline)     |        24 |          1.662 |           1.279 |   -6.552 |           1.02  |             1.206 |
| combo decay+emis+core |         8 |          1.514 |           1.067 |  -21.083 |           0.886 |             1.069 |
| dual fast/slow        |         8 |          1.399 |           1.011 |  -24.253 |           0.824 |             0.992 |
| forecast mix          |         8 |          1.532 |           1.081 |  -20.416 |           0.888 |             1.084 |

Gain over the baseline family (56/14 walk-forward):

| family                |   gain APR pts |   95% CI low |   95% CI high |   1st half |   2nd half |   days better % |
|:----------------------|---------------:|-------------:|--------------:|-----------:|-----------:|----------------:|
| forecast mix          |         -0.198 |       -0.236 |        -0.086 |     -0.286 |     -0.109 |          44.528 |
| combo decay+emis+core |         -0.212 |       -0.275 |        -0.089 |     -0.306 |     -0.118 |          41.631 |
| dual fast/slow        |         -0.268 |       -0.315 |        -0.15  |     -0.396 |     -0.139 |          40.558 |

Walk-forward choosing across the whole grid: 1.19% APR vs baseline family 1.28%. PBO over the grid: 0.00.
