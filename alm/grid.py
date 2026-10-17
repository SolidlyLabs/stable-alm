"""The strategy grid that walk-forward chooses from. Bars are 5 minutes: 12 = 1h, 288 = 1d."""
from itertools import product

from .strategies import HoldUSDC, Ladder, Range24h, Recenter, StaticBand, VolBand


def grid() -> list:
    out = [HoldUSDC()]
    # The 2024 vote-optimizer rule: out of range -> recentre +-2% with a swap.
    out.append(Recenter(k=200, dwell=1, cooldown=0, swap=True))
    for k in (0, 1, 2, 3, 5, 8, 12, 20):
        out.append(StaticBand(k=k))
    for k, (dwell, cooldown) in product((0, 1, 2, 3, 5, 8), ((1, 0), (1, 72), (12, 0), (12, 72), (72, 0), (288, 0))):
        out.append(Recenter(k=k, dwell=dwell, cooldown=cooldown, swap=True))
        out.append(Recenter(k=k, dwell=dwell, cooldown=cooldown, swap=False, limit=max(2, k + 1)))
    # Indicator bands. swap=False: widen/tighten/recentre around the anchor with the inventory as
    # is (leftover into a limit range), never swapping. swap=True: same, but swap to the ratio the
    # new band needs ("rebalance when necessary").
    for vol, lb, z, kmin, anchor, swap in product(("std", "atr"), (288, 2016), (1, 2, 3), (0, 1, 2), ("spot", "median"),
                                                  (False, True)):
        out.append(VolBand(vol=vol, lookback=lb, z=z, kmin=kmin, kmax=20, anchor=anchor, anchor_lb=lb, every=12,
                           move=2, dwell=72, cooldown=72, swap=swap, limit=3))
    for lb_days, span, every, turnover in product((1, 3, 7), (3, 6), (72, 288), (0.15, 0.4)):
        out.append(Ladder(lookback=lb_days * 86400, span=span, every=every, turnover=turnover))
    return out


GRIDS = {"main": grid}


def guard_grid() -> list:
    """Leading configs with and without the depeg guard (ticks from the weekly median)."""
    out = [HoldUSDC(), StaticBand(k=5), StaticBand(k=8), StaticBand(k=12)]
    for g in (None, 8, 15, 30):
        gp = {} if g is None else {"guard": g}
        for lb_days, span, every, turnover in product((1, 3), (3, 6), (72, 288), (0.4,)):
            out.append(Ladder(lookback=lb_days * 86400, span=span, every=every, turnover=turnover, **gp))
        for k, dwell in product((2, 3, 5, 8), (72, 288)):
            out.append(Recenter(k=k, dwell=dwell, cooldown=0, swap=True, **gp))
            out.append(Recenter(k=k, dwell=dwell, cooldown=0, swap=False, limit=k + 1, **gp))
        for vol, z, anchor in product(("std", "atr"), (1, 2, 3), ("median",)):
            out.append(VolBand(vol=vol, lookback=2016, z=z, kmin=1, kmax=20, anchor=anchor, anchor_lb=2016, every=12,
                               move=2, dwell=72, cooldown=72, swap=True, limit=3, **gp))
    return out


GRIDS["guard"] = guard_grid


def range24_grid() -> list:
    """24h-range rebalancing with a directional buffer and a wait before acting (seconds)."""
    out = []
    for pad, buf, dwell, guard in product((0, 1, 3), (0, 2, 5, 10), (0, 300, 1800), (None, 15)):
        gp = {} if guard is None else {"guard": guard}
        out.append(Range24h(lookback_s=86400, pad=pad, buffer=buf, dwell_s=dwell, min_width=2, swap=True, **gp))
    return out


def all_grid() -> list:
    """Every family, for every pair: the main grid + guarded leaders + the 24h-range family."""
    seen, out = set(), []
    for s in grid() + guard_grid() + range24_grid():
        if s.label() not in seen:
            seen.add(s.label())
            out.append(s)
    return out


def delay_grid() -> list:
    """Wait-before-rebalance test; run on 1-minute bars (--bars 60). dwell counts bars there."""
    out = []
    for k, dwell in product((1, 2, 3, 5, 8), (1, 5, 15, 60)):
        out.append(Recenter(k=k, dwell=dwell, cooldown=0, swap=True, guard=15))
    for buf, dwell_s in product((0, 3), (0, 300, 900, 3600)):
        out.append(Range24h(lookback_s=86400, pad=1, buffer=buf, dwell_s=dwell_s, min_width=2, swap=True, guard=15))
    return out


GRIDS.update({"range24": range24_grid, "all": all_grid, "delay": delay_grid})


def adaptive_grid() -> list:
    """Volatility-adaptive width and timing, with and without a trend skew."""
    from .strategies import Adaptive

    out = []
    for vol, z, timing, skew in product(("std", "atr", "ewma", "parkinson"), (1.5, 3.0), ("fixed", "vol_up", "vol_down"),
                                        ("none", "momentum", "meanrev", "ema")):
        out.append(Adaptive(vol=vol, vol_lb_s=86400, z=z, kmin=1, kmax=40, timing=timing, dwell_s=900, recheck_s=3600,
                            skew=skew, skew_lb_s=86400, skew_max=0 if skew == "none" else 5, skew_gain=1.0, move=2,
                            guard=15))
    return out


