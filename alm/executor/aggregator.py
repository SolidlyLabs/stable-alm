"""Swaps through the Solidly UI's own meta-aggregator routes (the same ones the swap card uses).

GET  https://solidly.com/api/swap/quote  -> best route across KyberSwap / Velora / OpenOcean
POST https://solidly.com/api/swap/build  -> calldata for the winning route, min-out from slippage
"""
import json
import urllib.request
from urllib.parse import urlencode

BASE = "https://solidly.com/api/swap"
HEADERS = {"User-Agent": "stable-alm/0.1", "Accept": "application/json"}


def quote(token_in: str, token_out: str, amount_in: int, dec_in: int, dec_out: int, chain_id: int = 8453) -> dict:
    q = urlencode({"chainId": chain_id, "tokenIn": token_in, "tokenOut": token_out, "amountIn": str(amount_in),
                   "decimalsIn": dec_in, "decimalsOut": dec_out})
    req = urllib.request.Request(f"{BASE}/quote/?{q}", headers=HEADERS)
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def build(best: dict, wallet: str, slippage: float, chain_id: int = 8453, deadline_minutes: int = 5) -> dict:
    body = json.dumps({"aggregatorId": best["aggregatorId"], "chainId": chain_id, "raw": best["raw"], "sender": wallet,
                       "recipient": wallet, "slippage": slippage, "deadlineMinutes": deadline_minutes}).encode()
    req = urllib.request.Request(f"{BASE}/build/", data=body, headers={**HEADERS, "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        built = json.load(r)
    if not built.get("calldata"):
        raise RuntimeError(f"aggregator build failed: {built}")
    if int(built.get("value") or 0) != 0:
        raise RuntimeError("aggregator route wants native ETH value; refusing")
    return built
