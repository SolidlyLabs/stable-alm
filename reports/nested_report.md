# Novel ideas vs the live ladder (`nested` grid)

Walk-forward (configs picked on the train window only, booked on the next test window), new-fee scenario, 15-tick depeg guard everywhere. Gains are out-of-sample APR points over the baseline ladder family, with a 95% block-bootstrap interval.

## usdc-usdt (62 configs, 631 days, capital 15000 USDC)

| family | configs | wf 28/7 APR% | wf 56/14 APR% | max_dd | wf 91/28 APR% | median cfg APR% |
|---|---|---|---|---|---|---|
| ladder (baseline) | 24 | 2.701 | 2.883 | -30.411 | 2.676 | 2.469 |
| ladder as nested ranges | 8 | 2.69 | 2.845 | -51.461 | 2.935 | 2.678 |
| pyramid narrow (<=±2) a=0.0 | 4 | 1.978 | 2.204 | -149.352 | 2.161 | 1.888 |
| pyramid narrow (<=±2) a=0.5 | 4 | 1.874 | 2.108 | -147.804 | 1.918 | 1.824 |
| pyramid narrow (<=±2) a=1.0 | 4 | 1.78 | 2.003 | -146.388 | 1.812 | 1.743 |
| pyramid wide (±3-6) a=0.0 | 6 | 2.0 | 2.15 | -132.246 | 1.997 | 1.967 |
| pyramid wide (±3-6) a=0.5 | 6 | 1.862 | 1.989 | -126.614 | 1.753 | 1.797 |
| pyramid wide (±3-6) a=1.0 | 6 | 1.693 | 1.791 | -123.429 | 1.571 | 1.602 |

Gain over the baseline family (56/14 walk-forward):

| family | gain APR pts | 95% CI low | 95% CI high | 1st half | 2nd half | days better % |
|---|---|---|---|---|---|---|
| ladder as nested ranges | -0.038 | -0.253 | 0.154 | 0.126 | -0.201 | 39.304 |
| pyramid narrow (<=±2) a=0.0 | -0.679 | -1.32 | -0.168 | -0.282 | -1.074 | 38.261 |
| pyramid wide (±3-6) a=0.0 | -0.733 | -1.389 | -0.173 | -0.488 | -0.978 | 35.478 |
| pyramid narrow (<=±2) a=0.5 | -0.775 | -1.416 | -0.241 | -0.411 | -1.138 | 35.826 |
| pyramid narrow (<=±2) a=1.0 | -0.88 | -1.519 | -0.35 | -0.595 | -1.165 | 34.435 |
| pyramid wide (±3-6) a=0.5 | -0.894 | -1.559 | -0.328 | -0.727 | -1.061 | 35.478 |
| pyramid wide (±3-6) a=1.0 | -1.092 | -1.803 | -0.499 | -0.954 | -1.23 | 34.957 |

Walk-forward choosing across the whole grid: 2.87% APR vs baseline family 2.88%. PBO over the grid: 0.08.

## cbeth-weth (62 configs, 988 days, capital 6 WETH)

| family | configs | wf 28/7 APR% | wf 56/14 APR% | max_dd | wf 91/28 APR% | median cfg APR% |
|---|---|---|---|---|---|---|
| ladder (baseline) | 24 | 5.957 | 5.898 | -0.018 | 5.896 | 4.504 |
| ladder as nested ranges | 8 | 5.85 | 5.836 | -0.019 | 5.856 | 5.464 |
| pyramid narrow (<=±2) a=0.0 | 4 | 4.806 | 4.872 | -0.026 | 4.944 | 4.254 |
| pyramid narrow (<=±2) a=0.5 | 4 | 4.758 | 4.778 | -0.025 | 4.848 | 4.168 |
| pyramid narrow (<=±2) a=1.0 | 4 | 4.655 | 4.642 | -0.025 | 4.709 | 4.028 |
| pyramid wide (±3-6) a=0.0 | 6 | 4.734 | 4.768 | -0.023 | 4.78 | 4.283 |
| pyramid wide (±3-6) a=0.5 | 6 | 4.456 | 4.47 | -0.021 | 4.46 | 3.943 |
| pyramid wide (±3-6) a=1.0 | 6 | 4.189 | 4.168 | -0.02 | 4.166 | 3.699 |

