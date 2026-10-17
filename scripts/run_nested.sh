#!/usr/bin/env bash
# Nested-range grid on the three stable/LST pairs, one pair at a time, 2 workers.
cd "$(dirname "$0")/.."
PY=${PYTHON:-python}
$PY scripts/run_grid.py --pool usdc-usdt --grid nested --start 2025-01-17 --workers 2 --out nested_usdc-usdt
$PY scripts/run_grid.py --pool cbeth-weth --grid nested --start 2024-01-31 --workers 2 --out nested_cbeth-weth
$PY scripts/run_grid.py --pool usdc-usdbc --grid nested --start 2024-01-31 --workers 2 --out nested_usdc-usdbc
echo "== ALL DONE"
