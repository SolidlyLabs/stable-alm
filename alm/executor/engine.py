"""Turn a strategy decision into exact Solidly V3 transactions and (optionally) execute them.

This is the Python reference for the Rust app (rs/src/engine.rs): scripts/export_parity.py checks the
Rust port against it. The desktop app is the Rust one; this module is kept for research and parity.

One pass for one pool (`Executor.run`):
  1. read the wallet's live positions and balances and value them. The pool's budget is its
     `allocation_pct` of the whole account (all pools' positions + free tokens, in USD);
  2. decide the mode:
       exit      allocation 0 (or pool disabled) with open positions -> withdraw everything
       rebalance the strategy wants new ranges (ladder: every `every` bars and > `turnover` moves),
                 or the pool holds > 110% of its budget
       topup     usable idle tokens > `idle_redeploy_pct` of the budget (a failed or partial
                 deposit, or a raised allocation) -> deploy them into the current ranges
       keep      otherwise
  3. safety checks: kill switch, depeg guard, spot vs 1h median, gas balance, daily rebalance cap;
  4. execute; every step is re-read from the chain, simulated (eth_call + estimateGas), then sent:
       a. resize instead of rebuilding (`resize_plan`): a position whose range is not in the target
          is withdrawn in full; one whose range stays is trimmed (partial burnAndCollect) when it
          holds more than its target share, or topped up by minting into the same range when it
          holds less. Differences within `resize_tolerance_pct` of the budget are left alone.
          Burns use min amounts and a deadline and are retried; a failure aborts before anything
          else is touched;
       b. wait until the chain is past the withdraw block, so aggregator quotes are fresh;
       c. sell non-pool reward tokens (SOLID) into the pool's numeraire if the quote is good
          enough, so claimed rewards compound at this rebalance (skipped for SOLID while the
          SOLID LP option deposits it into a SOLID pool instead);
       d. fund an allocation shortfall from the funding token, through the aggregator;
       e. swap toward the target ratio through the Solidly UI meta-aggregator: sized at the pool
          price, re-sized at the quote's effective price, 0.1% input buffer, capped at
          max_swap_cost_bps vs the pool price, re-quoted on every retry;
       f. mint each range short of its target share (sized by the shortfall) from live balances; a
          failing simulation shrinks liquidity 0.5% up to 5 times, then the step is retried;
       g. leftover above the idle threshold -> one more swap + mint round into the same ranges.
     Anything that still fails leaves tokens idle in the wallet. The next pass sees them and runs
     `topup`, so a failed deposit recovers on its own.
In dry-run (the default) nothing is signed: balances are projected through the steps, and each
transaction is simulated against the real chain wherever its inputs already exist on-chain.
"""
import json
import os
import time
from dataclasses import dataclass, field

import numpy as np
import pandas as pd

from .. import logsetup
from ..data import live_market, load_events, wallet_events_path
from ..settings import SOLID, load_pools
from ..strategies import Ladder, Range24h, Recenter, StaticBand
from . import aggregator, chain, prices
from .tickmath import MAX_TICK, MIN_TICK, Q96, amounts_for_liquidity, liquidity_for_amounts, sqrt_ratio_at_tick

from ..paths import HOME as ROOT  # noqa: E402
LOG = os.path.join(ROOT, "logs", "executor.jsonl")
STOP_FILE = os.environ.get("ALM_STOP_FILE") or os.path.join(ROOT, "config", "STOP")  # kill switch
DEADLINE_S = 600


class Abort(RuntimeError):
    """Stop this step; tokens are wherever the last confirmed transaction left them."""


@dataclass
class PoolConfig:
    name: str
    pool: str
    decimals: int
    strategy: dict
    slippage_bps: float = 2.0
    max_spot_dev_ticks: int = 3
    max_actions_per_day: int = 5
    min_swap_value: float = 5.0  # in token0 units; smaller imbalances stay idle
    max_swap_cost_bps: float = 3.0  # aggregator quote vs pool price, fee included
    numeraire: int = 0
    allocation_pct: float = 0.0
    decimals1: int = None  # token1's decimals when they differ from token0's (read on-chain anyway)
    max_ranges: int = 5  # most positions per pool; more are merged into neighbours
    enabled: bool = True
    extra: dict = field(default_factory=dict)


def load_configs() -> list:
    fields = set(PoolConfig.__dataclass_fields__)
    return [PoolConfig(**{k: v for k, v in c.items() if k in fields}) for c in load_pools()]


def resize_plan(current: list, target: list, budget0: float, tol: float, dust: float) -> tuple:
    """Turn the current positions into the target with the fewest token movements.

    current: (tl, tu, liquidity, value) per open position; target: (tl, tu, weight) with weights
    summing to 1; values in token0 units. A range in both is resized in place: trimmed by a partial
    burn when it holds more than tol above its goal (weight * budget0), topped up when it holds
    more than tol below. Returns (burns, adds): burns = [(tl, tu, liquidity to remove)],
    adds = [(tl, tu, value to add)]. A trim that would leave less than `dust` burns the position."""
    goal: dict = {}
    for tl, tu, w in target:
        goal[(tl, tu)] = goal.get((tl, tu), 0.0) + w * budget0
    burns, adds, held = [], [], set()
    for tl, tu, liq, val in current:
        key = (tl, tu)
        g = goal.get(key)
        if g is None or key in held:
            burns.append((tl, tu, liq))
            continue
        held.add(key)
        delta = g - val
        if delta < -tol:
            cut = liq if g < dust or val <= 0 else min(liq, int(liq * (1 - g / val)))
            if cut > 0:
                burns.append((tl, tu, cut))
        elif delta > tol:
            adds.append((tl, tu, delta))
    for key, g in goal.items():
        if key not in held and g > dust:
            adds.append((key[0], key[1], g))
    return burns, sorted(adds)


