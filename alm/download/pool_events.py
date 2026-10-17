"""Download every Swap / Mint / Burn of a concentrated-liquidity pool and store them decoded.

Output: data/raw/<pool>/events_<startBlock>.parquet, one file per segment so a stopped run resumes
where it left off. Each row is one event with the fields the simulator needs; timestamps come from
Base's fixed 2s block time.

Usage: python -m alm.download.pool_events <pool> <fromBlock> [toBlock]
"""
import os
import sys

import pandas as pd

from ..paths import DATA
from ..rpc import base_block_ts, batch, call

SWAP = "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67"
MINT = "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde"
BURN = "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c"
CHUNK = 10_000
PER_BATCH = 10
SEGMENT = 1_000_000


def s256(h: str) -> int:
    v = int(h, 16)
    return v - (1 << 256) if v >= 1 << 255 else v


def words(data: str) -> list:
    d = data[2:]
    return [d[i : i + 64] for i in range(0, len(d), 64)]


def decode(log: dict) -> dict:
    t = log["topics"]
    w = words(log["data"])
    block = int(log["blockNumber"], 16)
    row = {
        "block": block,
        "log_index": int(log["logIndex"], 16),
        "ts": base_block_ts(block),
        "tx": log["transactionHash"],
    }
    if t[0] == SWAP:
        row.update(
            kind="swap",
            # Strings: 18-decimal amounts overflow int64 in parquet.
            amount0=str(s256(w[0])),
            amount1=str(s256(w[1])),
            sqrt_price_x96=str(int(w[2], 16)),
            liquidity=str(int(w[3], 16)),
            tick=s256(w[4]),
        )
    else:
        # Mint: data = sender, amount, amount0, amount1 ; Burn: data = amount, amount0, amount1
        off = 1 if t[0] == MINT else 0
        row.update(
            kind="mint" if t[0] == MINT else "burn",
            owner="0x" + t[1][26:],
            tick_lower=s256(t[2]),
            tick_upper=s256(t[3]),
            amount=str(int(w[off], 16)),
            amount0=str(int(w[off + 1], 16)),
            amount1=str(int(w[off + 2], 16)),
        )
    return row


def fetch_range(pool: str, start: int, end: int) -> list:
    rows = []
    ranges = [(b, min(b + CHUNK - 1, end)) for b in range(start, end + 1, CHUNK)]
    for i in range(0, len(ranges), PER_BATCH):
        part = ranges[i : i + PER_BATCH]
        res = batch(
            [("eth_getLogs", [{"address": pool, "fromBlock": hex(a), "toBlock": hex(b), "topics": [[SWAP, MINT, BURN]]}]) for a, b in part]
        )
        for (a, b), r in zip(part, res):
            if not isinstance(r, list):
                # Too many logs or a transient error: split this chunk.
                mid = (a + b) // 2
                rows += fetch_range_single(pool, a, mid) + fetch_range_single(pool, mid + 1, b)
            else:
                rows += [decode(log) for log in r]
    return rows


def fetch_range_single(pool: str, a: int, b: int) -> list:
    r = call("eth_getLogs", [{"address": pool, "fromBlock": hex(a), "toBlock": hex(b), "topics": [[SWAP, MINT, BURN]]}])
    if isinstance(r, list):
        return [decode(log) for log in r]
    if b - a < 10:
        raise RuntimeError(f"getLogs failed for {a}-{b}: {r}")
    mid = (a + b) // 2
    return fetch_range_single(pool, a, mid) + fetch_range_single(pool, mid + 1, b)


def main() -> None:
    pool = sys.argv[1].lower()
    start = int(sys.argv[2])
    end = int(sys.argv[3]) if len(sys.argv) > 3 else int(call("eth_blockNumber", []), 16)
    out_dir = os.path.join(DATA, "raw", pool)
    os.makedirs(out_dir, exist_ok=True)
    seg = start - start % SEGMENT
    while seg <= end:
        a, b = max(seg, start), min(seg + SEGMENT - 1, end)
        path = os.path.join(out_dir, f"events_{seg}.parquet")
        complete = b == seg + SEGMENT - 1
        if os.path.exists(path) and complete:
            seg += SEGMENT
            continue
        rows = fetch_range(pool, a, b)
        df = pd.DataFrame(rows)
        if len(df):
            df = df.sort_values(["block", "log_index"])
        df.to_parquet(path, index=False)
        print(f"{pool[:10]} {a}-{b}: {len(df)} events", flush=True)
        seg += SEGMENT


if __name__ == "__main__":
    main()