GRIDS["adaptive"] = adaptive_grid


def full_grid() -> list:
    return all_grid() + adaptive_grid()


GRIDS["full"] = full_grid


def ideas_grid() -> list:
    """Novel-ideas round (reports/IDEAS.md): the live ladder family as the baseline, then one family
    per idea. Every config runs the 15-tick depeg guard, like the live system."""
    from .ladder_x import LadderX

    g = {"guard": 15}
    out = []
    for lb_days, span, every, turnover in product((1, 3, 7), (3, 6), (72, 288), (0.15, 0.4)):
        out.append(Ladder(lookback=lb_days * 86400, span=span, every=every, turnover=turnover, **g))
    for hl_h, lb_days, span, turnover in product((3, 6, 12), (1, 3), (3, 6), (0.15, 0.4)):
        out.append(LadderX(lookback=lb_days * 86400, span=span, every=72, turnover=turnover, decay_hl_s=hl_h * 3600, **g))
    for every, turnover, span, hl in product((12, 24), (0.4, 0.6, 0.8), (3, 6), (0, 21600)):
        out.append(LadderX(lookback=86400, span=span, every=every, turnover=turnover, **({"decay_hl_s": hl} if hl else {}),
                           tag="fast", **g))
    for exit_s, span, every in product((1800, 7200, 21600), (3, 6), (72, 288)):
        out.append(LadderX(lookback=86400, span=span, every=every, turnover=0.4, exit_s=exit_s, **g))
    for frac, k, span in product((0.1, 0.2), (3, 6), (3, 6)):
        out.append(LadderX(lookback=86400, span=span, every=72, turnover=0.4, core_frac=frac, core_k=k, **g))
    for lb_days, span, every, turnover in product((1, 3), (3, 6), (72, 288), (0.15, 0.4)):
        out.append(LadderX(lookback=lb_days * 86400, span=span, every=every, turnover=turnover, swapfree=True, **g))
    for lb_days, span, every, turnover in product((1, 3), (3, 6), (72, 288), (0.15, 0.4)):
        out.append(LadderX(lookback=lb_days * 86400, span=span, every=every, turnover=turnover, emis=True, **g))
    for span, every, turnover in product((3, 6), (72, 288), (0.15, 0.4)):
        out.append(LadderX(lookback=86400, span=span, every=every, turnover=turnover, center="blend", **g))
    return out


GRIDS["ideas"] = ideas_grid


def ideas2_grid() -> list:
    """Round 2 (reports/IDEAS.md): forecast placement, a fast+slow dual ladder, and the round-1
    positives combined. Baseline family included again for the comparison."""
    from .ladder_x import Dual, LadderX

    g = {"guard": 15}
    out = []
    for lb_days, span, every, turnover in product((1, 3, 7), (3, 6), (72, 288), (0.15, 0.4)):
        out.append(Ladder(lookback=lb_days * 86400, span=span, every=every, turnover=turnover, **g))
    for span, every, fc_days in product((3, 6), (72, 288), (3, 7)):
        out.append(LadderX(lookback=86400, span=span, every=every, turnover=0.4, forecast=0.5, fc_lookback_s=fc_days * 86400, **g))
    for fspan, slow_days, w in product((3, 6), (3, 7), (0.5, 0.7)):
        out.append(Dual(fast={"lookback": 86400, "span": fspan, "every": 72, "turnover": 0.4},
                        slow={"lookback": slow_days * 86400, "span": 6, "every": 288, "turnover": 0.4}, w=w, **g))
    for span, every, k in product((3, 6), (72, 288), (3, 6)):
        out.append(LadderX(lookback=86400, span=span, every=every, turnover=0.4, decay_hl_s=21600, emis=True,
                           core_frac=0.1, core_k=k, **g))
    return out


GRIDS["ideas2"] = ideas2_grid


def nested_grid() -> list:
    """Overlapping ranges vs the ladder (alm/nested.py): the live ladder family as the baseline, the
    ladder rebuilt as nested ranges, and centred pyramids of overlapping ranges."""
    from .nested import NestedLadder, Pyramid

    g = {"guard": 15}
    out = []
    for lb_days, span, every, turnover in product((1, 3, 7), (3, 6), (72, 288), (0.15, 0.4)):
        out.append(Ladder(lookback=lb_days * 86400, span=span, every=every, turnover=turnover, **g))
    for span, every, turnover in product((3, 6), (72, 288), (0.15, 0.4)):
        out.append(NestedLadder(lookback=86400, span=span, every=every, turnover=turnover, **g))
    for ks, a, every in product(((0, 1), (0, 1, 2), (0, 1, 2, 3), (0, 2, 4, 6), (0, 1, 3, 6)), (0.0, 0.5, 1.0), (72, 288)):
        out.append(Pyramid(ks=ks, a=a, lookback=86400, span=3, every=every, turnover=0.4, **g))
    return out


GRIDS["nested"] = nested_grid