def make_strategy(spec: dict):
    kind = spec["type"]
    params = {k: v for k, v in spec.items() if k != "type"}
    return {"ladder": Ladder, "static": StaticBand, "recenter": Recenter, "range24h": Range24h}[kind](**params)


def log(event: dict) -> None:
    os.makedirs(os.path.dirname(LOG), exist_ok=True)
    event = {"ts": int(time.time()), **event}
    with open(LOG, "a", encoding="utf-8") as f:
        f.write(json.dumps(event, default=str) + "\n")


def actions_today(pool: str) -> int:
    """Executed rebalances/top-ups/exits today (one = several transactions)."""
    if not os.path.exists(LOG):
        return 0
    day = int(time.time()) // 86400
    n = 0
    with open(LOG, encoding="utf-8") as f:
        for line in f:
            try:
                e = json.loads(line)
            except ValueError:
                continue
            if e.get("pool") == pool and e.get("kind") == "rebalance" and e.get("live") and e["ts"] // 86400 == day:
                n += 1
    return n


def minted_ranges(pool: str, lines: int = 5000) -> set:
    """(tl, tu) of every mint the executor sent for this pool, from the tail of the executor log."""
    if not os.path.exists(LOG):
        return set()
    out = set()
    with open(LOG, "rb") as f:
        f.seek(0, 2)
        f.seek(max(0, f.tell() - lines * 600))
        tail = f.read().decode("utf-8", "replace").splitlines()
    for line in tail:
        if '"sent"' not in line or '"mint ' not in line:
            continue
        try:
            e = json.loads(line)
        except ValueError:
            continue
        if e.get("kind") == "sent" and str(e.get("pool", "")).lower() == pool.lower() and str(e.get("what", "")).startswith("mint "):
            tl, tu = e["what"][5:].split(":")
            out.add((int(tl), int(tu)))
    return out


def wallet_positions(pool: str, wallet: str) -> list:
    """Open ranges from the wallet's Mint/Burn history, confirmed against positions() on-chain."""
    wpath = wallet_events_path(pool, wallet)
    if os.path.exists(wpath):
        lp = pd.read_parquet(wpath)
    else:
        try:
            ev = load_events(pool)
            lp = ev[(ev.kind != "swap") & (ev.owner == wallet.lower())]
        except FileNotFoundError:  # pool not synced yet: scan the wallet's Mint/Burn logs directly
            from ..datasync import wallet_history

            lp = wallet_history(pool.lower(), wallet, int(chain.call("eth_blockNumber", []), 16))
    keys = {(int(r.tick_lower), int(r.tick_upper)) for r in lp.itertuples()} if len(lp) else set()
    # Ranges minted by the executor since the history was last synced (the sync runs once per
    # cycle, so right after a rebalance the new ranges are only in the executor log).
    keys |= minted_ranges(pool)
    out = []
    for tl, tu in sorted(keys):
        liq = chain.position_liquidity(pool, wallet, tl, tu)
        if liq > 0:
            out.append((tl, tu, liq))
    return out


def cap_ranges(ranges: list, n: int) -> list:
    """At most n positions: neighbouring ranges are merged into n contiguous groups of about equal
    weight (each group spans from its first lower to its last upper tick). Fewer positions = fewer
    transactions per rebalance; a volatile pair's ladder can otherwise ask for dozens."""
    ranges = sorted(ranges)
    if n <= 0 or len(ranges) <= n:
        return ranges
    total = sum(w for *_, w in ranges) or 1.0
    groups, cur, acc = [], [], 0.0
    for r in ranges:
        cur.append(r)
        acc += r[2]
        if acc >= total * (len(groups) + 1) / n and len(groups) < n - 1:
            groups.append(cur)
            cur = []
    if cur:
        groups.append(cur)
    return [(g[0][0], g[-1][1], sum(x[2] for x in g)) for g in groups]


