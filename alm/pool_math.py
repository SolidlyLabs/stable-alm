"""Concentrated-liquidity position math (Uniswap V3 / Solidly V3), in float.

Conventions: token0 = USDC, token1 = USDT, both 6 decimals, so raw-unit ratios are price ratios.
p = 1.0001**tick is USDT per USDC. Amounts are in whole tokens (not wei).
"""
import math

import numpy as np


def sqrt_at(tick):
    return np.power(1.0001, np.asarray(tick, dtype=float) / 2.0)


def amounts_for_liquidity(liq: float, sp: float, tl: int, tu: int):
    """Token amounts (x=token0, y=token1) held by liquidity `liq` on [tl, tu) at sqrt price sp."""
    sa, sb = float(sqrt_at(tl)), float(sqrt_at(tu))
    if sp <= sa:
        return liq * (1 / sa - 1 / sb), 0.0
    if sp >= sb:
        return 0.0, liq * (sb - sa)
    return liq * (1 / sp - 1 / sb), liq * (sp - sa)


def liquidity_for_amounts(x: float, y: float, sp: float, tl: int, tu: int) -> float:
    """Max liquidity mintable on [tl, tu) from x token0 and y token1 at sqrt price sp."""
    sa, sb = float(sqrt_at(tl)), float(sqrt_at(tu))
    if sp <= sa:
        return x / (1 / sa - 1 / sb)
    if sp >= sb:
        return y / (sb - sa)
    lx = x / (1 / sp - 1 / sb) if x > 0 else 0.0
    ly = y / (sp - sa) if y > 0 else 0.0
    return min(lx, ly)


def ratio_for_range(sp: float, tl: int, tu: int):
    """Fraction of position value (in token0 terms) that must be token0 for a range at sp."""
    x, y = amounts_for_liquidity(1.0, sp, tl, tu)
    p = sp * sp
    v = x + y / p
    return 1.0 if v == 0 else x / v


def value_token0(x: float, y: float, sp: float) -> float:
    """Value in token0 (USDC) of x token0 + y token1 at sqrt price sp."""
    return x + y / (sp * sp)


def tick_of_price(p: float) -> int:
    return int(math.floor(math.log(p) / math.log(1.0001)))
