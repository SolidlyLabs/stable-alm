//! Exact Uniswap V3 TickMath / LiquidityAmounts (Solidly V3 uses the same math), in big integers,
//! matching alm/executor/tickmath.py bit for bit.

use num_bigint::BigUint;
use num_traits::{One, Zero};

pub const MIN_TICK: i64 = -887272;
pub const MAX_TICK: i64 = 887272;

pub fn q96() -> BigUint {
    BigUint::one() << 96
}

const MAGIC: [(u64, &str); 19] = [
    (0x2, "FFF97272373D413259A46990580E213A"),
    (0x4, "FFF2E50F5F656932EF12357CF3C7FDCC"),
    (0x8, "FFE5CACA7E10E4E61C3624EAA0941CD0"),
    (0x10, "FFCB9843D60F6159C9DB58835C926644"),
    (0x20, "FF973B41FA98C081472E6896DFB254C0"),
    (0x40, "FF2EA16466C96A3843EC78B326B52861"),
    (0x80, "FE5DEE046A99A2A811C461F1969C3053"),
    (0x100, "FCBE86C7900A88AEDCFFC83B479AA3A4"),
    (0x200, "F987A7253AC413176F2B074CF7815E54"),
    (0x400, "F3392B0822B70005940C7A398E4B70F3"),
    (0x800, "E7159475A2C29B7443B29C7FA6E889D9"),
    (0x1000, "D097F3BDFD2022B8845AD8F792AA5825"),
    (0x2000, "A9F746462D870FDF8A65DC1F90E061E5"),
    (0x4000, "70D869A156D2A1B890BB3DF62BAF32F7"),
    (0x8000, "31BE135F97D08FD981231505542FCFA6"),
    (0x10000, "9AA508B5B7A84E1C677DE54F3E99BC9"),
    (0x20000, "5D6AF8DEDB81196699C329225EE604"),
    (0x40000, "2216E584F5FA1EA926041BEDFE98"),
    (0x80000, "48A170391F7DC42444E8FA2"),
];

fn h(s: &str) -> BigUint {
    BigUint::parse_bytes(s.as_bytes(), 16).unwrap()
}

pub fn sqrt_ratio_at_tick(tick: i64) -> BigUint {
    let a = tick.unsigned_abs();
    assert!(a <= MAX_TICK as u64, "tick out of range");
    let mut ratio = if a & 1 != 0 { h("FFFCB933BD6FAD37AA2D162D1A594001") } else { BigUint::one() << 128 };
    for (bit, magic) in MAGIC {
        if a & bit != 0 {
            ratio = (ratio * h(magic)) >> 128;
        }
    }
    if tick > 0 {
        ratio = ((BigUint::one() << 256) - BigUint::one()) / ratio;
    }
    let rem: BigUint = &ratio % (BigUint::one() << 32usize);
    (ratio >> 32) + if rem.is_zero() { BigUint::zero() } else { BigUint::one() }
}

fn order(sa: &BigUint, sb: &BigUint) -> (BigUint, BigUint) {
    if sa > sb {
        (sb.clone(), sa.clone())
    } else {
        (sa.clone(), sb.clone())
    }
}

pub fn liquidity_for_amount0(sa: &BigUint, sb: &BigUint, amount0: &BigUint) -> BigUint {
    let (sa, sb) = order(sa, sb);
    amount0 * (&sa * &sb / q96()) / (&sb - &sa)
}

pub fn liquidity_for_amount1(sa: &BigUint, sb: &BigUint, amount1: &BigUint) -> BigUint {
    let (sa, sb) = order(sa, sb);
    amount1 * q96() / (&sb - &sa)
}

pub fn liquidity_for_amounts(sp: &BigUint, sa: &BigUint, sb: &BigUint, a0: &BigUint, a1: &BigUint) -> BigUint {
    let (sa, sb) = order(sa, sb);
    if sp <= &sa {
        liquidity_for_amount0(&sa, &sb, a0)
    } else if sp < &sb {
        liquidity_for_amount0(sp, &sb, a0).min(liquidity_for_amount1(&sa, sp, a1))
    } else {
        liquidity_for_amount1(&sa, &sb, a1)
    }
}

/// Token amounts (raw units) for liquidity on [sa, sb) at sqrt price sp, rounded down.
pub fn amounts_for_liquidity(sp: &BigUint, sa: &BigUint, sb: &BigUint, liq: &BigUint) -> (BigUint, BigUint) {
    let (sa, sb) = order(sa, sb);
    if sp <= &sa {
        (liq * q96() * (&sb - &sa) / &sb / &sa, BigUint::zero())
    } else if sp < &sb {
        (liq * q96() * (&sb - sp) / &sb / sp, liq * (sp - &sa) / q96())
    } else {
        (BigUint::zero(), liq * (&sb - &sa) / q96())
    }
}

/// amounts_for_liquidity at ticks, as f64 raw amounts (for valuation).
pub fn amounts_at(sp: &BigUint, tl: i64, tu: i64, liq: &BigUint) -> (BigUint, BigUint) {
    amounts_for_liquidity(sp, &sqrt_ratio_at_tick(tl), &sqrt_ratio_at_tick(tu), liq)
}

pub fn to_f64(v: &BigUint) -> f64 {
    num_traits::ToPrimitive::to_f64(v).unwrap_or(f64::INFINITY)
}