def snap_ranges(ranges: list, spacing: int) -> list:
    """Align (tl, tu, w) ranges to the pool's tick spacing; merge ranges that collapse together."""
    if spacing <= 1:
        return sorted(ranges)
    lo_b, hi_b = -(-MIN_TICK // spacing) * spacing, (MAX_TICK // spacing) * spacing
    merged: dict = {}
    for tl, tu, w in ranges:
        a = max((tl // spacing) * spacing, lo_b)
        b = min(-(-tu // spacing) * spacing, hi_b)
        if b <= a:
            b = a + spacing
        merged[(a, b)] = merged.get((a, b), 0.0) + w
    return [(a, b, w) for (a, b), w in sorted(merged.items())]


class Executor:
    def __init__(self, cfg: PoolConfig, wallet: str, settings: dict, execute: bool = False, budget_usd: float = None):
        self.cfg, self.wallet, self.settings, self.execute = cfg, wallet.lower(), settings, execute
        self.log = logsetup.get(f"pool.{cfg.name}")
        self.account = chain.load_account() if execute else None
        if self.account and self.account.address.lower() != self.wallet:
            raise RuntimeError(f"signer {self.account.address} does not match the configured wallet {self.wallet}")
        self.t0, self.t1 = (t.lower() for t in chain.pool_tokens(cfg.pool))
        self.spacing = chain.tick_spacing(cfg.pool)
        # Per-token decimals from the chain: values are kept in token0 units (raw / 10**dec0); raw
        # prices (token1 raw per token0 raw) make that exact for any decimal combination.
        self.dec0, self.dec1 = prices.meta(self.t0)["decimals"], prices.meta(self.t1)["decimals"]
        self.unit, self.unit1 = 10 ** self.dec0, 10 ** self.dec1
        self.budget_usd = budget_usd
        self.attempts = max(1, int(settings["retries"]["attempts"]))
        self.backoff = float(settings["retries"]["backoff_s"])
        # Dry run: projected token deltas, allowances and deployed value through the steps.
        self.virtual: dict = {}
        self.virtual_allow: dict = {}
        self.v_deployed = 0.0
        self.v_removed = 0.0
        self.withdrawn: dict = {}  # (tl, tu) -> liquidity withdrawn this pass (live)
        self.pos_val0 = 0.0
        self.report = []

    # -- helpers ---------------------------------------------------------------------------------
    def say(self, msg: str, level: str = "info") -> None:
        self.report.append(msg)
        getattr(self.log, level)(msg)

    def bal(self, token: str) -> int:
        return chain.erc20_balance(token, self.wallet) + self.virtual.get(token.lower(), 0)

    def move(self, token: str, delta: int) -> None:
        if not self.execute:
            self.virtual[token.lower()] = self.virtual.get(token.lower(), 0) + int(delta)

    def price(self) -> tuple:
        sp, tick, fee = chain.slot0(self.cfg.pool)
        return sp, tick, fee, (sp / Q96) ** 2

    def value0(self, x: int, y: int, p: float) -> float:
        return (x + y / p) / self.unit

    def positions(self) -> list:
        return wallet_positions(self.cfg.pool, self.wallet)

    def deployed_value(self) -> float:
        """Value (token0 units) currently in this pool's positions."""
        if not self.execute:
            return self.pos_val0 - self.v_removed + self.v_deployed
        sp, _, _, p = self.price()
        v = 0.0
        for tl, tu, liq in self.positions():
            a0, a1 = amounts_for_liquidity(sp, sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu), liq)
            v += self.value0(a0, a1, p)
        return v

    def usable(self, budget0: float, p: float) -> tuple:
        """Wallet amounts of the pool's tokens it may still deploy, scaled down to the budget room."""
        b0, b1 = max(self.bal(self.t0), 0), max(self.bal(self.t1), 0)
        have = self.value0(b0, b1, p)
        room = budget0 - self.deployed_value()
        if have <= 0 or room <= 0:
            return 0, 0
        k = min(1.0, room / have)
        return int(b0 * k), int(b1 * k)

    def retry(self, what: str, fn, *args):
        """Run fn with retries; failures are logged and retried with a growing backoff."""
        last = None
        for attempt in range(1, self.attempts + 1):
            try:
                return fn(*args)
            except Abort:
                raise
            except Exception as e:  # noqa: BLE001 - every failure is logged and retried
                last = e
                final = attempt == self.attempts
                self.say(f"  {what}: attempt {attempt}/{self.attempts} failed: {e}", "error" if final else "warning")
                log({"pool": self.cfg.pool, "kind": "error" if final else "retry", "what": what, "attempt": attempt,
                     "error": str(e)})
                if not final:
                    time.sleep(self.backoff * attempt)
        raise Abort(f"{what} failed after {self.attempts} attempts: {last}")

    def tx(self, to: str, data: str, what: str, live: bool) -> dict:
        """Simulate, then send if executing. `live` = this step can be simulated on the current chain."""
        entry = {"pool": self.cfg.pool, "what": what, "to": to, "data": data}
        if live or self.execute:
            sim = chain.simulate(self.wallet, to, data)
            entry["gas"] = sim["gas"]
            self.say(f"    simulated OK, gas {sim['gas']:,}")
        else:
            self.say("    (projected: rebuilt and simulated live after the previous step)")
        if self.execute:
            res = chain.send(self.account, to, data, entry["gas"])
            entry.update(kind="sent", **res)
            self.say(f"    sent {res['hash']} (block {res['block']}, gas {res['gas_used']:,})")
        else:
            entry["kind"] = "dry_run"
        log(entry)
        return entry

    # -- checks ----------------------------------------------------------------------------------
    def checks(self, market, tick: int) -> list:
        problems = []
        if os.path.exists(STOP_FILE) or os.environ.get("ALM_DISABLED") == "1":
            problems.append("kill switch is on (STOP file or ALM_DISABLED=1)")
        recent = market.tick[market.ts >= market.ts[-1] - 3600]
        med = float(np.median(recent)) if len(recent) else tick
        if abs(tick - med) > self.cfg.max_spot_dev_ticks:
            problems.append(f"spot tick {tick} is {abs(tick - med):.0f} ticks from the 1h median {med:.0f} "
                            "(possible manipulation or fast move)")
        if chain.eth_balance(self.wallet) < 2 * 10**14:
            problems.append("wallet has < 0.0002 ETH for gas")
        if actions_today(self.cfg.pool) >= self.cfg.max_actions_per_day:
            problems.append("daily rebalance cap reached")
        return problems

    def blocked(self, market, tick: int, exit_only: bool = False) -> bool:
        problems = self.checks(market, tick)
        if exit_only:  # withdrawing is allowed unless the kill switch is on or gas is missing
            problems = [x for x in problems if "kill switch" in x or "ETH for gas" in x]
        for x in problems:
            self.say(f"  BLOCKED: {x}", "warning")
        if problems:
            log({"pool": self.cfg.pool, "kind": "blocked", "problems": problems})
        return bool(problems)

    def begin(self, mode: str, target=None) -> None:
        log({"pool": self.cfg.pool, "kind": "rebalance", "mode": mode, "live": self.execute,
             "ranges": target.ranges if target else []})

    # -- main ------------------------------------------------------------------------------------
    def run(self, market) -> dict:
        from ..sim import State

        cfg = self.cfg
        sp, tick, fee, p = self.price()
        positions = self.positions()
        b0, b1 = self.bal(self.t0), self.bal(self.t1)
        self.say(f"[{cfg.name}] tick {tick}  fee {fee} pips  wallet {b0 / self.unit:,.4f} t0 + {b1 / self.unit1:,.4f} t1")
        for tl, tu, liq in positions:
            a0, a1 = amounts_for_liquidity(sp, sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu), liq)
            self.pos_val0 += self.value0(a0, a1, p)
            self.say(f"  position [{tl},{tu}) {a0 / self.unit:,.4f} t0 + {a1 / self.unit1:,.4f} t1 "
                     f"{'IN RANGE' if tl <= tick < tu else 'out of range'}")
        pos_val = self.pos_val0
        usd0 = prices.usd(self.t0)
        idle0 = self.value0(b0, b1, p)
        budget0 = (self.budget_usd / usd0) if self.budget_usd is not None else pos_val + idle0
        self.say(f"  deployed {pos_val:,.4f}  idle {idle0:,.4f}  budget {budget0:,.4f} (token0 units, "
                 f"{cfg.allocation_pct:g}% of the account)")
        summary = {"pool": cfg.pool, "name": cfg.name, "tick": tick, "fee_pips": fee, "deployed0": pos_val,
                   "idle0": idle0, "budget0": budget0, "usd0": usd0, "positions": positions, "mode": "keep"}

        if (budget0 <= 0 or not cfg.enabled) and positions:
            self.say("  allocation is 0 / pool disabled: withdrawing all positions")
            summary["mode"] = "exit"
            if not self.blocked(market, tick, exit_only=True):
                self.begin("exit")
                self.burn_all(positions)
            return summary
        if budget0 <= 0 or not cfg.enabled:
            return summary

        strat = make_strategy(cfg.strategy)
        strat.prepare(market)
        strat.reset()
        bidx = len(market.bar_end) - 1
        # The ladder sizes its allocation from st.x: the pool's budget, in token0 units.
        sst = State(i=int(market.bar_idx[bidx]), bar=bidx, tick=tick, sp=sp / Q96,
                    ranges=[(tl, tu, liq / market.liq_scale) for tl, tu, liq in positions], x=budget0, y=0.0,
                    last_action_bar=-10**9,
                    bars_out_of_range=0 if any(tl <= tick < tu for tl, tu, _ in positions) else 10**6, market=market)
        if strat.frozen(sst):
            self.say(f"  DEPEG GUARD: holding, no action (tick {tick} too far from the 7-day median)")
            log({"pool": cfg.pool, "kind": "guard_freeze", "tick": tick})
            summary["mode"] = "guard"
            return summary

        target = self.strategy_target(strat, sst, positions, sp, p)
        idle_usable = min(idle0, max(budget0 - pos_val, 0.0))
        thr = budget0 * float(self.settings["idle_redeploy_pct"]) / 100
        if target is None and pos_val > budget0 * 1.10:
            self.say(f"  pool holds {pos_val:,.2f} > 110% of its budget {budget0:,.2f}: rebalance down to budget")
            target = self.current_as_target(positions, sp, p) or self.fresh_target(strat, sst)
        if target is not None:
            summary["mode"] = "rebalance"
            self.say(f"  {strat.label()} wants: " + ", ".join(f"[{a},{c}) {w:.0%}" for a, c, w in target.ranges))
            if self.blocked(market, tick):
                return summary
            self.begin("rebalance", target)
            self.resize(positions, target, budget0, sp, p)
        elif idle_usable > max(thr, cfg.min_swap_value):
            summary["mode"] = "topup"
            target = self.current_as_target(positions, sp, p) or self.fresh_target(strat, sst)
            if target is None:
                return summary
            self.say(f"  {idle_usable:,.4f} idle (> {thr:,.4f}): deploying into " +
                     ", ".join(f"[{a},{c})" for a, c, _ in target.ranges))
            if self.blocked(market, tick):
                return summary
            self.begin("topup", target)
            self.deploy(target, budget0)
        else:
            self.say(f"  {strat.label()}: keep current position")
            log({"pool": cfg.pool, "kind": "keep", "tick": tick})
        return summary

    # -- targets ---------------------------------------------------------------------------------
    def strategy_target(self, strat, sst, positions, sp, p):
        cfg = self.cfg
        if isinstance(strat, Ladder):
            # The backtest re-solves every `every` bars; the executor runs every few minutes, so it
            # keeps that cadence itself. Only real runs move the clock.
            state_path = os.path.join(ROOT, "logs", f"last_solve_{cfg.name}.json")
            last = json.load(open(state_path))["ts"] if os.path.exists(state_path) else 0
            period = strat.every * 300
            if positions and time.time() - last < period:
                self.say(f"  ladder: next re-solve in {(period - (time.time() - last)) / 3600:.1f} h")
                return None
            if self.execute:
                os.makedirs(os.path.dirname(state_path), exist_ok=True)
                json.dump({"ts": int(time.time())}, open(state_path, "w"))
            strat.every = 1
        elif positions:
            tl, tu, _ = max(positions, key=lambda r: r[2])
            strat.cur = ((tl + tu - 1) // 2, (tu - tl - 1) // 2)
        target = strat.decide(sst)
        if target is None:
            return None
        target.ranges = cap_ranges(snap_ranges(target.ranges, self.spacing), self.cfg.max_ranges)
        if isinstance(strat, Ladder) and positions:
            cur_w: dict = {}
            for tl, tu, liq in positions:
                a0, a1 = amounts_for_liquidity(sp, sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu), liq)
                for t in range(tl, tu):
                    cur_w[t] = cur_w.get(t, 0) + self.value0(a0, a1, p) / (tu - tl)
            s = sum(cur_w.values()) or 1
            new_w: dict = {}
            for a, c, w in target.ranges:
                for t in range(a, c):
                    new_w[t] = new_w.get(t, 0) + w / (c - a)
            moved = sum(abs(new_w.get(t, 0) - cur_w.get(t, 0) / s) for t in set(cur_w) | set(new_w)) / 2
            if moved < strat.turnover:
                self.say(f"  ladder target differs by {moved:.0%} < turnover {strat.turnover:.0%}: keep")
                return None
        return target

    def fresh_target(self, strat, sst):
        strat.reset()
        if isinstance(strat, Ladder):
            strat.every = 1
        t = strat.decide(sst)
        if t is not None:
            t.ranges = cap_ranges(snap_ranges(t.ranges, self.spacing), self.cfg.max_ranges)
        return t

    def current_as_target(self, positions, sp, p):
        from ..sim import Target

        if not positions:
            return None
        vals = []
        for tl, tu, liq in positions:
            a0, a1 = amounts_for_liquidity(sp, sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu), liq)
            vals.append((tl, tu, self.value0(a0, a1, p)))
        s = sum(v for *_, v in vals) or 1
        return Target([(tl, tu, v / s) for tl, tu, v in vals], swap=True)

    # -- a. resize / withdraw --------------------------------------------------------------------
    def resize(self, positions, target, budget0: float, sp: int, p: float) -> None:
        """Rebalance by difference: withdraw dropped ranges, trim or top up the ranges that stay."""
        from ..sim import Target

        w = np.array([r[2] for r in target.ranges], dtype=float)
        ranges = [(tl, tu, wi) for (tl, tu, _), wi in zip(target.ranges, w / w.sum())]
        current = []
        for tl, tu, liq in positions:
            a0, a1 = amounts_for_liquidity(sp, sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu), liq)
            current.append((tl, tu, liq, self.value0(a0, a1, p)))
        tol = max(self.cfg.min_swap_value, budget0 * float(self.settings.get("resize_tolerance_pct", 1.0)) / 100)
        burns, adds = resize_plan(current, ranges, budget0, tol, self.cfg.min_swap_value)
        liq_of = {(tl, tu): liq for tl, tu, liq in positions}
        partial = sum(c < liq_of[(a, b)] for a, b, c in burns)
        full = len(burns) - partial
        into = sum((a, b) in liq_of for a, b, _ in adds)
        self.say(f"  plan: withdraw {full} position(s), trim {partial}, add to {into} existing, "
                 f"open {len(adds) - into} new, leave {len(positions) - len(burns) - into} as they are "
                 f"(tolerance {tol:,.4f})")
        log({"pool": self.cfg.pool, "kind": "plan", "burns": burns, "adds": adds, "tolerance": tol})
        self.burn_all(burns)
        if adds:
            self.deploy(Target(adds, swap=target.swap), budget0)
        else:
            self.say("  nothing to deposit; withdrawn tokens stay in the wallet")

    def burn_all(self, positions) -> None:
        """Withdraw (tl, tu, liquidity) per position; the liquidity may be only part of it."""
        last_block = 0
        for tl, tu, liq in positions:
            res = self.retry(f"withdraw [{tl},{tu})", self.burn_one, tl, tu, liq)
            if res and res.get("block"):
                last_block = max(last_block, res["block"])
        if self.execute and last_block:
            # b. fresh quotes only: wait until the chain is past the withdraw block, then a short
            # settle (the route may pass through this very pool).
            t0 = time.time()
            while int(chain.call("eth_blockNumber", []), 16) <= last_block and time.time() - t0 < 60:
                time.sleep(1)
            time.sleep(4)

    def burn_one(self, tl: int, tu: int, amount: int = None):
        if (tl, tu) in self.withdrawn:  # a retry after the burn landed must not withdraw again
            self.say(f"  a. [{tl},{tu}) already withdrawn this pass; skip")
            return None
        have = chain.position_liquidity(self.cfg.pool, self.wallet, tl, tu)
        liq = have if amount is None else min(amount, have)
        if liq == 0:
            self.say(f"  a. [{tl},{tu}) already empty; skip")
            return None
        sp, _, _, _ = self.price()
        slip = self.cfg.slippage_bps / 1e4
        a0, a1 = amounts_for_liquidity(sp, sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu), liq)
        data = chain.calldata(
            "burnAndCollect(address,int24,int24,uint128,uint256,uint256,uint128,uint128,uint256)",
            ["address", "int24", "int24", "uint128", "uint256", "uint256", "uint128", "uint128", "uint256"],
            [self.wallet, tl, tu, liq, int(a0 * (1 - slip)), int(a1 * (1 - slip)), chain.MAX_UINT128, chain.MAX_UINT128,
             int(time.time()) + DEADLINE_S])
        part = f" ({liq / have:.0%} of the position)" if liq < have else ""
        self.say(f"  a. burnAndCollect [{tl},{tu}) liquidity {liq}{part} -> ~{a0 / self.unit:,.4f} t0 + "
                 f"{a1 / self.unit1:,.4f} t1")
        res = self.tx(self.cfg.pool, data, f"burn {tl}:{tu}", live=True)
        if self.execute:
            self.withdrawn[(tl, tu)] = liq
        self.move(self.t0, a0)
        self.move(self.t1, a1)
        self.v_removed += self.value0(a0, a1, self.price()[3])
        return res

    # -- c-g. deploy -----------------------------------------------------------------------------
    def deploy(self, target, budget0: float) -> None:
        w = np.array([r[2] for r in target.ranges], dtype=float)
        w = w / w.sum()
        ranges = sorted((tl, tu, wi) for (tl, tu, _), wi in zip(target.ranges, w))
        self.sell_rewards()
        self.fund(budget0, ranges)
        thr = budget0 * float(self.settings["idle_redeploy_pct"]) / 100
        minted_any = False
        for rnd in range(2):
            if target.swap:
                try:
                    self.retry("ratio swap", self.ratio_swap, ranges, budget0)
                except Abort as e:
                    self.say(f"  swap skipped: {e}; minting with the balances on hand", "warning")
            for i, (tl, tu, wi) in enumerate(ranges):
                share = wi / sum(x[2] for x in ranges[i:])  # of what is still unminted this round
                try:
                    if self.retry(f"mint [{tl},{tu})", self.mint_one, tl, tu, share, budget0):
                        minted_any = True
                except Abort as e:
                    self.say(f"  mint [{tl},{tu}) gave up: {e}; tokens stay idle for the next pass", "error")
            if not self.execute:
                break
            _, _, _, p = self.price()
            x, y = self.usable(budget0, p)
            left = self.value0(x, y, p)
            if left <= max(thr, self.cfg.min_swap_value):
                break
            self.say(f"  {left:,.4f} usable left over (> {thr:,.4f}): top-up round {rnd + 2}")
        if not minted_any:
            self.say("  nothing minted this pass; tokens are idle and the next pass will redeploy them", "warning")
        self.say(f"  left idle: {self.bal(self.t0) / self.unit:,.4f} t0 + {self.bal(self.t1) / self.unit1:,.4f} t1")

    def need0_fraction(self, ranges, sp: int, p: float) -> float:
        """Fraction of value the target ranges hold in token0 at the current price."""
        f = 0.0
        for tl, tu, wi in ranges:
            a0, a1 = amounts_for_liquidity(sp, sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu), 10**18)
            f += wi * (a0 / (a0 + a1 / p) if (a0 + a1) else 1.0)
        return f

    def ratio_swap(self, ranges, budget0: float) -> None:
        sp, _, _, p = self.price()
        x, y = self.usable(budget0, p)
        f = self.need0_fraction(ranges, sp, p)
        diff0 = f * (x + y / p) - x  # > 0: need more token0 -> sell token1
        if abs(diff0) / self.unit <= self.cfg.min_swap_value:
            self.say("  e. ratio already close enough; no swap")
            return
        zero_for_one = diff0 < 0
        tin, tout = (self.t0, self.t1) if zero_for_one else (self.t1, self.t0)
        amount_in = int(-diff0) if zero_for_one else int(diff0 * p)
        # Two-pass sizing: re-size at the quote's effective rate e (out per in), then a 0.1% buffer.
        q = aggregator.quote(tin, tout, amount_in, prices.meta(tin)["decimals"], prices.meta(tout)["decimals"])
        e = int((q.get("best") or {}).get("amountOut") or 0) / amount_in if amount_in else 0
        if e > 0:
            if zero_for_one:  # sell s token0: (x - s) = f * ((x - s) + (y + s e) / p)
                s = (x * (1 - f) - f * y / p) / ((1 - f) + f * e / p)
            else:  # sell t token1: (x + t e) = f * ((x + t e) + (y - t) / p)
                s = (f * (x + y / p) - x) / (e * (1 - f) + f / p)
            amount_in = int(max(s, 0))
        amount_in = min(int(amount_in * 0.999), max(self.bal(tin), 0))
        if amount_in <= 0:
            return
        fair_out = amount_in * p if zero_for_one else amount_in / p
        self.swap(tin, tout, amount_in, fair_out, self.cfg.max_swap_cost_bps, "e. ratio swap")

    def swap(self, tin: str, tout: str, amount_in: int, fair_out: float, max_cost_bps: float, label: str) -> int:
        """One aggregator swap: quote, cost check, exact approval to the quoted spender, build,
        simulate, send. Returns the amount received (projected in dry run)."""
        din, dout = prices.meta(tin)["decimals"], prices.meta(tout)["decimals"]
        q = aggregator.quote(tin, tout, amount_in, din, dout)
        best = q.get("best")
        if not best:
            raise RuntimeError(f"no aggregator route: {q.get('failures')}")
        out = int(best["amountOut"])
        if fair_out:
            cost = (1 - out / fair_out) * 1e4
        else:  # no pool price to compare with: use the aggregator's own USD valuation
            iu, ou = float(best.get("amountInUsd") or 0), float(best.get("amountOutUsd") or 0)
            cost = (1 - ou / iu) * 1e4 if iu else 1e9
        venues = sorted({v for leg in best.get("legs", []) for st in leg.get("steps", []) for v in st.get("venues", [])})
        self.say(f"  {label}: {amount_in / 10**din:,.6f} {prices.meta(tin)['symbol']} -> {out / 10**dout:,.6f} "
                 f"{prices.meta(tout)['symbol']} via {best['aggregatorId']} ({', '.join(venues)}), cost {cost:+.2f} bp")
        if cost > max_cost_bps:
            raise RuntimeError(f"swap would cost {cost:.2f} bp > max {max_cost_bps} bp")
        self.ensure_allowance(tin, amount_in, spender=best["spender"])
        built = aggregator.build(best, self.wallet, self.cfg.slippage_bps / 1e4)
        if built["routerAddress"].lower() != best["routerAddress"].lower():
            raise RuntimeError("router changed between quote and build; re-quoting")
        before = chain.erc20_balance(tout, self.wallet)
        self.tx(built["routerAddress"], built["calldata"], f"swap via {best['aggregatorId']}",
                live=chain.erc20_balance(tin, self.wallet) >= amount_in)
        if self.execute:
            got = chain.erc20_balance(tout, self.wallet) - before
            self.say(f"    received {got / 10**dout:,.6f}")
            return got
        self.move(tin, -amount_in)
        self.move(tout, out)
        return out

    def mint_one(self, tl: int, tu: int, share: float, budget0: float) -> bool:
        sp, _, _, p = self.price()
        sa, sb = sqrt_ratio_at_tick(tl), sqrt_ratio_at_tick(tu)
        x, y = self.usable(budget0, p)
        a0, a1 = amounts_for_liquidity(sp, sa, sb, 10**18)
        f0 = a0 / (a0 + a1 / p) if (a0 + a1) else 1.0
        v0 = (x + y / p) * share
        x_r, y_r = min(int(f0 * v0), x), min(int((1 - f0) * v0 * p), y)
        liq = int(liquidity_for_amounts(sp, sa, sb, x_r, y_r) * 0.999)
        if liq > 0:
            m0, m1 = amounts_for_liquidity(sp, sa, sb, liq)
            if self.value0(m0, m1, p) < self.cfg.min_swap_value:  # dust: not worth a transaction
                liq = 0
        if liq <= 0:
            self.say(f"  f. mint [{tl},{tu}): nothing to add (usable {x / self.unit:,.4f} t0 / {y / self.unit1:,.4f} t1)")
            return False
        slip = self.cfg.slippage_bps / 1e4
        for _ in range(6):  # UI pattern: a failing simulation shrinks liquidity 0.5% and tries again
            m0, m1 = amounts_for_liquidity(sp, sa, sb, liq)
            self.ensure_allowance(self.t0, m0 + m0 // 1000 + 10)
            self.ensure_allowance(self.t1, m1 + m1 // 1000 + 10)
            data = chain.calldata(
                "mint(address,int24,int24,uint128,uint256,uint256,uint256)",
                ["address", "int24", "int24", "uint128", "uint256", "uint256", "uint256"],
                [self.wallet, tl, tu, liq, int(m0 * (1 - slip)), int(m1 * (1 - slip)), int(time.time()) + DEADLINE_S])
            self.say(f"  f. mint [{tl},{tu}) liquidity {liq} uses {m0 / self.unit:,.4f} t0 + {m1 / self.unit1:,.4f} t1")
            live = (chain.erc20_balance(self.t0, self.wallet) >= m0 and chain.erc20_balance(self.t1, self.wallet) >= m1
                    and chain.erc20_allowance(self.t0, self.wallet, self.cfg.pool) >= m0
                    and chain.erc20_allowance(self.t1, self.wallet, self.cfg.pool) >= m1)
            try:
                self.tx(self.cfg.pool, data, f"mint {tl}:{tu}", live=live)
            except chain.TxError as e:
                if str(e).startswith(("eth_call reverted", "estimateGas failed")):
                    liq = int(liq * 0.995)
                    self.say(f"    simulation failed ({e.reason}); shrinking liquidity 0.5%", "warning")
                    continue
                raise
            self.move(self.t0, -m0)
            self.move(self.t1, -m1)
            if not self.execute:
                self.v_deployed += self.value0(m0, m1, p)
            return True
        raise RuntimeError(f"mint [{tl},{tu}) still fails after shrinking liquidity by ~3%")

    def sell_rewards(self) -> None:
        """c. compound claimed non-pool reward tokens (SOLID) at this rebalance."""
        sr = self.settings["sell_rewards"]
        if not sr.get("enabled"):
            return
        into = self.t0 if self.cfg.numeraire == 0 else self.t1
        for token in sr.get("tokens", []):
            token = token.lower()
            if token in (self.t0, self.t1):
                continue
            if token == SOLID and self.settings.get("solid_lp", {}).get("enabled"):
                continue  # claimed SOLID goes into the SOLID pool instead (solid_lp)
            amount = self.bal(token)
            if amount <= 0:
                continue
            try:
                self.retry(f"sell reward {prices.meta(token)['symbol']}", self._sell_reward, token, into, amount, sr)
            except Abort as e:
                self.say(f"  reward sale skipped: {e}", "warning")

    def _sell_reward(self, token: str, into: str, amount: int, sr: dict) -> None:
        """Largest chunk (all, 1/2, 1/4, ...) whose quote loses <= max_impact_pct of its USD value."""
        din, dout = prices.meta(token)["decimals"], prices.meta(into)["decimals"]
        sym = prices.meta(token)["symbol"]
        chunk = amount
        for _ in range(6):
            best = aggregator.quote(token, into, chunk, din, dout).get("best") or {}
            iu, ou = float(best.get("amountInUsd") or 0), float(best.get("amountOutUsd") or 0)
            loss = (1 - ou / iu) * 100 if iu else 100.0
            if ou < sr["min_value_usd"]:
                self.say(f"  c. reward {sym}: {chunk / 10**din:,.2f} is worth ${ou:.2f} < ${sr['min_value_usd']}; kept")
                return
            if loss <= sr["max_impact_pct"]:
                self.swap(token, into, chunk, 0, sr["max_impact_pct"] * 100, f"c. reward sale ({loss:.1f}% impact)")
                return
            chunk //= 2
        self.say(f"  c. reward {sym}: impact above {sr['max_impact_pct']}% even for small amounts; kept")

    def fund(self, budget0: float, ranges) -> None:
        """d. buy an allocation shortfall with the funding tokens (the account split across pools).

        Funding tokens that are this pool's own tokens are skipped (the ratio swap balances them).
        The others are used largest USD balance first until the shortfall is covered."""
        fts = [t for t in self.settings.get("funding_tokens") or [] if t not in (self.t0, self.t1)]
        if not fts:
            return
        sp, _, _, p = self.price()
        have = self.value0(max(self.bal(self.t0), 0), max(self.bal(self.t1), 0), p) + self.deployed_value()
        short_usd = (budget0 - have) * prices.usd(self.t0)
        if short_usd < 10.0:
            return
        avail = []
        for ft in fts:
            try:
                b = max(self.bal(ft), 0)
                avail.append((b / 10 ** prices.meta(ft)["decimals"] * prices.usd(ft), ft, b))
            except Exception as e:  # noqa: BLE001 - an unpriceable token is skipped
                self.say(f"  d. funding: {ft[:10]} skipped ({e})", "warning")
        into = self.t0 if self.need0_fraction(ranges, sp, p) >= 0.5 else self.t1
        for usd_free, ft, b in sorted(avail, reverse=True):
            if short_usd < 10.0:
                break
            if usd_free < 1.0:
                continue
            px, dft = prices.usd(ft), prices.meta(ft)["decimals"]
            amount = min(b, int(short_usd / px * 10**dft))
            try:
                self.retry(f"funding swap {prices.meta(ft)['symbol']}", self.swap, ft, into, amount, 0,
                           max(self.cfg.max_swap_cost_bps, 10.0), "d. funding swap")
                short_usd -= amount / 10**dft * px
            except Abort as e:
                self.say(f"  funding from {prices.meta(ft)['symbol']} skipped: {e}", "warning")
        if short_usd >= 10.0:
            self.say(f"  d. funding: still ${short_usd:,.2f} short; the funding tokens are used up", "warning")

    def ensure_allowance(self, token: str, amount: int, spender: str = None) -> None:
        """Exact-amount approvals only (never unlimited). A non-zero allowance that is too small is
        reset to 0 first, because USDT-style tokens refuse to change a non-zero allowance."""
        spender = (spender or self.cfg.pool).lower()
        key = (token.lower(), spender)
        onchain = chain.erc20_allowance(token, self.wallet, spender)
        cur = self.virtual_allow.get(key, onchain)
        if amount <= 0 or cur >= amount:
            return
        sym, dec = prices.meta(token)["symbol"], prices.meta(token)["decimals"]
        if cur > 0:
            self.say(f"  approve {sym} -> {spender[:10]}: reset to 0 first")
            data = chain.calldata("approve(address,uint256)", ["address", "uint256"], [spender, 0])
            self.retry("approve reset", self.tx, token, data, f"approve0 {sym}", True)
        self.say(f"  approve {sym} -> {spender[:10]} for {amount / 10**dec:,.6f}")
        data = chain.calldata("approve(address,uint256)", ["address", "uint256"], [spender, amount])
        self.retry("approve", self.tx, token, data, f"approve {sym}", True)
        if not self.execute:
            self.virtual_allow[key] = amount


def market_for(cfg: PoolConfig, wallet: str, days: int = 10):
    t0, t1 = chain.pool_tokens(cfg.pool)
    return live_market(cfg.pool, wallet, prices.meta(t0)["decimals"], cfg.numeraire, days, prices.meta(t1)["decimals"])
