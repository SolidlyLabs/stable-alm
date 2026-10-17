"""Minimal chain access for the executor: reads, ABI encoding, gas, signing and sending.

Uses eth_abi / eth_account directly (no web3 provider), so the exact calldata is visible and every
transaction can be simulated with eth_call before it is sent. Reads go through alm.rpc.call (archive
proxy); signed transactions go through alm.rpc.send_call (ALM_SEND_RPC_URL).
"""
import json
import os
import time

from eth_abi import decode, encode
from eth_account import Account
from eth_utils import keccak, to_checksum_address

from .. import logsetup
from ..rpc import call, send_call

log = logsetup.get("chain")
ABI_DIR = os.path.join(os.path.dirname(__file__), "..", "abi")
MAX_UINT128 = (1 << 128) - 1
MAX_UINT256 = (1 << 256) - 1

# Revert strings of Solidly V3 / Uniswap V3 pools and common routers, in plain words for the log.
REVERT_HINTS = {
    "Price slippage check": "pool price moved past the min amounts (slippage)",
    "STF": "token transferFrom failed (balance or allowance too low)",
    "TF": "token transfer failed",
    "LOK": "pool is locked (reentrancy guard)",
    "TLU": "tickLower must be below tickUpper",
    "TLM": "tickLower below the minimum tick",
    "TUM": "tickUpper above the maximum tick",
    "AS": "swap amount is zero",
    "SPL": "price limit is outside the allowed range",
    "Transaction too old": "deadline passed",
    "EXPIRED": "deadline passed",
    "Too little received": "aggregator min-out not met (price moved)",
    "Return amount is not enough": "aggregator min-out not met (price moved)",
    "insufficient funds": "not enough ETH for gas",
}


class TxError(RuntimeError):
    """A transaction would revert or did revert; `reason` is the decoded revert string."""

    def __init__(self, message: str, reason: str = ""):
        super().__init__(message)
        self.reason = reason


def selector(signature: str) -> bytes:
    return keccak(text=signature)[:4]


def calldata(signature: str, types: list, args: list) -> str:
    return "0x" + (selector(signature) + encode(types, args)).hex()


def revert_reason(err) -> str:
    """Decode an RPC error (dict or string) to a revert string plus a hint, where known."""
    if not isinstance(err, dict):
        return str(err)
    msg = str(err.get("message", ""))
    data = err.get("data")
    if isinstance(data, dict):
        data = data.get("data") or data.get("result")
    text = msg
    if isinstance(data, str) and data.startswith("0x08c379a0") and len(data) >= 138:  # Error(string)
        try:
            text = decode(["string"], bytes.fromhex(data[10:]))[0]
        except Exception:
            pass
    elif isinstance(data, str) and data.startswith("0x4e487b71"):  # Panic(uint256)
        text = f"panic 0x{int(data[10:], 16):x}"
    for k, hint in REVERT_HINTS.items():
        if k in text or k in msg:
            return f"{text} ({hint})"
    return text or json.dumps(err)


# --- reads -------------------------------------------------------------------------------------
# The read RPC is a load balancer: consecutive "latest" reads can hit nodes a block or two behind,
# which right after our own transaction shows balances, allowances and positions from before it.
# Once a transaction of ours is mined, reads are pinned to a block at or after it; a node that does
# not have that block yet answers "Unknown block" and the read is retried (another node, or later).
_floor = 0
_head = [0, 0.0]  # last seen head block, time


def _tag(block: str) -> str:
    if block != "latest" or not _floor:
        return block
    if time.time() - _head[1] > 1.0:
        try:
            _head[:] = [int(call("eth_blockNumber", []), 16), time.time()]
        except Exception:  # noqa: BLE001 - fall back to the floor itself
            pass
    return hex(max(_head[0], _floor))


def _unknown_block(r) -> bool:
    return isinstance(r, dict) and ("unknown block" in str(r.get("message", "")).lower()
                                    or "header not found" in str(r.get("message", "")).lower())


