"""Market data for the UI, read from the Solidly UI (solidly.com) and its data processor.

- Pool list, TVL, active liquidity, tokens with USD prices: POST /api/getInitData/ (the UI's own
  init call). It wants a JWT {userId: <wallet>} signed (HS256) with the UI's public API key
  (NEXT_PUBLIC_API_KEY, shipped in the site's JS bundle). It is read from the bundle at runtime
  (cached a day, so a rotated key is picked up); ALM_SOLIDLY_API_KEY overrides it.
- Per-pool hourly fees / emissions / bribes / volume / TVL / active TVL: GET /api/getPoolAprHistory/
  (public). The 1h / 6h / 24h windows of the shortlist come from there, so they are exact sums.
- Wallet balances of every known token: batched balanceOf through the archive RPC.
Everything is cached (pools 5 min, APR windows 10 min, balances 2 min).
"""
import base64
import hashlib
import hmac
import json
import os
import re
import threading
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor

from . import logsetup
from .rpc import batch

BASE = "https://solidly.com/api"
HEADERS = {"User-Agent": "stable-alm/0.1", "Content-Type": "application/json"}
CHAIN_ID = 8453
WINDOWS = (1, 6, 24)
_log = logsetup.get("market")
_lock = threading.Lock()
_cache: dict = {}


def _cached(key, ttl: float, fn):
    with _lock:
        hit = _cache.get(key)
        if hit and time.time() - hit[1] < ttl:
            return hit[0]
    val = fn()
    with _lock:
        _cache[key] = (val, time.time())
    return val


def _b64(raw: bytes) -> bytes:
    return base64.urlsafe_b64encode(raw).rstrip(b"=")


def _jwt(user: str, key: str) -> str:
    head = _b64(json.dumps({"alg": "HS256", "typ": "JWT"}, separators=(",", ":")).encode())
    body = _b64(json.dumps({"userId": user, "iat": int(time.time())}, separators=(",", ":")).encode())
    sig = _b64(hmac.new(key.encode(), head + b"." + body, hashlib.sha256).digest())
    return (head + b"." + body + b"." + sig).decode()


def api_key() -> str:
    """The Solidly UI's public API key: env override, else read from the site's _app bundle."""
    if os.environ.get("ALM_SOLIDLY_API_KEY"):
        return os.environ["ALM_SOLIDLY_API_KEY"]

    def discover():
        ua = {"User-Agent": "Mozilla/5.0 stable-alm"}
        html = urllib.request.urlopen(urllib.request.Request("https://solidly.com/", headers=ua), timeout=30).read().decode("utf-8", "replace")
        srcs = sorted(set(re.findall(r'src="(/_next/static/[^"]+\.js)"', html)), key=lambda s: "_app" not in s)
        for src in srcs:
            js = urllib.request.urlopen(urllib.request.Request("https://solidly.com" + src, headers=ua), timeout=30).read().decode("utf-8", "replace")
            m = re.search(r'sign\(\{userId:[^}]{0,80}\},"([^"]{8,300})"', js)
            if m:
                return m.group(1)
        raise RuntimeError("could not find the Solidly UI's public API key in its bundle; set ALM_SOLIDLY_API_KEY")

    return _cached("api_key", 86400, discover)


def init_data(wallet: str) -> dict:
    key = api_key()

    def fetch():
        req = urllib.request.Request(f"{BASE}/getInitData/", data=json.dumps({"userAddress": wallet, "chainId": CHAIN_ID}).encode(),
                                     headers={**HEADERS, "Authorization": "Bearer " + _jwt(wallet, key)})
        return json.load(urllib.request.urlopen(req, timeout=90))

    return _cached(("init", wallet.lower()), 300, fetch)


# Peg families: a pair is "pegged" only when both tokens belong to the same family (and their prices
# agree within 0.8-1.25). A price near 1 alone is not enough (AERO trading at ~$1 is not a stable).
PEG_FAMILIES = {
    "USD": {"USDC", "USDT", "USDBC", "DAI", "DOLA", "USD+", "USDE", "SUSDE", "USDS", "GHO", "CRVUSD", "FRAX", "LUSD", "PYUSD",
            "USDA", "EUSD", "USDZ", "USDPLUS", "AXLUSDC", "USDT0"},
    "ETH": {"WETH", "ETH", "CBETH", "WSTETH", "WEETH", "RETH", "EZETH", "WRSETH", "RSETH", "SUPEROETHB", "OETH", "METH"},
    "BTC": {"CBBTC", "WBTC", "TBTC", "LBTC", "SOLVBTC"},
    "EUR": {"EURC", "EURE", "EURA", "AGEUR"},
    "MXN": {"MXNE", "XOC"},
}


def peg_family(symbol: str):
    s = (symbol or "").strip().upper()
    return next((f for f, members in PEG_FAMILIES.items() if s in members), None)


def is_pegged(sym0: str, sym1: str, price_ratio: float = None) -> bool:
    f0, f1 = peg_family(sym0), peg_family(sym1)
    if not f0 or f0 != f1:
        return False
    return price_ratio is None or 0.8 <= price_ratio <= 1.25


