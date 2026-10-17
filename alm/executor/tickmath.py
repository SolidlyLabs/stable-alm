"""Exact Uniswap V3 TickMath / LiquidityAmounts in Python integers (Solidly V3 uses the same math)."""

Q96 = 1 << 96
MIN_TICK, MAX_TICK = -887272, 887272
MIN_SQRT_RATIO = 4295128739
MAX_SQRT_RATIO = 1461446703485210103287273052203988822378723970342

_MAGIC = [
    (0x2, 0xFFF97272373D413259A46990580E213A),
    (0x4, 0xFFF2E50F5F656932EF12357CF3C7FDCC),
    (0x8, 0xFFE5CACA7E10E4E61C3624EAA0941CD0),
    (0x10, 0xFFCB9843D60F6159C9DB58835C926644),
    (0x20, 0xFF973B41FA98C081472E6896DFB254C0),
    (0x40, 0xFF2EA16466C96A3843EC78B326B52861),
    (0x80, 0xFE5DEE046A99A2A811C461F1969C3053),
    (0x100, 0xFCBE86C7900A88AEDCFFC83B479AA3A4),
    (0x200, 0xF987A7253AC413176F2B074CF7815E54),
    (0x400, 0xF3392B0822B70005940C7A398E4B70F3),
    (0x800, 0xE7159475A2C29B7443B29C7FA6E889D9),
    (0x1000, 0xD097F3BDFD2022B8845AD8F792AA5825),
    (0x2000, 0xA9F746462D870FDF8A65DC1F90E061E5),
    (0x4000, 0x70D869A156D2A1B890BB3DF62BAF32F7),
    (0x8000, 0x31BE135F97D08FD981231505542FCFA6),
    (0x10000, 0x9AA508B5B7A84E1C677DE54F3E99BC9),
    (0x20000, 0x5D6AF8DEDB81196699C329225EE604),
    (0x40000, 0x2216E584F5FA1EA926041BEDFE98),
    (0x80000, 0x48A170391F7DC42444E8FA2),
]


def sqrt_ratio_at_tick(tick: int) -> int:
    a = abs(tick)
    assert a <= MAX_TICK, "tick out of range"
    ratio = 0xFFFCB933BD6FAD37AA2D162D1A594001 if a & 0x1 else 1 << 128
    for bit, magic in _MAGIC:
        if a & bit:
            ratio = (ratio * magic) >> 128
    if tick > 0:
        ratio = ((1 << 256) - 1) // ratio
    return (ratio >> 32) + (0 if ratio % (1 << 32) == 0 else 1)


def liquidity_for_amount0(sa: int, sb: int, amount0: int) -> int:
    if sa > sb:
        sa, sb = sb, sa
    return amount0 * (sa * sb // Q96) // (sb - sa)


def liquidity_for_amount1(sa: int, sb: int, amount1: int) -> int:
    if sa > sb:
        sa, sb = sb, sa
    return amount1 * Q96 // (sb - sa)


def liquidity_for_amounts(sp: int, sa: int, sb: int, amount0: int, amount1: int) -> int:
    if sa > sb:
        sa, sb = sb, sa
    if sp <= sa:
        return liquidity_for_amount0(sa, sb, amount0)
    if sp < sb:
        return min(liquidity_for_amount0(sp, sb, amount0), liquidity_for_amount1(sa, sp, amount1))
    return liquidity_for_amount1(sa, sb, amount1)


def amounts_for_liquidity(sp: int, sa: int, sb: int, liq: int) -> tuple:
    """Token amounts (raw units) for liquidity on [sa, sb) at sqrt price sp, rounded down."""
    if sa > sb:
        sa, sb = sb, sa
    if sp <= sa:
        return (liq * Q96 * (sb - sa) // sb // sa, 0)
    if sp < sb:
        return (liq * Q96 * (sb - sp) // sb // sp, liq * (sp - sa) // Q96)
    return (0, liq * (sb - sa) // Q96)