def _read(method: str, params_for) -> object:
    """call(method, params_for(tag)), retried while the node has not reached the pinned block."""
    for i in range(15):
        r = call(method, params_for(_tag("latest")))
        if not _unknown_block(r):
            return r
        _head[1] = 0.0  # re-read the head next time
        time.sleep(1 + i * 0.5)
    raise RuntimeError(f"{method}: no RPC node reached block {_floor} in time")


def note_mined(block: int) -> None:
    global _floor
    _floor = max(_floor, int(block))


def eth_call(to: str, data: str, frm: str = None, block: str = "latest") -> str:
    tx = {"to": to, "data": data}
    if frm:
        tx["from"] = frm
    r = _read("eth_call", lambda tag: [tx, tag]) if block == "latest" else call("eth_call", [tx, block])
    if not isinstance(r, str):
        reason = revert_reason(r)
        raise TxError(f"eth_call reverted: {reason}", reason)
    return r


def slot0(pool: str) -> tuple:
    r = bytes.fromhex(eth_call(pool, calldata("slot0()", [], []))[2:])
    sp, tick, fee = decode(["uint160", "int24", "uint24"], r[:96])
    return sp, tick, fee


def position_liquidity(pool: str, owner: str, tl: int, tu: int) -> int:
    key = keccak(bytes.fromhex(owner[2:].lower()) + tl.to_bytes(3, "big", signed=True) + tu.to_bytes(3, "big", signed=True))
    r = bytes.fromhex(eth_call(pool, calldata("positions(bytes32)", ["bytes32"], [key]))[2:])
    return decode(["uint128"], r[:32])[0]


def erc20_balance(token: str, owner: str) -> int:
    return int(eth_call(token, calldata("balanceOf(address)", ["address"], [owner])), 16)


def erc20_allowance(token: str, owner: str, spender: str) -> int:
    return int(eth_call(token, calldata("allowance(address,address)", ["address", "address"], [owner, spender])), 16)


def erc20_meta(token: str) -> dict:
    dec = int(eth_call(token, calldata("decimals()", [], [])), 16)
    try:
        raw = bytes.fromhex(eth_call(token, calldata("symbol()", [], []))[2:])
        sym = decode(["string"], raw)[0] if len(raw) > 32 else raw.rstrip(b"\0").decode()
    except Exception:
        sym = token[:8]
    return {"address": token.lower(), "decimals": dec, "symbol": sym}


def pool_tokens(pool: str) -> tuple:
    t0 = "0x" + eth_call(pool, calldata("token0()", [], []))[-40:]
    t1 = "0x" + eth_call(pool, calldata("token1()", [], []))[-40:]
    return t0, t1


def tick_spacing(pool: str) -> int:
    return int(eth_call(pool, calldata("tickSpacing()", [], [])), 16)


def eth_balance(addr: str) -> int:
    return int(_read("eth_getBalance", lambda tag: [addr, tag]), 16)


# --- signer ------------------------------------------------------------------------------------

def _read_secret(var: str):
    """Value of VAR, or the contents of the file named by VAR_FILE."""
    if os.environ.get(var):
        return os.environ[var].strip()
    path = os.environ.get(f"{var}_FILE")
    if path and os.path.exists(path):
        with open(path, encoding="utf-8") as f:
            return f.read().strip()
    return None


def signer_configured() -> bool:
    return bool(_read_secret("ALM_PRIVATE_KEY")
                or (os.environ.get("ALM_KEYSTORE") and os.path.exists(os.environ["ALM_KEYSTORE"])))


def load_account():
    """The signing key comes only from the environment, a secret file, or an encrypted keystore,
    in that order.

    ALM_PRIVATE_KEY / ALM_PRIVATE_KEY_FILE, or ALM_KEYSTORE + ALM_KEYSTORE_PASSWORD(_FILE).
    The key is never logged or written anywhere in clear by this code.
    """
    key = _read_secret("ALM_PRIVATE_KEY")
    if key:
        return Account.from_key(key if key.startswith("0x") else "0x" + key)
    path = os.environ.get("ALM_KEYSTORE")
    if path:
        password = _read_secret("ALM_KEYSTORE_PASSWORD")
        if password is None:
            raise RuntimeError("ALM_KEYSTORE is set but ALM_KEYSTORE_PASSWORD(_FILE) is not")
        with open(path, encoding="utf-8") as f:
            return Account.from_key(Account.decrypt(json.load(f), password))
    raise RuntimeError("No signer: set ALM_PRIVATE_KEY, ALM_PRIVATE_KEY_FILE, or ALM_KEYSTORE")


