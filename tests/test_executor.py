"""Offline tests for the executor's math and calldata (no network)."""
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from eth_abi import decode  # noqa: E402

from alm.executor import chain  # noqa: E402
from alm.executor.tickmath import (  # noqa: E402
    MAX_SQRT_RATIO, MAX_TICK, MIN_SQRT_RATIO, MIN_TICK, Q96, amounts_for_liquidity, liquidity_for_amounts,
    sqrt_ratio_at_tick,
)
from alm.pool_math import amounts_for_liquidity as f_amounts  # noqa: E402

WALLET = "0x1111111111111111111111111111111111111111"


class TickMath(unittest.TestCase):
    def test_bounds(self):
        self.assertEqual(sqrt_ratio_at_tick(0), Q96)
        self.assertEqual(sqrt_ratio_at_tick(MIN_TICK), MIN_SQRT_RATIO)
        self.assertEqual(sqrt_ratio_at_tick(MAX_TICK), MAX_SQRT_RATIO)

    def test_monotonic(self):
        prev = 0
        for t in range(-50, 51):
            s = sqrt_ratio_at_tick(t)
            self.assertGreater(s, prev)
            prev = s

    def test_liquidity_roundtrip_matches_float_model(self):
        sp = sqrt_ratio_at_tick(13) + 10**20
        sa, sb = sqrt_ratio_at_tick(12), sqrt_ratio_at_tick(19)
        liq = liquidity_for_amounts(sp, sa, sb, 10_000 * 10**6, 5_000 * 10**6)
        a0, a1 = amounts_for_liquidity(sp, sa, sb, liq)
        self.assertLessEqual(a0, 10_000 * 10**6)
        self.assertLessEqual(a1, 5_000 * 10**6)
        fx, fy = f_amounts(liq / 1e6, sp / Q96, 12, 19)
        self.assertAlmostEqual(a0 / 1e6, fx, delta=1e-3)
        self.assertAlmostEqual(a1 / 1e6, fy, delta=1e-3)

    def test_out_of_range_single_sided(self):
        sa, sb = sqrt_ratio_at_tick(20), sqrt_ratio_at_tick(21)
        a0, a1 = amounts_for_liquidity(sqrt_ratio_at_tick(10), sa, sb, 10**12)
        self.assertGreater(a0, 0)
        self.assertEqual(a1, 0)


class Calldata(unittest.TestCase):
    def test_selectors(self):
        self.assertEqual(chain.selector("approve(address,uint256)").hex(), "095ea7b3")
        self.assertEqual(chain.selector("slot0()").hex(), "3850c7bd")
        self.assertEqual(chain.selector("balanceOf(address)").hex(), "70a08231")

    def test_mint_encoding(self):
        types = ["address", "int24", "int24", "uint128", "uint256", "uint256", "uint256"]
        args = [WALLET, -3, 4, 123456789, 1, 2, 1_800_000_000]
        data = chain.calldata("mint(address,int24,int24,uint128,uint256,uint256,uint256)", types, args)
        dec = decode(types, bytes.fromhex(data[10:]))
        self.assertEqual(dec[0].lower(), WALLET)
        self.assertEqual(list(dec[1:]), args[1:])

    def test_burn_encoding_negative_ticks(self):
        types = ["address", "int24", "int24", "uint128", "uint256", "uint256", "uint128", "uint128", "uint256"]
        args = [WALLET, -887272, 887272, 5, 0, 0, chain.MAX_UINT128, chain.MAX_UINT128, 1]
        sig = "burnAndCollect(address,int24,int24,uint128,uint256,uint256,uint128,uint128,uint256)"
        data = chain.calldata(sig, types, args)
        self.assertEqual(list(decode(types, bytes.fromhex(data[10:]))[1:3]), [-887272, 887272])


class Config(unittest.TestCase):
    def test_pool_config_loads_and_strategy_builds(self):
        from alm.executor.engine import load_configs, make_strategy

        cfgs = load_configs()
        self.assertTrue(cfgs)
        for c in cfgs:
            s = make_strategy(c.strategy)
            self.assertIn("guard", s.params)  # every live strategy runs with the depeg guard


if __name__ == "__main__":
    unittest.main()


def test_resize_plan_keeps_same_ranges_and_only_trims_or_tops_up():
    from alm.executor.engine import resize_plan

    # Budget cut from 1000 to 660 with the same three ranges: every position is trimmed, none rebuilt.
    current = [(11, 12, 1000, 100.0), (12, 13, 3000, 300.0), (14, 15, 6000, 600.0)]
    target = [(11, 12, 0.1), (12, 13, 0.3), (14, 15, 0.6)]
    burns, adds = resize_plan(current, target, 660.0, tol=5.0, dust=5.0)
    assert adds == []
    assert [(a, b) for a, b, _ in burns] == [(11, 12), (12, 13), (14, 15)]
    for (_, _, liq, _), (_, _, cut) in zip(current, burns):
        assert abs(cut / liq - 0.34) <= 2e-3  # partial: 34% of each position, not all of it


def test_resize_plan_mixes_withdraw_trim_add_and_new():
    from alm.executor.engine import resize_plan

    current = [(10, 11, 500, 50.0), (12, 13, 4000, 400.0), (13, 14, 2000, 200.0), (14, 15, 3500, 350.0)]
    target = [(12, 13, 0.2), (13, 14, 0.2), (14, 15, 0.4), (15, 16, 0.2)]  # budget 1000
    burns, adds = resize_plan(current, target, 1000.0, tol=10.0, dust=5.0)
    assert (10, 11, 500) in burns  # range dropped: withdrawn in full
    assert (12, 13, 2000) in burns  # 400 -> 200: half the liquidity
    assert all(b[:2] != (13, 14) for b in burns)  # exactly on target: untouched
    assert dict(((a, b), v) for a, b, v in adds) == {(14, 15): 50.0, (15, 16): 200.0}


def test_resize_plan_tolerance_and_dust():
    from alm.executor.engine import resize_plan

    burns, adds = resize_plan([(1, 2, 100, 104.0)], [(1, 2, 1.0)], 100.0, tol=5.0, dust=5.0)
    assert burns == [] and adds == []  # within tolerance: no transaction
    burns, _ = resize_plan([(1, 2, 100, 100.0), (2, 3, 100, 100.0)], [(1, 2, 0.98), (2, 3, 0.02)], 100.0,
                           tol=1.0, dust=5.0)
    assert (2, 3, 100) in burns  # the trim would leave dust: withdraw the whole position
