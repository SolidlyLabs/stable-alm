"""USD prices for allocations and reward valuation, from the Solidly UI aggregator's quote (amountInUsd).

Cached for 10 minutes. USDC is 1 by definition; anything the aggregator cannot price raises.
"""
import time

from ..settings import USDC
from . import aggregator, chain

_cache: dict = {}
_meta: dict = {}
TTL = 600


def meta(token: str) -> dict:
    token = token.lower()
    if token not in _meta:
        _meta[token] = chain.erc20_meta(token)
    return _meta[token]


def usd(token: str) -> float:
    token = token.lower()
    if token == USDC:
        return 1.0
    hit = _cache.get(token)
    if hit and time.time() - hit[1] < TTL:
        return hit[0]
    d = meta(token)["decimals"]
    q = aggregator.quote(token, USDC, 10**d, d, 6)
    best = q.get("best") or {}
    price = float(best.get("amountInUsd") or 0) or int(best.get("amountOut") or 0) / 1e6
    if price <= 0:
        raise RuntimeError(f"no USD price for {token}")
    _cache[token] = (price, time.time())
    return price
