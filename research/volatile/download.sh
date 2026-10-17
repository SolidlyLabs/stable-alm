#!/bin/sh
# 90 days of Swap/Mint/Burn for the volatile research pools, one pool at a time.
cd "$(dirname "$0")/../.."
for p in 0xd0b53d9277642d899df5c87a3966a349a798f224 0x6c561b446416e1a00e8e93e221854d6ea4171372 0xfbb6eed8e7aa03b138556eedaf5d271a5e1e43ef 0xaad23a67f2ac693abbe543489aeb3f24f561d517; do
  .venv-desktop/Scripts/python.exe -m alm.download.pool_events $p 48833724 52721724 || echo "FAILED $p"
done
echo DONE