def _token(t: dict) -> dict:
    return {"address": t["id"].lower(), "symbol": t.get("symbol"), "name": t.get("name"), "decimals": int(t["decimals"]),
            "price_usd": float(t.get("priceUSD") or 0), "logo": t.get("logoURI")}


def pools(wallet: str) -> list:
    """Every Solidly V3 pool on Base, largest TVL first."""
    out = []
    for p in init_data(wallet).get("filledPools", []):
        agg = p.get("poolAggregation") or {}
        ia = agg.get("intervalAggregations") or {}
        t0, t1 = _token(p["token0"]), _token(p["token1"])
        out.append({
            "pool": p["id"].lower(), "symbol": p.get("symbol"), "token0": t0, "token1": t1,
            "tick_spacing": p.get("tickSpacing"), "fee_tier": p.get("feeTier"), "fee_pips": p.get("feeCurrent"),
            "tvl_usd": float(p.get("totalTvlUsd") or 0), "active_usd": float(agg.get("activeLiquidityUSD") or 0),
            "volume_24h": float((ia.get("HOUR_24") or {}).get("volumeUSD") or 0),
            "processor_apr_24h": float((ia.get("HOUR_24") or {}).get("aprFromFeesPercentage") or 0)
            + float((ia.get("HOUR_24") or {}).get("aprFromEmissionsPercentage") or 0),
            "whitelisted": bool(p.get("whitelisted")),
            "supported": True,  # any decimal combination; see engine/data px_scale & liq_scale
            "pegged": is_pegged(t0["symbol"], t1["symbol"],
                                t0["price_usd"] / t1["price_usd"] if t0["price_usd"] and t1["price_usd"] else None),
        })
    return sorted(out, key=lambda x: -x["tvl_usd"])


def apr_windows(pool: str) -> dict:
    """Sums over the last 1/6/24 complete hours: fees, emissions, bribes, volume, mean active TVL."""
    def fetch():
        url = f"{BASE}/getPoolAprHistory/?chainId={CHAIN_ID}&poolId={pool}&interval=hour&limit=25&offset=0"
        rows = json.load(urllib.request.urlopen(urllib.request.Request(url, headers=HEADERS), timeout=60)).get("history", [])
        rows = sorted(rows, key=lambda r: int(r["timestamp"]), reverse=True)
        out = {}
        for h in WINDOWS:
            part = rows[:h]
            if not part:
                continue
            f = lambda k: sum(float(r.get(k) or 0) for r in part)  # noqa: E731
            out[f"h{h}"] = {"hours": len(part), "fees": f("feesUSD"), "emissions": f("emissionsUSD"), "bribes": f("bribesUSD"),
                           "volume": f("volumeUSD"), "active_tvl": f("activeTvlUSD") / len(part), "tvl": f("tvlUSD") / len(part)}
        return out

    return _cached(("apr", pool.lower()), 600, fetch)


def shortlist(wallet: str, min_tvl: float = 50.0, limit: int = 40) -> list:
    """Pools with TVL >= min_tvl (plus every pool already configured is handled by the caller),
    each with its 1/6/24h windows (fetched in parallel, 4 at a time)."""
    ps = [p for p in pools(wallet) if p["tvl_usd"] >= min_tvl][:limit]

    def one(p):
        try:
            p = {**p, "windows": apr_windows(p["pool"])}
        except Exception as e:  # noqa: BLE001
            p = {**p, "windows": {}, "error": str(e)}
        return p

    with ThreadPoolExecutor(4) as ex:
        return list(ex.map(one, ps))


def tokens(wallet: str, extra: list = ()) -> list:
    """All tokens of Solidly V3 pools (+ extra addresses) with the wallet's balance and USD value."""
    def fetch():
        known: dict = {}
        try:
            for p in pools(wallet):
                for t in (p["token0"], p["token1"]):
                    known.setdefault(t["address"], t)
        except Exception as e:  # noqa: BLE001
            _log.warning("token list from the Solidly UI unavailable: %s", e)
        for a in extra:
            known.setdefault(a.lower(), {"address": a.lower(), "symbol": a[:8], "decimals": None, "price_usd": 0})
        addrs = list(known)
        data = "0x70a08231" + "0" * 24 + wallet.lower()[2:]
        bals = []
        for i in range(0, len(addrs), 50):
            bals += batch([("eth_call", [{"to": a, "data": data}, "latest"]) for a in addrs[i:i + 50]])
        out = []
        for a, b in zip(addrs, bals):
            t = dict(known[a])
            raw = int(b, 16) if isinstance(b, str) and len(b) > 2 else 0
            dec = t.get("decimals")
            t["balance"] = raw / 10**dec if dec is not None else None
            t["balance_usd"] = (t["balance"] or 0) * (t.get("price_usd") or 0)
            out.append(t)
        return sorted(out, key=lambda t: (-t["balance_usd"], t.get("symbol") or ""))

    return _cached(("tokens", wallet.lower(), tuple(sorted(extra))), 120, fetch)