# --- transactions ------------------------------------------------------------------------------

def simulate(frm: str, to: str, data: str) -> dict:
    """eth_call + eth_estimateGas as the sender. Raises TxError with the revert reason if it would fail."""
    eth_call(to, data, frm)
    gas = _read("eth_estimateGas", lambda tag: [{"from": frm, "to": to, "data": data}, tag])
    if not isinstance(gas, str):
        reason = revert_reason(gas)
        raise TxError(f"estimateGas failed: {reason}", reason)
    return {"gas": int(gas, 16)}


def _receipt(h: str):
    rc = call("eth_getTransactionReceipt", [h])
    if not (isinstance(rc, dict) and rc.get("blockNumber")):
        rc = send_call("eth_getTransactionReceipt", [h])
    return rc if isinstance(rc, dict) and rc.get("blockNumber") else None


def send(account, to: str, data: str, gas: int, chain_id: int = 8453, wait: int = 90, bumps: int = 3) -> dict:
    """Sign and broadcast; if not mined within `wait` s, re-broadcast with the same nonce and fees
    raised 25% (replacement), up to `bumps` times. Returns when any broadcast copy is mined; raises
    TxError if it reverted."""
    nonce = int(send_call("eth_getTransactionCount", [account.address, "pending"]), 16)
    base = int(call("eth_getBlockByNumber", ["latest", False])["baseFeePerGas"], 16)
    tip = max(int(call("eth_maxPriorityFeePerGas", []), 16), 1_000_000)
    max_fee = base * 2 + tip
    hashes = []
    for attempt in range(bumps + 1):
        tx = {
            "chainId": chain_id, "nonce": nonce, "to": to_checksum_address(to), "data": data, "value": 0,
            "gas": int(gas * 1.25) + 10_000, "maxFeePerGas": max_fee, "maxPriorityFeePerGas": tip, "type": 2,
        }
        signed = account.sign_transaction(tx)
        raw = getattr(signed, "raw_transaction", None) or signed.rawTransaction
        h = send_call("eth_sendRawTransaction", ["0x" + raw.hex().removeprefix("0x")])
        if isinstance(h, str):
            hashes.append(h)
            log.info("broadcast %s nonce %d (attempt %d)", h, nonce, attempt + 1)
        else:
            msg = revert_reason(h)
            low = msg.lower()
            if "nonce too low" in low or "already known" in low:
                pass  # an earlier copy is in the mempool or mined; keep waiting on the hashes we have
            elif not hashes:
                raise TxError(f"send rejected: {msg}", msg)
            else:
                log.warning("replacement rejected: %s", msg)
        t0 = time.time()
        while time.time() - t0 < wait:
            for hh in hashes:
                rc = _receipt(hh)
                if rc:
                    if rc.get("status") != "0x1":
                        raise TxError(f"transaction {hh} reverted on-chain", "reverted")
                    note_mined(int(rc["blockNumber"], 16))  # later reads must see this transaction
                    return {"hash": hh, "gas_used": int(rc["gasUsed"], 16), "block": int(rc["blockNumber"], 16)}
            if int(send_call("eth_getTransactionCount", [account.address, "latest"]), 16) > nonce and hashes:
                # The nonce is used: one of our copies mined (receipt may lag a little).
                time.sleep(3)
                continue
            time.sleep(2)
        tip, max_fee = int(tip * 1.25) + 1, int(max_fee * 1.25) + 1
        log.warning("not mined in %ds; re-broadcasting with higher fees", wait)
    raise TxError(f"transaction (nonce {nonce}) not mined after {bumps} fee bumps: {hashes}", "not mined")
