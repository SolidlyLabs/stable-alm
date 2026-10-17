#!/usr/bin/env bash
# Every grid for every pair, one run at a time (2 workers each), under the new-fee scenario.
# Needs the pool events and processor history in data/ (scripts/download_pool_events.py, download_processor.py).
set -u
cd "$(dirname "$0")/.."
run() { echo "== $(date +%H:%M) $*"; python scripts/run_grid.py --workers 2 "$@" 2>&1 | tail -1; }
run --pool usdc-usdt  --grid full  --start 2025-01-17 --out full_usdc-usdt
run --pool usdc-usdbc --grid full  --start 2024-01-31 --out full_usdc-usdbc
run --pool cbeth-weth --grid full  --start 2024-01-31 --out full_cbeth-weth
for p in usdc-usdt usdc-usdbc cbeth-weth; do
  s=2024-01-31; [ $p = usdc-usdt ] && s=2025-01-17
  run --pool $p --grid delay --bars 60 --start $s --out delay_$p
done
FEE_FLOOR_OVERRIDE=0.44 run --pool usdc-usdt --grid full --start 2025-01-17 --out full_usdc-usdt_fee044
echo "== ALL DONE $(date +%H:%M)"
