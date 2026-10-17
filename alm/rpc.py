"""JSON-RPC access for Base.

Reads go to ALM_RPC_URL (default: the Solidly read-only archive proxy, which allows batches of up to
50 calls and rate-limits per IP; a 429 is retried after a pause). That proxy refuses
eth_sendRawTransaction (HTTP 403), so signed transactions go to ALM_SEND_RPC_URL instead
(default: Base's public endpoint).
"""
import json
import os
import time
import urllib.error
import urllib.request

RPC_URL = os.environ.get("ALM_RPC_URL") or "https://solidly.com/api/rpc/{chain_id}/"
SEND_RPC_URL = os.environ.get("ALM_SEND_RPC_URL") or "https://mainnet.base.org"
BASE_GENESIS_TS = 1686789347  # Base produces a block every 2s exactly since genesis


def base_block_ts(block: int) -> int:
    return BASE_GENESIS_TS + 2 * block


def _post(url: str, payload, timeout: int = 90):
    req = urllib.request.Request(
        url,
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json", "User-Agent": "stable-alm/0.1"},
    )
    return json.load(urllib.request.urlopen(req, timeout=timeout))


def call(method: str, params: list, chain_id: int = 8453):
    return batch([(method, params)], chain_id)[0]


def send_call(method: str, params: list, retries: int = 4):
    """One JSON-RPC call to the transaction endpoint. Returns the result, or the error dict."""
    delay = 2
    for attempt in range(retries):
        try:
            out = _post(SEND_RPC_URL, {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}, timeout=30)
            return out.get("result", out.get("error"))
        except (urllib.error.URLError, TimeoutError):
            if attempt == retries - 1:
                raise
            time.sleep(delay)
            delay *= 2


def batch(calls: list, chain_id: int = 8453, retries: int = 8):
    """Run (method, params) pairs as one JSON-RPC batch; returns results in order (or error dicts)."""
    url = RPC_URL.format(chain_id=chain_id)
    payload = [{"jsonrpc": "2.0", "id": i, "method": m, "params": p} for i, (m, p) in enumerate(calls)]
    delay = 5
    for _ in range(retries):
        try:
            out = _post(url, payload)
            if isinstance(out, dict):
                out = [out]
            by_id = {r.get("id"): r for r in out}
            return [by_id.get(i, {"error": "missing"}).get("result", by_id.get(i, {}).get("error")) for i in range(len(calls))]
        except urllib.error.HTTPError as e:
            if e.code in (429, 502, 503, 504):
                time.sleep(delay)
                delay = min(delay * 2, 60)
                continue
            raise
        except (urllib.error.URLError, TimeoutError):
            time.sleep(delay)
            delay = min(delay * 2, 60)
    raise RuntimeError("RPC batch failed after retries")