Gain over the baseline family (56/14 walk-forward):

| family | gain APR pts | 95% CI low | 95% CI high | 1st half | 2nd half | days better % |
|---|---|---|---|---|---|---|
| ladder as nested ranges | -0.062 | -0.228 | 0.189 | -0.019 | -0.105 | 26.824 |
| pyramid narrow (<=±2) a=0.0 | -1.026 | -1.373 | -0.692 | -0.581 | -1.471 | 38.197 |
| pyramid narrow (<=±2) a=0.5 | -1.12 | -1.508 | -0.753 | -0.483 | -1.757 | 37.876 |
| pyramid wide (±3-6) a=0.0 | -1.13 | -1.693 | -0.574 | 0.195 | -2.455 | 42.811 |
| pyramid narrow (<=±2) a=1.0 | -1.257 | -1.706 | -0.845 | -0.435 | -2.078 | 39.163 |
| pyramid wide (±3-6) a=0.5 | -1.428 | -2.135 | -0.753 | 0.323 | -3.179 | 42.704 |
| pyramid wide (±3-6) a=1.0 | -1.73 | -2.538 | -0.965 | 0.298 | -3.758 | 41.738 |

Walk-forward choosing across the whole grid: 5.85% APR vs baseline family 5.90%. PBO over the grid: 0.04.

## usdc-usdbc (62 configs, 988 days, capital 15000 USDC)

| family | configs | wf 28/7 APR% | wf 56/14 APR% | max_dd | wf 91/28 APR% | median cfg APR% |
|---|---|---|---|---|---|---|
| ladder (baseline) | 24 | 1.662 | 1.279 | -6.552 | 1.02 | 1.206 |
| ladder as nested ranges | 8 | 1.429 | 1.01 | -22.128 | 0.877 | 0.997 |
| pyramid narrow (<=±2) a=0.0 | 4 | 1.235 | 0.861 | -46.574 | 0.742 | 0.715 |
| pyramid narrow (<=±2) a=0.5 | 4 | 1.283 | 0.888 | -42.534 | 0.756 | 0.749 |
| pyramid narrow (<=±2) a=1.0 | 4 | 1.306 | 0.894 | -39.843 | 0.758 | 0.766 |
| pyramid wide (±3-6) a=0.0 | 6 | 1.398 | 0.937 | -36.711 | 0.741 | 0.814 |
| pyramid wide (±3-6) a=0.5 | 6 | 1.424 | 0.949 | -31.423 | 0.743 | 0.823 |
| pyramid wide (±3-6) a=1.0 | 6 | 1.407 | 0.925 | -28.911 | 0.716 | 0.79 |

Gain over the baseline family (56/14 walk-forward):

| family | gain APR pts | 95% CI low | 95% CI high | 1st half | 2nd half | days better % |
|---|---|---|---|---|---|---|
| ladder as nested ranges | -0.269 | -0.331 | -0.116 | -0.407 | -0.131 | 39.807 |
| pyramid wide (±3-6) a=0.5 | -0.33 | -0.419 | -0.181 | -0.313 | -0.347 | 35.086 |
| pyramid wide (±3-6) a=0.0 | -0.342 | -0.418 | -0.192 | -0.356 | -0.328 | 35.515 |
| pyramid wide (±3-6) a=1.0 | -0.354 | -0.459 | -0.198 | -0.327 | -0.382 | 34.549 |
| pyramid narrow (<=±2) a=1.0 | -0.385 | -0.536 | -0.186 | -0.449 | -0.322 | 37.446 |
| pyramid narrow (<=±2) a=0.5 | -0.391 | -0.547 | -0.188 | -0.467 | -0.316 | 37.983 |
| pyramid narrow (<=±2) a=0.0 | -0.418 | -0.583 | -0.204 | -0.516 | -0.321 | 39.056 |

Walk-forward choosing across the whole grid: 1.22% APR vs baseline family 1.28%. PBO over the grid: 0.00.
