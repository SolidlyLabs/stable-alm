"""Runtime settings shared by the worker and the web UI.

config/settings.json  global options (live/dry-run, cadence, claims, reward selling or SOLID LP, funding tokens)
config/pools.json     one entry per pool (strategy, limits, allocation_pct, enabled)
Both are re-read at the start of every cycle, so UI edits apply on the next pass. Writes are atomic.
"""
import json
import os
import tempfile
import threading

from .paths import CONFIG, HOME as ROOT  # noqa: E402,F401 - ROOT: the writable state folder
SETTINGS_PATH = os.path.join(CONFIG, "settings.json")
POOLS_PATH = os.path.join(CONFIG, "pools.json")
_lock = threading.Lock()

USDC = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913"
SOLID = "0x777cf5ba9c291a1a8f57ff14836f6f9dc5c0f9dd"
SOLID_WETH_POOL = "0xd09f2ebaf365fc97a5bbdefc15703312165638e4"  # WETH/SOLID 1%, the deepest SOLID pool

DEFAULTS = {
    # Live trading needs BOTH this flag and a configured signer. Off = dry run (plan + simulate only).
    "execute": False,
    "wallet": os.environ.get("ALM_WALLET", "").lower(),
    "interval_s": 300,
    "claim": {"enabled": True, "every_days": 7},
    # Reward tokens that are not pool tokens (SOLID) are sold into the pool's tokens at the next
    # rebalance, through the aggregator, only if the quote loses at most max_impact_pct and is worth
    # at least min_value_usd. Pool-token rewards (USDC, USDT, ...) compound as they are.
    "sell_rewards": {"enabled": True, "tokens": [SOLID], "max_impact_pct": 3.0, "min_value_usd": 2.0},
    # Allocation shortfalls (a pool's share needs tokens the wallet does not hold) are bought with
    # these tokens through the aggregator, largest USD balance first. [] disables cross-pool funding.
    "funding_tokens": [USDC],
    # Claimed SOLID goes into a SOLID pool, full range, paired with the same USD value of the pool's
    # other token (wallet first, then bought with the funding tokens). Replaces SOLID sales while on.
    # The deposit waits while the pool's SOLID price is more than max_price_dev_pct off the market.
    "solid_lp": {"enabled": False, "pool": SOLID_WETH_POOL, "min_value_usd": 5.0, "max_price_dev_pct": 3.0,
                 "slippage_bps": 50.0, "max_swap_cost_bps": 50.0},
    "retries": {"attempts": 3, "backoff_s": 10},
    # Idle tokens worth more than this % of a pool's allocation are deployed without waiting for
    # the strategy's next re-solve (recovers failed or partial deposits).
    "idle_redeploy_pct": 2.0,
    # A rebalance keeps a position whose range stays in the target and only trims (partial
    # withdraw) or tops it up; differences below this % of the pool's allocation are left alone.
    "resize_tolerance_pct": 1.0,
}

POOL_DEFAULTS = {
    "strategy": {"type": "ladder", "lookback": 86400, "span": 3, "every": 72, "turnover": 0.4, "guard": 15},
    "slippage_bps": 2.0,
    "max_swap_cost_bps": 3.0,
    "max_spot_dev_ticks": 3,
    "max_actions_per_day": 5,
    "min_swap_value": 5.0,
    "numeraire": 0,
    "max_ranges": 5,
    "allocation_pct": 0.0,
    "enabled": False,
}


def _write(path: str, obj) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(path), suffix=".tmp")
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(obj, f, indent=2)
        f.write("\n")
    os.replace(tmp, path)


def _merge(base: dict, over: dict) -> dict:
    out = dict(base)
    for k, v in (over or {}).items():
        out[k] = _merge(base[k], v) if isinstance(v, dict) and isinstance(base.get(k), dict) else v
    return out


def load_settings() -> dict:
    with _lock:
        cur = json.load(open(SETTINGS_PATH, encoding="utf-8")) if os.path.exists(SETTINGS_PATH) else {}
    if "funding_token" in cur:  # migrate the old single-token setting
        old = cur.pop("funding_token")
        cur.setdefault("funding_tokens", [old] if old else [])
    s = _merge(DEFAULTS, cur)
    s["funding_tokens"] = [t.lower() for t in s.get("funding_tokens") or [] if t]
    return s


def save_settings(s: dict) -> None:
    with _lock:
        _write(SETTINGS_PATH, s)


def load_pools() -> list:
    with _lock:
        raw = json.load(open(POOLS_PATH, encoding="utf-8")) if os.path.exists(POOLS_PATH) else []
    return [_merge(POOL_DEFAULTS, p) for p in raw]


def save_pools(pools: list) -> None:
    total = sum(float(p.get("allocation_pct", 0)) for p in pools if p.get("enabled"))
    if total > 100.0001:
        raise ValueError(f"allocations of enabled pools add up to {total:.1f}% (> 100%)")
    names = [p["name"] for p in pools]
    if len(set(names)) != len(names):
        raise ValueError("pool names must be unique")
    with _lock:
        _write(POOLS_PATH, pools)
